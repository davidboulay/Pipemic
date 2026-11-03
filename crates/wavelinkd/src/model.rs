//! Mixer model: channels × mixes with volume/mute per channel, per mix and
//! per crosspoint (route). Mirrors `channels` / `mixes` / `channelToMix` in
//! Wave Link 3's `config.json` (see docs/wavelink3-architecture.md).
//!
//! Stored as TOML at `$XDG_CONFIG_HOME/wavelink/mixer.toml`. Volumes are dB,
//! -60 ..= 0 like Wave Link; -60 means silent.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MIN_DB: f32 = -60.0;
/// The built-in filter-graph mixer has 8 inputs.
pub const MAX_CHANNELS: usize = 8;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Channel {
    pub id: String,
    pub label: String,
    /// Capture device, matched by node.name prefix. None = virtual sink
    /// "Wave Link <label>" that apps play into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default)]
    pub volume: f32,
    #[serde(default)]
    pub mute: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MixKind {
    /// Virtual source that OBS / Zoom record from.
    Source,
    /// Playback stream, routed by the session manager to the default output.
    Monitor,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Mix {
    pub id: String,
    pub label: String,
    pub kind: MixKind,
    #[serde(default)]
    pub volume: f32,
    #[serde(default)]
    pub mute: bool,
}

/// One crosspoint. Missing routes default to 0 dB, unmuted.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Route {
    pub mix: String,
    pub channel: String,
    #[serde(default)]
    pub volume: f32,
    #[serde(default)]
    pub mute: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(rename = "channel")]
    pub channels: Vec<Channel>,
    #[serde(rename = "mix")]
    pub mixes: Vec<Mix>,
    #[serde(default, rename = "route")]
    pub routes: Vec<Route>,
}

fn ch(id: &str, label: &str, device: Option<&str>) -> Channel {
    Channel { id: id.into(), label: label.into(), device: device.map(Into::into), volume: 0.0, mute: false }
}

impl Default for Config {
    fn default() -> Self {
        let mix = |id: &str, label: &str, kind| Mix { id: id.into(), label: label.into(), kind, volume: 0.0, mute: false };
        let muted = |mix: &str, channel: &str| Route { mix: mix.into(), channel: channel.into(), volume: 0.0, mute: true };
        Self {
            channels: vec![
                ch("mic", "Microphone", Some("alsa_input.usb-Elgato_Systems_Elgato_Wave_3")),
                ch("system", "System", None),
                ch("music", "Music", None),
                ch("browser", "Browser", None),
                ch("voicechat", "Voice Chat", None),
            ],
            mixes: vec![
                mix("personal", "Personal Mix", MixKind::Monitor),
                mix("stream", "Stream Mix", MixKind::Source),
                mix("chat", "Chat Mix", MixKind::Source),
            ],
            // Don't monitor your own mic; don't echo voice chat back to it.
            routes: vec![muted("personal", "mic"), muted("chat", "voicechat")],
        }
    }
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("wavelink").join("mixer.toml")
}

