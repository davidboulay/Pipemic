//! Installed apps, read from freedesktop `.desktop` entries, and a matcher
//! from an audio stream's names (application.name, binary, node.name, …)
//! to the entry it belongs to.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    /// File stem, lowercase (e.g. "spotify", "eu.betterbird.betterbird").
    pub stem: String,
    /// Display name as written ("Spotify").
    pub name: String,
    pub icon: String,
    pub categories: Vec<String>,
    /// Lowercase StartupWMClass.
    pub wmclass: String,
    /// Lowercase basename of the Exec binary.
    pub exec: String,
    pub hidden: bool,
}

impl Entry {
    /// Names a rule can use to match this app's streams.
    pub fn keys(&self) -> Vec<String> {
        let mut v = vec![self.name.clone(), self.stem.clone()];
        for k in [&self.wmclass, &self.exec] {
            if !k.is_empty() && !v.contains(k) {
                v.push(k.clone());
            }
        }
        v
    }
}

/// `$XDG_DATA_HOME`, `$XDG_DATA_DIRS`, and the flatpak export dirs.
pub fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut v = vec![std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"))];
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    v.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    v.push(home.join(".local/share/flatpak/exports/share"));
    v.push("/var/lib/flatpak/exports/share".into());
    let mut seen = Vec::new();
    v.retain(|p| {
        let new = !seen.contains(p);
        seen.push(p.clone());
        new
    });
    v
}

fn parse(path: &Path) -> Option<Entry> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut e = Entry {
        stem: path.file_stem()?.to_string_lossy().to_lowercase(),
        name: String::new(),
        icon: String::new(),
        categories: Vec::new(),
        wmclass: String::new(),
        exec: String::new(),
        hidden: false,
    };
    let mut main = false;
    let mut is_app = true;
    for line in text.lines() {
        if line.starts_with('[') {
            main = line.trim() == "[Desktop Entry]";
            continue;
        }
        if !main {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "Name" => e.name = v.to_owned(),
            "Icon" => e.icon = v.to_owned(),
            "Categories" => e.categories = v.split(';').filter(|c| !c.is_empty()).map(str::to_owned).collect(),
            "StartupWMClass" => e.wmclass = v.to_lowercase(),
            "Exec" => {
                let first = v.split_whitespace().find(|t| !t.contains('=') && *t != "env").unwrap_or("");
                e.exec = Path::new(first).file_name().unwrap_or_default().to_string_lossy().to_lowercase();
            }
            "NoDisplay" | "Hidden" => e.hidden |= v == "true",
            "Type" => is_app = v == "Application",
            _ => {}
        }
    }
    (is_app && !e.name.is_empty()).then_some(e)
}

/// Every application entry; earlier data dirs win (user overrides system).
pub fn scan() -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for d in data_dirs() {
        let Ok(rd) = std::fs::read_dir(d.join("applications")) else { continue };
        let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "desktop")).collect();
        paths.sort();
        for p in paths {
            if let Some(e) = parse(&p) {
                if !out.iter().any(|o| o.stem == e.stem) {
                    out.push(e);
                }
            }
        }
    }
    out
}

/// The entry a stream with these names belongs to. Exact name / WM class /
/// file stem first, then reverse-DNS stems ("eu.betterbird.Betterbird") and
/// the Exec binary. A "-bin" suffix is ignored ("betterbird-bin").
pub fn find<'a>(entries: &'a [Entry], keys: &[String]) -> Option<&'a Entry> {
    let lower: Vec<String> = keys
        .iter()
        .map(|k| k.to_lowercase())
        .map(|k| k.strip_suffix("-bin").map(str::to_owned).unwrap_or(k))
        .filter(|k| !k.is_empty())
        .collect();
    let visible = || entries.iter().filter(|e| !e.hidden);
    visible()
        .find(|e| lower.iter().any(|k| e.name.to_lowercase() == *k || e.wmclass == *k || e.stem == *k))
        .or_else(|| visible().find(|e| lower.iter().any(|k| e.stem.ends_with(&format!(".{k}")) || e.exec == *k)))
        .or_else(|| entries.iter().find(|e| lower.iter().any(|k| e.stem == *k || e.exec == *k)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(stem: &str, name: &str, wm: &str, exec: &str) -> Entry {
        Entry {
            stem: stem.into(),
            name: name.into(),
            icon: String::new(),
            categories: vec![],
            wmclass: wm.into(),
            exec: exec.into(),
            hidden: false,
        }
    }

    #[test]
    fn matching() {
        let all = vec![
            e("spotify", "Spotify", "spotify", "spotify"),
            e("eu.betterbird.betterbird", "Betterbird", "eu.betterbird.betterbird", "betterbird"),
            e("google-chrome", "Google Chrome", "google-chrome", "google-chrome-stable"),
        ];
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(find(&all, &k(&["Spotify", "spotify"])).unwrap().name, "Spotify");
        assert_eq!(find(&all, &k(&["betterbird-bin"])).unwrap().name, "Betterbird");
        assert_eq!(find(&all, &k(&["Google Chrome", "chrome"])).unwrap().name, "Google Chrome");
        assert!(find(&all, &k(&["kded6"])).is_none());
    }
}
