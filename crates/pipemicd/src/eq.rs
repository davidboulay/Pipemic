//! Per-channel equalizer: up to 8 biquad bands, run by a builtin `bq_raw`
//! filter-chain node per band and side (see `Config::fx_args`).
//!
//! Coefficients are computed here (RBJ "Audio EQ Cookbook") and pushed as
//! `b0..a2` controls, so band types and values change live without
//! rebuilding the graph. Measured on 2026-11-06: the Elgato Equalizer
//! add-on uses exactly these formulas (docs/addons-research.md §1), so its
//! presets import 1:1.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Bands per channel (the Elgato Equalizer has 8).
pub const BANDS: usize = 8;
/// The fx nodes run at this rate (PipeWire resamples if the graph differs),
/// so the coefficients don't depend on the graph rate.
pub const RATE: f64 = 48000.0;
/// Coefficients of a band that does nothing: b0..b2, a0..a2.
pub const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Highpass,
    Lowshelf,
    Peak,
    Highshelf,
    Lowpass,
}

impl Kind {
    /// Elgato Equalizer `Band N Filter Type` index (UI order, measured).
    pub fn from_elgato(i: u32) -> Option<Self> {
        [Self::Highpass, Self::Lowshelf, Self::Peak, Self::Highshelf, Self::Lowpass].get(i as usize).copied()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Band {
    pub kind: Kind,
    /// Hz, 20..=20000.
    pub freq: f32,
    /// 0.1..=10 (also shapes high/low-pass: 0.71 = Butterworth).
    pub q: f32,
    /// dB, -24..=24; ignored by high/low-pass.
    #[serde(default)]
    pub gain: f32,
    #[serde(default = "yes")]
    pub on: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Equalizer {
    /// Preset name shown in the UI (e.g. the add-on's name).
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, rename = "band")]
    pub bands: Vec<Band>,
}

impl Band {
    pub fn validate(&self) -> Result<()> {
        if !(20.0..=20000.0).contains(&self.freq) {
            bail!("band frequency {} Hz out of range (20..20000)", self.freq);
        }
        if !(0.1..=10.0).contains(&self.q) {
            bail!("band Q {} out of range (0.1..10)", self.q);
        }
        if !(-24.0..=24.0).contains(&self.gain) || !self.gain.is_finite() {
            bail!("band gain {} dB out of range (-24..24)", self.gain);
        }
        Ok(())
    }

