//! `ha2lux doctor` : diagnostic de la chaîne DDC/CI et backlight.
//!
//! Par défaut le diagnostic est **en lecture seule** : sysfs, nœuds `/dev`,
//! `/proc`, `/etc`. Il ouvre les nœuds `/dev/i2c-N` pour tester les droits
//! mais n'envoie aucune trame. La sonde DDC (vraies trames « Get VCP » sur
//! les bus des écrans connectés) n'a lieu qu'avec `probe = true`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ha2lux_core::DisplayBackend;
use ha2lux_core::hw::level_from_raw;
use ha2lux_i18n::t;
use serde::Serialize;

use crate::backlight;
use crate::ddc::{DdcBackend, DdcTimings};
use crate::discovery::{BusSource, discover_external, list_adapters};
use crate::drm::{self, Connector};
use crate::i2c::{BusLocks, I2cOpener, LinuxI2cOpener};

/// Gravité d'un contrôle, de la moins à la plus sévère.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Tout va bien.
    Ok,
    /// Information.
    Info,
    /// Anomalie probable, sans blocage certain.
    Warn,
    /// Blocage : la fonction ne marchera pas.
    Fail,
}

/// Résultat d'un contrôle.
#[derive(Clone, Debug, Serialize)]
pub struct Check {
    /// Identifiant stable.
    pub id: String,
    /// Gravité.
    pub status: Status,
    /// Constat.
    pub summary: String,
    /// Remède proposé (jamais exécuté par ha2lux).
    pub hint: Option<String>,
}

/// EDID résumé.
#[derive(Clone, Debug, Serialize)]
pub struct EdidReport {
    /// Fabricant (code PNP).
    pub manufacturer: String,
    /// Code produit.
    pub product: u16,
    /// Série numérique.
    pub serial: u32,
    /// Nom du moniteur.
    pub name: Option<String>,
    /// Identité stable retenue par ha2lux.
    pub id: String,
    /// Somme de contrôle correcte.
    pub checksum_ok: bool,
}

/// Un connecteur DRM.
#[derive(Clone, Debug, Serialize)]
pub struct ConnectorReport {
    /// Nom (ex. `DP-1`).
    pub name: String,
    /// Écran branché.
    pub connected: bool,
    /// Panneau interne.
    pub internal: bool,
    /// Bus I²C exposé.
    pub ddc_bus: Option<u32>,
    /// Droits d'accès au nœud du bus (`None` si pas de bus).
    pub bus_access: Option<bool>,
    /// EDID.
    pub edid: Option<EdidReport>,
    /// Remarques.
    pub notes: Vec<String>,
}

/// Un rétroéclairage.
#[derive(Clone, Debug, Serialize)]
pub struct BacklightReport {
    /// Nom sysfs.
    pub name: String,
    /// Type déclaré.
    pub kind: String,
    /// `max_brightness`.
    pub max: u32,
    /// Valeur courante.
    pub current: Option<u32>,
}

/// Un adaptateur I²C.
#[derive(Clone, Debug, Serialize)]
pub struct BusReport {
    /// Numéro `N` de `/dev/i2c-N`.
    pub bus: u32,
    /// Nom déclaré par le noyau.
    pub name: Option<String>,
    /// Le nœud peut être ouvert par cet utilisateur.
    pub accessible: bool,
    /// Bus DDC d'une carte graphique (le seul type sondé en repli).
    pub gpu_ddc: bool,
}

/// Résultat d'une sonde DDC.
#[derive(Clone, Debug, Serialize)]
pub struct ProbeReport {
    /// Connecteur.
    pub connector: String,
    /// Identité de l'écran.
    pub id: String,
    /// Bus I²C.
    pub bus: u32,
    /// Origine de l'association écran ↔ bus : `drm` ou `edid-i2c`.
    pub bus_source: String,
    /// Sonde réussie.
    pub ok: bool,
    /// Maximum VCP 0x10.
    pub max: Option<u32>,
    /// Valeur courante.
    pub current: Option<u32>,
    /// Niveau 0-100.
    pub level: Option<u8>,
    /// Erreur lisible.
    pub error: Option<String>,
}

