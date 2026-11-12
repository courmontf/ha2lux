//! Association écran ↔ bus DDC quand sysfs ne la donne pas (NVIDIA propriétaire).
//!
//! Avec le pilote NVIDIA propriétaire, les connecteurs DRM n'ont pas de lien
//! `ddc`. Les bus DDC existent pourtant : ce sont des adaptateurs I²C nommés
//! « NVIDIA i2c adapter N … ». On les associe aux connecteurs en **comparant
//! l'EDID lu sur le bus** (adresse 0x50) à celui du connecteur.
//!
//! Garde-fous :
//! - seuls les adaptateurs dont le **nom** est celui d'un pilote GPU connu sont
//!   ouverts (jamais un bus SMBus ou de capteurs) ;
//! - la lecture d'EDID est une **vraie transaction I²C** : elle n'a lieu que si
//!   l'appelant la demande explicitement (`ha2lux doctor --probe`, puis le
//!   rescan du démon) ;
//! - une association ambiguë (deux écrans à l'EDID identique) est refusée.
//!
//! Les fonctions sont bloquantes : à appeler depuis `spawn_blocking`.

use std::path::Path;

use ha2lux_core::Edid;

use crate::drm::{self, Connector, DdcCandidate};
use crate::i2c::{I2cOpener, read_edid};

/// Répertoire sysfs des nœuds I²C.
pub const DEFAULT_I2C_DEV_DIR: &str = "/sys/class/i2c-dev";

/// Fragments de noms d'adaptateurs I²C qui sont des bus DDC de carte graphique.
const GPU_DDC_ADAPTER_NAMES: [&str; 1] = ["NVIDIA i2c adapter"];

/// Un adaptateur I²C.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct I2cAdapter {
    /// Numéro `N` de `i2c-N`.
    pub bus: u32,
    /// Nom déclaré par le noyau.
    pub name: String,
}

impl I2cAdapter {
    /// Bus DDC d'une carte graphique (le seul type que l'on ose sonder).
    pub fn is_gpu_ddc(&self) -> bool {
        GPU_DDC_ADAPTER_NAMES.iter().any(|p| self.name.contains(p))
    }
}

/// Liste les adaptateurs de `dir` (lecture seule), triés par numéro.
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

/// D'où vient l'association écran ↔ bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusSource {
    /// Lien `ddc` de sysfs.
    Drm,
    /// EDID lu sur le bus et comparé à celui du connecteur.
    EdidOverI2c,
}

/// Résultat d'une découverte.
#[derive(Clone, Debug, Default)]
pub struct Discovery {
    /// Écrans pilotables, avec l'origine de leur bus.
    pub candidates: Vec<(DdcCandidate, BusSource)>,
    /// Remarques lisibles (ambiguïtés, bus illisibles).
    pub notes: Vec<String>,
}

/// Découvre les écrans externes pilotables. Sans `resolve_by_edid`, seul le lien
/// `ddc` de sysfs est utilisé et **aucune transaction I²C** n'a lieu.
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
            // Une lecture d'EDID par adaptateur GPU ; un port vide échoue, c'est normal.
            let mut on_bus: Vec<(u32, u64)> = Vec::new();
            for adapter in &adapters {
                match read_edid(opener, adapter.bus).map(|raw| Edid::parse(&raw)) {
                    Ok(Ok(edid)) => on_bus.push((adapter.bus, edid.fingerprint)),
                    Ok(Err(error)) => {
                        notes.push(format!("i2c-{}: invalid EDID ({error})", adapter.bus))
                    }
                    Err(_) => {} // aucun écran sur ce port
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

/// Raccourci : connecteurs externes encore sans bus DDC après le seul usage de sysfs.
pub fn without_bus(connectors: &[Connector]) -> Vec<&Connector> {
    connectors
        .iter()
        .filter(|c| c.connected && !c.internal && c.ddc_bus.is_none())
        .collect()
}
