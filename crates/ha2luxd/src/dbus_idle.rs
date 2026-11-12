//! Repli D-Bus pour l'inactivité : `org.gnome.Mutter.IdleMonitor` (GNOME), seul
//! service dont l'unité (millisecondes) est définie sans ambiguïté.
//!
//! Sur KDE Plasma, `org.freedesktop.ScreenSaver.GetSessionIdleTime` n'est pas pris
//! en charge sous Wayland et son unité varie selon les versions : KDE passe donc
//! par le protocole Wayland (`wayland`), pas par ce module.

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

/// Inactivité de la session lue chez Mutter.
pub struct MutterIdleTime {
    connection: zbus::Connection,
}

impl MutterIdleTime {
    /// Lit l'inactivité par la connexion au bus de session donnée.
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