/// Rapport complet.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    /// Contrôles.
    pub checks: Vec<Check>,
    /// Connecteurs DRM.
    pub connectors: Vec<ConnectorReport>,
    /// Adaptateurs I²C.
    pub buses: Vec<BusReport>,
    /// Rétroéclairages.
    pub backlights: Vec<BacklightReport>,
    /// Sondes DDC (vide sans `--probe`).
    pub probes: Vec<ProbeReport>,
}

impl Report {
    /// Gravité maximale.
    pub fn worst(&self) -> Status {
        self.checks
            .iter()
            .map(|c| c.status)
            .max()
            .unwrap_or(Status::Ok)
    }

    /// Code de sortie : 3 s'il y a au moins un blocage, sinon 0.
    pub fn exit_code(&self) -> u8 {
        if self.worst() == Status::Fail { 3 } else { 0 }
    }

    /// Rendu JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Rendu texte pour le terminal.
    pub fn render_text(&self) -> String {
        let mut out = t!("doctor-title") + "\n\n";
        for check in &self.checks {
            let tag = match check.status {
                Status::Ok => " OK ",
                Status::Info => "INFO",
                Status::Warn => "WARN",
                Status::Fail => "FAIL",
            };
            out.push_str(&t!(
                "doctor-check-line",
                tag = tag,
                id = check.id.as_str(),
                summary = check.summary.as_str()
            ));
            out.push('\n');
            if let Some(hint) = &check.hint {
                for line in hint.lines() {
                    out.push_str(&format!("       → {line}\n"));
                }
            }
        }
        if !self.connectors.is_empty() {
            out.push('\n');
            out.push_str(&t!("doctor-heading-connectors"));
            out.push('\n');
            for c in &self.connectors {
                let kind = if c.internal {
                    t!("doctor-kind-internal")
                } else {
                    t!("doctor-kind-external")
                };
                let state = if c.connected {
                    t!("doctor-state-connected")
                } else {
                    t!("doctor-state-free")
                };
                let bus = c.ddc_bus.map_or_else(
                    || t!("doctor-bus-none"),
                    |b| {
                        let access = match c.bus_access {
                            Some(true) => t!("doctor-access-ok"),
                            Some(false) => t!("doctor-access-denied"),
                            None => "?".to_owned(),
                        };
                        t!("doctor-bus-with-access", bus = b, access = access)
                    },
                );
                out.push_str("  ");
                out.push_str(&t!(
                    "doctor-connector-line",
                    name = c.name.as_str(),
                    state = state,
                    kind = kind,
                    bus = bus
                ));
                out.push('\n');
                if let Some(e) = &c.edid {
                    let key = if e.checksum_ok {
                        "doctor-edid-line"
                    } else {
                        "doctor-edid-line-bad-checksum"
                    };
                    out.push_str("      ");
                    out.push_str(&t!(
                        key,
                        manufacturer = e.manufacturer.as_str(),
                        product = format!("{:04X}", e.product),
                        serial = e.serial.to_string(),
                        name = e.name.as_deref().unwrap_or("?"),
                        id = e.id.as_str()
                    ));
                    out.push('\n');
                }
                for note in &c.notes {
                    out.push_str(&format!("      · {note}\n"));
                }
            }
        }
        if !self.buses.is_empty() {
            out.push('\n');
            out.push_str(&t!("doctor-heading-buses"));
            out.push('\n');
            for b in &self.buses {
                let access = if b.accessible {
                    t!("doctor-access-ok")
                } else {
                    t!("doctor-access-denied")
                };
                let key = if b.gpu_ddc {
                    "doctor-bus-line-gpu"
                } else {
                    "doctor-bus-line"
                };
                out.push_str("  ");
                out.push_str(&t!(
                    key,
                    bus = b.bus.to_string(),
                    name = b.name.as_deref().unwrap_or("?"),
                    access = access
                ));
                out.push('\n');
            }
        }
        if !self.backlights.is_empty() {
            out.push('\n');
            out.push_str(&t!("doctor-heading-backlights"));
            out.push('\n');
            for b in &self.backlights {
                let current = b.current.map_or("?".to_owned(), |c| c.to_string());
                out.push_str("  ");
                out.push_str(&t!(
                    "doctor-backlight-line",
                    name = b.name.as_str(),
                    kind = b.kind.as_str(),
                    max = b.max.to_string(),
                    current = current
                ));
                out.push('\n');
            }
        }
        if !self.probes.is_empty() {
            out.push('\n');
            out.push_str(&t!("doctor-heading-probes"));
            out.push('\n');
            for p in &self.probes {
                out.push_str("  ");
                match (&p.error, p.max, p.current, p.level) {
                    (None, Some(max), Some(current), Some(level)) => out.push_str(&t!(
                        "doctor-probe-line-ok",
                        connector = p.connector.as_str(),
                        id = p.id.as_str(),
                        max = max.to_string(),
                        current = current.to_string(),
                        level = level.to_string(),
                        bus = p.bus.to_string(),
                        source = p.bus_source.as_str()
                    )),
                    _ => out.push_str(&t!(
                        "doctor-probe-line-fail",
                        connector = p.connector.as_str(),
                        reason = p
                            .error
                            .clone()
                            .unwrap_or_else(|| t!("doctor-probe-unknown-reason"))
                    )),
                }
                out.push('\n');
            }
        }
        out
    }
}

