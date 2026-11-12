//! File locations (spec §10.1), following the XDG convention.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Unable to determine the directories.
#[derive(Debug, Error)]
#[error(
    "cannot determine the configuration directories: neither XDG_CONFIG_HOME/XDG_STATE_HOME nor HOME is set"
)]
pub struct PathsError;

/// Daemon files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_CONFIG_HOME/ha2lux/config.toml`.
    pub config_file: PathBuf,
    /// `$XDG_STATE_HOME/ha2lux/state.toml`.
    pub state_file: PathBuf,
    /// `$XDG_CONFIG_HOME/ha2lux/secrets.json`: fallback (0600) if the keyring is absent.
    pub secrets_file: PathBuf,
}

/// An XDG variable is only valid if it is an absolute path.
fn xdg(get: &impl Fn(&str) -> Option<String>, var: &str, home_relative: &str) -> Option<PathBuf> {
    get(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            get("HOME")
                .map(|h| Path::new(&h).join(home_relative))
                .filter(|p| p.is_absolute())
        })
}

impl Paths {
    /// Paths derived from the supplied environment (injectable for tests).
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, PathsError> {
        let config = xdg(&get, "XDG_CONFIG_HOME", ".config").ok_or(PathsError)?;
        let state = xdg(&get, "XDG_STATE_HOME", ".local/state").ok_or(PathsError)?;
        Ok(Self {
            secrets_file: config.join("ha2lux").join("secrets.json"),
            config_file: config.join("ha2lux").join("config.toml"),
            state_file: state.join("ha2lux").join("state.toml"),
        })
    }

    /// Paths derived from the process environment.
    pub fn system() -> Result<Self, PathsError> {
        Self::from_env(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
    }

    /// Both files in a single directory (tests).
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            secrets_file: dir.join("secrets.json"),
            config_file: dir.join("config.toml"),
            state_file: dir.join("state.toml"),
        }
    }
}
