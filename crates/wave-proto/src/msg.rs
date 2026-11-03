//! Typed Wave:3 messages (API 5.3). See `docs/wave3-protocol.md`.

use crate::{fixed, Error};
use std::fmt;

/// Message IDs (wValue of GET_MEM / SET_MEM).
pub mod id {
    pub const CONFIG: u16 = 0;
    pub const STATUS: u16 = 1;
    pub const VERSION: u16 = 10;
}

pub const CONFIG_LEN: usize = 16;
pub const STATUS_LEN: usize = 8;
pub const VERSION_LEN: usize = 52;

/// What the hardware knob controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeSelect {
    Mic,
    Headphone,
    Mix,
    /// A value outside the schema; kept so it round-trips unchanged.
    Other(u8),
}

impl VolumeSelect {
    pub fn from_raw(v: u8) -> Self {
        match v {
            1 => Self::Mic,
            2 => Self::Headphone,
            3 => Self::Mix,
            o => Self::Other(o),
        }
    }
    pub fn raw(self) -> u8 {
        match self {
            Self::Mic => 1,
            Self::Headphone => 2,
            Self::Mix => 3,
            Self::Other(o) => o,
        }
    }
}

impl fmt::Display for VolumeSelect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mic => f.write_str("mic"),
            Self::Headphone => f.write_str("headphone"),
            Self::Mix => f.write_str("mix"),
            Self::Other(o) => write!(f, "unknown({o})"),
        }
    }
}

/// msg 0: `config` (16 bytes, read/write).
///
/// Offsets 0–12 were confirmed on the device; 13–15 (LEDs, gain lock) are
/// placed by schema order only. Bytes 2–3 are not in the schema and are
/// carried through untouched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// dB, 0.0 ..= 40.0 step 0.5
    pub input_gain: f32,
    /// Bytes 2–3, meaning unknown (reads as -20.0 in 8.8). Never modify.
    pub unknown_2_3: [u8; 2],
    pub input_mute: bool,
    pub clipguard: bool,
    pub lowcut: bool,
    /// dB, -60.0 ..= 0.0 step 0.5
    pub headphone_volume: f32,
    pub headphone_mute: bool,
    /// Percent, 0 ..= 100 step 5
    pub direct_monitor: f32,
    pub volume_select: VolumeSelect,
    pub all_leds_off: bool,
    pub leds_flip: bool,
    pub gain_lock: bool,
    /// Raw bool bytes as read, so a non-0/1 value survives a round trip.
    raw_bools: [u8; 7],
}

// Offsets of the bool bytes, in the order stored in `raw_bools`.
const BOOL_OFFS: [usize; 7] = [4, 5, 6, 9, 13, 14, 15];

impl Config {
    pub fn decode(b: &[u8]) -> Result<Self, Error> {
        if b.len() != CONFIG_LEN {
            return Err(Error::BadLength { msg: id::CONFIG, expected: CONFIG_LEN, got: b.len() });
        }
        let mut raw_bools = [0u8; 7];
        for (i, &off) in BOOL_OFFS.iter().enumerate() {
            raw_bools[i] = b[off];
        }
        Ok(Self {
            input_gain: fixed::read_le(b, 0),
            unknown_2_3: [b[2], b[3]],
            input_mute: b[4] != 0,
            clipguard: b[5] != 0,
            lowcut: b[6] != 0,
            headphone_volume: fixed::read_le(b, 7),
            headphone_mute: b[9] != 0,
            direct_monitor: fixed::read_le(b, 10),
            volume_select: VolumeSelect::from_raw(b[12]),
            all_leds_off: b[13] != 0,
            leds_flip: b[14] != 0,
            gain_lock: b[15] != 0,
            raw_bools,
        })
    }

