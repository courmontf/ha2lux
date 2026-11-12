//! D-Bus fallback for idle detection: `org.gnome.Mutter.IdleMonitor` (GNOME), the only
//! service whose unit (milliseconds) is unambiguously defined.
//!
//! On KDE Plasma, `org.freedesktop.ScreenSaver.GetSessionIdleTime` is not supported
//! under Wayland and its unit varies between versions: KDE therefore goes
//! through the Wayland protocol (`wayland`), not through this module.

use crate::idle::IdleTime;

mod proxies {
    #![allow(missing_docs)]

    #[zbus::proxy(
        interface = "org.gnome.Mutter.IdleMonitor",
        default_service = "org.gnome.Mutter.IdleMonitor",
        default_path = "/org/gnome/Mutter/IdleMonitor/Core"
    )]
    pub trait MutterIdle {
        fn get_idletime(&self) -> zbus::Result<u64>;
    }
}

use proxies::MutterIdleProxy;

/// Session idle time read from Mutter.
pub struct MutterIdleTime {
    connection: zbus::Connection,
}

impl MutterIdleTime {
    /// Reads the idle time through the given session bus connection.
    pub fn new(connection: zbus::Connection) -> Self {
        Self { connection }
    }
}

impl IdleTime for MutterIdleTime {
    async fn idle_ms(&mut self) -> Option<u64> {
        let proxy = MutterIdleProxy::new(&self.connection).await.ok()?;
        proxy.get_idletime().await.ok()
    }
}