    /// RBJ biquad coefficients b0, b1, b2, a0, a1, a2, normalised to a0 = 1.
    pub fn coeffs(&self, rate: f64) -> [f32; 6] {
        if !self.on {
            return IDENTITY;
        }
        let a = 10f64.powf(self.gain as f64 / 40.0);
        let w = 2.0 * std::f64::consts::PI * self.freq as f64 / rate;
        let (c, s) = (w.cos(), w.sin());
        let al = s / (2.0 * self.q as f64);
        let sq = 2.0 * a.sqrt() * al;
        let (b, aa) = match self.kind {
            Kind::Highpass => ([(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0], [1.0 + al, -2.0 * c, 1.0 - al]),
            Kind::Lowpass => ([(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0], [1.0 + al, -2.0 * c, 1.0 - al]),
            Kind::Peak => ([1.0 + al * a, -2.0 * c, 1.0 - al * a], [1.0 + al / a, -2.0 * c, 1.0 - al / a]),
            Kind::Lowshelf => (
                [a * ((a + 1.0) - (a - 1.0) * c + sq), 2.0 * a * ((a - 1.0) - (a + 1.0) * c), a * ((a + 1.0) - (a - 1.0) * c - sq)],
                [(a + 1.0) + (a - 1.0) * c + sq, -2.0 * ((a - 1.0) + (a + 1.0) * c), (a + 1.0) + (a - 1.0) * c - sq],
            ),
            Kind::Highshelf => (
                [a * ((a + 1.0) + (a - 1.0) * c + sq), -2.0 * a * ((a - 1.0) + (a + 1.0) * c), a * ((a + 1.0) + (a - 1.0) * c - sq)],
                [(a + 1.0) - (a - 1.0) * c + sq, 2.0 * ((a - 1.0) - (a + 1.0) * c), (a + 1.0) - (a - 1.0) * c - sq],
            ),
        };
        let n = aa[0];
        [(b[0] / n) as f32, (b[1] / n) as f32, (b[2] / n) as f32, 1.0, (aa[1] / n) as f32, (aa[2] / n) as f32]
    }
}

impl Equalizer {
    pub fn validate(&self) -> Result<()> {
        if self.bands.len() > BANDS {
            bail!("an equalizer has at most {BANDS} bands, got {}", self.bands.len());
        }
        for b in &self.bands {
            b.validate()?;
        }
        Ok(())
    }

    /// Coefficients for band slot `i` (identity when off or absent).
    pub fn slot(&self, i: usize) -> [f32; 6] {
        match self.bands.get(i) {
            Some(b) if self.enabled => b.coeffs(RATE),
            _ => IDENTITY,
        }
    }

    /// Parse an Elgato Equalizer preset (a Wave Link marketplace "presets"
    /// add-on): `<Preset><Info Name=…/><Parameters><PARAM id="Band N Filter X" value=…/>`.
    /// Bypassed bands are kept (off) so the band layout matches the original;
    /// `Visible` is UI-only in the original and ignored.
    pub fn from_elgato_xml(xml: &str) -> Result<Self> {
        if !xml.contains("<Preset") {
            bail!("not an Elgato EQ preset (no <Preset> element)");
        }
        let name = tags(xml, "Info").into_iter().find_map(|t| attr(t, "Name")).unwrap_or_else(|| "Imported EQ".into());
        let mut raw: [[Option<f64>; 5]; BANDS] = Default::default();
        for t in tags(xml, "PARAM") {
            let (Some(id), Some(v)) = (attr(t, "id"), attr(t, "value")) else { continue };
            let Some(rest) = id.strip_prefix("Band ") else { continue };
            let Some((n, field)) = rest.split_once(" Filter ") else { continue };
            let Ok(n) = n.trim().parse::<usize>() else { continue };
            let Some(slot) = n.checked_sub(1).filter(|&s| s < BANDS) else { continue };
            let f = match field.trim() {
                "Type" => 0,
                "Frequency" => 1,
                "Quality" => 2,
                "Gain" => 3,
                "Bypass" => 4,
                _ => continue,
            };
            raw[slot][f] = Some(v.trim().parse().with_context(|| format!("{id}: bad value {v:?}"))?);
        }
        let mut bands = Vec::new();
        for (i, r) in raw.iter().enumerate() {
            let [Some(t), Some(freq), Some(q), Some(g)] = [r[0], r[1], r[2], r[3]] else {
                if r.iter().any(Option::is_some) {
                    bail!("band {} is incomplete", i + 1);
                }
                continue;
            };
            let kind = Kind::from_elgato(t.round() as u32).with_context(|| format!("band {}: unknown filter type {t}", i + 1))?;
            if g <= 0.0 {
                bail!("band {}: gain must be a positive factor, got {g}", i + 1);
            }
            // Round to the original's steps (0.1 Hz, 0.01 Q, 0.01 dB).
            let band = Band {
                kind,
                freq: ((freq * 10.0).round() / 10.0) as f32,
                q: ((q * 100.0).round() / 100.0) as f32,
                gain: ((20.0 * g.log10() * 100.0).round() / 100.0) as f32,
                on: r[4].unwrap_or(0.0) < 0.5,
            };
            band.validate().with_context(|| format!("band {}", i + 1))?;
            bands.push(band);
        }
        if bands.is_empty() {
            bail!("the preset has no EQ bands");
        }
        Ok(Self { name, enabled: true, bands })
    }
}

/// Start tags `<name …>` / `<name …/>` (attribute text only).
fn tags<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        rest = after;
        if !after.starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>') {
            continue;
        }
        if let Some(end) = after.find('>') {
            out.push(&after[..end]);
        }
    }
    out
}

