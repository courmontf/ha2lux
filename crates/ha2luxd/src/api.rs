//! Requêtes acceptées par le démon, réponses et erreurs.
//!
//! **Toute valeur est validée ici**, dans le démon, avant d'atteindre la
//! machine d'états : la validation côté client (CLI, tray) n'est qu'un confort.

use ha2lux_core::{ControlError, DisplayId};
use ha2lux_i18n::wire;
use ha2lux_ipc::{DaemonError, DisplayDto};
use thiserror::Error;

/// Longueur maximale d'un identifiant d'écran.
pub const MAX_DISPLAY_ID: usize = 255;
/// Longueur maximale d'un nom de preset.
pub const MAX_PRESET_NAME: usize = 63;

/// Jeton transmis au démon : son `Debug` ne le montre jamais.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Requête adressée au démon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Niveau maître 0-100.
    SetMaster(u32),
    /// Variation du maître.
    StepMaster(i32),
    /// Niveau direct d'un écran.
    SetDisplay {
        /// Identité de l'écran.
        id: String,
        /// Niveau 0-100.
        level: u32,
    },
    /// Plage d'un écran.
    SetRange {
        /// Identité de l'écran.
        id: String,
        /// Borne basse (≤ 100).
        low: u32,
        /// Borne haute (≤ 100).
        high: u32,
    },
    /// Preset par nom.
    ApplyPreset(String),
    /// Active ou désactive une fonction.
    SetMode {
        /// `schedule`, `idle-dim` ou `auto`.
        feature: String,
        /// Nouvel état.
        on: bool,
    },
    /// Relance l'énumération.
    Rescan,
    /// L'interface interactive (popup du tray) est ouverte ou fermée : les relances
    /// périodiques des écrans indisponibles sont reportées tant qu'elle est ouverte.
    SetUiOpen(bool),
    /// Recharge la configuration.
    Reload,
    /// Oublie la courbe apprise.
    ResetCurve,
    /// Annule le dernier apprentissage.
    UndoLearn,
    /// Enregistre le jeton Home Assistant de l'URL configurée.
    SetToken(Secret),
    /// Supprime le jeton Home Assistant de l'URL configurée.
    ClearToken,
    /// Liste les capteurs d'éclairement de Home Assistant.
    ListSensors,
    /// Presets (nom, niveau).
    Presets,
    /// Liste des écrans.
    List,
    /// État détaillé.
    Status,
    /// Arrêt propre.
    Quit,
}

/// Réponse à une requête.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// Commande exécutée.
    Done,
    /// Liste des écrans.
    List(Vec<DisplayDto>),
    /// Presets (nom, niveau), dans l'ordre de la configuration.
    Presets(Vec<(String, u32)>),
    /// Capteurs d'éclairement : (entité, nom, pièce).
    Sensors(Vec<(String, String, String)>),
    /// État détaillé.
    Status(Box<StatusSnapshot>),
}

/// Instantané de l'état du démon (publié après chaque message traité).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusSnapshot {
    /// Version du démon.
    pub version: String,
    /// Maître courant.
    pub master: Option<u8>,
    /// Source de base : `auto-lux`, `planning` ou `static`.
    pub mode: String,
    /// Pas par défaut de `up`/`down` (réglage `general.step`).
    pub step: u8,
    /// Planning activé.
    pub schedule_enabled: bool,
    /// Nombre de points du planning.
    pub schedule_points: u32,
    /// Planning suspendu : heure de reprise `HH:MM`.
    pub schedule_resume: Option<String>,
    /// Atténuation d'inactivité activée.
    pub idle_enabled: bool,
    /// Niveau d'atténuation (0-100).
    pub idle_level: u8,
    /// Délai d'inactivité avant atténuation, en minutes.
    pub idle_minutes: u16,
    /// Atténuation en cours.
    pub dimmed: bool,
    /// Auto-luminosité activée.
    pub auto_enabled: bool,
    /// Auto-luminosité configurée (URL, jeton et entité).
    pub auto_configured: bool,
    /// Santé Home Assistant.
    pub ha_health: String,
    /// Dernier état brut lu sur Home Assistant.
    pub ha_raw: Option<String>,
    /// Flux WebSocket actif.
    pub ha_websocket: bool,
    /// Le jeton est refusé par Home Assistant.
    pub ha_auth_rejected: bool,
    /// Un jeton est enregistré pour l'URL configurée.
    pub ha_token_present: bool,
    /// Lux lissé (mesure suivie par le filtre).
    pub lux_smoothed: Option<f64>,
    /// Lux le plus récent.
    pub lux_latest: Option<f64>,
    /// Un changement de lumière est en cours de confirmation ou de rampe.
    pub auto_adjusting: bool,
    /// Niveau visé par la courbe pour le lux lissé.
    pub auto_target: Option<u8>,
    /// Courbe utilisée (lux, niveau).
    pub curve: Vec<(f64, u8)>,
    /// La courbe est celle par défaut.
    pub curve_default: bool,
    /// Cible de rampe en cours.
    pub ramp_target: Option<u8>,
    /// Configuration illisible : message (la configuration précédente est gardée).
    pub config_error: Option<String>,
    /// Écrans connus.
    pub displays: Vec<DisplayDto>,
    /// Écritures matérielles réussies.
    pub writes_ok: u64,
    /// Écritures définitivement échouées.
    pub writes_failed: u64,
    /// Nouvelles tentatives effectuées.
    pub retries: u64,
    /// Opérations matérielles abandonnées (timeout).
    pub abandoned_ops: u32,
    /// Dernière erreur matérielle.
    pub last_error: Option<String>,
}

