//! Stockage du jeton Home Assistant (spec §8.2) : **trousseau du bureau** (Secret Service,
//! par D-Bus) avec **repli sur un fichier de permissions 0600**.
//!
//! Le jeton est rangé sous la clé `hôte:port` du serveur ([`ha2lux_core::url::authority_key`]) :
//! si l'adresse de Home Assistant change d'hôte, le jeton de l'ancien serveur n'est **jamais**
//! retrouvé, donc jamais présenté au nouveau (T-SEC-04) ; l'utilisateur doit le ressaisir.
//!
//! Aucune fonction de ce module n'écrit un jeton dans un journal ou un message d'erreur.
#![forbid(unsafe_code)]

pub mod file;
pub mod service;

use std::future::Future;

use thiserror::Error;

pub use file::FileSecretStore;
pub use service::ServiceSecretStore;

/// Échec d'un accès aux secrets. Les messages ne contiennent jamais le jeton.
#[derive(Debug, Error)]
pub enum SecretError {
    /// Pas de trousseau joignable (service absent, bus indisponible).
    #[error("keyring unavailable: {0}")]
    Unavailable(String),
    /// Le trousseau est verrouillé et l'utilisateur n'a pas accepté de le déverrouiller.
    #[error("keyring locked: unlock refused or timed out")]
    Locked,
    /// Fichier de secrets lisible par d'autres utilisateurs.
    #[error("{0}: permissions too open (0600 expected), file ignored")]
    TooOpen(String),
    /// Autre erreur d'entrée/sortie ou de format.
    #[error("{0}")]
    Other(String),
}

impl SecretError {
    /// Message localisable pour un client (clé de catalogue + arguments) ; voir
    /// [`ha2lux_i18n::wire`]. Le `Display` reste le texte pour les journaux.
    pub fn to_wire(&self) -> String {
        use ha2lux_i18n::wire::encode;
        match self {
            Self::Unavailable(detail) => encode("err-keyring-unavailable", &[("detail", detail)]),
            Self::Locked => encode("err-keyring-locked", &[]),
            Self::TooOpen(path) => encode("err-secret-file-too-open", &[("path", path)]),
            Self::Other(message) => message.clone(),
        }
    }
}

/// Où un secret a été rangé.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    /// Trousseau du bureau.
    Keyring,
    /// Fichier de repli (0600).
    File,
}

/// Magasin de jetons, indexé par la clé `hôte:port` du serveur.
pub trait SecretStore: Send + Sync {
    /// Jeton enregistré pour ce serveur, s'il existe.
    fn get(
        &self,
        host_key: &str,
    ) -> impl Future<Output = Result<Option<String>, SecretError>> + Send;
    /// Enregistre (ou remplace) le jeton de ce serveur.
    fn set(
        &self,
        host_key: &str,
        token: &str,
    ) -> impl Future<Output = Result<Stored, SecretError>> + Send;
    /// Supprime le jeton de ce serveur. Renvoie `true` s'il existait.
    fn delete(&self, host_key: &str) -> impl Future<Output = Result<bool, SecretError>> + Send;
}

/// Trousseau d'abord, fichier en repli.
pub struct ChainStore {
    keyring: Option<ServiceSecretStore>,
    file: FileSecretStore,
}

impl ChainStore {
    /// Chaîne un trousseau (s'il est joignable) et un fichier de repli.
    pub fn new(keyring: Option<ServiceSecretStore>, file: FileSecretStore) -> Self {
        Self { keyring, file }
    }
}

impl SecretStore for ChainStore {
    async fn get(&self, host_key: &str) -> Result<Option<String>, SecretError> {
        if let Some(keyring) = &self.keyring {
            match keyring.get(host_key).await {
                Ok(Some(token)) => return Ok(Some(token)),
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(%error, "keyring unreadable: falling back to the file")
                }
            }
        }
        self.file.get(host_key).await
    }

    async fn set(&self, host_key: &str, token: &str) -> Result<Stored, SecretError> {
        if let Some(keyring) = &self.keyring {
            match keyring.set(host_key, token).await {
                Ok(stored) => {
                    // Un ancien exemplaire dans le fichier n'a plus lieu d'être.
                    if let Err(error) = self.file.delete(host_key).await {
                        tracing::debug!(%error, "old token in the file not removed");
                    }
                    return Ok(stored);
                }
                Err(error) => {
                    tracing::warn!(%error, "keyring unavailable: token stored in the 0600 file")
                }
            }
        }
        self.file.set(host_key, token).await
    }

    async fn delete(&self, host_key: &str) -> Result<bool, SecretError> {
        let in_keyring = match &self.keyring {
            Some(keyring) => keyring.delete(host_key).await.unwrap_or(false),
            None => false,
        };
        let in_file = self.file.delete(host_key).await?;
        Ok(in_keyring || in_file)
    }
}
