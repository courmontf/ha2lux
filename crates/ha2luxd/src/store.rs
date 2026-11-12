//! Lecture et écriture des fichiers de configuration et d'état.
//!
//! Toute écriture est **atomique** : fichier temporaire dans le même
//! répertoire, `fsync`, renommage, `fsync` du répertoire. Un arrêt brutal
//! (`kill -9`, coupure) laisse donc l'ancien fichier intact, jamais un fichier
//! tronqué (T-CFG-09). Fonctions bloquantes : à appeler depuis un thread
//! dédié, jamais depuis l'acteur décideur.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ha2lux_core::State;
use ha2lux_core::config::{Config, ConfigError, ConfigIssue, Loaded};
use thiserror::Error;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Erreurs de lecture ou d'écriture.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Erreur d'entrée/sortie sur un fichier.
    #[error("{path} : {source}")]
    Io {
        /// Fichier concerné.
        path: PathBuf,
        /// Cause.
        source: io::Error,
    },
    /// Contenu invalide (syntaxe, version).
    #[error("{path} : {error}")]
    Config {
        /// Fichier concerné.
        path: PathBuf,
        /// Cause.
        error: ConfigError,
    },
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.tmp.{}.{n}", std::process::id()))
}

/// Écrit `bytes` dans `path` de façon atomique.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, || Ok(()))
}

/// Comme [`write_atomic`], avec un point d'arrêt juste avant le renommage
/// (simulation d'une interruption dans les tests). En cas d'erreur, le
/// fichier temporaire est supprimé et l'original reste intact.
pub fn write_atomic_with(
    path: &Path,
    bytes: &[u8],
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let temp = temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        before_rename()?;
        fs::rename(&temp, path)?;
        // Rend le renommage durable.
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        // Meilleur effort : le fichier temporaire peut déjà avoir disparu.
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Supprime les fichiers temporaires laissés par un arrêt brutal. Renvoie leur nombre.
pub fn cleanup_stale_temps(path: &Path) -> usize {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return 0;
    };
    let prefix = format!(".{}.tmp.", name.to_string_lossy());
    let Ok(entries) = fs::read_dir(parent) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .filter(|e| fs::remove_file(e.path()).is_ok())
        .count()
}

/// Copie `path` vers `path.bak` (écriture atomique) s'il existe.
pub fn backup(path: &Path) -> io::Result<()> {
    match fs::read(path) {
        Ok(bytes) => {
            let mut name = path.as_os_str().to_owned();
            name.push(".bak");
            write_atomic(&PathBuf::from(name), &bytes)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Lit et valide la configuration. Une syntaxe invalide ou une version trop
/// récente renvoient une erreur et **ne modifient jamais** le fichier.
pub fn read_config(path: &Path) -> Result<Loaded, StoreError> {
    let text = fs::read_to_string(path).map_err(|source| StoreError::Io {
        path: path.to_owned(),
        source,
    })?;
    Config::parse(&text).map_err(|error| StoreError::Config {
        path: path.to_owned(),
        error,
    })
}

/// Résultat du chargement au démarrage.
#[derive(Debug)]
pub enum ConfigLoad {
    /// Fichier absent : créé avec les valeurs par défaut (T-CFG-01).
    Created(Loaded),
    /// Fichier lu.
    Loaded(Loaded),
    /// Syntaxe invalide : valeurs par défaut en mémoire, fichier préservé et
    /// jamais réécrit tant qu'il n'est pas corrigé puis rechargé.
    Invalid {
        /// Raison, sans extrait du fichier.
        error: ConfigError,
    },
}

/// Charge la configuration, ou la crée si elle n'existe pas. Une version de
/// schéma plus récente est refusée (erreur), fichier intact (T-CFG-08).
pub fn load_or_create_config(path: &Path) -> Result<ConfigLoad, StoreError> {
    cleanup_stale_temps(path);
    match fs::read_to_string(path) {
        Ok(text) => match Config::parse(&text) {
            Ok(loaded) => Ok(ConfigLoad::Loaded(loaded)),
            Err(error @ ConfigError::UnsupportedVersion { .. }) => Err(StoreError::Config {
                path: path.to_owned(),
                error,
            }),
            Err(error) => Ok(ConfigLoad::Invalid { error }),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let config = Config::default();
            let text = config.to_toml().map_err(|error| StoreError::Config {
                path: path.to_owned(),
                error,
            })?;
            write_atomic(path, text.as_bytes()).map_err(|source| StoreError::Io {
                path: path.to_owned(),
                source,
            })?;
            Ok(ConfigLoad::Created(Loaded {
                config,
                issues: Vec::new(),
            }))
        }
        Err(source) => Err(StoreError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Résultat du chargement de l'état.
#[derive(Debug, Default)]
pub struct StateLoad {
    /// État lu (ou vide).
    pub state: State,
    /// Remarques de lecture.
    pub issues: Vec<ConfigIssue>,
    /// Le fichier était illisible : il a été mis de côté en `.corrupt`.
    pub set_aside: bool,
}

/// Charge l'état. Un fichier illisible ou d'une version inconnue est conservé
/// sous `state.toml.corrupt` (jamais écrasé en silence) et l'état repart vide.
pub fn load_state(path: &Path) -> StateLoad {
    cleanup_stale_temps(path);
    let Ok(text) = fs::read_to_string(path) else {
        return StateLoad::default();
    };
    match State::parse(&text) {
        Ok((state, issues)) => StateLoad {
            state,
            issues,
            set_aside: false,
        },
        Err(error) => {
            tracing::warn!(%error, "state file unreadable: kept as .corrupt, empty state");
            let mut aside = path.as_os_str().to_owned();
            aside.push(".corrupt");
            // Si la mise de côté échoue, on ne supprime rien : l'ancien fichier sera
            // simplement remplacé au prochain enregistrement, comme avant.
            let moved = fs::rename(path, PathBuf::from(aside)).is_ok();
            StateLoad {
                set_aside: moved,
                ..StateLoad::default()
            }
        }
    }
}
