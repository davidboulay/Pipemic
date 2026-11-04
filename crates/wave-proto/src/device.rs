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

/// A Wave:3 handle. Interface 3 is claimed only for the duration of each
/// operation, so several processes (pipemicd, pipemicctl) can share the mic.
pub struct Wave3 {
    handle: DeviceHandle<GlobalContext>,
    iface: u8,
    api: (u8, u8),
}

impl Wave3 {
    /// Open the first Wave:3 and check the API version.
    pub fn open() -> Result<Self, Error> {
        let handle = rusb::open_device_with_vid_pid(VID, PID_WAVE3).ok_or(Error::NotFound)?;
        Self::from_handle(handle)
    }

    pub fn from_handle(handle: DeviceHandle<GlobalContext>) -> Result<Self, Error> {
        let mut dev = Self { handle, iface: VENDOR_IFACE, api: (0, 0) };
        let mut b = [0u8; 2];
        dev.claimed(|d| d.read_exact(id::VERSION, &mut b))?;
        dev.api = (b[0], b[1]);
        if dev.api.0 != SUPPORTED_API_MAJOR {
            return Err(Error::UnsupportedApi(dev.api.0, dev.api.1));
        }
        Ok(dev)
    }

    /// Run `f` with interface 3 claimed; retries briefly if another process
    /// holds it.
    fn claimed<T>(&self, f: impl FnOnce(&Self) -> Result<T, Error>) -> Result<T, Error> {
        let mut tries = 0;
        loop {
            match self.handle.claim_interface(self.iface) {
                Ok(()) => break,
                Err(rusb::Error::Busy) if tries < 25 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e.into()),
            }
        }
        let r = f(self);
        let _ = self.handle.release_interface(self.iface);
        r
    }

    pub fn api_version(&self) -> (u8, u8) {
        self.api
    }

    fn windex(&self) -> u16 {
        (ENTITY << 8) | self.iface as u16
    }

    /// GET_MEM into `buf`; returns the number of bytes the device sent.
    pub fn read_raw(&self, msg_id: u16, buf: &mut [u8]) -> Result<usize, Error> {
        self.claimed(|d| d.get_mem(msg_id, buf))
    }

    fn get_mem(&self, msg_id: u16, buf: &mut [u8]) -> Result<usize, Error> {
        Ok(self.handle.read_control(RT_IN, GET_MEM, msg_id, self.windex(), buf, TIMEOUT)?)
    }

    fn read_exact(&self, msg_id: u16, buf: &mut [u8]) -> Result<(), Error> {
        let n = self.get_mem(msg_id, buf)?;
        if n != buf.len() {
            return Err(Error::BadLength { msg: msg_id, expected: buf.len(), got: n });
        }
        Ok(())
    }

    pub fn config_bytes(&self) -> Result<[u8; msg::CONFIG_LEN], Error> {
        self.claimed(Self::config_bytes_inner)
    }

    fn config_bytes_inner(&self) -> Result<[u8; msg::CONFIG_LEN], Error> {
        let mut b = [0u8; msg::CONFIG_LEN];
        self.read_exact(id::CONFIG, &mut b)?;
        Ok(b)
    }

    pub fn config(&self) -> Result<Config, Error> {
        Config::decode(&self.config_bytes()?)
    }

    pub fn status(&self) -> Result<Status, Error> {
        let mut b = [0u8; msg::STATUS_LEN];
        self.claimed(|d| d.read_exact(id::STATUS, &mut b))?;
        Status::decode(&b)
    }

    pub fn version(&self) -> Result<Version, Error> {
        let mut b = [0u8; msg::VERSION_LEN];
        self.claimed(|d| d.read_exact(id::VERSION, &mut b))?;
        Version::decode(&b)
    }

    /// Compute the bytes a setting would produce, without touching the device.
    /// Returns (current bytes, new bytes).
    pub fn plan(&self, settings: &[Setting]) -> Result<([u8; 16], [u8; 16]), Error> {
        self.claimed(|d| d.plan_inner(settings))
    }

    fn plan_inner(&self, settings: &[Setting]) -> Result<([u8; 16], [u8; 16]), Error> {
        let before = self.config_bytes_inner()?;
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
    /// The whole read-modify-write-verify runs under one claim.
    pub fn update(&self, settings: &[Setting]) -> Result<([u8; 16], [u8; 16]), Error> {
        self.claimed(|d| d.update_inner(settings))
    }

    fn update_inner(&self, settings: &[Setting]) -> Result<([u8; 16], [u8; 16]), Error> {
        let (before, want) = self.plan_inner(settings)?;
        if want == before {
            return Ok((before, before));
        }
        let n = self.handle.write_control(RT_OUT, SET_MEM, id::CONFIG, self.windex(), &want, TIMEOUT)?;
        if n != want.len() {
            return Err(Error::BadLength { msg: id::CONFIG, expected: want.len(), got: n });
        }
        let after = self.config_bytes_inner()?;
        if after != want {
            return Err(Error::VerifyFailed { wanted: want, got: after });
        }
        Ok((before, after))
    }
}

