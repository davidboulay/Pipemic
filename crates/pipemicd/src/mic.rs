//! Wave:3 hardware control: a background thread polls the mic's config
//! (so pad taps show up live), applies requested changes, and reconnects
//! after a replug. Interface 3 is only claimed per transfer (see wave-proto),
//! so `pipemicctl get/set` keep working alongside the daemon.

use serde::Serialize;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use wave_proto::{Setting, Wave3};

#[derive(Serialize, Clone, Default, Debug, PartialEq)]
pub struct MicState {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    pub gain: f32,
    pub mute: bool,
    pub clipguard: bool,
    pub lowcut: bool,
    pub headphone_volume: f32,
    pub headphone_mute: bool,
    /// Headphone mix, % mic (0 = all computer, 100 = all mic).
    pub monitor: f32,
    /// What the front knob (and its LED ring) controls: "mic", "headphone", "mix".
    pub knob: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<MicInfo>,
}

/// Fixed device facts, read once on connect (msg 10).
#[derive(Serialize, Clone, Default, Debug, PartialEq)]
pub struct MicInfo {
    pub model: String,
    pub firmware: String,
    pub bootloader: String,
    pub hardware: String,
    pub api: String,
}

impl MicInfo {
    fn from(v: &wave_proto::Version) -> Self {
        let short = |s: &wave_proto::SemVer| {
            let mut t = format!("{}.{}.{} ({})", s.major, s.minor, s.patch, s.build);
            if !s.commit.is_empty() {
                t.push(' ');
                t.push_str(&s.commit);
            }
            t
        };
        Self {
            model: "Wave:3".into(),
            firmware: short(&v.firmware),
            bootloader: short(&v.bootloader),
            hardware: v.hw_version_board.to_string(),
            api: format!("{}.{}", v.api_major, v.api_minor),
        }
    }
}

pub fn knob_name(v: wave_proto::VolumeSelect) -> String {
    use wave_proto::VolumeSelect as V;
    match v {
        V::Mic => "mic".into(),
        V::Headphone => "headphone".into(),
        V::Mix => "mix".into(),
        V::Other(n) => format!("other-{n}"),
    }
}

pub fn knob_from(name: &str) -> Option<wave_proto::VolumeSelect> {
    use wave_proto::VolumeSelect as V;
    match name {
        "mic" => Some(V::Mic),
        "headphone" => Some(V::Headphone),
        "mix" => Some(V::Mix),
        _ => None,
    }
}

type Job = (Vec<Setting>, mpsc::Sender<Result<(), String>>);

#[derive(Clone)]
pub struct Mic {
    state: Arc<Mutex<MicState>>,
    tx: mpsc::Sender<Job>,
}

const POLL: Duration = Duration::from_millis(200);
const RETRY: Duration = Duration::from_secs(2);
const MUTE_SYNC: Duration = Duration::from_secs(1);
/// PipeWire node name prefix of the Wave:3 capture source.
const SOURCE_PREFIX: &str = "alsa_input.usb-Elgato_Systems_Elgato_Wave_3";

impl Mic {
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(MicState::default()));
        let (tx, rx) = mpsc::channel::<Job>();
        let st = state.clone();
        std::thread::spawn(move || run(st, rx));
        Self { state, tx }
    }

    pub fn state(&self) -> MicState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Apply settings on the mic (read-modify-write, verified). Blocks until done.
    pub fn apply(&self, settings: Vec<Setting>) -> Result<(), String> {
        let (rtx, rrx) = mpsc::channel();
        self.tx
            .send((settings, rtx))
            .map_err(|_| "mic thread stopped".to_string())?;
        rrx.recv_timeout(Duration::from_secs(3))
            .map_err(|_| "mic did not answer".to_string())?
    }
}

