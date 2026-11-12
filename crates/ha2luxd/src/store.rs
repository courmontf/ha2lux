//! Reading and writing of the configuration and state files.
//!
//! Every write is **atomic**: temporary file in the same
//! directory, `fsync`, rename, `fsync` of the directory. A sudden stop
//! (`kill -9`, power loss) therefore leaves the old file intact, never a
//! truncated file (T-CFG-09). Blocking functions: to be called from a
//! dedicated thread, never from the decision actor.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ha2lux_core::State;
use ha2lux_core::config::{Config, ConfigError, ConfigIssue, Loaded};
use thiserror::Error;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Read or write errors.
#[derive(Debug, Error)]
pub enum StoreError {
    /// I/O error on a file.
    #[error("{path} : {source}")]
    Io {
        /// File concerned.
        path: PathBuf,
        /// Cause.
        source: io::Error,
    },
    /// Invalid content (syntax, version).
    #[error("{path} : {error}")]
    Config {
        /// File concerned.
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

/// Writes `bytes` to `path` atomically.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, || Ok(()))
}

/// Like [`write_atomic`], with a stop point right before the rename
/// (simulating an interruption in tests). On error, the
/// temporary file is deleted and the original stays intact.
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
        // Makes the rename durable.
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        // Best effort: the temporary file may already be gone.
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Deletes the temporary files left by a sudden stop. Returns their count.
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

/// Copies `path` to `path.bak` (atomic write) if it exists.
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

/// Reads and validates the configuration. Invalid syntax or a version that is too
/// recent return an error and **never modify** the file.
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

/// Result of loading at startup.
#[derive(Debug)]
pub enum ConfigLoad {
    /// File missing: created with the default values (T-CFG-01).
    Created(Loaded),
    /// File read.
    Loaded(Loaded),
    /// Invalid syntax: default values in memory, file preserved and
    /// never rewritten until it is fixed and then reloaded.
    Invalid {
        /// Reason, without an excerpt of the file.
        error: ConfigError,
    },
}

/// Loads the configuration, or creates it if it does not exist. A more recent
/// schema version is rejected (error), file intact (T-CFG-08).
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

/// Result of loading the state.
#[derive(Debug, Default)]
pub struct StateLoad {
    /// State read (or empty).
    pub state: State,
    /// Read notes.
    pub issues: Vec<ConfigIssue>,
    /// The file was unreadable: it was set aside as `.corrupt`.
    pub set_aside: bool,
}

/// Loads the state. An unreadable file or one of an unknown version is kept
/// as `state.toml.corrupt` (never silently overwritten) and the state starts empty.
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
            // If setting it aside fails, nothing is deleted: the old file will simply be
            // replaced at the next save, as before.
            let moved = fs::rename(path, PathBuf::from(aside)).is_ok();
            StateLoad {
                set_aside: moved,
                ..StateLoad::default()
            }
        }
    }
}