    pub fn encode(&self) -> Result<[u8; CONFIG_LEN], Error> {
        let mut b = [0u8; CONFIG_LEN];
        let put = |b: &mut [u8], off: usize, v: f32, name: &'static str| -> Result<(), Error> {
            let raw = fixed::encode(v).ok_or(Error::OutOfRange { name, value: v })?;
            b[off..off + 2].copy_from_slice(&raw.to_le_bytes());
            Ok(())
        };
        put(&mut b, 0, self.input_gain, "input_gain")?;
        b[2..4].copy_from_slice(&self.unknown_2_3);
        put(&mut b, 7, self.headphone_volume, "headphone_volume")?;
        put(&mut b, 10, self.direct_monitor, "direct_monitor")?;
        b[12] = self.volume_select.raw();
        let bools = [
            self.input_mute,
            self.clipguard,
            self.lowcut,
            self.headphone_mute,
            self.all_leds_off,
            self.leds_flip,
            self.gain_lock,
        ];
        for (i, (&off, &v)) in BOOL_OFFS.iter().zip(bools.iter()).enumerate() {
            // Keep the original byte if the logical value didn't change.
            let orig = self.raw_bools[i];
            b[off] = if (orig != 0) == v { orig } else { v as u8 };
        }
        Ok(b)
    }

    /// Apply one setting, validating range and step against the API 5.3 schema.
    pub fn apply(&mut self, s: Setting) -> Result<(), Error> {
        match s {
            Setting::InputGain(v) => self.input_gain = check("gain", v, 0.0, 40.0, 0.5)?,
            Setting::InputMute(v) => self.input_mute = v,
            Setting::Clipguard(v) => self.clipguard = v,
            Setting::Lowcut(v) => self.lowcut = v,
            Setting::HeadphoneVolume(v) => {
                self.headphone_volume = check("headphone_volume", v, -60.0, 0.0, 0.5)?
            }
            Setting::HeadphoneMute(v) => self.headphone_mute = v,
            Setting::DirectMonitor(v) => {
                self.direct_monitor = check("direct_monitor", v, 0.0, 100.0, 5.0)?
            }
            Setting::VolumeSelect(v) => {
                if let VolumeSelect::Other(_) = v {
                    return Err(Error::InvalidValue("volume_select must be mic, headphone or mix".into()));
                }
                self.volume_select = v
            }
            Setting::AllLedsOff(v) => self.all_leds_off = v,
            Setting::LedsFlip(v) => self.leds_flip = v,
            Setting::GainLock(v) => self.gain_lock = v,
        }
        Ok(())
    }
}

fn check(name: &'static str, v: f32, min: f32, max: f32, step: f32) -> Result<f32, Error> {
    if !(min..=max).contains(&v) {
        return Err(Error::OutOfRange { name, value: v });
    }
    let n = (v - min) / step;
    if (n - n.round()).abs() > 1e-3 {
        return Err(Error::InvalidValue(format!("{name} must be a multiple of {step} (got {v})")));
    }
    Ok(min + n.round() * step)
}

/// A single writable config field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Setting {
    InputGain(f32),
    InputMute(bool),
    Clipguard(bool),
    Lowcut(bool),
    HeadphoneVolume(f32),
    HeadphoneMute(bool),
    DirectMonitor(f32),
    VolumeSelect(VolumeSelect),
    AllLedsOff(bool),
    LedsFlip(bool),
    GainLock(bool),
}

/// msg 1: `status` (8 bytes, read-only).
///
/// The schema names the two u32s `touch_pressed_ms` and `touch_signal`, but on
/// firmware 1.2.2 both always carry the same value: the raw capacitive reading
/// of the top touch pad (about 100–450 idle, 8 000–9 300 while touched,
/// constant during a 3 s hold). Compare it against the thresholds in
/// [`Version`] to tell whether the pad is touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Schema name `touch_pressed_ms`; mirrors `touch_signal` on fw 1.2.2.
    pub touch_pressed_ms: u32,
    /// Raw touch-pad reading.
    pub touch_signal: u32,
}

impl Status {
    pub fn decode(b: &[u8]) -> Result<Self, Error> {
        if b.len() != STATUS_LEN {
            return Err(Error::BadLength { msg: id::STATUS, expected: STATUS_LEN, got: b.len() });
        }
        Ok(Self {
            touch_pressed_ms: u32_le(b, 0),
            touch_signal: u32_le(b, 4),
        })
    }

    /// True if the pad reads above `upper` (from `Version::touch_threshold_upper`).
    pub fn touched(&self, upper: u16) -> bool {
        self.touch_signal > upper as u32
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
    pub build: u32,
    pub commit: String,
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{} build {}", self.major, self.minor, self.patch, self.build)?;
        if !self.commit.is_empty() {
            write!(f, " ({})", self.commit)?;
        }
        Ok(())
    }
}

/// msg 10: `version` (52 bytes, read-only).
#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub api_major: u8,
    pub api_minor: u8,
    pub vbus_voltage: f32,
    pub mic_type: u8,
    pub hw_version_board: u8,
    pub bootloader: SemVer,
    pub firmware: SemVer,
    pub serial: String,
    pub touch_threshold_lower: u16,
    pub touch_threshold_upper: u16,
}

