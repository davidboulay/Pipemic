//! Wave:3 firmware images and DFU mode switching (XMOS DFU extensions).
//! Notes: `docs/wave3-dfu.md`.
//!
//! Deliberately **no flash write** here yet: only image validation and the
//! enter/leave round-trip, which was verified on the device without writing.

use crate::device::{PID_WAVE3, VID};
use crate::Error;
use rusb::{DeviceHandle, GlobalContext};
use std::time::{Duration, Instant};

/// Wave:3 in bootloader / DFU mode ("USB Bootloader", bcdDevice 0.37).
pub const PID_BOOTLOADER: u16 = 0x0071;
/// DFU interface in run mode (FE/01/01).
pub const DFU_IFACE_RUN: u8 = 4;
/// DFU interface in bootloader mode (FE/01/02).
pub const DFU_IFACE_BOOT: u8 = 0;

const RT_OUT: u8 = 0x21;
const RT_IN: u8 = 0xA1;
const DFU_GETSTATUS: u8 = 0x03;
const DFU_CLRSTATUS: u8 = 0x04;
const XMOS_RESETINTODFU: u8 = 0xF2;
const XMOS_RESETFROMDFU: u8 = 0xF3;
const TIMEOUT: Duration = Duration::from_millis(500);
/// How long to wait for the device to re-enumerate after a mode switch.
/// Observed: under 1 s; the macOS app waits 10 s.
pub const REENUM_TIMEOUT: Duration = Duration::from_secs(10);

const SUFFIX_LEN: usize = 16;

// ---------------------------------------------------------------- image

/// A validated `.dfu` file: DFU 1.1 suffix present, CRC correct.
#[derive(Debug, Clone, PartialEq)]
pub struct Image<'a> {
    /// What gets downloaded: the file minus the 16-byte suffix.
    pub payload: &'a [u8],
    pub vid: u16,
    pub pid: u16,
    /// Firmware version as BCD, e.g. 0x0122 = 1.2.2.
    pub bcd_device: u16,
    pub bcd_dfu: u16,
    pub crc: u32,
}

impl<'a> Image<'a> {
    /// Parse and check the DFU suffix and CRC. Does not check the target.
    pub fn parse(file: &'a [u8]) -> Result<Self, Error> {
        if file.len() <= SUFFIX_LEN {
            return Err(Error::BadImage(format!("{} bytes is too short", file.len())));
        }
        let s = &file[file.len() - SUFFIX_LEN..];
        let u16le = |o: usize| u16::from_le_bytes([s[o], s[o + 1]]);
        // Suffix layout (DFU 1.1 §B.1, little-endian, stored at the end):
        // bcdDevice idProduct idVendor bcdDFU "UFD" bLength dwCRC
        if &s[8..11] != b"UFD" {
            return Err(Error::BadImage("no DFU suffix (missing \"UFD\" signature)".into()));
        }
        if s[11] as usize != SUFFIX_LEN {
            return Err(Error::BadImage(format!("unexpected suffix length {}", s[11])));
        }
        let crc = u32::from_le_bytes([s[12], s[13], s[14], s[15]]);
        let computed = dfu_crc(&file[..file.len() - 4]);
        if crc != computed {
            return Err(Error::CrcMismatch { stored: crc, computed });
        }
        Ok(Self {
            payload: &file[..file.len() - SUFFIX_LEN],
            bcd_device: u16le(0),
            pid: u16le(2),
            vid: u16le(4),
            bcd_dfu: u16le(6),
            crc,
        })
    }

    /// Refuse images that aren't built for the Wave:3 (0fd9:0070).
    pub fn check_wave3(&self) -> Result<(), Error> {
        if self.vid == VID && self.pid == PID_WAVE3 {
            Ok(())
        } else {
            Err(Error::WrongTarget { vid: self.vid, pid: self.pid })
        }
    }

    /// `bcdDevice` as "major.minor.patch" (one BCD digit each for minor/patch).
    pub fn version(&self) -> String {
        bcd_version(self.bcd_device)
    }
}

/// 0x0122 -> "1.2.2".
pub fn bcd_version(bcd: u16) -> String {
    format!("{}.{}.{}", bcd >> 8, (bcd >> 4) & 0xf, bcd & 0xf)
}

/// DFU suffix CRC: CRC-32 (poly 0xEDB88320, init 0xFFFFFFFF) with **no**
/// final inversion, over every byte before dwCRC.
pub fn dfu_crc(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    crc
}

// ---------------------------------------------------------------- status

/// DFU_GETSTATUS reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub status: u8,
    pub poll_timeout_ms: u32,
    pub state: u8,
}

