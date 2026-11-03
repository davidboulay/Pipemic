//! Signed 8.8 fixed point, as used by the Wave:3 for dB and percent values.
//!
//! `0x1580` = 21.5, `0xEE80` = -17.5, `0x3700` = 55.0. Stored little-endian.

/// Raw i16 (8.8) to f32.
pub fn decode(raw: i16) -> f32 {
    raw as f32 / 256.0
}

/// f32 to raw i16 (8.8), rounded to the nearest 1/256. Returns `None` if the
/// value doesn't fit (outside -128.0 ..= 127.996) or isn't finite.
pub fn encode(value: f32) -> Option<i16> {
    if !value.is_finite() {
        return None;
    }
    let scaled = (value * 256.0).round();
    if scaled < i16::MIN as f32 || scaled > i16::MAX as f32 {
        return None;
    }
    Some(scaled as i16)
}

pub(crate) fn read_le(bytes: &[u8], off: usize) -> f32 {
    decode(i16::from_le_bytes([bytes[off], bytes[off + 1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(decode(0x1580), 21.5);
        assert_eq!(decode(0xEE80u16 as i16), -17.5);
        assert_eq!(decode(0xEC00u16 as i16), -20.0);
        assert_eq!(decode(0x3700), 55.0);
        assert_eq!(encode(21.5), Some(0x1580));
        assert_eq!(encode(-17.5), Some(0xEE80u16 as i16));
        assert_eq!(encode(-60.0), Some(-60 * 256));
    }

    #[test]
    fn out_of_range() {
        assert_eq!(encode(128.0), None);
        assert_eq!(encode(f32::NAN), None);
        assert_eq!(encode(-128.0), Some(i16::MIN));
    }
}
