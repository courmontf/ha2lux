//! Home Assistant token storage (spec §8.2): **desktop keyring** (Secret Service,
//! over D-Bus) with **fallback to a file with 0600 permissions**.
//!
//! The token is stored under the server's `host:port` key ([`ha2lux_core::url::authority_key`]):
//! if the Home Assistant address changes host, the old server's token is **never**
//! found, hence never presented to the new one (T-SEC-04); the user must re-enter it.
//!
//! No function of this module writes a token to a log or an error message.
#![forbid(unsafe_code)]

pub mod file;
pub mod service;

use std::future::Future;

use thiserror::Error;

pub use file::FileSecretStore;
pub use service::ServiceSecretStore;

/// Failure of a secrets access. Messages never contain the token.
#[derive(Debug, Error)]
pub enum SecretError {
    /// No reachable keyring (service absent, bus unavailable).
    #[error("keyring unavailable: {0}")]
    Unavailable(String),
    /// The keyring is locked and the user did not agree to unlock it.
    #[error("keyring locked: unlock refused or timed out")]
    Locked,
    /// Secrets file readable by other users.
    #[error("{0}: permissions too open (0600 expected), file ignored")]
    TooOpen(String),
    /// Other I/O or format error.
    #[error("{0}")]
    Other(String),
}

impl SecretError {
    /// Localizable message for a client (catalog key + arguments); see
    /// [`ha2lux_i18n::wire`]. `Display` remains the text for logs.
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

/// Where a secret was stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    /// Desktop keyring.
    Keyring,
    /// Fallback file (0600).
    File,
}

/// Token store, indexed by the server's `host:port` key.
pub trait SecretStore: Send + Sync {
    /// Token stored for this server, if any.
    fn get(
        &self,
        host_key: &str,
    ) -> impl Future<Output = Result<Option<String>, SecretError>> + Send;
    /// Stores (or replaces) this server's token.
    fn set(
        &self,
        host_key: &str,
        token: &str,
    ) -> impl Future<Output = Result<Stored, SecretError>> + Send;
    /// Deletes this server's token. Returns `true` if it existed.
    fn delete(&self, host_key: &str) -> impl Future<Output = Result<bool, SecretError>> + Send;
}

/// Keyring first, file as fallback.
pub struct ChainStore {
    keyring: Option<ServiceSecretStore>,
    file: FileSecretStore,
}

impl ChainStore {
    /// Chains a keyring (if reachable) and a fallback file.
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
                    // An old copy in the file is no longer needed.
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
