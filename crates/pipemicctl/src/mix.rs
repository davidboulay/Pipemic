//! `pipemicctl mix …`: talk to pipemicd over its control socket.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum MixCmd {
    /// Set a channel's own volume / mute (applies to every mix)
    Channel {
        channel: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Set a mix's master volume / mute
    Master {
        mix: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Wave:3 hardware: gain <0..40>, mute, unmute, lowcut <on|off>, clipguard <on|off>
    Mic {
        setting: String,
        #[arg(allow_hyphen_values = true)]
        value: Option<String>,
    },
    /// Set one channel's level inside one mix
    Route {
        mix: String,
        channel: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
}

pub const VALUE_HELP: &str = "VALUE: a volume in dB (-60..0, e.g. -12), or mute / unmute";

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("pipemicd.sock")
}

fn request(req: Value) -> Result<Value> {
    let path = socket_path();
    let mut s = UnixStream::connect(&path).with_context(|| format!("connecting to pipemicd at {} (is it running?)", path.display()))?;
    s.write_all(format!("{req}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line).context("bad reply from pipemicd")?;
    if reply["ok"] != true {
        bail!("pipemicd: {}", reply["error"].as_str().unwrap_or("unknown error"));
    }
    Ok(reply)
}

/// "-12" / "-12dB" -> volume, "mute" / "unmute" -> mute flag.
fn value_fields(v: &str) -> Result<(Value, Value)> {
    Ok(match v.to_ascii_lowercase().as_str() {
        "mute" | "off" => (Value::Null, json!(true)),
        "unmute" | "on" => (Value::Null, json!(false)),
        s => {
            let db: f32 = s.trim_end_matches("db").trim().parse().with_context(|| format!("not a dB value or mute/unmute: {v:?}"))?;
            (json!(db), Value::Null)
        }
    })
}

fn on_off(v: Option<&str>) -> Result<bool> {
    match v.map(str::to_ascii_lowercase).as_deref() {
        Some("on" | "true" | "1") => Ok(true),
        Some("off" | "false" | "0") => Ok(false),
        _ => bail!("expected on or off"),
    }
}

fn mic_request(setting: &str, value: Option<&str>) -> Result<Value> {
    Ok(match setting {
        "mute" => json!({"cmd": "mic", "mute": true}),
        "unmute" => json!({"cmd": "mic", "mute": false}),
        "gain" => {
            let v = value.context("usage: pipemicctl mix mic gain <0..40>")?;
            let db: f32 = v.trim_end_matches("dB").trim().parse().with_context(|| format!("not a number: {v:?}"))?;
            json!({"cmd": "mic", "gain": db})
        }
        "lowcut" => json!({"cmd": "mic", "lowcut": on_off(value)?}),
        "clipguard" => json!({"cmd": "mic", "clipguard": on_off(value)?}),
        _ => bail!("mic setting must be gain, mute, unmute, lowcut or clipguard"),
    })
}

pub fn run(cmd: Option<MixCmd>) -> Result<()> {
    let req = match cmd {
        None => json!({"cmd": "status"}),
        Some(MixCmd::Mic { setting, value }) => mic_request(&setting, value.as_deref())?,
        Some(MixCmd::Channel { channel, value }) => {
            let (volume, mute) = value_fields(&value)?;
            json!({"cmd": "channel", "channel": channel, "volume": volume, "mute": mute})
        }
        Some(MixCmd::Master { mix, value }) => {
            let (volume, mute) = value_fields(&value)?;
            json!({"cmd": "mix", "mix": mix, "volume": volume, "mute": mute})
        }
        Some(MixCmd::Route { mix, channel, value }) => {
            let (volume, mute) = value_fields(&value)?;
            json!({"cmd": "route", "mix": mix, "channel": channel, "volume": volume, "mute": mute})
        }
    };
    let is_mic = req["cmd"] == "mic";
    let mut reply = request(req)?;
    if is_mic {
        // Mic replies carry no mixer config; fetch it for the full table.
        let mic = reply["mic"].take();
        reply = request(json!({"cmd": "status"}))?;
        reply["mic"] = mic;
    }
    print_table(&reply["config"]);
    print_mic(&reply["mic"]);
    Ok(())
}

fn level(v: &Value) -> String {
    if v["mute"] == true {
        return "mute".into();
    }
    let db = v["volume"].as_f64().unwrap_or(0.0);
    if db <= -60.0 { "-inf".into() } else { format!("{db:.1}") }
}

/// Channels as rows, mixes as columns, all in dB.
fn print_table(cfg: &Value) {
    let empty = vec![];
    let chans = cfg["channel"].as_array().unwrap_or(&empty);
    let mixes = cfg["mix"].as_array().unwrap_or(&empty);
    let routes = cfg["route"].as_array().unwrap_or(&empty);
    print!("{:<12} {:>8}", "dB", "channel");
    for m in mixes {
        print!(" {:>9}", m["id"].as_str().unwrap_or("?"));
    }
    println!();
    for c in chans {
        print!("{:<12} {:>8}", c["id"].as_str().unwrap_or("?"), level(c));
        for m in mixes {
            let r = routes.iter().find(|r| r["mix"] == m["id"] && r["channel"] == c["id"]);
            print!(" {:>9}", r.map_or("0.0".into(), level));
        }
        println!();
    }
    print!("{:<12} {:>8}", "master", "");
    for m in mixes {
        print!(" {:>9}", level(m));
    }
    println!();
}

fn print_mic(m: &Value) {
    if m["connected"] != true {
        println!("\nWave:3       not connected");
        return;
    }
    let on = |k: &str| if m[k] == true { "on" } else { "off" };
    println!(
        "\nWave:3       gain {:.1} dB, {}, lowcut {}, clipguard {}",
        m["gain"].as_f64().unwrap_or(0.0),
        if m["mute"] == true { "MUTED" } else { "live" },
        on("lowcut"),
        on("clipguard"),
    );
}
