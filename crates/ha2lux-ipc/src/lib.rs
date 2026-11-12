//! Interface D-Bus de ha2lux, partagée par le démon, la CLI et le tray.
//!
//! Nom bien connu `org.ha2lux.Daemon`, objet `/org/ha2lux/Daemon`, interface
//! `org.ha2lux.Daemon1`. Toute valeur reçue est **validée par le démon**, jamais
//! seulement par le client.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

/// Nom bien connu du démon sur le bus de session.
pub const BUS_NAME: &str = "org.ha2lux.Daemon";
/// Chemin de l'objet principal.
pub const OBJECT_PATH: &str = "/org/ha2lux/Daemon";
/// Nom de l'interface.
pub const INTERFACE: &str = "org.ha2lux.Daemon1";
/// Préfixe des noms d'erreurs D-Bus.
pub const ERROR_PREFIX: &str = "org.ha2lux.Error";

/// Fonctions activables par `SetMode`.
pub const FEATURE_SCHEDULE: &str = "schedule";
/// Atténuation à l'inactivité.
pub const FEATURE_IDLE_DIM: &str = "idle-dim";
/// Auto-luminosité.
pub const FEATURE_AUTO: &str = "auto";

/// Un écran tel que le décrit `List()`.
///
/// Signature D-Bus : `(ssssiuuu)` ; `level` vaut −1 quand il est inconnu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DisplayDto {
    /// Identité stable (clé EDID).
    pub id: String,
    /// Nom affiché.
    pub name: String,
    /// `internal` ou `external`.
    pub kind: String,
    /// `controllable`, `unavailable`, `not-controllable` ou `absent`.
    pub state: String,
    /// Dernier niveau écran commandé (0-100), −1 si inconnu.
    pub level: i32,
    /// Borne basse de la plage.
    pub low: u32,
    /// Borne haute de la plage.
    pub high: u32,
    /// Maximum matériel brut (0 si inconnu).
    pub max_raw: u32,
}

/// Erreurs renvoyées par le démon, avec un nom D-Bus stable par variante.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.ha2lux.Error")]
pub enum DaemonError {
    /// Erreur de transport D-Bus.
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Argument mal formé.
    InvalidArgs(String),
    /// Valeur hors bornes.
    OutOfRange(String),
    /// Écran inconnu.
    UnknownDisplay(String),
    /// Écran connu mais indisponible.
    DisplayUnavailable(String),
    /// Aucun écran contrôlable.
    NoControllableDisplay(String),
    /// Preset inconnu (le message liste les presets disponibles).
    UnknownPreset(String),
    /// Fonction non configurée (ex. auto-luminosité sans URL/jeton/entité).
    NotConfigured(String),
    /// Configuration invalide : l'ancienne est conservée.
    ConfigInvalid(String),
    /// Erreur interne du démon.
    Internal(String),
}

/// Catégorie d'une erreur D-Bus de ha2lux, pour le choix du code de sortie du client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Argument invalide ou hors bornes.
    Usage,
    /// Commande refusée (écran ou preset inconnu, indisponible, non configuré…).
    Failed,
}

/// Classe une erreur zbus : `None` si ce n'est pas une erreur de ha2lux
/// (démon absent, pas de réponse, transport).
pub fn classify(error: &zbus::Error) -> Option<ErrorKind> {
    let zbus::Error::MethodError(name, _, _) = error else {
        return None;
    };
    match name
        .as_str()
        .strip_prefix(ERROR_PREFIX)?
        .strip_prefix('.')?
    {
        "InvalidArgs" | "OutOfRange" => Some(ErrorKind::Usage),
        _ => Some(ErrorKind::Failed),
    }
}

/// Proxy zbus du démon (côté CLI et tray).
///
/// Les éléments générés par la macro ne sont pas documentés un par un : voir le
/// trait `Daemon` pour la description de chaque méthode.
pub mod proxy {
    #![allow(missing_docs)]

    use std::collections::HashMap;

