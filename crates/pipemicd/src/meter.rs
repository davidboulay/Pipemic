//! Level meters: one capture stream (`pipemic.meter`) with two AUX ports
//! per tap (L, R). The graph keeper links each channel source and each mix
//! output into it; the RT process callback keeps a decaying peak per port
//! in atomics that the control thread reads for `{"cmd":"levels"}`.

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

pub const NODE_NAME: &str = "pipemic.meter";

/// Per buffer, a held peak falls to this fraction (≈ -1.4 dB per 1024-frame
/// quantum at 48 kHz, ≈ 60 dB/s).
const DECAY: f32 = 0.85;

#[derive(Clone, Copy)]
pub enum Kind {
    Channel,
    Mix,
}

/// Read side, cheap to clone into the control thread.
#[derive(Clone)]
pub struct Levels {
    taps: Arc<Vec<(Kind, String)>>,
    peaks: Arc<Vec<AtomicU32>>,
}

impl Levels {
    /// `{"channel":{"music":[l,r],…},"mix":{"stream":[l,r],…}}`, linear peaks 0..1+.
    pub fn snapshot(&self) -> Value {
        let (mut ch, mut mx) = (Map::new(), Map::new());
        for (i, (kind, id)) in self.taps.iter().enumerate() {
            let l = f32::from_bits(self.peaks[2 * i].load(Ordering::Relaxed));
            let r = f32::from_bits(self.peaks[2 * i + 1].load(Ordering::Relaxed));
            let v = json!([round(l), round(r)]);
            match kind {
                Kind::Channel => ch.insert(id.clone(), v),
                Kind::Mix => mx.insert(id.clone(), v),
            };
        }
        json!({"channel": ch, "mix": mx})
    }
}

fn round(x: f32) -> f64 {
    (x as f64 * 10000.0).round() / 10000.0
}

/// Input port for tap `i`, side 0 = L, 1 = R.
pub fn in_port(i: usize, side: usize) -> String {
    format!("input_AUX{}", 2 * i + side)
}

pub struct Meter {
    pub levels: Levels,
    _probe: Probe,
}

impl Meter {
    pub fn start(core: &pw::core::CoreRc, taps: Vec<(Kind, String)>) -> anyhow::Result<Self> {
        let probe = Probe::start(core, NODE_NAME, "Pipemic meters", 2 * taps.len())?;
        let levels = Levels { taps: Arc::new(taps), peaks: probe.peaks.clone() };
        Ok(Self { levels, _probe: probe })
    }
}

/// Mono slots that app streams get linked into, to tell an app that
/// plays sound from one that only holds a silent stream open.
pub const APP_NODE_NAME: &str = "pipemic.appmeter";
pub const APP_SLOTS: usize = 64;

/// Above this peak (≈ -66 dBFS) an app counts as playing.
pub const PLAYING: f32 = 0.0005;

/// A capture stream with `n` AUX input ports, keeping a decaying peak per port.
pub struct Probe {
    pub peaks: Arc<Vec<AtomicU32>>,
    _stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<()>,
}

impl Probe {
    pub fn start(core: &pw::core::CoreRc, name: &str, description: &str, n: usize) -> anyhow::Result<Self> {
        if n == 0 || n > 64 {
            anyhow::bail!("{name}: bad port count {n}");
        }
        let peaks: Arc<Vec<AtomicU32>> = Arc::new((0..n).map(|_| AtomicU32::new(0)).collect());
        let rt_peaks = peaks.clone();

        let stream = pw::stream::StreamRc::new(
            core.clone(),
            name,
            properties! {
                *pw::keys::MEDIA_TYPE => "Audio",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "DSP",
                *pw::keys::NODE_NAME => name,
                *pw::keys::NODE_DESCRIPTION => description,
                "node.autoconnect" => "false",
                "stream.dont-remix" => "true",
                "node.dont-reconnect" => "true",
            },
        )?;

        let listener = stream
            .add_local_listener()
            .process(move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else { return };
                let Some(data) = buffer.datas_mut().first_mut() else { return };
                let bytes = data.chunk().size() as usize;
                let Some(raw) = data.data() else { return };
                let raw = &raw[..bytes.min(raw.len())];
                let mut max = [0f32; 64];
                for (k, s) in raw.as_chunks::<4>().0.iter().enumerate() {
                    let v = f32::from_le_bytes(*s).abs();
                    let c = k % n;
                    if v > max[c] {
                        max[c] = v;
                    }
                }
                for (c, p) in rt_peaks.iter().enumerate() {
                    let prev = f32::from_bits(p.load(Ordering::Relaxed));
                    p.store(max[c].max(prev * DECAY).to_bits(), Ordering::Relaxed);
                }
            })
            .register()?;

        let mut info = spa::param::audio::AudioInfoRaw::new();
        info.set_format(spa::param::audio::AudioFormat::F32LE);
        info.set_channels(n as u32);
        let mut pos = [0u32; 64];
        for (i, p) in pos.iter_mut().take(n).enumerate() {
            *p = spa::sys::SPA_AUDIO_CHANNEL_AUX0 + i as u32;
        }
        info.set_position(pos);
        let obj = spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        };
        let bytes = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))?
            .0
            .into_inner();
        let mut params = [spa::pod::Pod::from_bytes(&bytes).ok_or_else(|| anyhow::anyhow!("bad format pod"))?];
        stream.connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )?;
        Ok(Self { peaks, _stream: stream, _listener: listener })
    }
}