/// Réglages du diagnostic.
pub struct DoctorOptions {
    /// Racine du système (`/` en production ; un faux arbre dans les tests).
    pub root: PathBuf,
    /// Envoyer de vraies trames DDC/CI aux écrans connectés.
    pub probe: bool,
    /// Identifiant de la session logind (`XDG_SESSION_ID`).
    pub session_id: Option<String>,
    /// Ouvreur I²C (`None` : réel, sur `<root>/dev`).
    pub opener: Option<Arc<dyn I2cOpener>>,
    /// Délais DDC/CI de la sonde.
    pub timings: DdcTimings,
}

impl DoctorOptions {
    /// Diagnostic du système réel.
    pub fn system(probe: bool) -> Self {
        Self {
            root: PathBuf::from("/"),
            probe,
            session_id: std::env::var("XDG_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            opener: None,
            timings: DdcTimings::standard(),
        }
    }
}

fn check(id: &str, status: Status, summary: impl Into<String>, hint: Option<String>) -> Check {
    Check {
        id: id.to_owned(),
        status,
        summary: summary.into(),
        hint,
    }
}

fn i2c_nodes(root: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(root.join("dev")) else {
        return Vec::new();
    };
    let mut nodes: Vec<u32> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_prefix("i2c-")?.parse().ok())
        .collect();
    nodes.sort_unstable();
    nodes
}

/// Ouvre le nœud en lecture/écriture pour tester les droits. Aucune trame n'est envoyée.
fn can_open(root: &Path, bus: u32) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(format!("dev/i2c-{bus}")))
        .map(drop)
        .map_err(|e| e.to_string())
}

fn find_udev_rule(root: &Path) -> Option<PathBuf> {
    for dir in [
        "etc/udev/rules.d",
        "usr/lib/udev/rules.d",
        "lib/udev/rules.d",
        "run/udev/rules.d",
    ] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let grants = text.contains("uaccess") || text.contains("GROUP=");
            let touches_i2c = text.lines().any(|l| {
                let l = l.trim();
                !l.starts_with('#') && (l.contains("i2c-dev") || l.contains("i2c-[0-9]"))
            });
            if grants && touches_i2c {
                return Some(entry.path());
            }
        }
    }
    None
}

fn user_gids(root: &Path) -> Vec<u32> {
    let Ok(status) = std::fs::read_to_string(root.join("proc/self/status")) else {
        return Vec::new();
    };
    status
        .lines()
        .filter(|l| l.starts_with("Gid:") || l.starts_with("Groups:"))
        .flat_map(|l| {
            l.split_whitespace()
                .skip(1)
                .filter_map(|n| n.parse().ok())
                .collect::<Vec<u32>>()
        })
        .collect()
}

/// `(gid du groupe i2c, l'utilisateur en est membre)` si le groupe existe.
fn i2c_group(root: &Path) -> Option<(u32, bool)> {
    let groups = std::fs::read_to_string(root.join("etc/group")).ok()?;
    let gid: u32 = groups
        .lines()
        .find(|l| l.split(':').next() == Some("i2c"))?
        .split(':')
        .nth(2)?
        .parse()
        .ok()?;
    Some((gid, user_gids(root).contains(&gid)))
}

