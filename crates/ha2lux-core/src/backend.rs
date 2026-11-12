//! Display backend contract: the only point of contact with the hardware.
//!
//! The core depends on no runtime: the async methods are plain `Future`s,
//! executed by the caller (the daemon's display task).

use std::fmt;
use std::future::Future;

use thiserror::Error;

/// Stable identifier of a display (EDID key when one exists).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DisplayId(String);

impl DisplayId {
    /// Builds an identifier from its textual representation.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Textual representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DisplayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Kind of display, which determines its backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayKind {
    /// Internal panel (kernel backlight).
    Internal,
    /// External display (DDC/CI).
    External,
}

/// Description of a display obtained through `probe`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Stable identifier.
    pub id: DisplayId,
    /// Display name.
    pub name: String,
    /// Internal or external.
    pub kind: DisplayKind,
    /// Maximum raw value (the minimum is 0).
    pub max: u32,
}

/// Backend errors, classified by the expected reaction.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BackendError {
    /// Transient failure: retrying is possible.
    #[error("transient failure: {0}")]
    Transient(String),
    /// No response within the allotted time.
    #[error("timed out")]
    Timeout,
    /// The display is no longer reachable (unplugged, link down).
    #[error("display unreachable: {0}")]
    Disconnected(String),
    /// Insufficient permissions (udev, logind): no point in retrying.
    #[error("access denied: {0}")]
    PermissionDenied(String),
    /// Invalid response or configuration (e.g. zero maximum).
    #[error("invalid response: {0}")]
    Invalid(String),
}

impl BackendError {
    /// True if a new attempt makes sense.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::Timeout)
    }
}

/// Backend of a display: DDC/CI, backlight or fake.
pub trait DisplayBackend: Send + 'static {
    /// Known description (cached value, no I/O).
    fn info(&self) -> &DisplayInfo;

    /// Queries the hardware: controllable ⇔ read succeeded and `max > 0`.
    fn probe(&mut self) -> impl Future<Output = Result<DisplayInfo, BackendError>> + Send;

    /// Reads the current raw value.
    fn read(&mut self) -> impl Future<Output = Result<u32, BackendError>> + Send;

    /// Writes a raw value (already normalized to `0..=max`).
    fn write(&mut self, raw: u32) -> impl Future<Output = Result<(), BackendError>> + Send;
}
