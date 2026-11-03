//! `wavectl mix …`: talk to wavelinkd over its control socket.

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
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("wavelinkd.sock")
}

fn request(req: Value) -> Result<Value> {
    let path = socket_path();
    let mut s = UnixStream::connect(&path).with_context(|| format!("connecting to wavelinkd at {} (is it running?)", path.display()))?;
    s.write_all(format!("{req}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line).context("bad reply from wavelinkd")?;
    if reply["ok"] != true {
        bail!("wavelinkd: {}", reply["error"].as_str().unwrap_or("unknown error"));
    }
    Ok(reply["config"].clone())
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

pub fn run(cmd: Option<MixCmd>) -> Result<()> {
    let req = match cmd {
        None => json!({"cmd": "status"}),
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
    print_table(&request(req)?);
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