fn connector_report(c: &Connector, access: Option<bool>) -> ConnectorReport {
    let mut notes = Vec::new();
    if c.connected && !c.internal && c.ddc_bus.is_none() {
        notes.push(t!("doctor-note-no-ddc-bus"));
    }
    if c.connected && c.edid.is_none() {
        notes.push(
            c.edid_problem
                .clone()
                .unwrap_or_else(|| t!("doctor-note-edid-missing")),
        );
    }
    if c.edid.as_ref().is_some_and(|e| !e.checksum_ok) {
        notes.push(t!("doctor-note-edid-checksum"));
    }
    if c.internal && c.connected {
        notes.push(t!("doctor-note-internal-panel"));
    }
    ConnectorReport {
        name: c.name.clone(),
        connected: c.connected,
        internal: c.internal,
        ddc_bus: c.ddc_bus,
        bus_access: access,
        edid: c.edid.as_ref().map(|e| EdidReport {
            manufacturer: e.manufacturer.clone(),
            product: e.product,
            serial: e.serial,
            name: e.name.clone(),
            id: e.identity(&c.name),
            checksum_ok: e.checksum_ok,
        }),
        notes,
    }
}

/// Exécute le diagnostic.
pub async fn run(options: &DoctorOptions) -> Report {
    let root = options.root.as_path();
    let mut report = Report::default();
    let nodes = i2c_nodes(root);
    let connectors = drm::scan(&root.join("sys/class/drm"));

    // 1. Module i2c-dev.
    let module = root.join("sys/module/i2c_dev").exists();
    report.checks.push(if module || !nodes.is_empty() {
        check(
            "module-i2c-dev",
            Status::Ok,
            t!("doctor-module-i2c-dev-ok"),
            None,
        )
    } else {
        check(
            "module-i2c-dev",
            Status::Fail,
            t!("doctor-module-i2c-dev-missing"),
            Some(t!("doctor-module-i2c-dev-missing-hint")),
        )
    });

    // 2. Nœuds.
    report.checks.push(if nodes.is_empty() {
        check(
            "noeuds-i2c",
            Status::Fail,
            t!("doctor-nodes-none"),
            Some(t!("doctor-nodes-none-hint")),
        )
    } else {
        check(
            "noeuds-i2c",
            Status::Ok,
            t!("doctor-nodes-ok", count = nodes.len().to_string()),
            None,
        )
    });

    // 3. Droits d'accès (ouverture seule, aucune trame).
    let access: std::collections::BTreeMap<u32, Result<(), String>> =
        nodes.iter().map(|b| (*b, can_open(root, *b))).collect();
    let ddc_buses: Vec<(String, u32)> = connectors
        .iter()
        .filter(|c| c.connected && !c.internal)
        .filter_map(|c| Some((c.name.clone(), c.ddc_bus?)))
        .collect();
    let denied: Vec<String> = ddc_buses
        .iter()
        .filter_map(|(name, bus)| match access.get(bus) {
            Some(Ok(())) => None,
            Some(Err(e)) => Some(t!(
                "doctor-access-denied-item",
                name = name.as_str(),
                bus = *bus,
                error = e.as_str()
            )),
            None => Some(t!(
                "doctor-access-denied-absent",
                name = name.as_str(),
                bus = *bus
            )),
        })
        .collect();
    let accessible = access.values().filter(|r| r.is_ok()).count();
    let access_hint = t!("doctor-access-hint");
    report.checks.push(if !denied.is_empty() {
        check(
            "acces-i2c",
            Status::Fail,
            t!("doctor-access-denied-buses", list = denied.join(" ; ")),
            Some(access_hint),
        )
    } else if !nodes.is_empty() && accessible == 0 {
        check(
            "acces-i2c",
            Status::Warn,
            t!("doctor-access-none-accessible"),
            Some(access_hint),
        )
    } else if nodes.is_empty() {
        check(
            "acces-i2c",
            Status::Info,
            t!("doctor-access-not-evaluated"),
            None,
        )
    } else {
        check(
            "acces-i2c",
            Status::Ok,
            t!(
                "doctor-access-summary",
                accessible = accessible.to_string(),
                total = nodes.len().to_string()
            ),
            None,
        )
    });

    // 4. Règle udev.
    report.checks.push(match find_udev_rule(root) {
        Some(path) => check(
            "regle-udev",
            Status::Ok,
            t!("doctor-udev-ok", path = path.display().to_string()),
            None,
        ),
        None => check(
            "regle-udev",
            Status::Warn,
            t!("doctor-udev-missing"),
            Some(t!("doctor-udev-missing-hint")),
        ),
    });

    // 5. Groupe i2c.
    report.checks.push(match i2c_group(root) {
        Some((gid, member)) => check(
            "groupe-i2c",
            Status::Info,
            if member {
                t!("doctor-group-member", gid = gid.to_string())
            } else {
                t!("doctor-group-not-member", gid = gid.to_string())
            },
            None,
        ),
        None => check("groupe-i2c", Status::Info, t!("doctor-group-none"), None),
    });

    // 6. Pilote noyau ddcci.
    if root.join("sys/module/ddcci").exists() || root.join("sys/module/ddcci_backlight").exists() {
        report.checks.push(check(
            "pilote-ddcci",
            Status::Warn,
            t!("doctor-ddcci"),
            Some(t!("doctor-ddcci-hint")),
        ));
    }

    // 7. NVIDIA propriétaire.
    if root.join("proc/driver/nvidia").exists() {
        report.checks.push(check(
            "pilote-nvidia",
            Status::Info,
            t!("doctor-nvidia"),
            Some(t!("doctor-nvidia-hint")),
        ));
    }

    // 8-9. Connecteurs DRM.
    let externals: Vec<&Connector> = connectors
        .iter()
        .filter(|c| c.connected && !c.internal)
        .collect();
    report.checks.push(if connectors.is_empty() {
        check("drm", Status::Warn, t!("doctor-drm-none"), None)
    } else {
        check(
            "drm",
            Status::Ok,
            t!(
                "doctor-drm-ok",
                connectors = connectors.len().to_string(),
                externals = externals.len().to_string()
            ),
            None,
        )
    });
    let gpu_buses = list_adapters(&root.join("sys/class/i2c-dev"))
        .iter()
        .filter(|a| a.is_gpu_ddc())
        .count();
    let without_bus: Vec<&str> = externals
        .iter()
        .filter(|c| c.ddc_bus.is_none())
        .map(|c| c.name.as_str())
        .collect();
    if !without_bus.is_empty() {
        report.checks.push(check(
            "ddc-connecteurs",
            Status::Warn,
            t!("doctor-ddc-connectors", list = without_bus.join(", ")),
            Some(if gpu_buses > 0 {
                t!("doctor-ddc-connectors-hint-gpu")
            } else {
                t!("doctor-ddc-connectors-hint-dock")
            }),
        ));
    }
    let without_edid: Vec<&str> = externals
        .iter()
        .filter(|c| c.edid.is_none())
        .map(|c| c.name.as_str())
        .collect();
    if !without_edid.is_empty() {
        report.checks.push(check(
            "edid",
            Status::Warn,
            t!("doctor-edid", list = without_edid.join(", ")),
            Some(t!("doctor-edid-hint")),
        ));
    }
    report.connectors = connectors
        .iter()
        .map(|c| {
            connector_report(
                c,
                c.ddc_bus.map(|b| access.get(&b).is_some_and(Result::is_ok)),
            )
        })
        .collect();

    // Adaptateurs I²C (noms lus dans sysfs, lecture seule).
    let adapters = list_adapters(&root.join("sys/class/i2c-dev"));
    report.buses = nodes
        .iter()
        .map(|bus| {
            let adapter = adapters.iter().find(|a| a.bus == *bus);
            BusReport {
                bus: *bus,
                name: adapter.map(|a| a.name.clone()),
                accessible: access.get(bus).is_some_and(Result::is_ok),
                gpu_ddc: adapter.is_some_and(crate::discovery::I2cAdapter::is_gpu_ddc),
            }
        })
        .collect();

    // 10. Backlight.
    let backlights = backlight::scan(&root.join("sys/class/backlight")).await;
    let has_internal = connectors.iter().any(|c| c.internal && c.connected);
    for device in &backlights {
        let current = tokio::fs::read_to_string(device.path.join("actual_brightness"))
            .await
            .or(tokio::fs::read_to_string(device.path.join("brightness")).await)
            .ok()
            .and_then(|t| t.trim().parse().ok());
        report.backlights.push(BacklightReport {
            name: device.name.clone(),
            kind: format!("{:?}", device.kind).to_lowercase(),
            max: device.max,
            current,
        });
    }
    report
        .checks
        .push(match (backlights.is_empty(), has_internal) {
            (false, _) => check(
                "backlight",
                Status::Ok,
                t!("doctor-backlight-ok", count = backlights.len().to_string()),
                None,
            ),
            (true, true) => check(
                "backlight",
                Status::Warn,
                t!("doctor-backlight-missing"),
                Some(t!("doctor-backlight-missing-hint")),
            ),
            (true, false) => check(
                "backlight",
                Status::Info,
                t!("doctor-backlight-no-internal"),
                None,
            ),
        });

    // 11. Session logind (nécessaire à l'écriture du backlight).
    if !backlights.is_empty() {
        report.checks.push(match &options.session_id {
            Some(id) => check(
                "session-logind",
                Status::Ok,
                t!("doctor-session-ok", id = id.as_str()),
                None,
            ),
            None => check(
                "session-logind",
                Status::Warn,
                t!("doctor-session-missing"),
                Some(t!("doctor-session-missing-hint")),
            ),
        });
    }

    // 12. Sonde DDC (vraies trames) uniquement sur demande.
    if options.probe {
        let opener: Arc<dyn I2cOpener> = options
            .opener
            .clone()
            .unwrap_or_else(|| Arc::new(LinuxI2cOpener::with_dev_dir(root.join("dev"))));
        let locks = BusLocks::new();
        let discovery = {
            let (opener, root) = (Arc::clone(&opener), options.root.clone());
            tokio::task::spawn_blocking(move || discover_external(&root, opener.as_ref(), true))
                .await
                .unwrap_or_default()
        };
        for note in &discovery.notes {
            report
                .checks
                .push(check("association-edid", Status::Warn, note.clone(), None));
        }
        for (candidate, source) in discovery.candidates {
            let mut probe = ProbeReport {
                connector: candidate.connector.clone(),
                id: candidate.info.id.to_string(),
                bus: candidate.bus,
                bus_source: match source {
                    BusSource::Drm => "drm",
                    BusSource::EdidOverI2c => "edid-i2c",
                }
                .to_owned(),
                ok: false,
                max: None,
                current: None,
                level: None,
                error: None,
            };
            if !access.get(&candidate.bus).is_some_and(Result::is_ok) {
                probe.error = Some(t!("doctor-probe-node-inaccessible"));
                report.probes.push(probe);
                continue;
            }
            let mut backend = DdcBackend::new(
                candidate.info.clone(),
                candidate.bus,
                Arc::clone(&opener),
                &locks,
                options.timings,
            );
            let outcome = tokio::time::timeout(Duration::from_secs(10), async {
                let info = backend.probe().await?;
                let current = backend.read().await?;
                Ok::<_, ha2lux_core::BackendError>((info.max, current))
            })
            .await;
            match outcome {
                Ok(Ok((max, current))) => {
                    probe.ok = true;
                    probe.max = Some(max);
                    probe.current = Some(current);
                    probe.level = Some(level_from_raw(current, max));
                }
                Ok(Err(error)) => probe.error = Some(error.to_string()),
                Err(_) => probe.error = Some(t!("doctor-probe-timeout", seconds = 10)),
            }
            report.probes.push(probe);
        }
        let failed: Vec<&str> = report
            .probes
            .iter()
            .filter(|p| !p.ok)
            .map(|p| p.connector.as_str())
            .collect();
        report.checks.push(if report.probes.is_empty() {
            check("sonde-ddc", Status::Info, t!("doctor-probe-none"), None)
        } else if failed.is_empty() {
            check(
                "sonde-ddc",
                Status::Ok,
                t!("doctor-probe-ok", count = report.probes.len().to_string()),
                None,
            )
        } else {
            check(
                "sonde-ddc",
                Status::Fail,
                t!("doctor-probe-failed", list = failed.join(", ")),
                Some(t!("doctor-probe-failed-hint")),
            )
        });
    } else {
        report.checks.push(check(
            "sonde-ddc",
            Status::Info,
            t!("doctor-probe-not-run"),
            Some(t!("doctor-probe-not-run-hint")),
        ));
    }
    report
}
