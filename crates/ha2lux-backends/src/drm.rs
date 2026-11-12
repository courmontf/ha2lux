//! Découverte des écrans via DRM/sysfs : connecteurs, EDID, bus DDC.
//!
//! Lecture seule. **Seuls** les bus reliés à un connecteur externe connecté
//! sont retenus : on n'écrit jamais sur un bus I²C non lié à un écran
//! (SMBus, capteurs…), ce qui peut être dangereux sur certains contrôleurs.
//!
//! Les fonctions sont bloquantes (quelques lectures sysfs) : les appeler depuis
//! `spawn_blocking`, jamais depuis un chemin d'interface.

use std::path::{Path, PathBuf};

use ha2lux_core::{DisplayId, DisplayInfo, DisplayKind, Edid};

/// Répertoire sysfs standard des connecteurs DRM.
pub const DEFAULT_DRM_DIR: &str = "/sys/class/drm";

/// Un connecteur DRM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connector {
    /// Carte (ex. `card1`).
    pub card: String,
    /// Nom du connecteur (ex. `DP-1`, `eDP-1`, `HDMI-A-2`).
    pub name: String,
    /// Écran branché (`status` = `connected`).
    pub connected: bool,
    /// Panneau interne (`eDP`, `LVDS`, `DSI`) : backlight, jamais de DDC.
    pub internal: bool,
    /// Bus I²C de ce connecteur (`N` de `i2c-N`), s'il est exposé.
    pub ddc_bus: Option<u32>,
    /// EDID analysé.
    pub edid: Option<Edid>,
    /// Pourquoi l'EDID est absent ou illisible.
    pub edid_problem: Option<String>,
    /// Répertoire sysfs du connecteur.
    pub path: PathBuf,
}

/// Un écran externe que l'on peut tenter de piloter en DDC/CI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdcCandidate {
    /// Identité (maximum inconnu, fixé par la sonde).
    pub info: DisplayInfo,
    /// Connecteur.
    pub connector: String,
    /// Bus I²C du connecteur.
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

/// Énumère les connecteurs de `drm_dir`, triés par (carte, nom).
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

/// Écrans externes pilotables en DDC/CI : connectés, non internes, avec un bus
/// DDC exposé et un EDID lisible. L'identité suit l'EDID (pas le connecteur) ;
/// deux écrans à l'identité identique sont départagés par leur connecteur.
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
