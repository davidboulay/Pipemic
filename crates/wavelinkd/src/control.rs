//! Control socket: newline-delimited JSON at `$XDG_RUNTIME_DIR/wavelinkd.sock`.
//!
//! Requests (one per line):
//!   {"cmd":"status"}
//!   {"cmd":"channel","channel":"music","volume":-10.0}          // dB
//!   {"cmd":"mix","mix":"stream","mute":true}
//!   {"cmd":"route","mix":"stream","channel":"music","volume":-6}
//! Reply: {"ok":true,"config":{…}} or {"ok":false,"error":"…"}

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
    Channel { channel: String, volume: Option<f32>, mute: Option<bool> },
    Mix { mix: String, volume: Option<f32>, mute: Option<bool> },
    Route { mix: String, channel: String, volume: Option<f32>, mute: Option<bool> },
}

#[derive(Serialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,
}

/// A request plus the way back to the client thread.
pub type Job = (Request, mpsc::Sender<Reply>);

pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("wavelinkd.sock")
}

/// Accept clients on a background thread; each request is handed to the main
/// loop through `tx` and answered with what comes back.
pub fn serve(tx: pipewire::channel::Sender<Job>) -> anyhow::Result<PathBuf> {
    let path = socket_path();
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            anyhow::bail!("another wavelinkd is already running ({})", path.display());
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let Ok(mut writer) = conn.try_clone() else { return };
                for line in BufReader::new(conn).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let reply = match serde_json::from_str::<Request>(&line) {
                        Err(e) => Reply { ok: false, error: Some(format!("bad request: {e}")), config: None },
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
