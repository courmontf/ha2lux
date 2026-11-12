//! Contrat des backends d'affichage : seul point de contact avec le matériel.
//!
//! Le cœur ne dépend d'aucun runtime : les méthodes asynchrones sont de
//! simples `Future`, exécutées par l'appelant (tâche d'écran du démon).

use std::fmt;
use std::future::Future;

use thiserror::Error;

/// Identifiant stable d'un écran (clé EDID quand elle existe).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DisplayId(String);

impl DisplayId {
    /// Construit un identifiant à partir de sa représentation textuelle.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Représentation textuelle.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DisplayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Nature d'un écran, qui détermine son backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayKind {
    /// Panneau interne (backlight noyau).
    Internal,
    /// Écran externe (DDC/CI).
    External,
}

/// Description d'un écran obtenue par `probe`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Identifiant stable.
    pub id: DisplayId,
    /// Nom affiché.
    pub name: String,
    /// Interne ou externe.
    pub kind: DisplayKind,
    /// Valeur brute maximale (le minimum est 0).
    pub max: u32,
}

/// Erreurs d'un backend, classées selon la réaction attendue.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BackendError {
    /// Échec passager : on peut réessayer.
    #[error("transient failure: {0}")]
    Transient(String),
    /// Aucune réponse dans le délai imparti.
    #[error("timed out")]
    Timeout,
    /// L'écran n'est plus joignable (débranché, lien coupé).
    #[error("display unreachable: {0}")]
    Disconnected(String),
    /// Droits insuffisants (udev, logind) : inutile de réessayer.
    #[error("access denied: {0}")]
    PermissionDenied(String),
    /// Réponse ou configuration invalide (ex. maximum nul).
    #[error("invalid response: {0}")]
    Invalid(String),
}

impl BackendError {
    /// Vrai si une nouvelle tentative a un sens.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::Timeout)
    }
}

/// Backend d'un écran : DDC/CI, backlight ou factice.
pub trait DisplayBackend: Send + 'static {
    /// Description connue (valeur mise en cache, sans E/S).
    fn info(&self) -> &DisplayInfo;

    /// Interroge le matériel : contrôlable ⇔ lecture réussie et `max > 0`.
    fn probe(&mut self) -> impl Future<Output = Result<DisplayInfo, BackendError>> + Send;

    /// Lit la valeur brute courante.
    fn read(&mut self) -> impl Future<Output = Result<u32, BackendError>> + Send;

    /// Écrit une valeur brute (déjà normalisée dans `0..=max`).
    fn write(&mut self, raw: u32) -> impl Future<Output = Result<(), BackendError>> + Send;
}