impl Status {
    pub fn decode(b: [u8; 6]) -> Self {
        Self { status: b[0], poll_timeout_ms: u32::from_le_bytes([b[1], b[2], b[3], 0]), state: b[4] }
    }

    pub fn state_name(&self) -> &'static str {
        match self.state {
            0 => "appIDLE",
            1 => "appDETACH",
            2 => "dfuIDLE",
            3 => "dfuDNLOAD-SYNC",
            4 => "dfuDNBUSY",
            5 => "dfuDNLOAD-IDLE",
            6 => "dfuMANIFEST-SYNC",
            7 => "dfuMANIFEST",
            8 => "dfuMANIFEST-WAIT-RESET",
            9 => "dfuUPLOAD-IDLE",
            10 => "dfuERROR",
            _ => "unknown",
        }
    }
}

// ---------------------------------------------------------------- modes

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Normal operation (0fd9:0070).
    Run,
    /// Bootloader / DFU mode (0fd9:0071).
    Bootloader,
    Absent,
}

/// Which mode the (single) Wave:3 is in. Doesn't open the device.
pub fn mode() -> Result<Mode, Error> {
    let mut run = 0;
    let mut boot = 0;
    for d in rusb::devices()?.iter() {
        let Ok(desc) = d.device_descriptor() else { continue };
        match (desc.vendor_id(), desc.product_id()) {
            (VID, PID_WAVE3) => run += 1,
            (VID, PID_BOOTLOADER) => boot += 1,
            _ => {}
        }
    }
    // The bootloader's serial ("xxxxxxxxxxxxxx") doesn't match the run-mode
    // one, so with several mics there'd be no way to pair them up.
    if run + boot > 1 {
        return Err(Error::InvalidValue("more than one Wave:3 connected; unplug the others".into()));
    }
    Ok(if run == 1 {
        Mode::Run
    } else if boot == 1 {
        Mode::Bootloader
    } else {
        Mode::Absent
    })
}

