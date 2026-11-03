mod mix;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::time::{Duration, Instant};
use wave_proto::{Config, Setting, Status, VolumeSelect, Wave3};

#[derive(Parser)]
#[command(version, about = "Control an Elgato Wave:3 over USB")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show device info, config and status
    Get {
        /// Also print raw message bytes
        #[arg(long)]
        raw: bool,
    },
    /// Show the version message only
    Version,
    /// Change one setting (read-modify-write, verified by read-back)
    #[command(after_help = "\
PARAMS:
  gain        0..40 dB, step 0.5
  mute        on|off
  clipguard   on|off
  lowcut      on|off
  hp-volume   -60..0 dB, step 0.5
  hp-mute     on|off
  monitor     0..100 %, step 5 (direct monitor / playback mix)
  knob        mic|headphone|mix
  leds-off    on|off   (offset unconfirmed)
  leds-flip   on|off   (offset unconfirmed)
  gain-lock   on|off   (offset unconfirmed)")]
    Set {
        param: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
        /// Show the bytes that would be written, but don't write
        #[arg(long)]
        dry_run: bool,
    },
    /// Show or change the wavelinkd mixer (no args: show the matrix)
    #[command(after_help = mix::VALUE_HELP)]
    Mix {
        #[command(subcommand)]
        action: Option<mix::MixCmd>,
    },
    /// Hex-dump a raw message (GET_MEM, read-only); with --watch, print each change
    Dump {
        /// Message ID (0 config, 1 status, 10 version)
        id: u16,
        /// Keep polling and print a timestamped line whenever the bytes change
        #[arg(long)]
        watch: bool,
        /// Poll interval in milliseconds (with --watch)
        #[arg(long, default_value_t = 50)]
        interval: u64,
    },
    /// Poll config and status, printing every change
    Watch {
        /// Poll interval in milliseconds
        #[arg(long, default_value_t = 100)]
        interval: u64,
        /// Also report status (touch sensor) changes
        #[arg(long)]
        status: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Cmd::Mix { action } = cli.cmd {
        return mix::run(action);
    }
    let dev = Wave3::open().context(
        "opening Wave:3 (is it plugged in, and is udev/70-elgato-wave.rules installed?)",
    )?;
    match cli.cmd {
        Cmd::Get { raw } => get(&dev, raw),
        Cmd::Version => version(&dev),
        Cmd::Set { param, value, dry_run } => set(&dev, &param, &value, dry_run),
        Cmd::Watch { interval, status } => watch(&dev, interval, status),
        Cmd::Dump { id, watch, interval } => dump(&dev, id, watch, interval),
        Cmd::Mix { .. } => unreachable!(),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn onoff(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn version(dev: &Wave3) -> Result<()> {
    let v = dev.version()?;
    println!("serial      {}", v.serial);
    println!("api         {}.{}", v.api_major, v.api_minor);
    println!("firmware    {}", v.firmware);
    println!("bootloader  {}", v.bootloader);
    println!("board       hw {} / mic_type {}", v.hw_version_board, v.mic_type);
    println!("vbus        {:.3} V", v.vbus_voltage);
    println!("touch thr   {}..{}", v.touch_threshold_lower, v.touch_threshold_upper);
    Ok(())
}

fn fields(c: &Config) -> Vec<(&'static str, String)> {
    vec![
        ("gain", format!("{:.1} dB", c.input_gain)),
        ("mute", onoff(c.input_mute).into()),
        ("clipguard", onoff(c.clipguard).into()),
        ("lowcut", onoff(c.lowcut).into()),
        ("hp-volume", format!("{:.1} dB", c.headphone_volume)),
        ("hp-mute", onoff(c.headphone_mute).into()),
        ("monitor", format!("{:.0} %", c.direct_monitor)),
        ("knob", c.volume_select.to_string()),
        ("leds-off", onoff(c.all_leds_off).into()),
        ("leds-flip", onoff(c.leds_flip).into()),
        ("gain-lock", onoff(c.gain_lock).into()),
        ("bytes 2-3", hex(&c.unknown_2_3)),
    ]
}

fn get(dev: &Wave3, raw: bool) -> Result<()> {
    version(dev)?;
    let cb = dev.config_bytes()?;
    let c = Config::decode(&cb)?;
    println!();
    for (k, v) in fields(&c) {
        println!("{k:<11} {v}");
    }
    let s = dev.status()?;
    let upper = dev.version()?.touch_threshold_upper;
    println!();
    println!("touch pad   {} (signal {})", if s.touched(upper) { "touched" } else { "idle" }, s.touch_signal);
    if raw {
        println!();
        println!("config raw  {}", hex(&cb));
    }
    Ok(())
}

fn parse_bool(v: &str) -> Result<bool> {
    Ok(match v.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => true,
        "off" | "false" | "0" | "no" => false,
        _ => bail!("expected on/off, got {v:?}"),
    })
}

fn parse_f32(v: &str) -> Result<f32> {
    v.trim_end_matches("dB").trim_end_matches('%').trim().parse().with_context(|| format!("not a number: {v:?}"))
}

fn parse_setting(param: &str, v: &str) -> Result<Setting> {
    Ok(match param {
        "gain" => Setting::InputGain(parse_f32(v)?),
        "mute" => Setting::InputMute(parse_bool(v)?),
        "clipguard" => Setting::Clipguard(parse_bool(v)?),
        "lowcut" => Setting::Lowcut(parse_bool(v)?),
        "hp-volume" | "headphone" => Setting::HeadphoneVolume(parse_f32(v)?),
        "hp-mute" => Setting::HeadphoneMute(parse_bool(v)?),
        "monitor" => Setting::DirectMonitor(parse_f32(v)?),
        "knob" => Setting::VolumeSelect(match v {
            "mic" => VolumeSelect::Mic,
            "headphone" | "hp" => VolumeSelect::Headphone,
            "mix" => VolumeSelect::Mix,
            _ => bail!("knob must be mic, headphone or mix"),
        }),
        "leds-off" => Setting::AllLedsOff(parse_bool(v)?),
        "leds-flip" => Setting::LedsFlip(parse_bool(v)?),
        "gain-lock" => Setting::GainLock(parse_bool(v)?),
        _ => bail!("unknown param {param:?} (see `wavectl set --help`)"),
    })
}

fn set(dev: &Wave3, param: &str, value: &str, dry_run: bool) -> Result<()> {
    let s = parse_setting(param, value)?;
    let (before, after) = if dry_run { dev.plan(&[s])? } else { dev.update(&[s])? };
    println!("before  {}", hex(&before));
    println!("{}  {}", if dry_run { "would " } else { "after " }, hex(&after));
    if before == after {
        println!("(no change)");
    } else if dry_run {
        println!("(dry run, nothing written)");
    } else {
        println!("written and verified");
    }
    Ok(())
}

fn watch(dev: &Wave3, interval: u64, with_status: bool) -> Result<()> {
    let t0 = Instant::now();
    let mut prev: Option<(Config, Status)> = None;
    loop {
        let c = dev.config()?;
        let s = dev.status()?;
        let t = t0.elapsed().as_secs_f32();
        match &prev {
            None => {
                let line: Vec<String> = fields(&c).into_iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("{t:7.1}s  {}", line.join("  "));
            }
            Some((pc, ps)) => {
                for ((k, old), (_, new)) in fields(pc).into_iter().zip(fields(&c)) {
                    if old != new {
                        println!("{t:7.1}s  {k}: {old} -> {new}");
                    }
                }
                if with_status && ps != &s {
                    println!("{t:7.1}s  touch signal {}", s.touch_signal);
                }
            }
        }
        prev = Some((c, s));
        std::thread::sleep(Duration::from_millis(interval));
    }
}

fn dump(dev: &Wave3, id: u16, watch: bool, interval: u64) -> Result<()> {
    let t0 = Instant::now();
    let mut prev: Vec<u8> = Vec::new();
    loop {
        let mut buf = [0u8; 256];
        let n = dev.read_raw(id, &mut buf)?;
        let cur = &buf[..n];
        if !watch {
            println!("msg {id} ({n} bytes): {}", hex(cur));
            return Ok(());
        }
        if cur != prev.as_slice() {
            let marked: Vec<String> = cur
                .iter()
                .enumerate()
                .map(|(i, b)| if prev.get(i).is_some_and(|p| p != b) { format!("[{b:02x}]") } else { format!(" {b:02x} ") })
                .collect();
            println!("{:7.2}s {}", t0.elapsed().as_secs_f32(), marked.join(""));
            prev = cur.to_vec();
        }
        std::thread::sleep(Duration::from_millis(interval));
    }
}
