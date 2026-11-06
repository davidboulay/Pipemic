//! Add-ons on the UI side: EQ preset files in the Downloads folder, and
//! opening marketplace product pages. The daemon does the actual import
//! (`{"cmd":"eq","import":path}`), so it validates every file itself.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// An Elgato EQ preset file found on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalPreset {
    pub name: String,
    pub path: PathBuf,
}

/// The user's Downloads folder (XDG user dirs, else ~/Downloads).
pub fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let cfg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    if let Ok(t) = std::fs::read_to_string(cfg.join("user-dirs.dirs")) {
        for line in t.lines() {
            if let Some(v) = line.trim().strip_prefix("XDG_DOWNLOAD_DIR=") {
                let v = v.trim_matches('"').replace("$HOME", &home.to_string_lossy());
                return PathBuf::from(v);
            }
        }
    }
    home.join("Downloads")
}

/// Preset name if `xml` looks like an Elgato EQ preset.
pub fn preset_name(xml: &str) -> Option<String> {
    if !xml.contains("<Preset") || !xml.contains("Band 1 Filter") {
        return None;
    }
    let i = xml.find("<Info")?;
    let tag = &xml[i..i + xml[i..].find('>')?];
    let j = tag.find("Name=")? + 5;
    let q = tag[j..].chars().next()?;
    let rest = &tag[j + 1..];
    let name = &rest[..rest.find(q)?];
    Some(name.replace("&amp;", "&").replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">"))
}

/// EQ presets in the Downloads folder and its direct subfolders, newest
/// first, one per name.
pub fn scan_downloads() -> Vec<LocalPreset> {
    let root = downloads_dir();
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    fn walk(dir: &Path, depth: u8, files: &mut Vec<(std::time::SystemTime, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() && depth == 0 {
                walk(&p, 1, files);
            } else if md.is_file() && md.len() < 256 * 1024 && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xml")) {
                files.push((md.modified().unwrap_or(std::time::UNIX_EPOCH), p));
            }
        }
    }
    walk(&root, 0, &mut files);
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<LocalPreset> = Vec::new();
    for (_, path) in files {
        let Some(name) = std::fs::read_to_string(&path).ok().as_deref().and_then(preset_name) else { continue };
        if !out.iter().any(|p| p.name == name) {
            out.push(LocalPreset { name, path });
        }
    }
    out
}

/// Open a web page in the user's browser.
pub fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open").arg(url).spawn().map(|_| ()).map_err(|e| format!("couldn't open the browser: {e}"))
}

/// "Peak 2037 Hz, +12.0 dB, Q 3.1" for one band of the daemon's config.
pub fn band_label(b: &Value) -> String {
    let kind = match b["kind"].as_str().unwrap_or("") {
        "highpass" => "High-pass",
        "lowpass" => "Low-pass",
        "lowshelf" => "Low shelf",
        "highshelf" => "High shelf",
        "peak" => "Peak",
        k => k,
    };
    let f = b["freq"].as_f64().unwrap_or(0.0);
    let freq = if f >= 1000.0 { format!("{:.2} kHz", f / 1000.0).replace(".00 ", " ") } else { format!("{f:.0} Hz") };
    let q = b["q"].as_f64().unwrap_or(0.0);
    match b["kind"].as_str() {
        Some("highpass" | "lowpass") => format!("{kind} {freq}, Q {q:.2}"),
        _ => format!("{kind} {freq}, {:+.1} dB, Q {q:.2}", b["gain"].as_f64().unwrap_or(0.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(preset_name(r#"<Preset><Info Name="Warm &amp; Clear" Position="1"/><PARAM id="Band 1 Filter Type"/></Preset>"#).as_deref(), Some("Warm & Clear"));
        assert_eq!(preset_name("<Preset><Info Name='x'/></Preset>"), None, "no bands");
        assert_eq!(preset_name("<Foo/>"), None);
    }

    #[test]
    fn labels() {
        let b = serde_json::json!({"kind":"peak","freq":2037.0,"q":3.1,"gain":12.04});
        assert_eq!(band_label(&b), "Peak 2.04 kHz, +12.0 dB, Q 3.10");
        let b = serde_json::json!({"kind":"highpass","freq":100.0,"q":0.71,"gain":0.0});
        assert_eq!(band_label(&b), "High-pass 100 Hz, Q 0.71");
    }
}
