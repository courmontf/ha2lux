//! Accès au bus I²C : abstraction testable, verrous par bus, erreurs système.
//!
//! L'implémentation réelle ([`LinuxI2cOpener`]) s'appuie sur la crate `i2cdev`
//! (ouverture de `/dev/i2c-N` puis `I2C_SLAVE`) : aucun `unsafe` dans ha2lux.
//! Un bus I²C **n'est jamais ouvert** sans avoir été relié à un connecteur DRM
//! connecté (voir `drm`).

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use ha2lux_core::BackendError;
use i2cdev::core::I2CDevice;
use i2cdev::linux::LinuxI2CDevice;

/// Un esclave I²C ouvert (bloquant). Les appels peuvent durer : ils ne doivent
/// jamais être faits depuis un chemin d'interface ni depuis l'acteur décideur.
pub trait I2cDevice: Send {
    /// Envoie les octets à l'esclave.
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
    /// Lit exactement `buf.len()` octets de l'esclave.
    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()>;
}

/// Adresse esclave de l'EDID (EEPROM DDC2B).
pub const EDID_ADDRESS: u16 = 0x50;

/// Ouvre un esclave d'un bus I²C.
pub trait I2cOpener: Send + Sync + 'static {
    /// Ouvre le bus `bus` (numéro `N` de `/dev/i2c-N`) sur l'esclave `address`
    /// (0x37 pour DDC/CI, 0x50 pour l'EDID).
    fn open(&self, bus: u32, address: u16) -> io::Result<Box<dyn I2cDevice>>;
}

/// Lit le bloc EDID de base d'un bus (écriture du pointeur 0 puis lecture de
/// 128 octets). **Transaction I²C réelle** : bloquante, à n'exécuter que sur un
/// bus relié à un écran, depuis un thread bloquant.
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

/// Ouvreur réel : `/dev/i2c-N` (ou un autre répertoire de nœuds).
#[derive(Clone, Debug)]
pub struct LinuxI2cOpener {
    dev_dir: PathBuf,
}

impl LinuxI2cOpener {
    /// Ouvreur sur `/dev`.
    pub fn new() -> Self {
        Self {
            dev_dir: PathBuf::from("/dev"),
        }
    }

    /// Ouvreur sur un autre répertoire de nœuds.
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

/// Un verrou asynchrone par bus I²C : plusieurs écrans peuvent partager un bus
/// (hubs, docks, MST) et ne doivent jamais être accédés en même temps.
#[derive(Debug, Default)]
pub struct BusLocks {
    locks: Mutex<HashMap<u32, Arc<tokio::sync::Mutex<()>>>>,
}

impl BusLocks {
    /// Registre vide.
    pub fn new() -> Self {
        Self::default()
    }

    /// Verrou du bus `bus` (le même `Arc` pour un même numéro).
    pub fn get(&self, bus: u32) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(bus).or_default())
    }
}

const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const EACCES: i32 = 13;
const ENODEV: i32 = 19;

/// Classe une erreur système en erreur de backend.
///
/// - droits (`EACCES`, `EPERM`) : définitif, il faut une règle udev ;
/// - nœud disparu (`ENOENT`, `ENODEV`) : écran injoignable, on refermera le fd ;
/// - tout le reste (`ENXIO`/`EREMOTEIO` = l'écran n'acquitte pas, `EIO`,
///   `ETIMEDOUT`, `EBUSY`…) : transitoire, l'écran dort ou le lien se rétablit.
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