/// Poll until the device shows up in `want`.
pub fn wait_for(want: Mode, timeout: Duration) -> Result<(), Error> {
    let t0 = Instant::now();
    loop {
        if mode()? == want {
            // Give udev a moment to apply permissions to the new node.
            std::thread::sleep(Duration::from_millis(300));
            return Ok(());
        }
        if t0.elapsed() > timeout {
            return Err(Error::Timeout(match want {
                Mode::Run => "Wave:3 did not come back in run mode (0fd9:0070)",
                Mode::Bootloader => "Wave:3 did not appear in bootloader mode (0fd9:0071)",
                Mode::Absent => "Wave:3 did not disconnect",
            }));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A request that makes the device reset may fail on the host side
/// (observed: `Pipe` for 0xF3). The re-enumeration is what counts.
fn reset_request_ok(r: rusb::Result<usize>) -> Result<(), Error> {
    match r {
        Ok(_) | Err(rusb::Error::Pipe | rusb::Error::NoDevice | rusb::Error::Io) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn get_status(h: &DeviceHandle<GlobalContext>, iface: u8) -> Result<Status, Error> {
    let mut b = [0u8; 6];
    let n = h.read_control(RT_IN, DFU_GETSTATUS, 0, iface as u16, &mut b, TIMEOUT)?;
    if n != 6 {
        return Err(Error::BadLength { msg: 0xD003, expected: 6, got: n });
    }
    Ok(Status::decode(b))
}

fn open(pid: u16, iface: u8) -> Result<DeviceHandle<GlobalContext>, Error> {
    let h = rusb::open_device_with_vid_pid(VID, pid).ok_or(Error::NotFound)?;
    let _ = h.set_auto_detach_kernel_driver(true);
    h.claim_interface(iface)?;
    Ok(h)
}

/// GETSTATUS on the run-mode DFU interface (expect appIDLE).
pub fn run_status() -> Result<Status, Error> {
    let h = open(PID_WAVE3, DFU_IFACE_RUN)?;
    let s = get_status(&h, DFU_IFACE_RUN);
    let _ = h.release_interface(DFU_IFACE_RUN);
    s
}

/// Send XMOS RESETINTODFU (0xF2) and wait for the bootloader to enumerate.
/// Audio drops out until [`Bootloader::leave`].
pub fn enter_bootloader() -> Result<Bootloader, Error> {
    let h = open(PID_WAVE3, DFU_IFACE_RUN)?;
    reset_request_ok(h.write_control(RT_OUT, XMOS_RESETINTODFU, 0, DFU_IFACE_RUN as u16, &[], TIMEOUT))?;
    drop(h);
    wait_for(Mode::Bootloader, REENUM_TIMEOUT)?;
    Bootloader::open()
}

/// The Wave:3 in bootloader mode.
pub struct Bootloader {
    handle: DeviceHandle<GlobalContext>,
}

impl Bootloader {
    pub fn open() -> Result<Self, Error> {
        Ok(Self { handle: open(PID_BOOTLOADER, DFU_IFACE_BOOT).map_err(|e| match e {
            Error::NotFound => Error::InvalidValue(
                "bootloader 0fd9:0071 not accessible (is udev/70-elgato-wave.rules installed?)".into(),
            ),
            e => e,
        })? })
    }

    pub fn status(&self) -> Result<Status, Error> {
        get_status(&self.handle, DFU_IFACE_BOOT)
    }

    /// Clear dfuERROR if present (DFU_CLRSTATUS). Writes nothing to flash.
    pub fn clear_error(&self) -> Result<Status, Error> {
        let s = self.status()?;
        if s.state == 10 {
            self.handle.write_control(RT_OUT, DFU_CLRSTATUS, 0, DFU_IFACE_BOOT as u16, &[], TIMEOUT)?;
            return self.status();
        }
        Ok(s)
    }

    /// Send XMOS RESETFROMDFU (0xF3) and wait for run mode.
    pub fn leave(self) -> Result<(), Error> {
        reset_request_ok(self.handle.write_control(
            RT_OUT,
            XMOS_RESETFROMDFU,
            0,
            DFU_IFACE_BOOT as u16,
            &[],
            TIMEOUT,
        ))?;
        drop(self);
        wait_for(Mode::Run, REENUM_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a file with a valid suffix around `payload`.
    fn make(payload: &[u8], bcd: u16, pid: u16, vid: u16) -> Vec<u8> {
        let mut f = payload.to_vec();
        for v in [bcd, pid, vid, 0x0100] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(b"UFD");
        f.push(16);
        let crc = dfu_crc(&f);
        f.extend_from_slice(&crc.to_le_bytes());
        f
    }

    #[test]
    fn crc_matches_reference() {
        // Standard CRC-32 of "123456789" is 0xCBF43926; DFU omits the final
        // inversion.
        assert_eq!(dfu_crc(b"123456789"), !0xCBF4_3926);
    }

    #[test]
    fn parses_valid_image() {
        let f = make(b"firmware", 0x0122, PID_WAVE3, VID);
        let img = Image::parse(&f).unwrap();
        assert_eq!(img.payload, b"firmware");
        assert_eq!((img.vid, img.pid, img.bcd_device, img.bcd_dfu), (VID, PID_WAVE3, 0x0122, 0x0100));
        assert_eq!(img.version(), "1.2.2");
        img.check_wave3().unwrap();
    }

    #[test]
    fn rejects_corruption() {
        let mut f = make(b"firmware", 0x0122, PID_WAVE3, VID);
        f[0] ^= 1;
        assert!(matches!(Image::parse(&f), Err(Error::CrcMismatch { .. })));
    }

    #[test]
    fn rejects_missing_suffix() {
        let mut f = make(b"firmware", 0x0122, PID_WAVE3, VID);
        let n = f.len();
        f[n - 8] = b'X';
        assert!(matches!(Image::parse(&f), Err(Error::BadImage(_))));
        assert!(matches!(Image::parse(b"short"), Err(Error::BadImage(_))));
    }

    #[test]
    fn rejects_other_device() {
        let f = make(b"firmware", 0x0100, 0x0080, VID);
        let img = Image::parse(&f).unwrap();
        assert!(matches!(img.check_wave3(), Err(Error::WrongTarget { pid: 0x0080, .. })));
    }

    #[test]
    fn decodes_status() {
        // Observed on the device in bootloader mode.
        let s = Status::decode([0x00, 0x0a, 0x00, 0x00, 0x02, 0x00]);
        assert_eq!((s.status, s.poll_timeout_ms, s.state), (0, 10, 2));
        assert_eq!(s.state_name(), "dfuIDLE");
    }

    /// Real 1.2.2 image, if present: `WAVE3_DFU=path cargo test -p wave-proto`.
    #[test]
    fn real_image() {
        let Ok(path) = std::env::var("WAVE3_DFU") else { return };
        let f = std::fs::read(path).unwrap();
        let img = Image::parse(&f).unwrap();
        img.check_wave3().unwrap();
        assert_eq!(img.crc, 0xaff4_13c3);
        assert_eq!(img.payload.len(), 98_368);
        assert_eq!(img.version(), "1.2.2");
    }
}
