//! Clock bridge for a device channel (the mic): two streams and a ring.
//!
//! The capture side (`pipemic.dev.<id>.in`) runs on the device's clock and
//! writes into the ring. The playback side (`pipemic.dev.<id>`) runs on the
//! mixes' clock, reads from the ring and nudges its resampler so the ring
//! stays near a target fill. Neither side waits for the other.
//!
//! This replaces libpipewire-module-loopback, whose playback side is always
//! a trigger node run by its capture side (PipeWire 1.6 module-loopback.c,
//! `PW_STREAM_FLAG_TRIGGER`). That tied the whole output graph to the mic's
//! cycle: each output cycle waited up to 8 ms for the Wave:3 (measured
//! 2026-11-10, pw-profiler), and incoming call audio crackled.

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Ring capacity in frames (power of two, ~340 ms at 48 kHz).
const CAP: usize = 1 << 14;
/// Fill the playback side keeps before reading: one 1024 capture step plus
/// margin for the phase between the two clocks (~43 ms).
const TARGET: f64 = 2048.0;
/// Above this fill the ring is cut back to the target (a stall upstream).
const MAX_FILL: usize = 6144;
/// Rate correction limits (±0.3 % covers any two real audio clocks).
const MAX_CORR: f64 = 0.003;
const KP: f64 = 2e-6;
const KI: f64 = 2e-9;

/// Single-producer single-consumer ring of mono f32 frames.
struct Ring {
    buf: Box<[AtomicU32]>,
    write: AtomicUsize,
    read: AtomicUsize,
}

impl Ring {
    fn new() -> Self {
        Self { buf: (0..CAP).map(|_| AtomicU32::new(0)).collect(), write: AtomicUsize::new(0), read: AtomicUsize::new(0) }
    }
    fn fill(&self) -> usize {
        self.write.load(Ordering::Acquire).wrapping_sub(self.read.load(Ordering::Acquire))
    }
    /// Producer: append, dropping what doesn't fit.
    fn push(&self, frames: impl Iterator<Item = f32>) {
        let r = self.read.load(Ordering::Acquire);
        let mut w = self.write.load(Ordering::Relaxed);
        for v in frames {
            if w.wrapping_sub(r) >= CAP {
                break;
            }
            self.buf[w % CAP].store(v.to_bits(), Ordering::Relaxed);
            w = w.wrapping_add(1);
        }
        self.write.store(w, Ordering::Release);
    }
    /// Consumer: read up to `out.len()` frames, zero the rest. Returns frames read.
    fn pop(&self, out: &mut [f32]) -> usize {
        let w = self.write.load(Ordering::Acquire);
        let mut r = self.read.load(Ordering::Relaxed);
        let n = w.wrapping_sub(r).min(out.len());
        for o in out[..n].iter_mut() {
            *o = f32::from_bits(self.buf[r % CAP].load(Ordering::Relaxed));
            r = r.wrapping_add(1);
        }
        out[n..].fill(0.0);
        self.read.store(r, Ordering::Release);
        n
    }
    /// Consumer: drop frames so that `keep` remain.
    fn skip_to(&self, keep: usize) {
        let w = self.write.load(Ordering::Acquire);
        let r = self.read.load(Ordering::Relaxed);
        if w.wrapping_sub(r) > keep {
            self.read.store(w.wrapping_sub(keep), Ordering::Release);
        }
    }
}

/// Counters the main thread logs now and then.
#[derive(Default)]
pub struct Stats {
    pub underruns: AtomicU64,
    pub overruns: AtomicU64,
    /// Smoothed fill in frames and rate correction in ppm, f32 bits.
    pub fill: AtomicU32,
    pub ppm: AtomicU32,
    /// Frames the output asked for in its last cycle.
    pub requested: AtomicU32,
    /// 0 = waiting for prefill, 1 = playing, 2 = rate control unsupported.
    pub state: AtomicU32,
}

