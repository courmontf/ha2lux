//! Blocages de l'atténuation à l'inactivité (spec §7.3) : inhibiteurs d'inactivité
//! (lecteur vidéo, présentation, visioconférence), microphone et caméra.
//!
//! Chaque sonde est **tolérante** : un service absent (pas de GNOME, pas de PipeWire,
//! pas de logind) n'est pas une erreur, il n'apporte simplement aucune information.
//! Chaque sonde est bornée dans le temps.
//!
//! Le plein écran n'est pas détecté : Wayland n'offre pas de moyen générique de le
//! savoir. Les lecteurs vidéo passent par les inhibiteurs d'inactivité, que le
//! compositeur et logind relaient.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::idle::{BlockReport, Blockers};

/// Délai maximal d'une sonde.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1_500);
/// Taille maximale acceptée pour la sortie de `pw-dump`.
const MAX_PW_DUMP: usize = 8 * 1024 * 1024;

type Inhibitor = (String, String, String, String, u32, u32);

/// Inhibiteurs logind à prendre en compte : `idle` dans `what`, mode `block`, hors
/// processus `own_pid` (ha2lux lui-même). Les inhibiteurs `sleep`, `shutdown`,
/// `handle-*-key` (ex. PowerDevil) ou en mode `delay` n'empêchent pas d'atténuer.
pub fn parse_logind_inhibitors(list: &[Inhibitor], own_pid: u32) -> Vec<String> {
    list.iter()
        .filter(|(what, _, _, mode, _, pid)| {
            *pid != own_pid && mode == "block" && what.split(':').any(|w| w == "idle")
        })
        .map(|(_, who, why, _, _, _)| {
            if why.is_empty() {
                format!("inhibiteur d'inactivité : {who}")
            } else {
                format!("inhibiteur d'inactivité : {who} ({why})")
            }
        })
        .collect()
}

fn prop<'a>(props: &'a Value, key: &str) -> Option<&'a str> {
    props.get(key).and_then(Value::as_str)
}

fn truthy(props: &Value, key: &str) -> bool {
    match props.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true" || s == "1",
        _ => false,
    }
}

/// Analyse la sortie de `pw-dump` : microphone ou caméra utilisés quand un nœud de
/// capture (`Audio/Source`, `Video/Source`, `Stream/Input/Audio`, `Stream/Input/Video`)
/// est à l'état `running`. Les flux qui n'écoutent qu'un *moniteur* de sortie (vumètres)
/// ne comptent pas.
pub fn parse_pw_dump(json: &str) -> Vec<String> {
    let Ok(Value::Array(objects)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let (mut microphone, mut camera) = (false, false);
    for object in &objects {
        if object.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(info) = object.get("info") else {
            continue;
        };
        if info.get("state").and_then(Value::as_str) != Some("running") {
            continue;
        }
        let Some(props) = info.get("props") else {
            continue;
        };
        if truthy(props, "stream.capture.sink") || prop(props, "media.category") == Some("Monitor")
        {
            continue;
        }
        match prop(props, "media.class") {
            Some("Audio/Source" | "Stream/Input/Audio") => microphone = true,
            Some("Video/Source" | "Stream/Input/Video") => camera = true,
            _ => {}
        }
    }
    let mut reasons = Vec::new();
    if microphone {
        reasons.push("microphone en cours d'utilisation".to_owned());
    }
    if camera {
        reasons.push("caméra en cours d'utilisation".to_owned());
    }
    reasons
}

/// Processus (hors `own_pid`) qui ont un nœud `/dev/video*` ouvert : caméra utilisée
/// directement (V4L2), sans passer par PipeWire. Lecture seule de `<proc>/<pid>/fd`.
pub fn scan_video_users(proc_dir: &Path, own_pid: u32) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(proc_dir) else {
        return Vec::new();
    };
    let mut users = Vec::new();
    for entry in entries.flatten().take(8_192) {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue; // processus d'un autre utilisateur ou terminé
        };
        let uses_camera = fds
            .flatten()
            .filter_map(|fd| std::fs::read_link(fd.path()).ok())
            .any(|target| target.to_str().is_some_and(|t| t.starts_with("/dev/video")));
        if uses_camera {
            let name = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
            users.push(format!("caméra ouverte par {}", name.trim()));
        }
    }
    users.sort();
    users.dedup();
    users
}

mod proxies {
    #![allow(missing_docs)]

