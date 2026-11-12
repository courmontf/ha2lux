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
    #[error("trousseau indisponible : {0}")]
    Unavailable(String),
    /// Le trousseau est verrouillé et l'utilisateur n'a pas accepté de le déverrouiller.
    #[error("trousseau verrouillé : déverrouillage refusé ou expiré")]
    Locked,
    /// Fichier de secrets lisible par d'autres utilisateurs.
    #[error("{0} : permissions trop larges (attendu 0600), fichier ignoré")]
    TooOpen(String),
    /// Autre erreur d'entrée/sortie ou de format.
    #[error("{0}")]
    Other(String),
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
                Err(error) => tracing::debug!(%error, "trousseau illisible : repli sur le fichier"),
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
                        tracing::debug!(%error, "ancien jeton du fichier non supprimé");
                    }
                    return Ok(stored);
                }
                Err(error) => {
                    tracing::warn!(%error, "trousseau indisponible : jeton rangé dans le fichier 0600")
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
