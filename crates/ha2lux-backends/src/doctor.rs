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
        let mut out = String::from("ha2lux doctor\n\n");
        for check in &self.checks {
            let tag = match check.status {
                Status::Ok => " OK ",
                Status::Info => "INFO",
                Status::Warn => "WARN",
                Status::Fail => "FAIL",
            };
            out.push_str(&format!("[{tag}] {} : {}\n", check.id, check.summary));
            if let Some(hint) = &check.hint {
                for line in hint.lines() {
                    out.push_str(&format!("       → {line}\n"));
                }
            }
        }
        if !self.connectors.is_empty() {
            out.push_str("\nConnecteurs DRM\n");
            for c in &self.connectors {
                let kind = if c.internal { "interne" } else { "externe" };
                let state = if c.connected { "connecté" } else { "libre" };
                let bus = c.ddc_bus.map_or("pas de bus DDC".to_owned(), |b| {
                    let access = match c.bus_access {
                        Some(true) => "accès ok",
                        Some(false) => "accès refusé",
                        None => "?",
                    };
                    format!("i2c-{b} ({access})")
                });
                out.push_str(&format!("  {} : {state}, {kind}, {bus}\n", c.name));
                if let Some(e) = &c.edid {
                    out.push_str(&format!(
                        "      EDID : {} {:04X} série {} « {} » → id {}{}\n",
                        e.manufacturer,
                        e.product,
                        e.serial,
                        e.name.as_deref().unwrap_or("?"),
                        e.id,
                        if e.checksum_ok {
                            ""
                        } else {
                            " (somme de contrôle fausse)"
                        },
                    ));
                }
                for note in &c.notes {
                    out.push_str(&format!("      · {note}\n"));
                }
            }
        }
        if !self.buses.is_empty() {
            out.push_str("\nBus I²C\n");
            for b in &self.buses {
                let access = if b.accessible {
                    "accès ok"
                } else {
                    "accès refusé"
                };
                let gpu = if b.gpu_ddc {
                    " [bus DDC de carte graphique]"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "  i2c-{} : {} ({access}){gpu}\n",
                    b.bus,
                    b.name.as_deref().unwrap_or("?")
                ));
            }
        }
        if !self.backlights.is_empty() {
            out.push_str("\nRétroéclairages\n");
            for b in &self.backlights {
                let current = b.current.map_or("?".to_owned(), |c| c.to_string());
                out.push_str(&format!(
                    "  {} : type {}, max {}, courant {current}\n",
                    b.name, b.kind, b.max
                ));
            }
        }
        if !self.probes.is_empty() {
            out.push_str("\nSondes DDC/CI (VCP 0x10)\n");
            for p in &self.probes {
                match (&p.error, p.max, p.current, p.level) {
                    (None, Some(max), Some(current), Some(level)) => out.push_str(&format!(
                        "  {} ({}) : max {max}, courant {current} = {level} % [bus i2c-{} via {}]\n",
                        p.connector, p.id, p.bus, p.bus_source
                    )),
                    _ => out.push_str(&format!(
                        "  {} : ÉCHEC — {}\n",
                        p.connector,
                        p.error.as_deref().unwrap_or("raison inconnue")
                    )),
                }
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

fn check(id: &str, status: Status, summary: impl Into<String>, hint: Option<&str>) -> Check {
    Check {
        id: id.to_owned(),
        status,
        summary: summary.into(),
        hint: hint.map(str::to_owned),
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
        notes.push(
            "aucun bus DDC exposé : écran derrière un dock/MST/adaptateur, ou pilote NVIDIA propriétaire".to_owned(),
        );
    }
    if c.connected && c.edid.is_none() {
        notes.push(
            c.edid_problem
                .clone()
                .unwrap_or_else(|| "EDID absent".to_owned()),
        );
    }
    if c.edid.as_ref().is_some_and(|e| !e.checksum_ok) {
        notes.push("somme de contrôle EDID fausse (fréquent, sans gravité)".to_owned());
    }
    if c.internal && c.connected {
        notes.push("panneau interne : piloté par le backlight, jamais en DDC".to_owned());
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
        check("module-i2c-dev", Status::Ok, "module i2c-dev chargé (ou intégré au noyau)", None)
    } else {
        check(
            "module-i2c-dev",
            Status::Fail,
            "module i2c-dev absent : aucun /dev/i2c-* n'est créé",
            Some("à lancer vous-même : sudo modprobe i2c-dev\nau démarrage : copier packaging/modules-load.d/ha2lux-i2c-dev.conf dans /etc/modules-load.d/"),
        )
    });

    // 2. Nœuds.
    report.checks.push(if nodes.is_empty() {
        check(
            "noeuds-i2c",
            Status::Fail,
            "aucun nœud /dev/i2c-*",
            Some("charger le module i2c-dev, puis relancer ha2lux doctor"),
        )
    } else {
        check(
            "noeuds-i2c",
            Status::Ok,
            format!("{} nœud(s) /dev/i2c-*", nodes.len()),
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
            Some(Err(e)) => Some(format!("{name} (i2c-{bus}) : {e}")),
            None => Some(format!("{name} (i2c-{bus}) : nœud /dev/i2c-{bus} absent")),
        })
        .collect();
    let accessible = access.values().filter(|r| r.is_ok()).count();
    const ACCESS_HINT: &str = "installer packaging/udev/60-ha2lux-i2c.rules dans /etc/udev/rules.d/, puis : sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev\nou ajouter l'utilisateur au groupe i2c et se reconnecter";
    report.checks.push(if !denied.is_empty() {
        check(
            "acces-i2c",
            Status::Fail,
            format!("accès refusé aux bus des écrans : {}", denied.join(" ; ")),
            Some(ACCESS_HINT),
        )
    } else if !nodes.is_empty() && accessible == 0 {
        check(
            "acces-i2c",
            Status::Warn,
            "aucun nœud /dev/i2c-* n'est accessible par cet utilisateur",
            Some(ACCESS_HINT),
        )
    } else if nodes.is_empty() {
        check("acces-i2c", Status::Info, "non évalué : aucun nœud", None)
    } else {
        check(
            "acces-i2c",
            Status::Ok,
            format!("{accessible}/{} nœud(s) accessible(s)", nodes.len()),
            None,
        )
    });

    // 4. Règle udev.
    report.checks.push(match find_udev_rule(root) {
        Some(path) => check("regle-udev", Status::Ok, format!("règle udev trouvée : {}", path.display()), None),
        None => check(
            "regle-udev",
            Status::Warn,
            "aucune règle udev donnant accès à i2c-dev n'a été trouvée",
            Some("fichier fourni : packaging/udev/60-ha2lux-i2c.rules (à installer dans /etc/udev/rules.d/)"),
        ),
    });

    // 5. Groupe i2c.
    report.checks.push(match i2c_group(root) {
        Some((gid, member)) => check(
            "groupe-i2c",
            Status::Info,
            format!(
                "groupe i2c présent (gid {gid}), utilisateur {}",
                if member { "membre" } else { "non membre" }
            ),
            None,
        ),
        None => check(
            "groupe-i2c",
            Status::Info,
            "pas de groupe i2c (l'accès repose sur l'ACL udev « uaccess »)",
            None,
        ),
    });

    // 6. Pilote noyau ddcci.
    if root.join("sys/module/ddcci").exists() || root.join("sys/module/ddcci_backlight").exists() {
        report.checks.push(check(
            "pilote-ddcci",
            Status::Warn,
            "le pilote noyau ddcci est chargé : il peut occuper l'adresse 0x37 et gêner l'accès direct",
            Some("décharger ddcci si l'écriture échoue avec « device busy »"),
        ));
    }

    // 7. NVIDIA propriétaire.
    if root.join("proc/driver/nvidia").exists() {
        report.checks.push(check(
            "pilote-nvidia",
            Status::Info,
            "pilote NVIDIA propriétaire détecté : les bus DDC des connecteurs peuvent ne pas être exposés dans sysfs",
            Some("si un écran externe n'a pas de bus DDC, utiliser les nœuds i2c du pilote NVIDIA"),
        ));
    }

    // 8-9. Connecteurs DRM.
    let externals: Vec<&Connector> = connectors
        .iter()
        .filter(|c| c.connected && !c.internal)
        .collect();
    report.checks.push(if connectors.is_empty() {
        check(
            "drm",
            Status::Warn,
            "aucun connecteur DRM lisible dans /sys/class/drm",
            None,
        )
    } else {
        check(
            "drm",
            Status::Ok,
            format!(
                "{} connecteur(s), {} écran(s) externe(s) connecté(s)",
                connectors.len(),
                externals.len()
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
            format!("écran(s) externe(s) sans bus DDC exposé : {}", without_bus.join(", ")),
            Some(if gpu_buses > 0 {
                "pilote NVIDIA : les bus DDC existent (voir « Bus I²C ») mais ne sont pas liés aux connecteurs ; ha2lux doctor --probe lit l'EDID de chaque bus GPU pour les associer"
            } else {
                "dock, MST, adaptateur DP→HDMI ou DisplayLink : le DDC y est souvent absent ; surcharge possible par écran (ddc = false)"
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
            format!(
                "écran(s) connecté(s) sans EDID lisible : {}",
                without_edid.join(", ")
            ),
            Some("sans EDID, l'écran n'a pas d'identité stable et n'est pas piloté"),
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
    report.checks.push(match (backlights.is_empty(), has_internal) {
        (false, _) => check("backlight", Status::Ok, format!("{} rétroéclairage(s) détecté(s)", backlights.len()), None),
        (true, true) => check(
            "backlight",
            Status::Warn,
            "panneau interne connecté mais aucun rétroéclairage dans /sys/class/backlight",
            Some("certains portables n'en exposent pas (pilote hybride, firmware) : voir acpi_backlight= dans la ligne de commande du noyau"),
        ),
        (true, false) => check("backlight", Status::Info, "aucun panneau interne (poste fixe)", None),
    });

    // 11. Session logind (nécessaire à l'écriture du backlight).
    if !backlights.is_empty() {
        report.checks.push(match &options.session_id {
            Some(id) => check("session-logind", Status::Ok, format!("session logind {id} (écriture du backlight via SetBrightness)"), None),
            None => check(
                "session-logind",
                Status::Warn,
                "pas de XDG_SESSION_ID : le backlight ne sera réglable que depuis une session graphique",
                Some("lancer depuis la session de bureau (pas via SSH) ; un service systemd --user retombe sur la session graphique de l'utilisateur"),
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
                probe.error = Some("nœud inaccessible, sonde ignorée".to_owned());
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
                Err(_) => probe.error = Some("aucune réponse en 10 s".to_owned()),
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
            check("sonde-ddc", Status::Info, "aucun écran externe à sonder", None)
        } else if failed.is_empty() {
            check("sonde-ddc", Status::Ok, format!("{} écran(s) répondent en DDC/CI", report.probes.len()), None)
        } else {
            check(
                "sonde-ddc",
                Status::Fail,
                format!("pas de réponse DDC/CI : {}", failed.join(", ")),
                Some("vérifier que DDC/CI est activé dans le menu OSD de l'écran, le câble, et éviter dock/KVM ; un écran qui dort répond « null message »"),
            )
        });
    } else {
        report.checks.push(check(
            "sonde-ddc",
            Status::Info,
            "sonde DDC non exécutée (elle envoie de vraies trames aux écrans)",
            Some("ajouter --probe pour lire la luminosité (VCP 0x10) de chaque écran externe"),
        ));
    }
    report
}
