//! Requests accepted by the daemon, responses and errors.
//!
//! **Every value is validated here**, in the daemon, before reaching the
//! state machine: client-side validation (CLI, tray) is only a convenience.

use ha2lux_core::{ControlError, DisplayId};
use ha2lux_i18n::wire;
use ha2lux_ipc::{DaemonError, DisplayDto};
use thiserror::Error;

/// Maximum length of a display identifier.
pub const MAX_DISPLAY_ID: usize = 255;
/// Maximum length of a preset name.
pub const MAX_PRESET_NAME: usize = 63;

/// Token passed to the daemon: its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Request addressed to the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Master level 0-100.
    SetMaster(u32),
    /// Master change.
    StepMaster(i32),
    /// Direct level of a display.
    SetDisplay {
        /// Display identity.
        id: String,
        /// Level 0-100.
        level: u32,
    },
    /// Range of a display.
    SetRange {
        /// Display identity.
        id: String,
        /// Lower bound (<= 100).
        low: u32,
        /// Upper bound (<= 100).
        high: u32,
    },
    /// Preset by name.
    ApplyPreset(String),
    /// Enables or disables a feature.
    SetMode {
        /// `schedule`, `idle-dim` or `auto`.
        feature: String,
        /// New state.
        on: bool,
    },
    /// Restarts the enumeration.
    Rescan,
    /// The interactive UI (tray popup) is open or closed: periodic retries
    /// of unavailable displays are postponed while it is open.
    SetUiOpen(bool),
    /// Reloads the configuration.
    Reload,
    /// Forgets the learned curve.
    ResetCurve,
    /// Cancels the last learning step.
    UndoLearn,
    /// Stores the Home Assistant token for the configured URL.
    SetToken(Secret),
    /// Deletes the Home Assistant token for the configured URL.
    ClearToken,
    /// Lists the Home Assistant illuminance sensors.
    ListSensors,
    /// Presets (name, level).
    Presets,
    /// List of displays.
    List,
    /// Detailed state.
    Status,
    /// Clean shutdown.
    Quit,
}

/// Response to a request.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// Command executed.
    Done,
    /// List of displays.
    List(Vec<DisplayDto>),
    /// Presets (name, level), in configuration order.
    Presets(Vec<(String, u32)>),
    /// Illuminance sensors: (entity, name, room).
    Sensors(Vec<(String, String, String)>),
    /// Detailed state.
    Status(Box<StatusSnapshot>),
}

/// Snapshot of the daemon state (published after each processed message).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusSnapshot {
    /// Daemon version.
    pub version: String,
    /// Current master.
    pub master: Option<u8>,
    /// Base source: `auto-lux`, `planning` or `static`.
    pub mode: String,
    /// Default step of `up`/`down` (`general.step` setting).
    pub step: u8,
    /// Schedule enabled.
    pub schedule_enabled: bool,
    /// Number of schedule points.
    pub schedule_points: u32,
    /// Schedule suspended: resume time `HH:MM`.
    pub schedule_resume: Option<String>,
    /// Idle dimming enabled.
    pub idle_enabled: bool,
    /// Dimming level (0-100).
    pub idle_level: u8,
    /// Idle delay before dimming, in minutes.
    pub idle_minutes: u16,
    /// Dimming in progress.
    pub dimmed: bool,
    /// Auto-brightness enabled.
    pub auto_enabled: bool,
    /// Auto-brightness configured (URL, token and entity).
    pub auto_configured: bool,
    /// Home Assistant health.
    pub ha_health: String,
    /// Last raw state read from Home Assistant.
    pub ha_raw: Option<String>,
    /// Active WebSocket stream.
    pub ha_websocket: bool,
    /// The token is rejected by Home Assistant.
    pub ha_auth_rejected: bool,
    /// A token is stored for the configured URL.
    pub ha_token_present: bool,
    /// Smoothed lux (measurement tracked by the filter).
    pub lux_smoothed: Option<f64>,
    /// Most recent lux.
    pub lux_latest: Option<f64>,
    /// A light change is being confirmed or ramped.
    pub auto_adjusting: bool,
    /// Level targeted by the curve for the smoothed lux.
    pub auto_target: Option<u8>,
    /// Curve in use (lux, level).
    pub curve: Vec<(f64, u8)>,
    /// The curve is the default one.
    pub curve_default: bool,
    /// Current ramp target.
    pub ramp_target: Option<u8>,
    /// Unreadable configuration: message (the previous configuration is kept).
    pub config_error: Option<String>,
    /// Known displays.
    pub displays: Vec<DisplayDto>,
    /// Successful hardware writes.
    pub writes_ok: u64,
    /// Permanently failed writes.
    pub writes_failed: u64,
    /// Retries performed.
    pub retries: u64,
    /// Hardware operations abandoned (timeout).
    pub abandoned_ops: u32,
    /// Last hardware error.
    pub last_error: Option<String>,
}

