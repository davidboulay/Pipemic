//! `pipemicctl fw`: firmware catalog, image download/validation, and the
//! DFU enter/leave round-trip. No flashing yet (see docs/wave3-dfu.md).

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use wave_proto::dfu::{self, Image, Mode};
use wave_proto::Wave3;

/// The catalog has no hashes, so pin the images we've seen.
const KNOWN_SHA256: &[(&str, &str)] =
    &[("1.2.2", "05cf543a36373a9685a3460a889df5cc7189403ae419296f23098ae74c6a8b1f")];

const BROWSER_NOTE: &str = "note: browsers may lose all audio devices after the mic re-enumerates. \
If Chrome/Meet shows no mic, end \"Utility: Audio Service\" in Chrome's task manager (Shift+Esc).";

#[derive(Subcommand)]
pub enum FwCmd {
    /// Show the mic's mode, firmware version and DFU state
    Status,
    /// Compare the mic's firmware with Elgato's catalog
    Check,
    /// Download a firmware image from the catalog and validate it (writes a file, not the mic)
    Fetch {
        /// Version to fetch (default: newest in the catalog)
        #[arg(long)]
        version: Option<String>,
        /// Output path (default: ./wave3-<version>.dfu)
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Validate a local .dfu file (suffix, CRC, target, known hash)
    Inspect { file: PathBuf },
    /// Switch the mic into bootloader mode and straight back (no flash write; audio drops ~1 s)
    DfuTest {
        /// Required: confirms you're OK with the mic disconnecting briefly
        #[arg(long)]
        yes: bool,
    },
    /// Bring a mic stuck in bootloader mode (0fd9:0071) back to normal
    Leave,
}

pub fn run(cmd: FwCmd) -> Result<()> {
    match cmd {
        FwCmd::Status => status(),
        FwCmd::Check => check(),
        FwCmd::Fetch { version, out } => fetch(version, out),
        FwCmd::Inspect { file } => inspect(&file),
        FwCmd::DfuTest { yes } => dfu_test(yes),
        FwCmd::Leave => leave(),
    }
}

// ---------------------------------------------------------------- catalog

fn catalog() -> Result<Vec<wave_catalog::Firmware>> {
    wave_catalog::wave3_firmware().map_err(anyhow::Error::msg)
}

fn device_version() -> Result<Option<String>> {
    match dfu::mode()? {
        Mode::Run => {
            let v = Wave3::open()?.version()?;
            Ok(Some(format!("{}.{}.{}", v.firmware.major, v.firmware.minor, v.firmware.patch)))
        }
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------- commands

fn status() -> Result<()> {
    match dfu::mode()? {
        Mode::Absent => println!("mode        not connected"),
        Mode::Run => {
            let v = Wave3::open()?.version()?;
            let s = dfu::run_status()?;
            println!("mode        run (0fd9:0070)");
            println!("firmware    {}", v.firmware);
            println!("bootloader  {}", v.bootloader);
            println!("dfu state   {} (status {})", s.state_name(), s.status);
        }
        Mode::Bootloader => {
            let b = dfu::Bootloader::open()?;
            let s = b.status()?;
            println!("mode        BOOTLOADER (0fd9:0071)");
            println!("dfu state   {} (status {})", s.state_name(), s.status);
            println!("            run `pipemicctl fw leave` to return to normal");
        }
    }
    Ok(())
}

fn check() -> Result<()> {
    let entries = catalog()?;
    let latest = entries.last().context("catalog lists no Wave:3 firmware")?;
    let current = device_version()?;
    println!("catalog     {}", entries.iter().map(|e| e.version.as_str()).collect::<Vec<_>>().join(", "));
    println!("latest      {} ({})", latest.version, latest.importance);
    println!("            {}", latest.url);
    match current {
        None => println!("installed   unknown (mic not in run mode)"),
        Some(cur) => {
            println!("installed   {cur}");
            if wave_catalog::is_newer(&latest.version, &cur) {
                println!("=> update available");
            } else {
                println!("=> up to date");
            }
        }
    }
    Ok(())
}

fn fetch(version: Option<String>, out: Option<PathBuf>) -> Result<()> {
    let entries = catalog()?;
    let e = match &version {
        Some(v) => entries.iter().find(|e| &e.version == v).with_context(|| {
            format!("{v} not in catalog ({})", entries.iter().map(|e| e.version.as_str()).collect::<Vec<_>>().join(", "))
        })?,
        None => entries.last().context("catalog lists no Wave:3 firmware")?,
    };
    if !e.url.starts_with("https://") {
        bail!("refusing non-HTTPS download URL {}", e.url);
    }
    println!("downloading {}", e.url);
    let bytes = ureq::get(&e.url).call()?.body_mut().with_config().limit(16 << 20).read_to_vec()?;
    validate(&bytes, Some(&e.version))?;
    let path = out.unwrap_or_else(|| PathBuf::from(format!("wave3-{}.dfu", e.version)));
    std::fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
    println!("saved       {}", path.display());
    Ok(())
}

fn inspect(file: &PathBuf) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    validate(&bytes, None)
}

/// Print and check everything we can about an image. Errors if it's not a
/// valid Wave:3 image, or if a pinned hash for its version doesn't match.
fn validate(bytes: &[u8], expect_version: Option<&str>) -> Result<()> {
    let img = Image::parse(bytes)?;
    let sha = hex(&Sha256::digest(bytes));
    println!("size        {} bytes (payload {})", bytes.len(), img.payload.len());
    println!("target      {:04x}:{:04x}", img.vid, img.pid);
    println!("version     {} (bcdDevice {:04x}), DFU {:04x}", img.version(), img.bcd_device, img.bcd_dfu);
    println!("crc         {:08x} ok", img.crc);
    println!("sha256      {sha}");
    img.check_wave3()?;
    if let Some(v) = expect_version {
        if img.version() != v {
            bail!("catalog says {v} but the image's suffix says {}", img.version());
        }
    }
    match KNOWN_SHA256.iter().find(|(v, _)| *v == img.version()) {
        Some((_, known)) if *known == sha => println!("hash        matches pinned {}", img.version()),
        Some(_) => bail!("sha256 does not match the pinned hash for {}", img.version()),
        None => println!("hash        no pinned hash for {} (suffix CRC only)", img.version()),
    }
    Ok(())
}

fn dfu_test(yes: bool) -> Result<()> {
    if !yes {
        bail!("this disconnects the mic for about a second; re-run with --yes");
    }
    let before = device_version()?.context("Wave:3 not found in run mode")?;
    let s = dfu::run_status()?;
    println!("run mode    firmware {before}, dfu state {}", s.state_name());
    println!("entering bootloader (0xF2)...");
    let b = dfu::enter_bootloader()?;
    let s = b.status()?;
    println!("bootloader  dfu state {} (status {})", s.state_name(), s.status);
    if s.state != 2 || s.status != 0 {
        println!("warning: expected dfuIDLE / status 0");
    }
    println!("leaving bootloader (0xF3)...");
    b.leave()?;
    let after = device_version()?.context("Wave:3 not back in run mode")?;
    println!("run mode    firmware {after}");
    if after != before {
        bail!("firmware version changed ({before} -> {after})?!");
    }
    println!("ok: DFU round-trip works");
    println!("{BROWSER_NOTE}");
    Ok(())
}

fn leave() -> Result<()> {
    match dfu::mode()? {
        Mode::Run => println!("already in run mode"),
        Mode::Absent => bail!("Wave:3 not connected"),
        Mode::Bootloader => {
            let b = dfu::Bootloader::open()?;
            b.clear_error()?;
            b.leave()?;
            println!("back in run mode");
            println!("{BROWSER_NOTE}");
        }
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
