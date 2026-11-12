//! Affichage du niveau par une **notification remplaçable** (`org.freedesktop.Notifications`).
//!
//! On montre le niveau **maître** (pas celui d'un écran, qui se bloque en bout de plage).
//! La notification est remplacée à chaque cran : pas d'empilement.

use std::collections::HashMap;

use zbus::zvariant::Value;

/// Durée d'affichage, en millisecondes.
pub const EXPIRE_MS: i32 = 900;
/// Clé de synchronisation : les serveurs qui ne gèrent pas `replaces_id` la respectent.
pub const SYNC_KEY: &str = "ha2lux-osd";

/// Proxy minimal du serveur de notifications.
pub mod proxy {
    #![allow(missing_docs)]

    use std::collections::HashMap;

    use zbus::zvariant::Value;

    #[zbus::proxy(
        interface = "org.freedesktop.Notifications",
        default_service = "org.freedesktop.Notifications",
        default_path = "/org/freedesktop/Notifications"
    )]
    pub trait Notifications {
        #[allow(clippy::too_many_arguments)]
        fn notify(
            &self,
            app_name: &str,
            replaces_id: u32,
            app_icon: &str,
            summary: &str,
            body: &str,
            actions: &[&str],
            hints: HashMap<&str, Value<'_>>,
            expire_timeout: i32,
        ) -> zbus::Result<u32>;
    }
}

/// Indications jointes à la notification : barre de progression, remplacement, discrétion.
pub fn hints(level: u8) -> HashMap<&'static str, Value<'static>> {
    let mut hints = HashMap::new();
    hints.insert("value", Value::I32(i32::from(level.min(100))));
    hints.insert("x-canonical-private-synchronous", Value::from(SYNC_KEY));
    hints.insert("transient", Value::Bool(true));
    hints.insert("urgency", Value::U8(0));
    hints
}

/// Texte de l'OSD.
pub fn summary(level: u8) -> String {
    ha2lux_i18n::t!("tray-osd", level = level.to_string())
}

/// Affichage de l'OSD.
pub struct Osd {
    proxy: proxy::NotificationsProxy<'static>,
    last_id: u32,
}

impl Osd {
    /// Crée l'OSD sur cette connexion.
    pub async fn new(connection: &zbus::Connection) -> zbus::Result<Self> {
        Ok(Self {
            proxy: proxy::NotificationsProxy::new(connection).await?,
            last_id: 0,
        })
    }

    /// Affiche (ou remplace) la notification. Une erreur n'a pas de conséquence : l'OSD est un confort.
    pub async fn show(&mut self, level: u8) {
        match self
            .proxy
            .notify(
                "ha2lux",
                self.last_id,
                "display-brightness-symbolic",
                &summary(level),
                "",
                &[],
                hints(level),
                EXPIRE_MS,
            )
            .await
        {
            Ok(id) => self.last_id = id,
            Err(error) => tracing::debug!(%error, "notification de niveau impossible"),
        }
    }
}
