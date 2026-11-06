//! Icons: built-in symbolic glyphs for the UI, and app icons looked up the
//! freedesktop way (desktop entry → Icon= → icon theme), cached per app.

use iced::widget::{image, svg};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---- built-in glyphs (24×24, stroked; tinted by the svg widget) ----------

macro_rules! glyph {
    ($body:expr) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#,
            $body,
            "</svg>"
        )
    };
}

pub const MIC: &str = glyph!(r#"<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>"#);
pub const MUSIC: &str = glyph!(r#"<path d="M9 18V5l11-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="17" cy="16" r="3"/>"#);
pub const BROWSER: &str = glyph!(r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18"/>"#);
pub const SYSTEM: &str = glyph!(r#"<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>"#);
pub const CHAT: &str = glyph!(r#"<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12z"/>"#);
pub const GENERIC: &str = glyph!(r#"<path d="M4 12h3l3-7 4 14 3-7h3"/>"#);
pub const HEADPHONES: &str = glyph!(r#"<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="4" height="7" rx="1.5"/><rect x="17" y="14" width="4" height="7" rx="1.5"/>"#);
pub const LINK: &str = glyph!(r#"<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>"#);
pub const SLASH: &str = glyph!(r#"<path d="M3 3l18 18"/>"#);
pub const CHEVRON: &str = glyph!(r#"<path d="M6 9l6 6 6-6"/>"#);
pub const SPEAKER: &str = glyph!(r#"<path d="M11 5L6 9H3v6h3l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7M18.5 5.5a9 9 0 0 1 0 13"/>"#);
pub const GEAR: &str = glyph!(r#"<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>"#);
/// Arrow rising out of a tray: "sent out to others". (Reads cleanly with
/// the mute slash across it, unlike a diagonal arrow.)
pub const OUTGOING: &str = glyph!(r#"<path d="M12 15V4M7.5 8.5L12 4l4.5 4.5"/><path d="M4 14v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4"/>"#);
/// Equalizer / effects: three faders.
pub const EQ: &str = glyph!(r#"<path d="M6 4v5M6 13v7M12 4v11M12 19v1M18 4v2M18 10v10"/><circle cx="6" cy="11" r="2"/><circle cx="12" cy="17" r="2"/><circle cx="18" cy="8" r="2"/>"#);
pub const PLUG: &str = glyph!(r#"<path d="M9 2v5M15 2v5M6 7h12v4a6 6 0 0 1-12 0zM12 17v5"/>"#);

pub fn glyph(src: &'static str) -> svg::Handle {
    svg::Handle::from_memory(src.as_bytes())
}

/// Glyph for a channel id.
pub fn channel_glyph(id: &str) -> &'static str {
    match id {
        "mic" => MIC,
        "music" => MUSIC,
        "browser" => BROWSER,
        "system" | "other" => SYSTEM,
        "voicechat" => CHAT,
        _ => GENERIC,
    }
}

// ---- app icons -----------------------------------------------------------

#[derive(Clone, Debug)]
pub enum AppIcon {
    Svg(svg::Handle),
    Raster(image::Handle),
}

#[derive(Default)]
pub struct Icons {
    theme_dirs: Option<Vec<PathBuf>>,
    cache: HashMap<String, Option<AppIcon>>,
}

fn icon_theme() -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let kdeglobals = std::fs::read_to_string(home.join(".config/kdeglobals")).unwrap_or_default();
    let mut in_icons = false;
    for line in kdeglobals.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_icons = l == "[Icons]";
        } else if in_icons {
            if let Some(t) = l.strip_prefix("Theme=") {
                return t.to_owned();
            }
        }
    }
    "breeze-dark".into()
}

/// App-icon directories of the theme chain, most specific first, bigger sizes first.
fn load_theme_dirs() -> Vec<PathBuf> {
    let theme = icon_theme();
    let mut themes = vec![theme.clone()];
    if let Some(base) = theme.strip_suffix("-Dark").or_else(|| theme.strip_suffix("-dark")) {
        themes.push(base.to_owned());
    }
    themes.extend(["breeze-dark", "breeze", "Papirus", "Adwaita", "hicolor"].map(String::from));
    let mut out = Vec::new();
    for t in &themes {
        for base in desktop_apps::data_dirs() {
            let root = base.join("icons").join(t);
            let Ok(rd) = std::fs::read_dir(&root) else { continue };
            let mut sizes: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            // "scalable" first, then larger pixel sizes.
            sizes.sort_by_key(|p| {
                let n = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                if n.starts_with("scalable") {
                    0
                } else {
                    -(n.split('x').next().and_then(|x| x.parse::<i32>().ok()).unwrap_or(1))
                }
            });
            for s in sizes {
                for sub in ["apps", "scalable/apps", "48/apps"] {
                    let d = s.join(sub);
                    if d.is_dir() {
                        out.push(d);
                    }
                }
                // breeze layout: apps/<size>/
                if s.file_name().is_some_and(|n| n == "apps") {
                    for sub in ["scalable", "64", "48", "32"] {
                        let d = s.join(sub);
                        if d.is_dir() {
                            out.push(d);
                        }
                    }
                }
            }
        }
    }
    out.push("/usr/share/pixmaps".into());
    out
}

impl Icons {
    /// Resolve a desktop-entry Icon= value (theme name or absolute path).
    pub fn get(&mut self, name: &str) -> Option<AppIcon> {
        if let Some(c) = self.cache.get(name) {
            return c.clone();
        }
        let path = if Path::new(name).is_absolute() {
            Path::new(name).exists().then(|| PathBuf::from(name))
        } else {
            let dirs = self.theme_dirs.get_or_insert_with(load_theme_dirs);
            dirs.iter().find_map(|d| ["svg", "png"].iter().map(|e| d.join(format!("{name}.{e}"))).find(|p| p.exists()))
        };
        let found = path.map(|p| {
            if p.extension().is_some_and(|e| e == "svg") {
                AppIcon::Svg(svg::Handle::from_path(p))
            } else {
                AppIcon::Raster(image::Handle::from_path(p))
            }
        });
        self.cache.insert(name.to_owned(), found.clone());
        found
    }
}
