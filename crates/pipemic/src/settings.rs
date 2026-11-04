//! App preferences: update check, start at login. (The output fallback is
//! a daemon setting and lives in mixer.toml.)
//!
//! Updates come from the git checkout the install script recorded in
//! `$XDG_DATA_HOME/pipemic/source`: "check" fetches and counts commits on
//! the upstream branch that this build doesn't have; "update" pulls and
//! re-runs `packaging/install-user.sh`.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

/// Commit this binary was built from (build.rs), or "" if unknown.
pub const BUILD_COMMIT: &str = env!("PIPEMIC_GIT");

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}
fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}
fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"))
}

#[derive(Debug, Clone)]
pub struct Prefs {
    pub auto_check: bool,
}

fn prefs_path() -> PathBuf {
    config_dir().join("pipemic/ui.json")
}

impl Prefs {
    pub fn load() -> Self {
        let v: Value = std::fs::read_to_string(prefs_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        Self { auto_check: v["auto_check_updates"].as_bool().unwrap_or(true) }
    }
    pub fn save(&self) {
        let p = prefs_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, json!({"auto_check_updates": self.auto_check}).to_string());
    }
}

// ---- start at login (XDG autostart) --------------------------------------

fn autostart_path() -> PathBuf {
    config_dir().join("autostart/pipemic.desktop")
}

pub fn autostart() -> bool {
    autostart_path().exists()
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    let p = autostart_path();
    if !on {
        return match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(
        &p,
        "[Desktop Entry]\nType=Application\nName=Pipemic\nExec=pipemic\nIcon=pipemic\nX-GNOME-Autostart-enabled=true\n",
    )
    .map_err(|e| e.to_string())
}

// ---- updates -------------------------------------------------------------

pub fn source_dir() -> Option<PathBuf> {
    let p = std::fs::read_to_string(data_dir().join("pipemic/source")).ok()?;
    let p = PathBuf::from(p.trim());
    p.join(".git").exists().then_some(p)
}

fn git(dir: &PathBuf, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("git failed").to_owned())
    }
}

#[derive(Debug, Clone)]
pub enum UpdateState {
    Idle,
    Busy(&'static str),
    UpToDate,
    Available(u32),
    Updated,
    Failed(String),
}

/// Blocking: fetch and count upstream commits not in this build.
pub fn check() -> UpdateState {
    let Some(dir) = source_dir() else { return UpdateState::Failed("source folder unknown: run packaging/install-user.sh once".into()) };
    if let Err(e) = git(&dir, &["fetch", "--quiet"]) {
        return UpdateState::Failed(e);
    }
    let base = if BUILD_COMMIT.is_empty() { "HEAD" } else { BUILD_COMMIT };
    match git(&dir, &["rev-list", "--count", &format!("{base}..@{{u}}")]) {
        Ok(n) => match n.parse::<u32>() {
            Ok(0) => UpdateState::UpToDate,
            Ok(n) => UpdateState::Available(n),
            Err(_) => UpdateState::Failed(n),
        },
        Err(e) => UpdateState::Failed(e),
    }
}

/// Blocking: pull and reinstall (rebuilds, restarts the daemon).
pub fn update() -> UpdateState {
    let Some(dir) = source_dir() else { return UpdateState::Failed("source folder unknown".into()) };
    if let Err(e) = git(&dir, &["pull", "--ff-only", "--quiet"]) {
        return UpdateState::Failed(e);
    }
    match Command::new(dir.join("packaging/install-user.sh")).output() {
        Ok(o) if o.status.success() => UpdateState::Updated,
        Ok(o) => UpdateState::Failed(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("install failed").to_owned()),
        Err(e) => UpdateState::Failed(e.to_string()),
    }
}