    use zbus::zvariant::OwnedValue;

    use crate::DisplayDto;

    #[zbus::proxy(
        interface = "org.ha2lux.Daemon1",
        default_service = "org.ha2lux.Daemon",
        default_path = "/org/ha2lux/Daemon"
    )]
    pub trait Daemon {
        /// Définit le niveau maître (0-100).
        fn set_master(&self, level: u32) -> zbus::Result<()>;
        /// Variation relative du maître (−100..=100, non nulle).
        fn step_master(&self, delta: i32) -> zbus::Result<()>;
        /// Niveau direct d'un écran (0-100 de son propre niveau).
        fn set_display(&self, id: &str, level: u32) -> zbus::Result<()>;
        /// Plage d'un écran (`low`, `high` ≤ 100, normalisée par le démon).
        fn set_range(&self, id: &str, low: u32, high: u32) -> zbus::Result<()>;
        /// Applique un preset (insensible à la casse).
        fn apply_preset(&self, name: &str) -> zbus::Result<()>;
        /// Active ou désactive `schedule`, `idle-dim` ou `auto`.
        fn set_mode(&self, feature: &str, on: bool) -> zbus::Result<()>;
        /// Relance l'énumération des écrans.
        fn rescan(&self) -> zbus::Result<()>;
        /// Signale que l'interface interactive (popup) est ouverte : les relances
        /// périodiques des écrans indisponibles sont reportées pendant ce temps.
        fn set_ui_open(&self, open: bool) -> zbus::Result<()>;
        /// Recharge la configuration.
        fn reload(&self) -> zbus::Result<()>;
        /// Oublie la courbe apprise.
        fn reset_curve(&self) -> zbus::Result<()>;
        /// Annule le dernier apprentissage.
        fn undo_learn(&self) -> zbus::Result<()>;
        /// Enregistre le jeton Home Assistant (jamais en argument de ligne de commande).
        fn set_token(&self, token: &str) -> zbus::Result<()>;
        /// Supprime le jeton Home Assistant.
        fn clear_token(&self) -> zbus::Result<()>;
        /// Capteurs d'éclairement de Home Assistant : (entité, nom, pièce).
        fn sensors(&self) -> zbus::Result<Vec<(String, String, String)>>;
        /// Presets : (nom, niveau maître), dans l'ordre de la configuration.
        fn presets(&self) -> zbus::Result<Vec<(String, u32)>>;
        /// Liste des écrans.
        fn list(&self) -> zbus::Result<Vec<DisplayDto>>;
        /// État détaillé du démon.
        fn status(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
        /// Demande l'arrêt propre du démon (utilisée par le remplacement d'instance).
        fn quit(&self) -> zbus::Result<()>;

        /// Le niveau maître a changé.
        #[zbus(signal)]
        fn level_changed(&self, master: u32) -> zbus::Result<()>;

        /// La liste ou l'état des écrans a changé.
        #[zbus(signal)]
        fn displays_changed(&self) -> zbus::Result<()>;

        /// La source de pilotage de base a changé (`auto-lux`, `planning`, `static`).
        ///
        /// Nom D-Bus `ModeChanged` ; la méthode Rust s'appelle `mode_switched` pour ne pas
        /// entrer en conflit avec le flux de changements de la propriété `Mode`.
        #[zbus(signal, name = "ModeChanged")]
        fn mode_switched(&self, mode: &str) -> zbus::Result<()>;

        /// Une écriture sur un écran a définitivement échoué.
        #[zbus(signal)]
        fn display_failed(&self, id: &str, reason: &str) -> zbus::Result<()>;

        /// Niveau maître courant, −1 s'il est inconnu.
        #[zbus(property)]
        fn master(&self) -> zbus::Result<i32>;

        /// Source de pilotage de base : `auto-lux`, `planning` ou `static`.
        #[zbus(property)]
        fn mode(&self) -> zbus::Result<String>;
    }
}

pub use proxy::DaemonProxy;
