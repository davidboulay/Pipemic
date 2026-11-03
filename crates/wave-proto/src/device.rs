//! USB transport: legacy UAC1 memory requests on the vendor interface.

use crate::msg::{self, id, Config, Setting, Status, Version};
use crate::Error;
use rusb::{DeviceHandle, GlobalContext};
use std::time::Duration;

pub const VID: u16 = 0x0fd9;
pub const PID_WAVE3: u16 = 0x0070;
/// Vendor interface (FF/F0/01). No kernel driver binds it, so claiming it
/// leaves `snd-usb-audio` (interfaces 0–2) alone.
pub const VENDOR_IFACE: u8 = 3;

const ENTITY: u16 = 0x33;
const GET_MEM: u8 = 0x85;
const SET_MEM: u8 = 0x05;
const RT_IN: u8 = 0xA1; // class | interface | device-to-host
const RT_OUT: u8 = 0x21; // class | interface | host-to-device
const TIMEOUT: Duration = Duration::from_millis(500);

/// Only API 5.x has been decoded.
pub const SUPPORTED_API_MAJOR: u8 = 5;

pub struct Wave3 {
    handle: DeviceHandle<GlobalContext>,
    iface: u8,
    api: (u8, u8),
}

impl Wave3 {
    /// Open the first Wave:3, claim the vendor interface and check the API version.
    pub fn open() -> Result<Self, Error> {
        let handle = rusb::open_device_with_vid_pid(VID, PID_WAVE3).ok_or(Error::NotFound)?;
        Self::from_handle(handle)
    }

    pub fn from_handle(handle: DeviceHandle<GlobalContext>) -> Result<Self, Error> {
        handle.claim_interface(VENDOR_IFACE)?;
        let mut dev = Self { handle, iface: VENDOR_IFACE, api: (0, 0) };
        let mut b = [0u8; 2];
        dev.read_exact(id::VERSION, &mut b)?;
        dev.api = (b[0], b[1]);
        if dev.api.0 != SUPPORTED_API_MAJOR {
            return Err(Error::UnsupportedApi(dev.api.0, dev.api.1));
        }
        Ok(dev)
    }

    pub fn api_version(&self) -> (u8, u8) {
        self.api
    }

    fn windex(&self) -> u16 {
        (ENTITY << 8) | self.iface as u16
    }

    /// GET_MEM into `buf`; returns the number of bytes the device sent.
    pub fn read_raw(&self, msg_id: u16, buf: &mut [u8]) -> Result<usize, Error> {
        Ok(self.handle.read_control(RT_IN, GET_MEM, msg_id, self.windex(), buf, TIMEOUT)?)
    }

    fn read_exact(&self, msg_id: u16, buf: &mut [u8]) -> Result<(), Error> {
        let n = self.read_raw(msg_id, buf)?;
        if n != buf.len() {
            return Err(Error::BadLength { msg: msg_id, expected: buf.len(), got: n });
        }
        Ok(())
    }

    pub fn config_bytes(&self) -> Result<[u8; msg::CONFIG_LEN], Error> {
        let mut b = [0u8; msg::CONFIG_LEN];
        self.read_exact(id::CONFIG, &mut b)?;
        Ok(b)
    }

    pub fn config(&self) -> Result<Config, Error> {
        Config::decode(&self.config_bytes()?)
    }

    pub fn status(&self) -> Result<Status, Error> {
        let mut b = [0u8; msg::STATUS_LEN];
        self.read_exact(id::STATUS, &mut b)?;
        Status::decode(&b)
    }

    pub fn version(&self) -> Result<Version, Error> {
        let mut b = [0u8; msg::VERSION_LEN];
        self.read_exact(id::VERSION, &mut b)?;
        Version::decode(&b)
    }

    /// Compute the bytes a setting would produce, without touching the device.
    /// Returns (current bytes, new bytes).
    pub fn plan(&self, settings: &[Setting]) -> Result<([u8; 16], [u8; 16]), Error> {
        let before = self.config_bytes()?;
        let mut cfg = Config::decode(&before)?;
        // Refuse to write anything we can't reproduce byte-for-byte.
        if cfg.encode()? != before {
            return Err(Error::RoundTrip);
        }
        for &s in settings {
            cfg.apply(s)?;
        }
        Ok((before, cfg.encode()?))
    }

    /// Read-modify-write of msg 0, then read back and verify.
    /// Returns (bytes before, bytes after).
    pub fn update(&self, settings: &[Setting]) -> Result<([u8; 16], [u8; 16]), Error> {
        let (before, want) = self.plan(settings)?;
        if want == before {
            return Ok((before, before));
        }
        let n = self.handle.write_control(RT_OUT, SET_MEM, id::CONFIG, self.windex(), &want, TIMEOUT)?;
        if n != want.len() {
            return Err(Error::BadLength { msg: id::CONFIG, expected: want.len(), got: n });
        }
        let after = self.config_bytes()?;
        if after != want {
            return Err(Error::VerifyFailed { wanted: want, got: after });
        }
        Ok((before, after))
    }
}

impl Drop for Wave3 {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.iface);
    }
}
