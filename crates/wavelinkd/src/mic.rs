//! Wave:3 hardware control: a background thread polls the mic's config
//! (so pad taps show up live), applies requested changes, and reconnects
//! after a replug. Interface 3 is only claimed per transfer (see wave-proto),
//! so `wavectl get/set` keep working alongside the daemon.

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
    pub monitor: f32,
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
                let serial = d.version().ok().map(|v| v.serial);
                eprintln!(
                    "wavelinkd: Wave:3 connected ({})",
                    serial.as_deref().unwrap_or("?")
                );
                if let Ok(mut s) = state.lock() {
                    s.serial = serial;
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
            let new = MicState {
                connected: true,
                serial: state.lock().ok().and_then(|s| s.serial.clone()),
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
                        "wavelinkd: mic {}",
                        if new.mute { "muted" } else { "unmuted" }
                    );
                }
                *s = new;
            }
        }
        Err(wave_proto::Error::Usb(rusb_err)) if is_gone(&rusb_err) => {
            eprintln!("wavelinkd: Wave:3 disconnected");
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
