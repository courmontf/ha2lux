//! Sources d'événements système : hotplug d'écrans (netlink, repli par sondage de
//! sysfs) et logind (veille, réveil, verrouillage, session active).
//!
//! Chaque source traduit ce qu'elle voit en [`SystemEvent`] et l'envoie à l'acteur,
//! qui décide (débounce, plancher, ré-application). Aucune source n'agit sur le
//! matériel elle-même.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use ha2lux_backends::backlight::resolve_session_path;
use ha2lux_backends::drm;
use netlink_sys::protocols::NETLINK_KOBJECT_UEVENT;
use netlink_sys::{AsyncSocket, AsyncSocketExt, SocketAddr, TokioSocket};

use crate::actor::{DaemonHandle, SystemEvent};

/// Événement du noyau (uevent), tel que lu sur le socket netlink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uevent {
    /// `add`, `remove`, `change`…
    pub action: String,
    /// Chemin de l'appareil dans `/sys`.
    pub devpath: String,
    /// Sous-système (`drm`, `i2c-dev`…), s'il est fourni.
    pub subsystem: Option<String>,
    /// Autres variables `CLÉ=VALEUR`.
    pub vars: Vec<(String, String)>,
}

impl Uevent {
    /// Valeur d'une variable.
    pub fn var(&self, key: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Analyse un message du noyau : `action@devpath\0CLÉ=VALEUR\0…`. Les messages qui
/// ne respectent pas ce format (dont les messages `libudev` binaires) sont ignorés.
pub fn parse_uevent(buffer: &[u8]) -> Option<Uevent> {
    let mut parts = buffer.split(|b| *b == 0).filter(|p| !p.is_empty());
    let head = std::str::from_utf8(parts.next()?).ok()?;
    let (action, devpath) = head.split_once('@')?;
    if action.is_empty()
        || !devpath.starts_with('/')
        || !action
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let mut event = Uevent {
        action: action.to_owned(),
        devpath: devpath.to_owned(),
        subsystem: None,
        vars: Vec::new(),
    };
    for part in parts {
        let Ok(text) = std::str::from_utf8(part) else {
            continue;
        };
        let Some((key, value)) = text.split_once('=') else {
            continue;
        };
        if key == "SUBSYSTEM" {
            event.subsystem = Some(value.to_owned());
        } else {
            event.vars.push((key.to_owned(), value.to_owned()));
        }
    }
    Some(event)
}

/// Vrai si l'événement concerne les écrans : changement de connecteur DRM
/// (`HOTPLUG=1`, ou ajout/retrait) ou apparition/disparition d'un nœud `i2c-dev`.
pub fn is_display_event(event: &Uevent) -> bool {
    match event.subsystem.as_deref() {
        Some("drm") => {
            event.var("HOTPLUG") == Some("1") || matches!(event.action.as_str(), "add" | "remove")
        }
        Some("i2c-dev") => matches!(event.action.as_str(), "add" | "remove"),
        _ => false,
    }
}

/// Écoute les événements du noyau (groupe netlink 1) et signale chaque changement
/// d'écran. Ne rend la main qu'en cas d'erreur du socket.
pub async fn watch_uevents(handle: DaemonHandle) -> io::Result<()> {
    let mut socket = TokioSocket::new(NETLINK_KOBJECT_UEVENT)?;
    // Groupe 1 : événements émis par le noyau lui-même (le groupe 2 est celui d'udev).
    socket.socket_mut().bind(&SocketAddr::new(0, 1))?;
    loop {
        let (buffer, sender) = socket.recv_from_full().await?;
        // Un message légitime du groupe 1 vient du noyau (port 0) ; on ignore le reste.
        if sender.port_number() != 0 {
            continue;
        }
        if let Some(event) = parse_uevent(&buffer)
            && is_display_event(&event)
            && handle.send_system(SystemEvent::Hotplug).await.is_err()
        {
            return Ok(()); // the daemon is stopped
        }
    }
}

/// Empreinte de l'état des connecteurs : statut, EDID, bus DDC.
pub fn connector_signature(drm_dir: &std::path::Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    for connector in drm::scan(drm_dir) {
        connector.name.hash(&mut hasher);
        connector.connected.hash(&mut hasher);
        connector.ddc_bus.hash(&mut hasher);
        connector
            .edid
            .as_ref()
            .map(|e| e.fingerprint)
            .hash(&mut hasher);
    }
    hasher.finish()
}

/// Repli quand netlink est indisponible : sonde périodiquement sysfs et signale un
/// changement des connecteurs. Se termine quand le démon s'arrête.
pub async fn poll_connectors(drm_dir: PathBuf, handle: DaemonHandle, period: Duration) {
    let scan = |dir: PathBuf| async move {
        tokio::task::spawn_blocking(move || connector_signature(&dir))
            .await
            .ok()
    };
    let mut last = scan(drm_dir.clone()).await;
    let mut interval = tokio::time::interval(period);
    interval.tick().await;
    loop {
        interval.tick().await;
        let now = scan(drm_dir.clone()).await;
        if now != last {
            last = now;
            if handle.send_system(SystemEvent::Hotplug).await.is_err() {
                return;
            }
        }
    }
}

mod proxies {
    #![allow(missing_docs)]

    #[zbus::proxy(
        interface = "org.freedesktop.login1.Manager",
        default_service = "org.freedesktop.login1",
        default_path = "/org/freedesktop/login1"
    )]
    pub trait LoginManager {
        #[zbus(signal)]
        fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
    }

