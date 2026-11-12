//! System event sources: display hotplug (netlink, fallback by polling
//! sysfs) and logind (sleep, wake-up, lock, active session).
//!
//! Each source translates what it sees into a [`SystemEvent`] and sends it to the actor,
//! which decides (debounce, floor, re-application). No source acts on the
//! hardware itself.

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

/// Kernel event (uevent), as read on the netlink socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uevent {
    /// `add`, `remove`, `change`...
    pub action: String,
    /// Device path in `/sys`.
    pub devpath: String,
    /// Subsystem (`drm`, `i2c-dev`...), if provided.
    pub subsystem: Option<String>,
    /// Other `KEY=VALUE` variables.
    pub vars: Vec<(String, String)>,
}

impl Uevent {
    /// Value of a variable.
    pub fn var(&self, key: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Parses a kernel message: `action@devpath\0KEY=VALUE\0...`. Messages that
/// do not follow this format (including binary `libudev` messages) are ignored.
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

/// True if the event concerns displays: DRM connector change
/// (`HOTPLUG=1`, or add/remove) or appearance/disappearance of an `i2c-dev` node.
pub fn is_display_event(event: &Uevent) -> bool {
    match event.subsystem.as_deref() {
        Some("drm") => {
            event.var("HOTPLUG") == Some("1") || matches!(event.action.as_str(), "add" | "remove")
        }
        Some("i2c-dev") => matches!(event.action.as_str(), "add" | "remove"),
        _ => false,
    }
}

/// Listens to kernel events (netlink group 1) and signals each display
/// change. Only returns on a socket error.
pub async fn watch_uevents(handle: DaemonHandle) -> io::Result<()> {
    let mut socket = TokioSocket::new(NETLINK_KOBJECT_UEVENT)?;
    // Group 1: events emitted by the kernel itself (group 2 is udev's).
    socket.socket_mut().bind(&SocketAddr::new(0, 1))?;
    loop {
        let (buffer, sender) = socket.recv_from_full().await?;
        // A legitimate group 1 message comes from the kernel (port 0); the rest is ignored.
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

/// Fingerprint of the connectors' state: status, EDID, DDC bus.
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

/// Fallback when netlink is unavailable: periodically polls sysfs and signals a
/// connector change. Ends when the daemon stops.
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

/// Watches logind on `connection` (system bus): sleep preparation and resume,
/// lock/unlock, session active or not. Only returns if the
/// daemon stops or a bus error occurs.
pub async fn watch_logind(
    connection: zbus::Connection,
    session_id: Option<String>,
    handle: DaemonHandle,
) -> Result<(), zbus::Error> {
    let manager = LoginManagerProxy::new(&connection).await?;
    let mut sleep = manager.receive_prepare_for_sleep().await?;

    // The session may be missing (systemd service without a graphical session): the
    // sleep watching is then kept, without lock or session-change handling.
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
