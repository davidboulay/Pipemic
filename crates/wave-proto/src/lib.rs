//! Elgato Wave:3 control protocol (LEWITT WaveAPI 5.3, legacy UAC1 vendor
//! strategy). Protocol notes: `docs/wave3-protocol.md`.

pub mod device;
pub mod dfu;
pub mod fixed;
pub mod msg;

pub use device::Wave3;
pub use msg::{Config, SemVer, Setting, Status, Version, VolumeSelect};
pub use rusb::Error as RusbError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no Wave:3 (0fd9:0070) found")]
    NotFound,
    #[error("USB: {0}")]
    Usb(#[from] rusb::Error),
    #[error("unsupported WaveAPI version {0}.{1} (only 5.x is known)")]
    UnsupportedApi(u8, u8),
    #[error("msg {msg}: expected {expected} bytes, got {got}")]
    BadLength { msg: u16, expected: usize, got: usize },
    #[error("{name} = {value} is out of range")]
    OutOfRange { name: &'static str, value: f32 },
    #[error("{0}")]
    InvalidValue(String),
    #[error("config does not re-encode byte-exact; refusing to write")]
    RoundTrip,
    #[error("bad firmware image: {0}")]
    BadImage(String),
    #[error("firmware image CRC mismatch: stored {stored:08x}, computed {computed:08x}")]
    CrcMismatch { stored: u32, computed: u32 },
    #[error("firmware image is for {vid:04x}:{pid:04x}, not a Wave:3 (0fd9:0070)")]
    WrongTarget { vid: u16, pid: u16 },
    #[error("timeout: {0}")]
    Timeout(&'static str),
    #[error("write not confirmed: wanted {wanted:02x?}, read back {got:02x?}")]
    VerifyFailed { wanted: [u8; 16], got: [u8; 16] },
}
