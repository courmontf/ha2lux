//! Display to DDC bus association when sysfs does not provide it (proprietary NVIDIA).
//!
//! With the proprietary NVIDIA driver, DRM connectors have no `ddc`
//! link. The DDC buses do exist, though: they are I²C adapters named
//! "NVIDIA i2c adapter N ...". They are matched to connectors by **comparing
//! the EDID read on the bus** (address 0x50) with the connector's.
//!
//! Garde-fous :
//! - only adapters whose **name** is that of a known GPU driver are
//!   opened (never an SMBus or sensor bus);
//! - reading the EDID is a **real I²C transaction**: it only happens if
//!   the caller asks for it explicitly (`ha2lux doctor --probe`, then the
//!   daemon's rescan);
//! - an ambiguous association (two displays with identical EDID) is refused.
//!
//! The functions are blocking: call them from `spawn_blocking`.

use std::path::Path;

use ha2lux_core::Edid;

use crate::drm::{self, Connector, DdcCandidate};
use crate::i2c::{I2cOpener, read_edid};

/// Sysfs directory of the I²C nodes.
pub const DEFAULT_I2C_DEV_DIR: &str = "/sys/class/i2c-dev";

/// Fragments of I²C adapter names that are graphics card DDC buses.
const GPU_DDC_ADAPTER_NAMES: [&str; 1] = ["NVIDIA i2c adapter"];

/// Un adaptateur I²C.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct I2cAdapter {
    /// Number `N` of `i2c-N`.
    pub bus: u32,
    /// Name declared by the kernel.
    pub name: String,
}

impl I2cAdapter {
    /// DDC bus of a graphics card (the only type we dare to probe).
    pub fn is_gpu_ddc(&self) -> bool {
        GPU_DDC_ADAPTER_NAMES.iter().any(|p| self.name.contains(p))
    }
}

/// Lists the adapters in `dir` (read-only), sorted by number.
pub fn list_adapters(dir: &Path) -> Vec<I2cAdapter> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut adapters: Vec<I2cAdapter> = entries
        .flatten()
        .filter_map(|e| {
            let bus = e.file_name().to_str()?.strip_prefix("i2c-")?.parse().ok()?;
            let name = std::fs::read_to_string(e.path().join("name"))
                .ok()?
                .trim()
                .to_owned();
            Some(I2cAdapter { bus, name })
        })
        .collect();
    adapters.sort_by_key(|a| a.bus);
    adapters
}

/// Where the display to bus association comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusSource {
    /// Lien `ddc` de sysfs.
    Drm,
    /// EDID read on the bus and compared with the connector's.
    EdidOverI2c,
}

/// Result of a discovery.
#[derive(Clone, Debug, Default)]
pub struct Discovery {
    /// Controllable displays, with the origin of their bus.
    pub candidates: Vec<(DdcCandidate, BusSource)>,
    /// Human-readable remarks (ambiguities, unreadable buses).
    pub notes: Vec<String>,
}

/// Discovers the controllable external displays. Without `resolve_by_edid`, only the sysfs `ddc`
/// link is used and **no I²C transaction** takes place.
pub fn discover_external(root: &Path, opener: &dyn I2cOpener, resolve_by_edid: bool) -> Discovery {
    let mut connectors = drm::scan(&root.join("sys/class/drm"));
    let mut sources: Vec<(String, BusSource)> = connectors
        .iter()
        .filter(|c| c.ddc_bus.is_some())
        .map(|c| (c.name.clone(), BusSource::Drm))
        .collect();
    let mut notes = Vec::new();

    if resolve_by_edid {
        let unresolved: Vec<usize> = connectors
            .iter()
            .enumerate()
            .filter(|(_, c)| c.connected && !c.internal && c.ddc_bus.is_none() && c.edid.is_some())
            .map(|(i, _)| i)
            .collect();
        if !unresolved.is_empty() {
            let adapters: Vec<I2cAdapter> = list_adapters(&root.join("sys/class/i2c-dev"))
                .into_iter()
                .filter(I2cAdapter::is_gpu_ddc)
                .collect();
            // One EDID read per GPU adapter; an empty port fails, which is normal.
            let mut on_bus: Vec<(u32, u64)> = Vec::new();
            for adapter in &adapters {
                match read_edid(opener, adapter.bus).map(|raw| Edid::parse(&raw)) {
                    Ok(Ok(edid)) => on_bus.push((adapter.bus, edid.fingerprint)),
                    Ok(Err(error)) => {
                        notes.push(format!("i2c-{}: invalid EDID ({error})", adapter.bus))
                    }
                    Err(_) => {} // no display on this port
                }
            }
            for &i in &unresolved {
                let Some(fingerprint) = connectors[i].edid.as_ref().map(|e| e.fingerprint) else {
                    continue;
                };
                let buses: Vec<u32> = on_bus
                    .iter()
                    .filter(|(_, f)| *f == fingerprint)
                    .map(|(b, _)| *b)
                    .collect();
                let twins = unresolved
                    .iter()
                    .filter(|&&j| {
                        connectors[j]
                            .edid
                            .as_ref()
                            .is_some_and(|e| e.fingerprint == fingerprint)
                    })
                    .count();
                match (buses.as_slice(), twins) {
                    ([bus], 1) => {
                        connectors[i].ddc_bus = Some(*bus);
                        sources.push((connectors[i].name.clone(), BusSource::EdidOverI2c));
                    }
                    ([], _) => notes.push(format!(
                        "{}: no GPU adapter presents its EDID (matching impossible)",
                        connectors[i].name
                    )),
                    _ => notes.push(format!(
                        "{}: ambiguous matching (several displays or buses with the same EDID), refused",
                        connectors[i].name
                    )),
                }
            }
        }
    }

    let candidates = drm::external_candidates(&connectors)
        .into_iter()
        .map(|candidate| {
            let source = sources
                .iter()
                .find(|(name, _)| *name == candidate.connector)
                .map_or(BusSource::Drm, |(_, s)| *s);
            (candidate, source)
        })
        .collect();
    Discovery { candidates, notes }
}

/// Shortcut: external connectors still without a DDC bus after using sysfs alone.
pub fn without_bus(connectors: &[Connector]) -> Vec<&Connector> {
    connectors
        .iter()
        .filter(|c| c.connected && !c.internal && c.ddc_bus.is_none())
        .collect()
}