impl Stats {
    pub fn summary(&self) -> String {
        format!(
            "fill {:.0} frames, rate {:+.0} ppm, step {}, {} underruns, {} overruns{}",
            f32::from_bits(self.fill.load(Ordering::Relaxed)),
            f32::from_bits(self.ppm.load(Ordering::Relaxed)),
            self.requested.load(Ordering::Relaxed),
            self.underruns.load(Ordering::Relaxed),
            self.overruns.load(Ordering::Relaxed),
            match self.state.load(Ordering::Relaxed) {
                0 => ", waiting for input",
                2 => ", no rate control",
                _ => "",
            }
        )
    }
}

/// Controller state, touched only from the playback process callback.
struct Control {
    playing: bool,
    avg: f64,
    integral: f64,
    rate_ok: bool,
}

pub struct Bridge {
    pub stats: Arc<Stats>,
    _capture: pw::stream::StreamRc,
    _playback: pw::stream::StreamRc,
    _listeners: (pw::stream::StreamListener<()>, pw::stream::StreamListener<()>),
}

fn mono_format() -> anyhow::Result<Vec<u8>> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(48000);
    info.set_channels(1);
    let mut pos = [0u32; 64];
    pos[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(pos);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    Ok(spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))?
        .0
        .into_inner())
}

impl Bridge {
    /// `name` is the playback node name; the capture side is `{name}.in`.
    pub fn start(core: &pw::core::CoreRc, name: &str, label: &str) -> anyhow::Result<Self> {
        let ring = Arc::new(Ring::new());
        let stats = Arc::new(Stats::default());
        // Each side gets its own node.group so PipeWire schedules them under
        // their own drivers. Both ask for (and force) a 1024 quantum: smaller
        // steps pulled the mix graph down to 256 on Bluetooth and in Slack
        // huddles (2026-11-06).
        let side = |node: String, group: String, desc: String, class: &str| {
            properties! {
                *pw::keys::MEDIA_TYPE => "Audio",
                *pw::keys::MEDIA_CATEGORY => if class == "in" { "Capture" } else { "Playback" },
                *pw::keys::MEDIA_ROLE => "DSP",
                *pw::keys::NODE_NAME => node,
                *pw::keys::NODE_DESCRIPTION => desc,
                "node.group" => group.clone(),
                "node.link-group" => group,
                "node.latency" => "1024/48000",
                "node.force-quantum" => "1024",
                "node.autoconnect" => "false",
                "node.dont-reconnect" => "true",
                "stream.dont-remix" => "true",
            }
        };
        let capture = pw::stream::StreamRc::new(
            core.clone(),
            &format!("{name}.in"),
            side(format!("{name}.in"), format!("{name}.device"), format!("Pipemic {label} (bridge input)"), "in"),
        )?;
        let playback = pw::stream::StreamRc::new(
            core.clone(),
            name,
            side(name.to_owned(), format!("{name}.mix"), format!("Pipemic {label} (bridge)"), "out"),
        )?;

        let cap_ring = ring.clone();
        let cap_listener = capture
            .add_local_listener()
            .process(move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else { return };
                let Some(data) = buffer.datas_mut().first_mut() else { return };
                let off = data.chunk().offset() as usize;
                let size = data.chunk().size() as usize;
                let Some(raw) = data.data() else { return };
                let end = (off + size).min(raw.len());
                let raw = &raw[off.min(end)..end];
                cap_ring.push(raw.as_chunks::<4>().0.iter().map(|s| f32::from_le_bytes(*s)));
            })
            .register()?;

