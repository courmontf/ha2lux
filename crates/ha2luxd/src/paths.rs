//! Emplacements des fichiers (spec §10.1), selon la convention XDG.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Impossible de déterminer les répertoires.
#[derive(Debug, Error)]
#[error(
    "cannot determine the configuration directories: neither XDG_CONFIG_HOME/XDG_STATE_HOME nor HOME is set"
)]
pub struct PathsError;

/// Fichiers du démon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_CONFIG_HOME/ha2lux/config.toml`.
    pub config_file: PathBuf,
    /// `$XDG_STATE_HOME/ha2lux/state.toml`.
    pub state_file: PathBuf,
    /// `$XDG_CONFIG_HOME/ha2lux/secrets.json` : repli (0600) si le trousseau est absent.
    pub secrets_file: PathBuf,
}

/// Une variable XDG n'est valable que si elle est un chemin absolu.
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
    /// Chemins déduits de l'environnement fourni (injectable pour les tests).
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, PathsError> {
        let config = xdg(&get, "XDG_CONFIG_HOME", ".config").ok_or(PathsError)?;
        let state = xdg(&get, "XDG_STATE_HOME", ".local/state").ok_or(PathsError)?;
        Ok(Self {
            secrets_file: config.join("ha2lux").join("secrets.json"),
            config_file: config.join("ha2lux").join("config.toml"),
            state_file: state.join("ha2lux").join("state.toml"),
        })
    }

    /// Chemins déduits de l'environnement du processus.
    pub fn system() -> Result<Self, PathsError> {
        Self::from_env(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
    }

    /// Les deux fichiers dans un même répertoire (tests).
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            secrets_file: dir.join("secrets.json"),
            config_file: dir.join("config.toml"),
            state_file: dir.join("state.toml"),
        }
    }
}