fn run(state: Arc<Mutex<MicState>>, rx: mpsc::Receiver<Job>) {
    let mut dev: Option<Wave3> = None;
    let mut last_try = Instant::now() - RETRY;
    let mut sync = MuteSync::default();
    let mut last_sync = Instant::now() - MUTE_SYNC;
    loop {
        if dev.is_none() && last_try.elapsed() >= RETRY {
            last_try = Instant::now();
            if let Ok(d) = Wave3::open() {
                let version = d.version().ok();
                let serial = version.as_ref().map(|v| v.serial.clone());
                eprintln!(
                    "pipemicd: Wave:3 connected ({})",
                    serial.as_deref().unwrap_or("?")
                );
                if let Ok(mut s) = state.lock() {
                    s.serial = serial;
                    s.info = version.as_ref().map(MicInfo::from);
                }
                dev = Some(d);
                std::thread::spawn(ensure_mic_input);
            }
        }

        // Wait for a command or the next poll tick. A command's reply is sent
        // after the refresh below, so the client sees the new state.
        let mut pending = None;
        match rx.recv_timeout(POLL) {
            Ok((settings, reply)) => {
                let r = match &dev {
                    None => Err("Wave:3 not connected".to_string()),
                    Some(d) => d.update(&settings).map(|_| ()).map_err(|e| e.to_string()),
                };
                pending = Some((r, reply));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        refresh(&mut dev, &state);
        if let Some((r, reply)) = pending {
            let _ = reply.send(r);
        }
        if last_sync.elapsed() >= MUTE_SYNC {
            last_sync = Instant::now();
            sync.tick(&mut dev, &state);
        }
    }
}

/// Keeps the PipeWire mute of the Wave:3 source equal to the mic's own mute
/// (red LED). Apps like Meet treat a muted source as "muted by system", and
/// WirePlumber can restore a stale mute when the node is recreated (replug,
/// profile change), so this runs every second, not only on pad taps.
#[derive(Default)]
struct MuteSync {
    /// Source node name and mute value at the last successful sync.
    last: Option<(String, bool)>,
}

#[derive(Debug, PartialEq)]
enum MuteAction {
    None,
    /// Hardware is the reference: copy its mute to PipeWire.
    SetPipeWire(bool),
    /// Only the PipeWire side changed since the last sync (e.g. the desktop's
    /// mic-mute key): push it to the mic so the LED follows.
    SetHardware(bool),
}

fn mute_action(last: Option<bool>, hw: bool, pw: bool) -> MuteAction {
    if hw == pw {
        MuteAction::None
    } else if last == Some(hw) {
        MuteAction::SetHardware(pw)
    } else {
        MuteAction::SetPipeWire(hw)
    }
}

impl MuteSync {
    fn tick(&mut self, dev: &mut Option<Wave3>, state: &Mutex<MicState>) {
        let hw = match state.lock() {
            Ok(s) if s.connected => s.mute,
            _ => {
                self.last = None;
                return;
            }
        };
        let Some(name) = wave3_source() else {
            self.last = None;
            return;
        };
        let Some(pw) = source_mute(&name) else { return };
        let last = self.last.as_ref().filter(|(n, _)| *n == name).map(|(_, m)| *m);
        let result = match mute_action(last, hw, pw) {
            MuteAction::None => Some(hw),
            MuteAction::SetPipeWire(m) => {
                eprintln!("pipemicd: PipeWire mute {} -> {} (follows Wave:3)", pw, m);
                set_source_mute(&name, m).then_some(m)
            }
            MuteAction::SetHardware(m) => {
                eprintln!("pipemicd: Wave:3 mute {} -> {} (follows PipeWire)", hw, m);
                let ok = dev
                    .as_ref()
                    .is_some_and(|d| d.update(&[Setting::InputMute(m)]).is_ok());
                if ok {
                    if let Ok(mut s) = state.lock() {
                        s.mute = m;
                    }
                }
                ok.then_some(m)
            }
        };
        self.last = result.map(|m| (name, m));
    }
}

fn pactl(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("pactl").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn wave3_source() -> Option<String> {
    pactl(&["list", "sources", "short"])?
        .lines()
        .filter_map(|l| l.split('\t').nth(1))
        .find(|n| n.starts_with(SOURCE_PREFIX) && !n.ends_with(".monitor"))
        .map(str::to_string)
}

/// Card profile with the mic input (and the headphone output).
const MIC_PROFILE: &str = "output:analog-stereo+input:mono-fallback";

/// After a replug the card has come back on an output-only profile (seen:
/// "Digital Stereo (IEC958) Output", 2026-11-06) or "Off": the mic shows Live
/// but nothing captures. If the card is there but its mic input isn't, switch
/// to the profile that has it. Waits up to ~10 s for the card to settle.
fn ensure_mic_input() {
    for _ in 0..10 {
        std::thread::sleep(Duration::from_secs(1));
        let Some(card) = wave3_card() else { continue };
        if wave3_source().is_some() {
            return;
        }
        // Give the session manager a moment to pick its own profile first.
        std::thread::sleep(Duration::from_secs(2));
        if wave3_source().is_none() {
            let ok = pactl(&["set-card-profile", &card, MIC_PROFILE]).is_some();
            eprintln!("pipemicd: Wave:3 came back without its mic input; set card profile {MIC_PROFILE}: {}", if ok { "ok" } else { "failed" });
        }
        return;
    }
}

fn wave3_card() -> Option<String> {
    pactl(&["list", "cards", "short"])?
        .lines()
        .filter_map(|l| l.split('\t').nth(1))
        .find(|n| n.contains("Elgato_Systems_Elgato_Wave_3"))
        .map(str::to_string)
}

fn source_mute(name: &str) -> Option<bool> {
    let out = pactl(&["get-source-mute", name])?;
    // "Mute: yes" / "Mute: no" (localised builds still print yes/no).
    let v = out.split(':').nth(1)?.trim();
    Some(v == "yes")
}

fn set_source_mute(name: &str, mute: bool) -> bool {
    pactl(&["set-source-mute", name, if mute { "1" } else { "0" }]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_wins_on_new_node() {
        assert_eq!(mute_action(None, false, true), MuteAction::SetPipeWire(false));
        assert_eq!(mute_action(None, true, false), MuteAction::SetPipeWire(true));
    }

    #[test]
    fn pad_tap_goes_to_pipewire() {
        // last sync: both unmuted; pad tapped -> hw muted
        assert_eq!(mute_action(Some(false), true, false), MuteAction::SetPipeWire(true));
    }

    #[test]
    fn desktop_mute_goes_to_mic() {
        assert_eq!(mute_action(Some(false), false, true), MuteAction::SetHardware(true));
        assert_eq!(mute_action(Some(true), true, false), MuteAction::SetHardware(false));
    }

    #[test]
    fn in_sync_does_nothing() {
        assert_eq!(mute_action(Some(true), true, true), MuteAction::None);
        assert_eq!(mute_action(None, false, false), MuteAction::None);
    }
}

fn refresh(dev: &mut Option<Wave3>, state: &Mutex<MicState>) {
    let Some(d) = dev.as_ref() else { return };
    match d.config() {
        Ok(c) => {
            let (serial, info) = state.lock().map(|s| (s.serial.clone(), s.info.clone())).unwrap_or_default();
            let new = MicState {
                connected: true,
                serial,
                info,
                knob: knob_name(c.volume_select),
                gain: c.input_gain,
                mute: c.input_mute,
                clipguard: c.clipguard,
                lowcut: c.lowcut,
                headphone_volume: c.headphone_volume,
                headphone_mute: c.headphone_mute,
                monitor: c.direct_monitor,
            };
            if let Ok(mut s) = state.lock() {
                if s.connected && s.mute != new.mute {
                    eprintln!(
                        "pipemicd: mic {}",
                        if new.mute { "muted" } else { "unmuted" }
                    );
                }
                *s = new;
            }
        }
        Err(wave_proto::Error::Usb(rusb_err)) if is_gone(&rusb_err) => {
            eprintln!("pipemicd: Wave:3 disconnected");
            *dev = None;
            if let Ok(mut s) = state.lock() {
                *s = MicState::default();
            }
        }
        Err(_) => {} // busy or transient; try again next tick
    }
}

fn is_gone(e: &wave_proto::RusbError) -> bool {
    matches!(
        e,
        wave_proto::RusbError::NoDevice
            | wave_proto::RusbError::Io
            | wave_proto::RusbError::NotFound
    )
}
