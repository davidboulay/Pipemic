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
