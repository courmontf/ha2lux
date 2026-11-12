//! Display discovery through DRM/sysfs: connectors, EDID, DDC buses.
//!
//! Read-only. **Only** buses linked to a connected external connector
//! are kept: we never write to an I²C bus not tied to a display
//! (SMBus, sensors...), which can be dangerous on some controllers.
//!
//! The functions are blocking (a few sysfs reads): call them from
//! `spawn_blocking`, never from an interface path.

use std::path::{Path, PathBuf};

use ha2lux_core::{DisplayId, DisplayInfo, DisplayKind, Edid};

/// Standard sysfs directory of DRM connectors.
pub const DEFAULT_DRM_DIR: &str = "/sys/class/drm";

/// Un connecteur DRM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connector {
    /// Card (e.g. `card1`).
    pub card: String,
    /// Connector name (e.g. `DP-1`, `eDP-1`, `HDMI-A-2`).
    pub name: String,
    /// Display plugged in (`status` = `connected`).
    pub connected: bool,
    /// Internal panel (`eDP`, `LVDS`, `DSI`): backlight, never DDC.
    pub internal: bool,
    /// I²C bus of this connector (`N` of `i2c-N`), if exposed.
    pub ddc_bus: Option<u32>,
    /// Parsed EDID.
    pub edid: Option<Edid>,
    /// Why the EDID is missing or unreadable.
    pub edid_problem: Option<String>,
    /// Sysfs directory of the connector.
    pub path: PathBuf,
}

/// An external display that we can try to drive over DDC/CI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdcCandidate {
    /// Identity (maximum unknown, set by the probe).
    pub info: DisplayInfo,
    /// Connecteur.
    pub connector: String,
    /// I²C bus of the connector.
    pub bus: u32,
}

/// Nom de connecteur interne (panneau) ?
pub fn is_internal_connector(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["edp", "lvds", "dsi"].iter().any(|p| lower.starts_with(p))
}

/// `card1-DP-2` → (`card1`, `DP-2`).
fn split_connector_dir(dir: &str) -> Option<(&str, &str)> {
    let (card, name) = dir.split_once('-')?;
    let digits = card.strip_prefix("card")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && !name.is_empty())
        .then_some((card, name))
}

fn bus_number(name: &str) -> Option<u32> {
    name.strip_prefix("i2c-")?.parse().ok()
}

fn ddc_bus_of(connector_dir: &Path) -> Option<u32> {
    let link = connector_dir.join("ddc");
    std::fs::canonicalize(&link)
        .ok()
        .and_then(|p| p.file_name().and_then(|n| n.to_str()).and_then(bus_number))
        .or_else(|| {
            std::fs::read_link(&link)
                .ok()
                .and_then(|p| p.file_name().and_then(|n| n.to_str()).and_then(bus_number))
        })
}

/// Enumerates the connectors of `drm_dir`, sorted by (card, name).
pub fn scan(drm_dir: &Path) -> Vec<Connector> {
    let Ok(entries) = std::fs::read_dir(drm_dir) else {
        return Vec::new();
    };
    let mut connectors = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some((card, name)) = file_name.to_str().and_then(split_connector_dir) else {
            continue;
        };
        let path = entry.path();
        let connected =
            std::fs::read_to_string(path.join("status")).is_ok_and(|s| s.trim() == "connected");
        let (edid, edid_problem) = match std::fs::read(path.join("edid")) {
            Ok(data) if data.is_empty() => (None, Some("EDID vide".to_owned())),
            Ok(data) => match Edid::parse(&data) {
                Ok(edid) => (Some(edid), None),
                Err(error) => (None, Some(error.to_string())),
            },
            Err(error) => (None, Some(format!("EDID illisible : {error}"))),
        };
        connectors.push(Connector {
            card: card.to_owned(),
            name: name.to_owned(),
            connected,
            internal: is_internal_connector(name),
            ddc_bus: ddc_bus_of(&path),
            edid,
            edid_problem,
            path,
        });
    }
    connectors.sort_by(|a, b| (&a.card, &a.name).cmp(&(&b.card, &b.name)));
    connectors
}

/// External displays controllable over DDC/CI: connected, not internal, with an exposed
/// DDC bus and a readable EDID. Identity follows the EDID (not the connector);
/// two displays with the same identity are told apart by their connector.
pub fn external_candidates(connectors: &[Connector]) -> Vec<DdcCandidate> {
    let mut candidates: Vec<DdcCandidate> = connectors
        .iter()
        .filter(|c| c.connected && !c.internal)
        .filter_map(|c| {
            let (bus, edid) = (c.ddc_bus?, c.edid.as_ref()?);
            Some(DdcCandidate {
                info: DisplayInfo {
                    id: DisplayId::new(edid.identity(&c.name)),
                    name: edid.display_name(&c.name),
                    kind: DisplayKind::External,
                    max: 0,
                },
                connector: c.name.clone(),
                bus,
            })
        })
        .collect();
    let ids: Vec<String> = candidates
        .iter()
        .map(|c| c.info.id.as_str().to_owned())
        .collect();
    for candidate in &mut candidates {
        let count = ids
            .iter()
            .filter(|id| id.as_str() == candidate.info.id.as_str())
            .count();
        if count > 1 {
            candidate.info.id =
                DisplayId::new(format!("{}@{}", candidate.info.id, candidate.connector));
        }
    }
    candidates
}