/// Request execution error.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApiError {
    /// Malformed argument.
    #[error("{0}")]
    InvalidArgs(String),
    /// Value out of bounds.
    #[error("{0}")]
    OutOfRange(String),
    /// Unknown display.
    #[error("unknown display: {0}")]
    UnknownDisplay(String),
    /// Unavailable display.
    #[error("display unavailable: {0}")]
    DisplayUnavailable(String),
    /// No controllable display.
    #[error("no controllable display")]
    NoControllableDisplay,
    /// Unknown preset; the message lists the available presets.
    #[error("{0}")]
    UnknownPreset(String),
    /// Feature not configured.
    #[error("{0}")]
    NotConfigured(String),
    /// Invalid configuration (the old one is kept).
    #[error("{0}")]
    ConfigInvalid(String),
    /// Failure accessing the keyring or Home Assistant.
    #[error("{0}")]
    Failed(String),
    /// The daemon is shutting down or no longer responds.
    #[error("the daemon is stopped")]
    Stopped,
}

impl From<ControlError> for ApiError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::OutOfRange { what, value } => Self::OutOfRange(wire::encode(
                "err-out-of-range",
                &[("what", &what), ("value", &value)],
            )),
            ControlError::UnknownDisplay(id) => Self::UnknownDisplay(id.to_string()),
            ControlError::DisplayUnavailable(id) => Self::DisplayUnavailable(id.to_string()),
            ControlError::NoControllableDisplay => Self::NoControllableDisplay,
        }
    }
}

impl From<ApiError> for DaemonError {
    fn from(error: ApiError) -> Self {
        // Messages are sent as key + arguments: each client
        // translates them into its own language (see `ha2lux_i18n::wire`).
        match error {
            ApiError::InvalidArgs(text) => Self::InvalidArgs(text),
            ApiError::OutOfRange(text) => Self::OutOfRange(text),
            ApiError::UnknownDisplay(id) => {
                Self::UnknownDisplay(wire::encode("err-unknown-display", &[("id", &id)]))
            }
            ApiError::DisplayUnavailable(id) => {
                Self::DisplayUnavailable(wire::encode("err-display-unavailable", &[("id", &id)]))
            }
            ApiError::NoControllableDisplay => {
                Self::NoControllableDisplay(wire::encode("err-no-controllable-display", &[]))
            }
            ApiError::UnknownPreset(text) => Self::UnknownPreset(text),
            ApiError::NotConfigured(text) => Self::NotConfigured(text),
            ApiError::ConfigInvalid(text) => Self::ConfigInvalid(text),
            ApiError::Failed(text) => Self::Internal(text),
            ApiError::Stopped => Self::Internal(wire::encode("err-daemon-stopped", &[])),
        }
    }
}

/// Validates a 0-100 value.
pub fn percent(what: &str, value: u32) -> Result<i64, ApiError> {
    if value <= 100 {
        Ok(i64::from(value))
    } else {
        Err(ApiError::OutOfRange(wire::encode(
            "err-percent-out-of-range",
            &[("what", &what), ("value", &value)],
        )))
    }
}

/// Validates a display identifier received from a client.
pub fn display_id(id: &str) -> Result<DisplayId, ApiError> {
    if id.is_empty() || id.chars().count() > MAX_DISPLAY_ID || id.chars().any(char::is_control) {
        return Err(ApiError::InvalidArgs(wire::encode(
            "err-display-id-invalid",
            &[("max", &MAX_DISPLAY_ID)],
        )));
    }
    Ok(DisplayId::new(id))
}

/// Validates a preset name received from a client.
pub fn preset_name(name: &str) -> Result<&str, ApiError> {
    if name.is_empty()
        || name.chars().count() > MAX_PRESET_NAME
        || name.chars().any(char::is_control)
    {
        return Err(ApiError::InvalidArgs(wire::encode(
            "err-preset-name-invalid",
            &[("max", &MAX_PRESET_NAME)],
        )));
    }
    Ok(name)
}
