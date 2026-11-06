//! Mixer model: channels × mixes with volume/mute per channel, per mix and
//! per crosspoint (route). Mirrors `channels` / `mixes` / `channelToMix` in
//! Wave Link 3's `config.json` (see docs/wavelink3-architecture.md).
//!
//! Stored as TOML at `$XDG_CONFIG_HOME/pipemic/mixer.toml`. Volumes are dB,
//! -60 ..= 0 like Wave Link; -60 means silent.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::eq::{Equalizer, BANDS, IDENTITY};

pub const MIN_DB: f32 = -60.0;
/// The built-in filter-graph mixer has 8 inputs.
pub const MAX_CHANNELS: usize = 8;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Channel {
    pub id: String,
    pub label: String,
    /// Capture device, matched by node.name prefix. None = virtual sink
    /// "Pipemic <label>" that apps play into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default)]
    pub volume: f32,
    #[serde(default)]
    pub mute: bool,
    /// UI: move this channel's mix faders together.
    #[serde(default)]
    pub link: bool,
    /// Apps whose desktop entry has one of these Categories= land here
    /// unless a rule says otherwise (e.g. "WebBrowser").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// Catch-all: apps no rule or category claims play here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
    /// Equalizer on this channel (before the mixes), e.g. an imported
    /// marketplace EQ preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<Equalizer>,
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
    /// Monitor mixes: output device (node.name). None = the default output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
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

/// Send app streams to a channel. A stream matches if any of its
/// application.name, application.process.binary, node.name or
/// pipewire.access.portal.app_id equals one of `apps` (case-insensitive,
/// `*` wildcard). Applied once, when the stream appears.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Rule {
    pub channel: String,
    pub apps: Vec<String>,
}

/// Case-insensitive match with `*` as wildcard.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.to_lowercase(), text.to_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == t;
    }
    let mut rest = t.as_str();
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(part) else { return false };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            let Some(pos) = rest.find(part) else { return false };
            rest = &rest[pos + part.len()..];
        }
    }
    true
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// When a monitor mix's chosen output is unplugged, play to the default
    /// output until it's back.
    #[serde(default = "yes")]
    pub output_fallback: bool,
    #[serde(rename = "channel")]
    pub channels: Vec<Channel>,
    #[serde(rename = "mix")]
    pub mixes: Vec<Mix>,
    #[serde(default, rename = "route")]
    pub routes: Vec<Route>,
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
    /// Apps kept out of their automatic (category) channel; they play to
    /// the default output unless a rule places them.
    #[serde(default)]
    pub excluded: Vec<String>,
}

fn ch(id: &str, label: &str, device: Option<&str>, categories: &[&str]) -> Channel {
    Channel {
        id: id.into(),
        label: label.into(),
        device: device.map(Into::into),
        volume: 0.0,
        mute: false,
        link: false,
        categories: categories.iter().map(|c| c.to_string()).collect(),
        fallback: false,
        eq: None,
    }
}

/// Decides which channel an app's streams go to: an explicit rule, else
/// the app's desktop category, else the catch-all channel.
#[derive(Clone, Default, Debug)]
pub struct Router {
    rules: Vec<(String, Vec<String>)>,
    excluded: Vec<String>,
    categories: Vec<(String, Vec<String>)>,
    fallback: Option<String>,
}

impl Router {
    /// (channel id, set by an explicit rule?)
    pub fn route(&self, keys: &[String], categories: &[String]) -> Option<(&str, bool)> {
        if let Some((c, _)) = self.rules.iter().find(|(_, ps)| ps.iter().any(|p| keys.iter().any(|k| glob(p, k)))) {
            return Some((c, true));
        }
        if self.is_excluded(keys) {
            return None;
        }
        self.auto(categories).map(|c| (c, false))
    }

    /// Was the app taken out of its automatic channel?
    pub fn is_excluded(&self, keys: &[String]) -> bool {
        self.excluded.iter().any(|p| keys.iter().any(|k| glob(p, k)))
    }

