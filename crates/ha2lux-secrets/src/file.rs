//! Fallback: JSON file `{ "hôte:port": "jeton" }` with 0600 permissions, atomic write.
//! Rejected on read if other users can read it.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::{SecretError, SecretStore, Stored};

/// Token store in a file.
pub struct FileSecretStore {
    path: PathBuf,
}

impl FileSecretStore {
    /// Store at `path` (for example `~/.config/ha2lux/secrets.json`).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn read_all(path: &Path) -> Result<BTreeMap<String, String>, SecretError> {
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(SecretError::Other(format!("{} : {e}", path.display()))),
        };
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(SecretError::TooOpen(path.display().to_string()));
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| SecretError::Other(format!("{} : {e}", path.display())))?;
        // The content is never copied into the error: it contains tokens.
        serde_json::from_str(&text)
            .map_err(|_| SecretError::Other(format!("{} : format illisible", path.display())))
    }

    fn write_all(path: &Path, secrets: &BTreeMap<String, String>) -> Result<(), SecretError> {
        let fail = |e: std::io::Error| SecretError::Other(format!("{} : {e}", path.display()));
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(fail)?;
        }
        let mut temp = path.as_os_str().to_owned();
        temp.push(format!(".tmp.{}", std::process::id()));
        let temp = PathBuf::from(temp);
        let body =
            serde_json::to_string_pretty(secrets).map_err(|e| SecretError::Other(e.to_string()))?;
        let result = (|| {
            // 0600 from creation: the content is never readable by another user.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(body.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temp, path)
        })();
        if result.is_err() {
            // Best effort: the temporary file may already be gone.
            let _ = std::fs::remove_file(&temp);
        }
        result.map_err(fail)
    }
}

impl SecretStore for FileSecretStore {
    async fn get(&self, host_key: &str) -> Result<Option<String>, SecretError> {
        let path = self.path.clone();
        let key = host_key.to_owned();
        tokio::task::spawn_blocking(move || Self::read_all(&path).map(|mut all| all.remove(&key)))
            .await
            .map_err(|e| SecretError::Other(e.to_string()))?
    }

    async fn set(&self, host_key: &str, token: &str) -> Result<Stored, SecretError> {
        let (path, key, token) = (self.path.clone(), host_key.to_owned(), token.to_owned());
        tokio::task::spawn_blocking(move || {
            let mut all = Self::read_all(&path)?;
            all.insert(key, token);
            Self::write_all(&path, &all)?;
            Ok(Stored::File)
        })
        .await
        .map_err(|e| SecretError::Other(e.to_string()))?
    }

    async fn delete(&self, host_key: &str) -> Result<bool, SecretError> {
        let (path, key) = (self.path.clone(), host_key.to_owned());
        tokio::task::spawn_blocking(move || {
            let mut all = Self::read_all(&path)?;
            let existed = all.remove(&key).is_some();
            if existed {
                Self::write_all(&path, &all)?;
            }
            Ok(existed)
        })
        .await
        .map_err(|e| SecretError::Other(e.to_string()))?
    }
}