impl Version {
    pub fn decode(b: &[u8]) -> Result<Self, Error> {
        if b.len() != VERSION_LEN {
            return Err(Error::BadLength { msg: id::VERSION, expected: VERSION_LEN, got: b.len() });
        }
        let semver = |o: usize| SemVer {
            major: b[o],
            minor: b[o + 1],
            patch: b[o + 2],
            build: u32_le(b, o + 3),
            commit: ascii(&b[o + 7..o + 15]),
        };
        Ok(Self {
            api_major: b[0],
            api_minor: b[1],
            vbus_voltage: i16::from_le_bytes([b[2], b[3]]) as f32 * 0.004,
            mic_type: b[4],
            hw_version_board: b[5],
            bootloader: semver(6),
            firmware: semver(21),
            serial: ascii(&b[36..48]),
            touch_threshold_lower: u16::from_le_bytes([b[48], b[49]]),
            touch_threshold_upper: u16::from_le_bytes([b[50], b[51]]),
        })
    }
}

fn u32_le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn ascii(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    b[..end].iter().map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { '?' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // First line of docs/wave3-config-watch.log (captured on the Mac).
    const LOG0: [u8; 16] = [
        0x80, 0x15, 0x00, 0xec, 0x00, 0x01, 0x00, 0x80, 0xee, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn decode_logged_config() {
        let c = Config::decode(&LOG0).unwrap();
        assert_eq!(c.input_gain, 21.5);
        assert_eq!(c.unknown_2_3, [0x00, 0xec]);
        assert!(!c.input_mute);
        assert!(c.clipguard);
        assert!(!c.lowcut);
        assert_eq!(c.headphone_volume, -17.5);
        assert!(!c.headphone_mute);
        assert_eq!(c.direct_monitor, 0.0);
        assert_eq!(c.volume_select, VolumeSelect::Headphone);
    }

    #[test]
    fn roundtrip_is_byte_exact() {
        assert_eq!(Config::decode(&LOG0).unwrap().encode().unwrap(), LOG0);
        // Odd bool byte (2) and odd enum survive untouched.
        let mut odd = LOG0;
        odd[5] = 2;
        odd[12] = 9;
        assert_eq!(Config::decode(&odd).unwrap().encode().unwrap(), odd);
    }

    #[test]
    fn apply_gain_changes_only_bytes_0_1() {
        let mut c = Config::decode(&LOG0).unwrap();
        c.apply(Setting::InputGain(22.0)).unwrap();
        let out = c.encode().unwrap();
        assert_eq!(&out[0..2], &[0x00, 0x16]);
        assert_eq!(&out[2..], &LOG0[2..]);
    }

    #[test]
    fn direct_monitor_55() {
        let mut b = LOG0;
        b[11] = 0x37;
        assert_eq!(Config::decode(&b).unwrap().direct_monitor, 55.0);
    }

    #[test]
    fn validation() {
        let mut c = Config::decode(&LOG0).unwrap();
        assert!(c.apply(Setting::InputGain(40.5)).is_err());
        assert!(c.apply(Setting::InputGain(21.3)).is_err());
        assert!(c.apply(Setting::HeadphoneVolume(1.0)).is_err());
        assert!(c.apply(Setting::DirectMonitor(52.0)).is_err());
        assert!(c.apply(Setting::VolumeSelect(VolumeSelect::Other(7))).is_err());
        assert!(c.apply(Setting::DirectMonitor(55.0)).is_ok());
    }

    #[test]
    fn decode_status_captured() {
        // Captured on fw 1.2.2: idle and touched.
        let idle = Status::decode(&[0x4d, 0x01, 0, 0, 0x4d, 0x01, 0, 0]).unwrap();
        assert_eq!(idle.touch_signal, 333);
        assert!(!idle.touched(1500));
        let touched = Status::decode(&9069u32.to_le_bytes().repeat(2)).unwrap();
        assert!(touched.touched(1500));
    }

    #[test]
    fn decode_version() {
        let mut b = [0u8; VERSION_LEN];
        b[0] = 5;
        b[1] = 3;
        b[2..4].copy_from_slice(&821i16.to_le_bytes()); // 3.284 V
        b[5] = 8;
        b[6..9].copy_from_slice(&[0, 3, 7]);
        b[9] = 3;
        b[21..24].copy_from_slice(&[1, 2, 2]);
        b[24] = 3;
        b[28..36].copy_from_slice(b"0b4251ee");
        b[36..48].copy_from_slice(b"SERIAL000001");
        b[48..50].copy_from_slice(&900u16.to_le_bytes());
        b[50..52].copy_from_slice(&1500u16.to_le_bytes());
        let v = Version::decode(&b).unwrap();
        assert_eq!((v.api_major, v.api_minor), (5, 3));
        assert_eq!(v.firmware.to_string(), "1.2.2 build 3 (0b4251ee)");
        assert_eq!(v.bootloader.to_string(), "0.3.7 build 3");
        assert_eq!(v.serial, "SERIAL000001");
        assert_eq!((v.touch_threshold_lower, v.touch_threshold_upper), (900, 1500));
        assert!((v.vbus_voltage - 3.284).abs() < 1e-4);
    }
}