        let pb_ring = ring;
        let pb_stats = stats.clone();
        let mut ctl = Control { playing: false, avg: TARGET, integral: 0.0, rate_ok: true };
        let mut scratch = vec![0f32; 8192];
        let pb_listener = playback
            .add_local_listener()
            .process(move |stream, _| {
                // Raw dequeue: `requested` (frames the resampler wants this
                // cycle) isn't in the safe wrapper we build with.
                let b = unsafe { stream.dequeue_raw_buffer() };
                if b.is_null() {
                    return;
                }
                unsafe {
                    let sb = (*b).buffer;
                    if sb.is_null() || (*sb).n_datas == 0 {
                        stream.queue_raw_buffer(b);
                        return;
                    }
                    let d = &mut *(*sb).datas;
                    let max = d.maxsize as usize / 4;
                    let req = (*b).requested as usize;
                    let n = if req == 0 { 1024 } else { req }.min(max).min(scratch.len());
                    let out = &mut scratch[..n];
                    pb_stats.requested.store(req as u32, Ordering::Relaxed);

                    let fill = pb_ring.fill();
                    if fill > MAX_FILL {
                        pb_ring.skip_to(TARGET as usize);
                        // Before playing starts the ring just fills up; only
                        // count a backlog that builds while playing.
                        if ctl.playing {
                            pb_stats.overruns.fetch_add(1, Ordering::Relaxed);
                        }
                        ctl.avg = TARGET;
                        ctl.integral = 0.0;
                    }
                    if !ctl.playing && pb_ring.fill() >= TARGET as usize {
                        pb_ring.skip_to(TARGET as usize);
                        ctl.playing = true;
                        ctl.avg = TARGET;
                        ctl.integral = 0.0;
                    }
                    if ctl.playing {
                        let fill = pb_ring.fill() as f64;
                        ctl.avg += (fill - ctl.avg) * 0.01;
                        let err = ctl.avg - TARGET;
                        ctl.integral = (ctl.integral + err).clamp(-MAX_CORR / KI, MAX_CORR / KI);
                        let corr = (KP * err + KI * ctl.integral).clamp(-MAX_CORR, MAX_CORR);
                        // For a playback stream, rate < 1 makes the resampler
                        // ask for more input frames per cycle, draining a ring
                        // that's too full (found by test, 2026-11-10).
                        if ctl.rate_ok && pw::sys::pw_stream_set_rate(stream.as_raw_ptr(), 1.0 - corr) < 0 {
                            ctl.rate_ok = false;
                            pb_stats.state.store(2, Ordering::Relaxed);
                        }
                        if pb_ring.pop(out) < n {
                            pb_stats.underruns.fetch_add(1, Ordering::Relaxed);
                            ctl.playing = false;
                        }
                        pb_stats.fill.store((ctl.avg as f32).to_bits(), Ordering::Relaxed);
                        pb_stats.ppm.store(((corr * 1e6) as f32).to_bits(), Ordering::Relaxed);
                        if ctl.rate_ok {
                            pb_stats.state.store(1, Ordering::Relaxed);
                        }
                    } else {
                        out.fill(0.0);
                        pb_stats.state.store(if ctl.rate_ok { 0 } else { 2 }, Ordering::Relaxed);
                    }

                    if !d.data.is_null() {
                        std::ptr::copy_nonoverlapping(out.as_ptr() as *const u8, d.data as *mut u8, n * 4);
                    }
                    let c = &mut *d.chunk;
                    c.offset = 0;
                    c.size = (n * 4) as u32;
                    c.stride = 4;
                    stream.queue_raw_buffer(b);
                }
            })
            .register()?;

        let fmt = mono_format()?;
        for (s, dir) in [(&playback, spa::utils::Direction::Output), (&capture, spa::utils::Direction::Input)] {
            let mut params = [spa::pod::Pod::from_bytes(&fmt).ok_or_else(|| anyhow::anyhow!("bad format pod"))?];
            s.connect(dir, None, pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS, &mut params)?;
        }
        Ok(Self { stats, _capture: capture, _playback: playback, _listeners: (cap_listener, pb_listener) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_roundtrip_and_skip() {
        let r = Ring::new();
        r.push((0..10).map(|i| i as f32));
        assert_eq!(r.fill(), 10);
        let mut out = [9.0f32; 4];
        assert_eq!(r.pop(&mut out), 4);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0]);
        r.skip_to(2);
        assert_eq!(r.fill(), 2);
        let mut out = [9.0f32; 4];
        assert_eq!(r.pop(&mut out), 2);
        assert_eq!(out, [8.0, 9.0, 0.0, 0.0]);
    }

    #[test]
    fn ring_drops_when_full() {
        let r = Ring::new();
        r.push((0..CAP + 100).map(|i| i as f32));
        assert_eq!(r.fill(), CAP);
    }
}
