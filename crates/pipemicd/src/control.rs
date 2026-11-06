//! Control socket: newline-delimited JSON at `$XDG_RUNTIME_DIR/pipemicd.sock`.
//!
//! Requests (one per line):
//!   {"cmd":"status"}
//!   {"cmd":"channel","channel":"music","volume":-10.0}          // dB
//!   {"cmd":"mix","mix":"stream","mute":true}
//!   {"cmd":"route","mix":"stream","channel":"music","volume":-6}
//!   {"cmd":"mic","gain":30.0,"mute":false,"lowcut":true,"clipguard":true,
//!    "headphone_volume":-10,"headphone_mute":false,"monitor":50,"knob":"mic"}
//!   {"cmd":"apps","installed":true}  // -> "apps":[{name,icon,keys,channel,explicit,running}],
//!                                     //    "outputs":[{name,description}]; installed adds every app
//!   {"cmd":"assign","app":"Spotify","channel":"music"}   // channel null = back to automatic
//!   {"cmd":"settings","output_fallback":true}
//!   {"cmd":"channel","channel":"music","link":true}      // UI: faders move together
//!   {"cmd":"mix","mix":"personal","output":"alsa_output…"} // "" = default output
//!   {"cmd":"eq","channel":"mic","import":"/path/Old_Radio.xml"}  // Elgato EQ preset add-on
//!   {"cmd":"eq","channel":"mic","enabled":false}   // bypass; "clear":true removes it;
//!                                                  // "set":{name,enabled,band:[{kind,freq,q,gain,on}]}
//!   {"cmd":"levels"}   // -> {"ok":true,"levels":{"channel":{id:[l,r]},"mix":{id:[l,r]}}}
//! Reply: {"ok":true,"config":{…},"mic":{…}} or {"ok":false,"error":"…"}
//! Levels are decaying linear peaks (1.0 = 0 dBFS); poll at ~30 Hz.

use crate::meter::Levels;
use crate::mic::{Mic, MicState};
use crate::model::Config;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;

#[derive(Deserialize, Debug)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    Status,
    Levels,
    Apps {
        #[serde(default)]
        installed: bool,
    },
    Settings { output_fallback: Option<bool> },
    Assign { app: String, channel: Option<String> },
    Channel { channel: String, volume: Option<f32>, mute: Option<bool>, link: Option<bool> },
    Mix { mix: String, volume: Option<f32>, mute: Option<bool>, output: Option<String> },
    Route { mix: String, channel: String, volume: Option<f32>, mute: Option<bool> },
    /// Applied in order: clear, set, import, enabled.
    Eq {
        channel: String,
        import: Option<String>,
        set: Option<crate::eq::Equalizer>,
        enabled: Option<bool>,
        #[serde(default)]
        clear: bool,
    },
    Mic(MicReq),
}

/// Wave:3 settings to change; absent fields stay as they are.
#[derive(Deserialize, Debug, Default)]
pub struct MicReq {
    gain: Option<f32>,
    mute: Option<bool>,
    lowcut: Option<bool>,
    clipguard: Option<bool>,
    headphone_volume: Option<f32>,
    headphone_mute: Option<bool>,
    /// Headphone mix, % mic.
    monitor: Option<f32>,
    /// "mic" | "headphone" | "mix"
    knob: Option<String>,
}

#[derive(Serialize, Default)]
pub struct Reply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mic: Option<MicState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub levels: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apps: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<serde_json::Value>,
}

impl Reply {
    pub fn err(e: impl Into<String>) -> Self {
        Self { ok: false, error: Some(e.into()), ..Default::default() }
    }
}

fn mic_settings(r: MicReq) -> Result<Vec<wave_proto::Setting>, String> {
    use wave_proto::Setting as S;
    let mut v = Vec::new();
    v.extend(r.gain.map(S::InputGain));
    v.extend(r.mute.map(S::InputMute));
    v.extend(r.lowcut.map(S::Lowcut));
    v.extend(r.clipguard.map(S::Clipguard));
    v.extend(r.headphone_volume.map(S::HeadphoneVolume));
    v.extend(r.headphone_mute.map(S::HeadphoneMute));
    v.extend(r.monitor.map(S::DirectMonitor));
    if let Some(k) = r.knob {
        v.push(S::VolumeSelect(crate::mic::knob_from(&k).ok_or_else(|| format!("knob must be mic, headphone or mix, not {k:?}"))?));
    }
    Ok(v)
}

/// A request plus the way back to the client thread.
pub type Job = (Request, mpsc::Sender<Reply>);

pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("pipemicd.sock")
}

/// Accept clients on a background thread; each request is handed to the main
/// loop through `tx` and answered with what comes back.
pub fn serve(tx: pipewire::channel::Sender<Job>, mic: Mic, levels: Levels) -> anyhow::Result<PathBuf> {
    let path = socket_path();
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            anyhow::bail!("another pipemicd is already running ({})", path.display());
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let tx = tx.clone();
            let mic = mic.clone();
            let levels = levels.clone();
            std::thread::spawn(move || {
                let Ok(mut writer) = conn.try_clone() else { return };
                for line in BufReader::new(conn).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let mut reply = match serde_json::from_str::<Request>(&line) {
                        Err(e) => Reply::err(format!("bad request: {e}")),
                        Ok(Request::Levels) => {
                            let mut out = serde_json::json!({"ok": true, "levels": levels.snapshot()}).to_string();
                            out.push('\n');
                            if writer.write_all(out.as_bytes()).is_err() {
                                break;
                            }
                            continue;
                        }
                        // Mic requests go straight to the USB thread.
                        Ok(Request::Mic(r)) => {
                            match mic_settings(r).and_then(|s| mic.apply(s)) {
                                Ok(()) => Reply { ok: true, ..Default::default() },
                                Err(e) => Reply::err(e),
                            }
                        }
                        Ok(req) => {
                            let (rtx, rrx) = mpsc::channel();
                            if tx.send((req, rtx)).is_err() {
                                break;
                            }
                            match rrx.recv() {
                                Ok(r) => r,
                                Err(_) => break,
                            }
                        }
                    };
                    if reply.ok {
                        reply.mic = Some(mic.state());
                    }
                    let mut out = serde_json::to_string(&reply).unwrap_or_default();
                    out.push('\n');
                    if writer.write_all(out.as_bytes()).is_err() {
                        break;
                    }
                }
            });
        }
    });
    Ok(path)
}