    #[zbus::proxy(
        interface = "org.freedesktop.login1.Session",
        default_service = "org.freedesktop.login1"
    )]
    pub trait LoginSession {
        #[zbus(signal)]
        fn lock(&self) -> zbus::Result<()>;
        #[zbus(signal)]
        fn unlock(&self) -> zbus::Result<()>;
        #[zbus(property)]
        fn active(&self) -> zbus::Result<bool>;
    }
}

use proxies::{LoginManagerProxy, LoginSessionProxy};

/// Surveille logind sur `connection` (bus système) : préparation de veille et retour,
/// verrouillage/déverrouillage, session active ou non. Ne rend la main que si le
/// démon s'arrête ou qu'une erreur de bus survient.
pub async fn watch_logind(
    connection: zbus::Connection,
    session_id: Option<String>,
    handle: DaemonHandle,
) -> Result<(), zbus::Error> {
    let manager = LoginManagerProxy::new(&connection).await?;
    let mut sleep = manager.receive_prepare_for_sleep().await?;

    // La session peut être introuvable (service systemd sans session graphique) : on
    // garde alors la surveillance de la veille, sans verrouillage ni changement de session.
    let session = match resolve_session_path(&connection, session_id.as_deref()).await {
        Ok(path) => Some(
            LoginSessionProxy::builder(&connection)
                .path(path)?
                .build()
                .await?,
        ),
        Err(error) => {
            tracing::warn!(%error, "logind session not found: lock and active session not monitored");
            None
        }
    };
    let mut lock = match &session {
        Some(s) => Some(s.receive_lock().await?),
        None => None,
    };
    let mut unlock = match &session {
        Some(s) => Some(s.receive_unlock().await?),
        None => None,
    };
    let mut active = match &session {
        Some(s) => Some(s.receive_active_changed().await),
        None => None,
    };
    tracing::info!(session = session.is_some(), "surveillance logind active");

    macro_rules! next {
        ($stream:expr) => {
            async {
                match $stream.as_mut() {
                    Some(stream) => stream.next().await,
                    None => std::future::pending().await,
                }
            }
        };
    }
    loop {
        let event = tokio::select! {
            Some(signal) = sleep.next() => match signal.args() {
                Ok(args) if args.start => SystemEvent::Suspend,
                Ok(_) => SystemEvent::Resume,
                Err(error) => {
                    tracing::debug!(%error, "signal PrepareForSleep illisible");
                    continue;
                }
            },
            Some(_) = next!(lock) => SystemEvent::Lock,
            Some(_) = next!(unlock) => SystemEvent::Unlock,
            Some(change) = next!(active) => match change.get().await {
                Ok(is_active) => SystemEvent::SessionActive(is_active),
                Err(error) => {
                    tracing::debug!(%error, "Active property unreadable");
                    continue;
                }
            },
            else => return Ok(()),
        };
        if handle.send_system(event).await.is_err() {
            return Ok(()); // the daemon is stopped
        }
    }
}