fn db_to_lin(db: f32) -> f32 {
    if db <= MIN_DB {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

pub fn check_db(v: f32) -> Result<f32> {
    if !(MIN_DB..=0.0).contains(&v) || !v.is_finite() {
        bail!("volume {v} dB out of range ({MIN_DB}..0)");
    }
    Ok(v)
}

impl Config {
    /// Load, or create the default file if there is none.
    pub fn load_or_init(path: &std::path::Path) -> Result<Self> {
        if !path.exists() {
            let c = Self::default();
            c.save(path)?;
            eprintln!("wavelinkd: wrote default config to {}", path.display());
            return Ok(c);
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let c: Self = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        c.validate()?;
        Ok(c)
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.channels.is_empty() || self.channels.len() > MAX_CHANNELS {
            bail!("need 1..={MAX_CHANNELS} channels, got {}", self.channels.len());
        }
        let mut seen = std::collections::HashSet::new();
        for id in self.channels.iter().map(|c| &c.id).chain(self.mixes.iter().map(|m| &m.id)) {
            if !seen.insert(id) || id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                bail!("bad or duplicate id {id:?}");
            }
        }
        for c in &self.channels {
            check_db(c.volume)?;
        }
        for m in &self.mixes {
            check_db(m.volume)?;
        }
        for r in &self.routes {
            self.channel_index(&r.channel)?;
            self.mix_index(&r.mix)?;
            check_db(r.volume)?;
        }
        Ok(())
    }

    pub fn channel_index(&self, id: &str) -> Result<usize> {
        self.channels.iter().position(|c| c.id == id).with_context(|| format!("no channel {id:?}"))
    }
    pub fn mix_index(&self, id: &str) -> Result<usize> {
        self.mixes.iter().position(|m| m.id == id).with_context(|| format!("no mix {id:?}"))
    }

    /// The route entry for (mix, channel), created on demand.
    pub fn route_mut(&mut self, mix: &str, channel: &str) -> &mut Route {
        if let Some(i) = self.routes.iter().position(|r| r.mix == mix && r.channel == channel) {
            return &mut self.routes[i];
        }
        self.routes.push(Route { mix: mix.into(), channel: channel.into(), volume: 0.0, mute: false });
        self.routes.last_mut().unwrap()
    }

    /// Linear gain for channel `ci` in mix `mi`.
    pub fn gain(&self, mi: usize, ci: usize) -> f32 {
        let (m, c) = (&self.mixes[mi], &self.channels[ci]);
        let r = self.routes.iter().find(|r| r.mix == m.id && r.channel == c.id);
        let (rv, rm) = r.map_or((0.0, false), |r| (r.volume, r.mute));
        if m.mute || c.mute || rm {
            return 0.0;
        }
        db_to_lin((m.volume + c.volume + rv).max(MIN_DB))
    }

    /// Filter-graph control values for a mix: ("mixL:Gain N", g), ("mixR:Gain N", g).
    pub fn mix_controls(&self, mi: usize) -> Vec<(String, f32)> {
        (0..self.channels.len())
            .flat_map(|ci| {
                let g = self.gain(mi, ci);
                [(format!("mixL:Gain {}", ci + 1), g), (format!("mixR:Gain {}", ci + 1), g)]
            })
            .collect()
    }

    pub fn sink_node_name(c: &Channel) -> String {
        format!("wavelink.channel.{}", c.id)
    }
    pub fn mix_in_node_name(m: &Mix) -> String {
        format!("wavelink.mix.{}.in", m.id)
    }
    pub fn mix_out_node_name(m: &Mix) -> String {
        format!("wavelink.mix.{}", m.id)
    }
    /// Capture port of a mix input for channel `i`, side 0 = L, 1 = R.
    pub fn mix_in_port(i: usize, side: usize) -> String {
        format!("input_AUX{}", 2 * i + side)
    }

    /// SPA-JSON args for libpipewire-module-filter-chain implementing one mix:
    /// two builtin mixers (L, R), inputs 2i / 2i+1 = channel i.
    pub fn filter_chain_args(&self, mi: usize) -> String {
        let m = &self.mixes[mi];
        let n = self.channels.len();
        let gains: Vec<f32> = (0..n).map(|ci| self.gain(mi, ci)).collect();
        let controls: String = gains.iter().enumerate().map(|(i, g)| format!("\"Gain {}\" = {g} ", i + 1)).collect();
        let inputs: String = (1..=n).map(|i| format!("\"mixL:In {i}\" \"mixR:In {i}\" ")).collect();
        let positions: String = (0..2 * n).map(|i| format!("AUX{i} ")).collect();
        let class = match m.kind {
            MixKind::Source => "media.class = \"Audio/Source\"",
            MixKind::Monitor => "",
        };
        format!(
            r#"{{
  node.description = "Wave Link {label}"
  media.name = "Wave Link {label}"
  filter.graph = {{
    nodes = [
      {{ name = mixL type = builtin label = mixer control = {{ {controls}}} }}
      {{ name = mixR type = builtin label = mixer control = {{ {controls}}} }}
    ]
    inputs = [ {inputs}]
    outputs = [ "mixL:Out" "mixR:Out" ]
  }}
  capture.props = {{
    node.name = "{in_name}"
    node.description = "Wave Link {label} (input)"
    audio.position = [ {positions}]
    node.autoconnect = false
    stream.dont-remix = true
  }}
  playback.props = {{
    node.name = "{out_name}"
    node.description = "Wave Link {label}"
    {class}
    audio.position = [ FL FR ]
  }}
}}"#,
            label = m.label,
            in_name = Self::mix_in_node_name(m),
            out_name = Self::mix_out_node_name(m),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_gains() {
        let c = Config::default();
        let (p, s) = (c.mix_index("personal").unwrap(), c.mix_index("stream").unwrap());
        let (mic, music) = (c.channel_index("mic").unwrap(), c.channel_index("music").unwrap());
        assert_eq!(c.gain(p, mic), 0.0);
        assert_eq!(c.gain(s, mic), 1.0);
        assert_eq!(c.gain(s, music), 1.0);
    }

    #[test]
    fn db_math() {
        let mut c = Config::default();
        c.channels[2].volume = -6.0;
        c.route_mut("stream", "music").volume = -14.0;
        let g = c.gain(1, 2);
        assert!((g - 0.1).abs() < 1e-3, "{g}"); // -20 dB
        c.mixes[1].volume = -60.0;
        assert_eq!(c.gain(1, 2), 0.0);
        c.mixes[1].volume = 0.0;
        c.mixes[1].mute = true;
        assert_eq!(c.gain(1, 0), 0.0);
    }

    #[test]
    fn toml_roundtrip() {
        let c = Config::default();
        let t = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&t).unwrap();
        back.validate().unwrap();
        assert_eq!(back.channels.len(), 5);
        assert_eq!(back.routes.len(), 2);
        assert!(t.contains("kind = \"monitor\""));
    }

    #[test]
    fn args_shape() {
        let c = Config::default();
        let a = c.filter_chain_args(1);
        assert!(a.contains("\"mixL:In 5\" \"mixR:In 5\""));
        assert!(a.contains("AUX9 ]"));
        assert!(a.contains("media.class = \"Audio/Source\""));
        let p = c.filter_chain_args(0);
        assert!(p.contains("\"Gain 1\" = 0 "), "{p}");
        assert!(!p.contains("media.class"));
    }

    #[test]
    fn validation() {
        let mut c = Config::default();
        c.routes.push(Route { mix: "nope".into(), channel: "mic".into(), volume: 0.0, mute: false });
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.channels[1].volume = 3.0;
        assert!(c.validate().is_err());
    }
}