    /// Where the app goes without rules: by category, else the catch-all.
    pub fn auto(&self, categories: &[String]) -> Option<&str> {
        self.categories
            .iter()
            .find(|(_, cs)| cs.iter().any(|c| categories.contains(c)))
            .map(|(c, _)| c.as_str())
            .or(self.fallback.as_deref())
    }
}

impl Default for Config {
    fn default() -> Self {
        let mix = |id: &str, label: &str, kind| Mix { id: id.into(), label: label.into(), kind, volume: 0.0, mute: false, output: None };
        let muted = |mix: &str, channel: &str| Route { mix: mix.into(), channel: channel.into(), volume: 0.0, mute: true };
        Self {
            output_fallback: true,
            channels: vec![
                ch("mic", "Microphone", Some("alsa_input.usb-Elgato_Systems_Elgato_Wave_3"), &[]),
                ch("music", "Music", None, &["Music", "Audio"]),
                ch("browser", "Browser", None, &["WebBrowser"]),
                ch("voicechat", "Voice Chat", None, &["Chat", "InstantMessaging", "VideoConference", "Telephony"]),
                // Manual only: holds the apps you add; nothing lands here by itself.
                ch("other", "Other", None, &[]),
            ],
            mixes: vec![
                // What you hear.
                mix("personal", "Personal", MixKind::Monitor),
                // What the other side hears: pick "Pipemic Outgoing" as the
                // mic in OBS / Discord / Zoom.
                mix("stream", "Outgoing", MixKind::Source),
            ],
            // Don't monitor your own mic. Only the mic goes out until you
            // unmute a channel in Outgoing (no surprise leaks to a call or
            // stream; voice chat staying muted also avoids echoing callers).
            routes: vec![
                muted("personal", "mic"),
                muted("stream", "music"),
                muted("stream", "browser"),
                muted("stream", "voicechat"),
                muted("stream", "other"),
            ],
            // Categories place most apps; rules are for exceptions.
            rules: vec![],
            excluded: vec![],
        }
    }
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    let path = base.join("pipemic").join("mixer.toml");
    // Carry settings over from the app's earlier name.
    let old = base.join("wavelink").join("mixer.toml");
    if !path.exists() && old.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::copy(&old, &path).is_ok() {
            eprintln!("pipemicd: moved settings from {}", old.display());
        }
    }
    path
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
            eprintln!("pipemicd: wrote default config to {}", path.display());
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
            if let Some(eq) = &c.eq {
                eq.validate().with_context(|| format!("equalizer of channel {:?}", c.id))?;
            }
        }
        for m in &self.mixes {
            check_db(m.volume)?;
        }
        if self.channels.iter().filter(|c| c.fallback).count() > 1 {
            bail!("only one channel can be the catch-all (fallback = true)");
        }
        for c in self.channels.iter().filter(|c| c.device.is_some()) {
            if c.fallback || !c.categories.is_empty() {
                bail!("device channel {:?} can't take apps (fallback / categories)", c.id);
            }
        }
        for r in &self.rules {
            let ci = self.channel_index(&r.channel)?;
            if self.channels[ci].device.is_some() {
                bail!("rule targets device channel {:?}; apps can only go to virtual channels", r.channel);
            }
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

    pub fn router(&self) -> Router {
        let virt = || self.channels.iter().filter(|c| c.device.is_none());
        let mut rules: Vec<(String, Vec<String>)> = Vec::new();
        for r in &self.rules {
            match rules.iter_mut().find(|(c, _)| *c == r.channel) {
                Some((_, ps)) => ps.extend(r.apps.iter().cloned()),
                None => rules.push((r.channel.clone(), r.apps.clone())),
            }
        }
        Router {
            rules,
            excluded: self.excluded.clone(),
            categories: virt().filter(|c| !c.categories.is_empty()).map(|c| (c.id.clone(), c.categories.clone())).collect(),
            fallback: virt().find(|c| c.fallback).map(|c| c.id.clone()),
        }
    }

    /// Sink node of a channel id.
    pub fn sink_for(id: &str) -> String {
        format!("pipemic.channel.{id}")
    }

    /// Route app `name` (also known as `keys`) to `channel`, or to no channel
    /// (the default output). Patterns from other rules that matched it go.
    pub fn assign(&mut self, name: &str, keys: &[String], channel: Option<&str>) -> Result<()> {
        if let Some(c) = channel {
            let ci = self.channel_index(c)?;
            if self.channels[ci].device.is_some() {
                bail!("apps can only go to virtual channels, not {c:?}");
            }
        }
        let mut all: Vec<String> = keys.to_vec();
        all.push(name.to_owned());
        for r in &mut self.rules {
            r.apps.retain(|p| !all.iter().any(|k| glob(p, k)));
        }
        self.rules.retain(|r| !r.apps.is_empty());
        self.excluded.retain(|p| !all.iter().any(|k| glob(p, k)));
        if let Some(c) = channel {
            match self.rules.iter_mut().find(|r| r.channel == c) {
                Some(r) => r.apps.push(name.to_owned()),
                None => self.rules.push(Rule { channel: c.to_owned(), apps: vec![name.to_owned()] }),
            }
        }
        Ok(())
    }

    /// Keep app `name` out of its automatic channel.
    pub fn exclude(&mut self, name: &str, keys: &[String]) {
        self.assign(name, keys, None).ok();
        self.excluded.push(name.to_owned());
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
        Self::sink_for(&c.id)
    }
    /// Clock bridge for a device channel: a loopback whose capture side runs
    /// on the device's clock and whose playback side feeds the mixes.
    pub fn bridge_node_name(c: &Channel) -> String {
        format!("pipemic.dev.{}", c.id)
    }
    /// SPA-JSON args for libpipewire-module-loopback bridging device channel `c`.
    pub fn bridge_args(c: &Channel) -> String {
        let n = Self::bridge_node_name(c);
        // Distinct node.group per side: otherwise both halves share one driver
        // and the device still follows the output's clock (robot voice, seen
        // 2026-11-06). Split, the loopback resamples between the two clocks.
        format!(
            r#"{{ node.description = "Pipemic {label} (bridge)" audio.position = [ MONO ] target.delay.sec = 0.02
  capture.props = {{ node.name = "{n}.in" node.group = "{n}.device" node.link-group = "{n}.device" node.autoconnect = false node.dont-reconnect = true }}
  playback.props = {{ node.name = "{n}" node.group = "{n}.mix" node.link-group = "{n}.mix" node.autoconnect = false node.dont-reconnect = true }} }}"#,
            label = c.label.replace('"', "")
        )
    }
    /// Effects node of a channel: everything the channel sends to the mixes
    /// and meters passes through it. Capture side `….in`, playback side bare.
    pub fn fx_node_name(c: &Channel) -> String {
        format!("pipemic.fx.{}", c.id)
    }
    pub fn fx_in_node_name(c: &Channel) -> String {
        format!("{}.in", Self::fx_node_name(c))
    }
    /// Audio positions of a channel: device channels are mono.
    pub fn sides(c: &Channel) -> &'static [&'static str] {
        if c.device.is_some() {
            &["MONO"]
        } else {
            &["FL", "FR"]
        }
    }
    /// Filter-chain node holding band `b` (0-based) of side `s`.
    fn eq_node(s: usize, b: usize) -> String {
        format!("eq{}_{}", ["L", "R"][s], b + 1)
    }

    /// Controls for a channel's fx node: b0..a2 of every band and side.
    pub fn fx_controls(c: &Channel) -> Vec<(String, f32)> {
        let mut out = Vec::new();
        for s in 0..Self::sides(c).len() {
            for b in 0..BANDS {
                let k = c.eq.as_ref().map_or(IDENTITY, |e| e.slot(b));
                for (name, v) in ["b0", "b1", "b2", "a0", "a1", "a2"].iter().zip(k) {
                    out.push((format!("{}:{name}", Self::eq_node(s, b)), v));
                }
            }
        }
        out
    }

    /// SPA-JSON args for the fx filter-chain of channel `c`: per side, a
    /// series of `bq_raw` biquads (identity until an EQ sets them). Runs at
    /// a fixed 48 kHz so the coefficients don't depend on the graph rate.
    pub fn fx_args(c: &Channel) -> String {
        let sides = Self::sides(c);
        let ctl: std::collections::HashMap<String, f32> = Self::fx_controls(c).into_iter().collect();
        let mut nodes = String::new();
        let mut links = String::new();
        for s in 0..sides.len() {
            for b in 0..BANDS {
                let n = Self::eq_node(s, b);
                let k: String = ["b0", "b1", "b2", "a0", "a1", "a2"].iter().map(|p| format!("{p} = {} ", ctl[&format!("{n}:{p}")])).collect();
                // bq_raw starts as an all-zero filter (silence) and only reads
                // the controls when they change, so the starting coefficients
                // must also go in `config` (PipeWire 1.6 plugin_builtin.c).
                nodes.push_str(&format!(
                    "      {{ name = {n} type = builtin label = bq_raw config = {{ coefficients = [ {{ rate = 48000 {k}}} ] }} control = {{ {k}}} }}\n"
                ));
                if b > 0 {
                    links.push_str(&format!("      {{ output = \"{}:Out\" input = \"{n}:In\" }}\n", Self::eq_node(s, b - 1)));
                }
            }
        }
        let inputs: String = (0..sides.len()).map(|s| format!("\"{}:In\" ", Self::eq_node(s, 0))).collect();
        let outputs: String = (0..sides.len()).map(|s| format!("\"{}:Out\" ", Self::eq_node(s, BANDS - 1))).collect();
        let pos = sides.join(" ");
        format!(
            r#"{{
  node.description = "Pipemic {label} (effects)"
  media.name = "Pipemic {label} (effects)"
  audio.rate = 48000
  filter.graph = {{
    nodes = [
{nodes}    ]
    links = [
{links}    ]
    inputs = [ {inputs}]
    outputs = [ {outputs}]
  }}
  capture.props = {{
    node.name = "{in_name}"
    audio.position = [ {pos} ]
    audio.rate = 48000
    node.autoconnect = false
    node.dont-reconnect = true
    stream.dont-remix = true
  }}
  playback.props = {{
    node.name = "{out_name}"
    audio.position = [ {pos} ]
    audio.rate = 48000
    node.autoconnect = false
    node.dont-reconnect = true
    stream.dont-remix = true
  }}
}}"#,
            label = c.label.replace('"', ""),
            in_name = Self::fx_in_node_name(c),
            out_name = Self::fx_node_name(c),
        )
    }

    pub fn mix_in_node_name(m: &Mix) -> String {
        format!("pipemic.mix.{}.in", m.id)
    }
    pub fn mix_out_node_name(m: &Mix) -> String {
        format!("pipemic.mix.{}", m.id)
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
  node.description = "Pipemic {label}"
  media.name = "Pipemic {label}"
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
    node.description = "Pipemic {label} (input)"
    audio.position = [ {positions}]
    node.autoconnect = false
    stream.dont-remix = true
  }}
  playback.props = {{
    node.name = "{out_name}"
    node.description = "Pipemic {label}"
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
        assert_eq!(c.gain(s, music), 0.0, "apps start muted in Outgoing");
        assert_eq!(c.gain(p, music), 1.0);
        assert_eq!(c.channels.last().unwrap().id, "other");
    }

    #[test]
    fn db_math() {
        let mut c = Config::default();
        c.channels[1].volume = -6.0;
        let r = c.route_mut("stream", "music");
        r.volume = -14.0;
        r.mute = false;
        let g = c.gain(1, 1);
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
        assert_eq!(back.routes.len(), 5);
        assert!(!back.channels.last().unwrap().fallback);
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
    fn fx_shape() {
        let mut c = Config::default();
        let mic = c.fx_args_for("mic");
        assert!(mic.contains("audio.position = [ MONO ]"), "{mic}");
        assert!(mic.contains("{ output = \"eqL_7:Out\" input = \"eqL_8:In\" }"));
        assert!(mic.contains("inputs = [ \"eqL_1:In\" ]"));
        assert!(!mic.contains("eqR_1"));
        let music = c.fx_args_for("music");
        assert!(music.contains("outputs = [ \"eqL_8:Out\" \"eqR_8:Out\" ]"));
        assert!(
            music.contains("label = bq_raw config = { coefficients = [ { rate = 48000 b0 = 1 b1 = 0 b2 = 0 a0 = 1 a1 = 0 a2 = 0 } ] } control = { b0 = 1 b1 = 0 b2 = 0 a0 = 1 a1 = 0 a2 = 0 }"),
            "{music}"
        );
        assert_eq!(Config::fx_controls(&c.channels[1]).len(), 2 * BANDS * 6);
        let eq = crate::eq::Equalizer {
            name: "t".into(),
            enabled: true,
            bands: vec![crate::eq::Band { kind: crate::eq::Kind::Peak, freq: 1000.0, q: 1.0, gain: 6.0, on: true }],
        };
        c.channels[0].eq = Some(eq);
        let ctl = Config::fx_controls(&c.channels[0]);
        assert_eq!(ctl.len(), BANDS * 6);
        assert_eq!(ctl[0].0, "eqL_1:b0");
        assert!((ctl[0].1 - 1.04395).abs() < 1e-4);
        assert_eq!(ctl[6].1, 1.0, "band 2 is identity");
        c.channels[0].eq.as_mut().unwrap().enabled = false;
        assert_eq!(Config::fx_controls(&c.channels[0])[0].1, 1.0);
        c.validate().unwrap();
        let t = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&t).unwrap();
        assert_eq!(back.channels[0].eq, c.channels[0].eq);
    }

    impl Config {
        fn fx_args_for(&self, id: &str) -> String {
            Config::fx_args(&self.channels[self.channel_index(id).unwrap()])
        }
    }

    #[test]
    fn globbing() {
        assert!(glob("spotify", "Spotify"));
        assert!(glob("google-chrome*", "google-chrome-stable"));
        assert!(glob("*chrome", "Google Chrome"));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("chrome", "Chromium"));
        assert!(!glob("google-chrome*", "chrome"));
    }

    #[test]
    fn routing() {
        let mut c = Config::default();
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = c.router();
        assert_eq!(r.route(&k(&["Spotify"]), &k(&["Audio", "Music"])), Some(("music", false)));
        assert_eq!(r.route(&k(&["Google Chrome"]), &k(&["Network", "WebBrowser"])), Some(("browser", false)));
        assert_eq!(r.route(&k(&["Slack"]), &k(&["Network", "InstantMessaging"])), Some(("voicechat", false)));
        assert_eq!(r.route(&k(&["kded6"]), &[]), None, "unplaced apps stay out of the mixer");
        let mut c2 = c.clone();
        c2.assign("Betterbird", &k(&["Betterbird"]), Some("other")).unwrap();
        assert_eq!(c2.router().route(&k(&["Betterbird"]), &k(&["Email"])), Some(("other", true)));
        let chrome = k(&["Google Chrome", "chrome"]);
        c.assign("Google Chrome", &chrome, Some("music")).unwrap();
        assert_eq!(c.router().route(&chrome, &k(&["WebBrowser"])), Some(("music", true)));
        c.assign("Google Chrome", &chrome, None).unwrap();
        assert_eq!(c.router().route(&chrome, &k(&["WebBrowser"])), Some(("browser", false)));
        assert_eq!(c.router().auto(&k(&["WebBrowser"])), Some("browser"));
        assert!(c.assign("x", &[], Some("mic")).is_err());
        c.validate().unwrap();
    }

    #[test]
    fn validation() {
        let mut c = Config::default();
        c.routes.push(Route { mix: "nope".into(), channel: "mic".into(), volume: 0.0, mute: false });
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.channels[1].volume = 3.0;
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.rules.push(Rule { channel: "mic".into(), apps: vec!["x".into()] });
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.channels[1].fallback = true;
        c.channels[2].fallback = true;
        assert!(c.validate().is_err());
    }
}
