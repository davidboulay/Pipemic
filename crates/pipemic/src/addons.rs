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
    /// From the marketplace file name, e.g. "Old_Radio-1.0.0_HdYjEzV.xml".
    pub version: Option<String>,
    pub modified: std::time::SystemTime,
}

impl LocalPreset {
    /// Is this file (a download of) marketplace add-on `name`? Compares
    /// letters and digits only, against the preset's own name and the
    /// file name's prefix.
    pub fn is(&self, name: &str) -> bool {
        let key = norm(name);
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let prefix = stem.rsplit_once('-').map_or(stem.as_str(), |(p, _)| p).to_string();
        !key.is_empty() && (norm(&self.name) == key || norm(&prefix) == key)
    }
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// "Old_Radio-1.0.0_HdYjEzV" -> "1.0.0".
pub fn file_version(stem: &str) -> Option<String> {
    let (_, tail) = stem.rsplit_once('-')?;
    let v = tail.split('_').next()?;
    (v.chars().next()?.is_ascii_digit() && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| v.to_string())
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

/// Pipemic's preset library: presets added from the marketplace.
pub fn library_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("pipemic/presets")
}

/// Copy a preset file into the library (same file name); returns the copy.
pub fn add_to_library(path: &Path) -> Result<PathBuf, String> {
    let dir = library_dir();
    if path.starts_with(&dir) {
        return Ok(path.to_path_buf());
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dst = dir.join(path.file_name().ok_or("not a file")?);
    std::fs::copy(path, &dst).map_err(|e| format!("copying {}: {e}", path.display()))?;
    Ok(dst)
}

/// Variant id from a marketplace preset link
/// (`wavelink://presets/install/<variant id>`); None for other add-ons.
pub fn preset_link(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let id = rest.strip_prefix("presets/install/")?.trim_end_matches('/');
    let id = id.split(['?', '#', '/']).next()?;
    (id.len() >= 8 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then(|| id.to_string())
}

/// Every preset file (library, then Downloads) with this preset name.
fn files_named(name: &str) -> Vec<PathBuf> {
    all_files().into_iter().filter(|(_, p)| std::fs::read_to_string(p).ok().as_deref().and_then(preset_name).as_deref() == Some(name)).map(|(_, p)| p).collect()
}

/// Move all files of preset `name` to the trash.
pub fn trash_preset(name: &str) -> Result<(), String> {
    for p in files_named(name) {
        trash(&p)?;
    }
    Ok(())
}

/// Candidate preset files: the library first, then Downloads (and its
/// direct subfolders), each newest first.
fn all_files() -> Vec<(std::time::SystemTime, PathBuf)> {
    let mut lib = Vec::new();
    walk(&library_dir(), 1, &mut lib);
    lib.sort_by(|a, b| b.0.cmp(&a.0));
    let mut dl = Vec::new();
    walk(&downloads_dir(), 0, &mut dl);
    dl.sort_by(|a, b| b.0.cmp(&a.0));
    lib.extend(dl);
    lib
}

/// EQ presets in the library and the Downloads folder, one per name (the
/// library copy wins).
pub fn scan_downloads() -> Vec<LocalPreset> {
    let files = all_files();
    let mut out: Vec<LocalPreset> = Vec::new();
    for (modified, path) in files {
        let Some(name) = std::fs::read_to_string(&path).ok().as_deref().and_then(preset_name) else { continue };
        if !out.iter().any(|p| p.name == name) {
            let version = path.file_stem().and_then(|s| file_version(&s.to_string_lossy()));
            out.push(LocalPreset { name, path, version, modified });
        }
    }
    out
}

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

/// Move a file to the trash (freedesktop trash via `gio`, else KDE's
/// `kioclient`); never deletes outright.
pub fn trash(path: &Path) -> Result<(), String> {
    let tries: [(&str, Vec<&std::ffi::OsStr>); 2] = [
        ("gio", vec!["trash".as_ref(), path.as_os_str()]),
        ("kioclient", vec!["move".as_ref(), path.as_os_str(), "trash:/".as_ref()]),
    ];
    for (cmd, args) in tries {
        if let Ok(st) = std::process::Command::new(cmd).args(args).status() {
            if st.success() {
                return Ok(());
            }
        }
    }
    Err(format!("couldn't move {} to the trash", path.display()))
}

fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".cache")).join("pipemic/addons")
}

/// Thumbnail bytes per add-on id, from the cache or the CDN (cached by the
/// URL's file name, so an updated image is fetched again).
pub fn thumbnails(list: Vec<(String, String)>) -> Vec<(String, Vec<u8>)> {
    let dir = cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let dir = &dir;
    std::thread::scope(|sc| {
        let jobs: Vec<_> = list.into_iter().map(|(id, url)| sc.spawn(move || thumbnail(dir, id, url))).collect();
        jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect()
    })
}

fn thumbnail(dir: &Path, id: String, url: String) -> Option<(String, Vec<u8>)> {
    std::iter::once((id, url))
        .filter_map(|(id, url)| {
            let file: String = url.rsplit('/').next()?.chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).collect();
            let path = dir.join(&file);
            if let Ok(b) = std::fs::read(&path) {
                return Some((id, b));
            }
            let b = wave_catalog::marketplace::get_bytes(&url).ok()?;
            let _ = std::fs::write(&path, &b);
            Some((id, b))
        })
        .next()
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
    fn matching() {
        assert_eq!(file_version("Old_Radio-1.0.0_HdYjEzV").as_deref(), Some("1.0.0"));
        assert_eq!(file_version("my-preset"), None);
        let p = LocalPreset {
            name: "Old Radio".into(),
            path: "/d/Old_Radio-1.0.0_HdYjEzV.xml".into(),
            version: Some("1.0.0".into()),
            modified: std::time::UNIX_EPOCH,
        };
        assert!(p.is("Old Radio"));
        assert!(p.is("old radio"));
        assert!(!p.is("Old Radio 2"));
        let q = LocalPreset { name: "Wave:3".into(), path: "/d/Wave_3-1.0.0_x.xml".into(), version: None, modified: std::time::UNIX_EPOCH };
        assert!(q.is("Wave 3"));
    }

    #[test]
    fn links() {
        assert_eq!(preset_link("wavelink://presets/install/e1085e92-1a5d-4ec5-a978-516a12de16c2").as_deref(), Some("e1085e92-1a5d-4ec5-a978-516a12de16c2"));
        assert_eq!(preset_link("wavelink-v2://presets/install/e1085e92-1a5d/?x=1").as_deref(), Some("e1085e92-1a5d"));
        assert_eq!(preset_link("wavelink://vst/install/e1085e92-1a5d"), None);
        assert_eq!(preset_link("wavelink://presets/install/../../etc"), None);
    }

    #[test]
    fn labels() {
        let b = serde_json::json!({"kind":"peak","freq":2037.0,"q":3.1,"gain":12.04});
        assert_eq!(band_label(&b), "Peak 2.04 kHz, +12.0 dB, Q 3.10");
        let b = serde_json::json!({"kind":"highpass","freq":100.0,"q":0.71,"gain":0.0});
        assert_eq!(band_label(&b), "High-pass 100 Hz, Q 0.71");
    }
}
