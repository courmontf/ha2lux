//! I²C bus access: testable abstraction, per-bus locks, system errors.
//!
//! The real implementation ([`LinuxI2cOpener`]) relies on the `i2cdev` crate
//! (opening `/dev/i2c-N` then `I2C_SLAVE`): no `unsafe` in ha2lux.
//! An I²C bus is **never opened** without having been tied to a connected
//! DRM connector (see `drm`).

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use ha2lux_core::BackendError;
use i2cdev::core::I2CDevice;
use i2cdev::linux::LinuxI2CDevice;

/// An open I²C slave (blocking). Calls can take a long time: they must
/// never be made from an interface path or from the decision actor.
pub trait I2cDevice: Send {
    /// Sends the bytes to the slave.
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
    /// Lit exactement `buf.len()` octets de l'esclave.
    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()>;
}

/// Adresse esclave de l'EDID (EEPROM DDC2B).
pub const EDID_ADDRESS: u16 = 0x50;

/// Opens a slave on an I²C bus.
pub trait I2cOpener: Send + Sync + 'static {
    /// Opens bus `bus` (number `N` of `/dev/i2c-N`) on slave `address`
    /// (0x37 for DDC/CI, 0x50 for the EDID).
    fn open(&self, bus: u32, address: u16) -> io::Result<Box<dyn I2cDevice>>;
}

/// Reads the base EDID block of a bus (write pointer 0 then read
/// 128 bytes). **Real I²C transaction**: blocking, to be run only on a
/// bus tied to a display, from a blocking thread.
pub fn read_edid(opener: &dyn I2cOpener, bus: u32) -> io::Result<Vec<u8>> {
    let mut device = opener.open(bus, EDID_ADDRESS)?;
    device.write(&[0x00])?;
    let mut block = vec![0u8; ha2lux_core::edid::EDID_BLOCK_LEN];
    device.read_exact(&mut block)?;
    Ok(block)
}

struct LinuxDevice(LinuxI2CDevice);

impl I2cDevice for LinuxDevice {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.0.write(data).map_err(io::Error::from)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.0.read(buf).map_err(io::Error::from)
    }
}

/// Real opener: `/dev/i2c-N` (or another node directory).
#[derive(Clone, Debug)]
pub struct LinuxI2cOpener {
    dev_dir: PathBuf,
}

impl LinuxI2cOpener {
    /// Opener on `/dev`.
    pub fn new() -> Self {
        Self {
            dev_dir: PathBuf::from("/dev"),
        }
    }

    /// Opener on another node directory.
    pub fn with_dev_dir(dev_dir: impl Into<PathBuf>) -> Self {
        Self {
            dev_dir: dev_dir.into(),
        }
    }
}

impl Default for LinuxI2cOpener {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cOpener for LinuxI2cOpener {
    fn open(&self, bus: u32, address: u16) -> io::Result<Box<dyn I2cDevice>> {
        let path = self.dev_dir.join(format!("i2c-{bus}"));
        let device = LinuxI2CDevice::new(path, address).map_err(io::Error::from)?;
        Ok(Box::new(LinuxDevice(device)))
    }
}

/// One async lock per I²C bus: several displays may share a bus
/// (hubs, docks, MST) and must never be accessed at the same time.
#[derive(Debug, Default)]
pub struct BusLocks {
    locks: Mutex<HashMap<u32, Arc<tokio::sync::Mutex<()>>>>,
}

impl BusLocks {
    /// Registre vide.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock for bus `bus` (the same `Arc` for the same number).
    pub fn get(&self, bus: u32) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(bus).or_default())
    }
}

const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const EACCES: i32 = 13;
const ENODEV: i32 = 19;

/// Classifies a system error as a backend error.
///
/// - permissions (`EACCES`, `EPERM`): permanent, a udev rule is needed;
/// - node gone (`ENOENT`, `ENODEV`): display unreachable, the fd will be closed;
/// - everything else (`ENXIO`/`EREMOTEIO` = the display does not acknowledge, `EIO`,
///   `ETIMEDOUT`, `EBUSY`...): transient, the display is asleep or the link is recovering.
pub fn classify_io_error(error: &io::Error) -> BackendError {
    let text = error.to_string();
    match error.raw_os_error() {
        Some(EACCES | EPERM) => BackendError::PermissionDenied(text),
        Some(ENOENT | ENODEV) => BackendError::Disconnected(text),
        _ => match error.kind() {
            io::ErrorKind::PermissionDenied => BackendError::PermissionDenied(text),
            io::ErrorKind::NotFound => BackendError::Disconnected(text),
            _ => BackendError::Transient(text),
        },
    }
}