/// Erreur d'exécution d'une requête.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApiError {
    /// Argument mal formé.
    #[error("{0}")]
    InvalidArgs(String),
    /// Valeur hors bornes.
    #[error("{0}")]
    OutOfRange(String),
    /// Écran inconnu.
    #[error("unknown display: {0}")]
    UnknownDisplay(String),
    /// Écran indisponible.
    #[error("display unavailable: {0}")]
    DisplayUnavailable(String),
    /// Aucun écran contrôlable.
    #[error("no controllable display")]
    NoControllableDisplay,
    /// Preset inconnu ; le message liste les presets disponibles.
    #[error("{0}")]
    UnknownPreset(String),
    /// Fonction non configurée.
    #[error("{0}")]
    NotConfigured(String),
    /// Configuration invalide (l'ancienne est conservée).
    #[error("{0}")]
    ConfigInvalid(String),
    /// Échec d'un accès au trousseau ou à Home Assistant.
    #[error("{0}")]
    Failed(String),
    /// Le démon s'arrête ou ne répond plus.
    #[error("the daemon is stopped")]
    Stopped,
}

impl From<ControlError> for ApiError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::OutOfRange { what, value } => Self::OutOfRange(wire::encode(
                "err-out-of-range",
                &[("what", &what), ("value", &value)],
            )),
            ControlError::UnknownDisplay(id) => Self::UnknownDisplay(id.to_string()),
            ControlError::DisplayUnavailable(id) => Self::DisplayUnavailable(id.to_string()),
            ControlError::NoControllableDisplay => Self::NoControllableDisplay,
        }
    }
}

impl From<ApiError> for DaemonError {
    fn from(error: ApiError) -> Self {
        // Les messages sont envoyés sous forme de clé + arguments : chaque client
        // les traduit dans sa langue (voir `ha2lux_i18n::wire`).
        match error {
            ApiError::InvalidArgs(text) => Self::InvalidArgs(text),
            ApiError::OutOfRange(text) => Self::OutOfRange(text),
            ApiError::UnknownDisplay(id) => {
                Self::UnknownDisplay(wire::encode("err-unknown-display", &[("id", &id)]))
            }
            ApiError::DisplayUnavailable(id) => {
                Self::DisplayUnavailable(wire::encode("err-display-unavailable", &[("id", &id)]))
            }
            ApiError::NoControllableDisplay => {
                Self::NoControllableDisplay(wire::encode("err-no-controllable-display", &[]))
            }
            ApiError::UnknownPreset(text) => Self::UnknownPreset(text),
            ApiError::NotConfigured(text) => Self::NotConfigured(text),
            ApiError::ConfigInvalid(text) => Self::ConfigInvalid(text),
            ApiError::Failed(text) => Self::Internal(text),
            ApiError::Stopped => Self::Internal(wire::encode("err-daemon-stopped", &[])),
        }
    }
}

/// Valide une valeur 0-100.
pub fn percent(what: &str, value: u32) -> Result<i64, ApiError> {
    if value <= 100 {
        Ok(i64::from(value))
    } else {
        Err(ApiError::OutOfRange(wire::encode(
            "err-percent-out-of-range",
            &[("what", &what), ("value", &value)],
        )))
    }
}

/// Valide un identifiant d'écran reçu d'un client.
pub fn display_id(id: &str) -> Result<DisplayId, ApiError> {
    if id.is_empty() || id.chars().count() > MAX_DISPLAY_ID || id.chars().any(char::is_control) {
        return Err(ApiError::InvalidArgs(wire::encode(
            "err-display-id-invalid",
            &[("max", &MAX_DISPLAY_ID)],
        )));
    }
    Ok(DisplayId::new(id))
}

/// Valide un nom de preset reçu d'un client.
pub fn preset_name(name: &str) -> Result<&str, ApiError> {
    if name.is_empty()
        || name.chars().count() > MAX_PRESET_NAME
        || name.chars().any(char::is_control)
    {
        return Err(ApiError::InvalidArgs(wire::encode(
            "err-preset-name-invalid",
            &[("max", &MAX_PRESET_NAME)],
        )));
    }
    Ok(name)
}