    #[zbus::proxy(
        interface = "org.freedesktop.login1.Manager",
        default_service = "org.freedesktop.login1",
        default_path = "/org/freedesktop/login1"
    )]
    pub trait LogindInhibitors {
        fn list_inhibitors(&self) -> zbus::Result<Vec<(String, String, String, String, u32, u32)>>;
    }

    #[zbus::proxy(
        interface = "org.freedesktop.PowerManagement.Inhibit",
        default_service = "org.freedesktop.PowerManagement.Inhibit",
        default_path = "/org/freedesktop/PowerManagement/Inhibit"
    )]
    pub trait PowerInhibit {
        fn has_inhibit(&self) -> zbus::Result<bool>;
    }

    #[zbus::proxy(
        interface = "org.gnome.SessionManager",
        default_service = "org.gnome.SessionManager",
        default_path = "/org/gnome/SessionManager"
    )]
    pub trait GnomeSession {
        fn is_inhibited(&self, flags: u32) -> zbus::Result<bool>;
    }
}

use proxies::{GnomeSessionProxy, LogindInhibitorsProxy, PowerInhibitProxy};

/// Réglages des sondes de blocage.
#[derive(Clone, Debug, Default)]
pub struct BlockerOptions {
    /// Connexion au bus système (inhibiteurs logind).
    pub system: Option<zbus::Connection>,
    /// Connexion au bus de session (inhibiteurs du bureau).
    pub session: Option<zbus::Connection>,
    /// Interroger PipeWire (`pw-dump`) pour le microphone et la caméra.
    pub pipewire: bool,
    /// Répertoire `/proc` à parcourir pour les caméras V4L2 ouvertes.
    pub proc_dir: Option<PathBuf>,
    /// Programme `pw-dump`.
    pub pw_dump: Option<PathBuf>,
}

/// Sondes de blocage réelles.
pub struct SystemBlockers {
    options: BlockerOptions,
    pw_missing: bool,
}

impl SystemBlockers {
    /// Crée les sondes.
    pub fn new(options: BlockerOptions) -> Self {
        Self {
            options,
            pw_missing: false,
        }
    }

    async fn logind(&self) -> Vec<String> {
        let Some(connection) = &self.options.system else {
            return Vec::new();
        };
        let call = async {
            let proxy = LogindInhibitorsProxy::new(connection).await.ok()?;
            proxy.list_inhibitors().await.ok()
        };
        match tokio::time::timeout(PROBE_TIMEOUT, call).await {
            Ok(Some(list)) => parse_logind_inhibitors(&list, std::process::id()),
            _ => Vec::new(),
        }
    }

    async fn desktop(&self) -> Vec<String> {
        let Some(connection) = &self.options.session else {
            return Vec::new();
        };
        let call = async {
            let mut reasons = Vec::new();
            if let Ok(proxy) = PowerInhibitProxy::new(connection).await
                && proxy.has_inhibit().await.unwrap_or(false)
            {
                reasons.push("inhibiteur d'inactivité du bureau (PowerManagement)".to_owned());
            }
            if let Ok(proxy) = GnomeSessionProxy::new(connection).await
                && proxy.is_inhibited(8).await.unwrap_or(false)
            {
                reasons.push("inhibiteur d'inactivité de GNOME".to_owned());
            }
            reasons
        };
        tokio::time::timeout(PROBE_TIMEOUT, call)
            .await
            .unwrap_or_default()
    }

    async fn pipewire(&mut self) -> Vec<String> {
        if !self.options.pipewire || self.pw_missing {
            return Vec::new();
        }
        let program = self
            .options
            .pw_dump
            .clone()
            .unwrap_or_else(|| PathBuf::from("pw-dump"));
        let mut command = tokio::process::Command::new(program);
        command
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(2 * PROBE_TIMEOUT, command.output()).await {
            Ok(Ok(output)) if output.status.success() && output.stdout.len() <= MAX_PW_DUMP => {
                parse_pw_dump(&String::from_utf8_lossy(&output.stdout))
            }
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("pw-dump introuvable : micro et caméra PipeWire non surveillés");
                self.pw_missing = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    async fn video(&self) -> Vec<String> {
        let Some(dir) = self.options.proc_dir.clone() else {
            return Vec::new();
        };
        let own = std::process::id();
        tokio::task::spawn_blocking(move || scan_video_users(&dir, own))
            .await
            .unwrap_or_default()
    }
}

impl Blockers for SystemBlockers {
    async fn probe(&mut self) -> BlockReport {
        let (logind, desktop, video) = tokio::join!(self.logind(), self.desktop(), self.video());
        let pipewire = self.pipewire().await;
        let mut reasons: Vec<String> = logind
            .into_iter()
            .chain(desktop)
            .chain(pipewire)
            .chain(video)
            .collect();
        reasons.dedup();
        BlockReport { reasons }
    }
}