/// Value of attribute `key` in a tag's attribute text, entities decoded.
fn attr(tag: &str, key: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(i) = rest.find(key) {
        let before_ok = i == 0 || rest[..i].ends_with(char::is_whitespace);
        let after = rest[i + key.len()..].trim_start();
        rest = &rest[i + key.len()..];
        let Some(after) = after.strip_prefix('=') else { continue };
        if !before_ok {
            continue;
        }
        let after = after.trim_start();
        let q = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let body = &after[1..];
        let end = body.find(q)?;
        return Some(unescape(&body[..end]));
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same shape as a marketplace preset; values of "Old Radio" typed in
    /// (Elgato's file is not redistributed).
    const OLD_RADIO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Preset>
  <Info Name="Old Radio" Position="6"/>
  <Parameters>
    <PARAM id="Band 1 Filter Bypass" value="0.0"/>
    <PARAM id="Band 1 Filter Frequency" value="1505.0"/>
    <PARAM id="Band 1 Filter Gain" value="1.0"/>
    <PARAM id="Band 1 Filter Quality" value="0.7100000381469727"/>
    <PARAM id="Band 1 Filter Type" value="4.0"/>
    <PARAM id="Band 1 Filter Visible" value="1.0"/>
    <PARAM id="Band 2 Filter Bypass" value="0.0"/>
    <PARAM id="Band 2 Filter Frequency" value="2732.0"/>
    <PARAM id="Band 2 Filter Gain" value="1.0"/>
    <PARAM id="Band 2 Filter Quality" value="0.7100000381469727"/>
    <PARAM id="Band 2 Filter Type" value="0.0"/>
    <PARAM id="Band 2 Filter Visible" value="1.0"/>
    <PARAM id="Band 3 Filter Bypass" value="0.0"/>
    <PARAM id="Band 3 Filter Frequency" value="2037.0"/>
    <PARAM id="Band 3 Filter Gain" value="4.0"/>
    <PARAM id="Band 3 Filter Quality" value="3.099999904632568"/>
    <PARAM id="Band 3 Filter Type" value="2.0"/>
    <PARAM id="Band 3 Filter Visible" value="1.0"/>
    <PARAM id="Band 4 Filter Bypass" value="1.0"/>
    <PARAM id="Band 4 Filter Frequency" value="770.0"/>
    <PARAM id="Band 4 Filter Gain" value="0.5"/>
    <PARAM id="Band 4 Filter Quality" value="1.0"/>
    <PARAM id="Band 4 Filter Type" value="2.0"/>
    <PARAM id="Band 4 Filter Visible" value="0.0"/>
  </Parameters>
</Preset>"#;

    #[test]
    fn imports_old_radio() {
        let eq = Equalizer::from_elgato_xml(OLD_RADIO).unwrap();
        assert_eq!(eq.name, "Old Radio");
        assert_eq!(eq.bands.len(), 4);
        assert_eq!(eq.bands[0], Band { kind: Kind::Lowpass, freq: 1505.0, q: 0.71, gain: 0.0, on: true });
        assert_eq!(eq.bands[1].kind, Kind::Highpass);
        assert_eq!(eq.bands[2], Band { kind: Kind::Peak, freq: 2037.0, q: 3.1, gain: 12.04, on: true });
        assert!(!eq.bands[3].on, "bypassed band stays, switched off");
        assert_eq!(eq.bands[3].gain, -6.02);
        assert_eq!(eq.slot(3), IDENTITY);
        assert_eq!(eq.slot(7), IDENTITY);
    }

    #[test]
    fn names_and_errors() {
        let x = r#"<Preset><Info Position="1" Name="Warm &amp; Clear"/><Parameters>
            <PARAM id="Band 1 Filter Type" value="2"/><PARAM id="Band 1 Filter Frequency" value="1000"/>
            <PARAM id="Band 1 Filter Quality" value="1"/><PARAM id="Band 1 Filter Gain" value="2"/></Parameters></Preset>"#;
        assert_eq!(Equalizer::from_elgato_xml(x).unwrap().name, "Warm & Clear");
        assert!(Equalizer::from_elgato_xml("<Foo/>").is_err());
        assert!(Equalizer::from_elgato_xml("<Preset><Parameters></Parameters></Preset>").is_err());
        let bad = x.replace(r#"Type" value="2""#, r#"Type" value="9""#);
        assert!(Equalizer::from_elgato_xml(&bad).is_err());
    }

    /// Reference values from a float64 RBJ implementation that nulled against
    /// the Elgato plugin at -60..-115 dB (2026-11-06).
    #[test]
    fn rbj_coefficients() {
        let close = |a: [f32; 6], b: [f64; 6]| a.iter().zip(b).all(|(x, y)| (*x as f64 - y).abs() < 1e-6);
        let peak = Band { kind: Kind::Peak, freq: 1000.0, q: 1.0, gain: 6.0, on: true }.coeffs(48000.0);
        assert!(close(peak, PEAK_1K_Q1_6DB), "{peak:?}");
        let hp = Band { kind: Kind::Highpass, freq: 100.0, q: 0.71, gain: 0.0, on: true }.coeffs(48000.0);
        assert!(close(hp, HP_100_Q071), "{hp:?}");
        let ls = Band { kind: Kind::Lowshelf, freq: 200.0, q: 0.71, gain: -6.0, on: true }.coeffs(48000.0);
        assert!(close(ls, LS_200_Q071_M6), "{ls:?}");
        // 0 dB peak / shelf is a no-op: numerator == denominator.
        for kind in [Kind::Peak, Kind::Lowshelf, Kind::Highshelf] {
            let f = Band { kind, freq: 5000.0, q: 1.0, gain: 0.0, on: true }.coeffs(48000.0);
            assert!((0..3).all(|i| (f[i] - f[i + 3]).abs() < 1e-6), "{kind:?} {f:?}");
        }
    }

    const PEAK_1K_Q1_6DB: [f64; 6] = [1.043953086990335, -1.8953207239365961, 0.8677222847598566, 1.0, -1.8953207239365961, 0.9116753717501915];
    const HP_100_Q071: [f64; 6] = [0.9908237258616364, -1.9816474517232727, 0.9908237258616364, 1.0, -1.9815625618145845, 0.9817323416319608];
    const LS_200_Q071_M6: [f64; 6] = [0.9936207579497408, -1.9564125995865367, 0.963266597144718, 1.0, -1.9561763464536073, 0.9571236082273885];
}
