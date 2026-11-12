//! Daemon access: a tiny trait (to test without a bus) and its
//! D-Bus implementation, with failures classified into exit codes.

use std::collections::HashMap;
use std::future::Future;

use ha2lux_i18n::t;
use ha2lux_ipc::{DaemonProxy, DisplayDto, ErrorKind, classify};
use serde::Serialize;
use zbus::zvariant::OwnedValue;

/// Token: its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Call addressed to the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// Stores the Home Assistant token.
    SetToken(Secret),
    /// Deletes the Home Assistant token.
    ClearToken,
    /// Home Assistant illuminance sensors.
    Sensors,
    /// Master level.
    SetMaster(u32),
    /// Master delta.
    StepMaster(i32),
    /// Direct level of a display.
    SetDisplay {
        /// Display identity.
        id: String,
        /// Level 0-100.
        level: u32,
    },
    /// Preset by name.
    ApplyPreset(String),
    /// Toggleable feature.
    SetMode {
        /// `schedule`, `idle-dim` or `auto`.
        feature: &'static str,
        /// New state.
        on: bool,
    },
    /// Restarts enumeration.
    Rescan,
    /// Reloads the configuration.
    Reload,
    /// Forgets the learned curve.
    ResetCurve,
    /// List of displays.
    List,
    /// Daemon state.
    Status,
}

/// Daemon response.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// Command executed.
    Done,
    /// List of displays.
    List(Vec<DisplayDto>),
    /// Illuminance sensors: (entity, name, room).
    Sensors(Vec<(String, String, String)>),
    /// Daemon state.
    Status(Box<StatusInfo>),
}

/// Call failure, classified by the exit code to return.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// Daemon absent (D-Bus name without owner, no bus): code 2.
    Absent(String),
    /// Daemon did not answer in time: code 4.
    NoReply(String),
    /// Invalid or out-of-range argument, rejected by the daemon: code 1.
    Usage(String),
    /// Command rejected (unknown or unavailable display or preset, etc.): code 3.
    Failed(String),
    /// Other transport anomaly: code 4.
    Other(String),
}

impl CallError {
    /// Exit code.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 1,
            Self::Absent(_) => 2,
            Self::Failed(_) => 3,
            Self::NoReply(_) | Self::Other(_) => 4,
        }
    }

    /// Message for the user.
    pub fn message(&self) -> String {
        match self {
            Self::Absent(detail) => t!("cli-err-daemon-absent", detail = detail.as_str()),
            Self::NoReply(detail) => t!("cli-err-daemon-no-reply", detail = detail.as_str()),
            Self::Usage(m) | Self::Failed(m) | Self::Other(m) => m.clone(),
        }
    }
}

/// Daemon state, as returned by `Status()`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StatusInfo {
    /// Daemon version.
    pub version: String,
    /// Master level.
    pub master: Option<u8>,
    /// Base source (`auto-lux`, `planning`, `static`).
    pub mode: String,
    /// Default step of `up`/`down`.
    pub step: u8,
    /// Schedule enabled.
    pub schedule_enabled: bool,
    /// Number of schedule points.
    pub schedule_points: u32,
    /// Resume time of the suspended schedule.
    pub schedule_resume: Option<String>,
    /// Idle dimming enabled.
    pub idle_enabled: bool,
    /// Dimming level.
    pub idle_level: u8,
    /// Idle delay (minutes).
    pub idle_minutes: u16,
    /// Dimming in progress.
    pub dimmed: bool,
    /// Auto-brightness enabled.
    pub auto_enabled: bool,
    /// Auto-brightness configured.
    pub auto_configured: bool,
    /// Health of the Home Assistant link.
    pub ha_health: String,
    /// Unreadable configuration.
    pub config_error: Option<String>,
    /// Last raw state read from Home Assistant.
    pub ha_raw: Option<String>,
    /// WebSocket stream active.
    pub ha_websocket: bool,
    /// Token rejected by Home Assistant.
    pub ha_auth_rejected: bool,
    /// Token stored.
    pub ha_token_present: bool,
    /// Smoothed lux.
    pub lux_smoothed: Option<f64>,
    /// Most recent lux.
    pub lux_latest: Option<f64>,
    /// Adjustment in progress.
    pub auto_adjusting: bool,
    /// Level targeted by the curve.
    pub auto_target: Option<u8>,
    /// Curve (lux, level).
    pub curve: Vec<(f64, u8)>,
    /// Default curve (nothing learned).
    pub curve_default: bool,
}

fn parse_curve(text: &str) -> Vec<(f64, u8)> {
    text.split(',')
        .filter_map(|pair| {
            let (lux, level) = pair.split_once(':')?;
            Some((lux.parse().ok()?, level.parse().ok()?))
        })
        .collect()
}

fn get<T: TryFrom<OwnedValue>>(map: &HashMap<String, OwnedValue>, key: &str) -> Option<T> {
    map.get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| T::try_from(v).ok())
}

impl StatusInfo {
    /// Reads the daemon's `a{sv}` dictionary; missing keys take their default value.
    pub fn from_map(map: &HashMap<String, OwnedValue>) -> Self {
        let master = get::<i32>(map, "master").and_then(|m| u8::try_from(m).ok());
        Self {
            version: get(map, "version").unwrap_or_default(),
            master,
            mode: get(map, "mode").unwrap_or_default(),
            step: get::<u32>(map, "step")
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(5),
            schedule_enabled: get(map, "schedule_enabled").unwrap_or_default(),
            schedule_points: get(map, "schedule_points").unwrap_or_default(),
            schedule_resume: get(map, "schedule_resume"),
            idle_enabled: get(map, "idle_enabled").unwrap_or_default(),
            idle_level: get::<u32>(map, "idle_level")
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(5),
            idle_minutes: get::<u32>(map, "idle_minutes")
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(5),
            dimmed: get(map, "dimmed").unwrap_or_default(),
            auto_enabled: get(map, "auto_enabled").unwrap_or_default(),
            auto_configured: get(map, "auto_configured").unwrap_or_default(),
            ha_health: get(map, "ha_health").unwrap_or_default(),
            config_error: get(map, "config_error"),
            ha_raw: get(map, "ha_raw"),
            ha_websocket: get(map, "ha_websocket").unwrap_or_default(),
            ha_auth_rejected: get(map, "ha_auth_rejected").unwrap_or_default(),
            ha_token_present: get(map, "ha_token_present").unwrap_or_default(),
            lux_smoothed: get(map, "lux_smoothed"),
            lux_latest: get(map, "lux_latest"),
            auto_adjusting: get(map, "auto_adjusting").unwrap_or_default(),
            auto_target: get::<i32>(map, "auto_target").and_then(|v| u8::try_from(v).ok()),
            curve: get::<String>(map, "curve")
                .map(|c| parse_curve(&c))
                .unwrap_or_default(),
            curve_default: get(map, "curve_default").unwrap_or_default(),
        }
    }
}

/// The daemon as seen by the CLI.
pub trait Daemon {
    /// Runs a call.
    fn call(&self, call: Call) -> impl Future<Output = Result<Answer, CallError>>;
}

/// Real D-Bus client.
pub struct ZbusDaemon {
    connection: zbus::Connection,
}

fn map_error(error: &zbus::Error) -> CallError {
    if let zbus::Error::MethodError(name, detail, _) = error {
        // The daemon sends a catalog key + arguments: we translate it here.
        let detail = ha2lux_i18n::wire::localize(&detail.clone().unwrap_or_default());
        if let Some(kind) = classify(error) {
            return match kind {
                ErrorKind::Usage => CallError::Usage(detail),
                ErrorKind::Failed => CallError::Failed(detail),
            };
        }
        return match name.as_str() {
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner" => {
                CallError::Absent(t!("cli-detail-no-owner"))
            }
            "org.freedesktop.DBus.Error.NoReply"
            | "org.freedesktop.DBus.Error.Timeout"
            | "org.freedesktop.DBus.Error.TimedOut" => CallError::NoReply(t!("cli-detail-timeout")),
            other => CallError::Other(format!("{other} {detail}").trim().to_owned()),
        };
    }
    match error {
        zbus::Error::InputOutput(_) | zbus::Error::Address(_) => {
            CallError::Absent(error.to_string())
        }
        other => CallError::Other(other.to_string()),
    }
}

impl ZbusDaemon {
    /// Connects to the session bus. Any connection error (no bus, invalid
    /// address, missing socket) amounts to being unable to reach the daemon: "absent".
    pub async fn connect_session() -> Result<Self, CallError> {
        let connection = zbus::Connection::session()
            .await
            .map_err(|e| CallError::Absent(e.to_string()))?;
        Ok(Self { connection })
    }

    /// Connects to an explicit bus (private bus of the tests).
    pub async fn connect_address(address: &str) -> Result<Self, CallError> {
        let connection = zbus::connection::Builder::address(address)
            .map_err(|e| CallError::Absent(e.to_string()))?
            .build()
            .await
            .map_err(|e| CallError::Absent(e.to_string()))?;
        Ok(Self { connection })
    }
}

impl Daemon for ZbusDaemon {
    async fn call(&self, call: Call) -> Result<Answer, CallError> {
        let proxy = DaemonProxy::new(&self.connection)
            .await
            .map_err(|e| map_error(&e))?;
        let done = |result: zbus::Result<()>| result.map(|()| Answer::Done);
        let result = match call {
            Call::SetMaster(level) => done(proxy.set_master(level).await),
            Call::StepMaster(delta) => done(proxy.step_master(delta).await),
            Call::SetDisplay { id, level } => done(proxy.set_display(&id, level).await),
            Call::ApplyPreset(name) => done(proxy.apply_preset(&name).await),
            Call::SetMode { feature, on } => done(proxy.set_mode(feature, on).await),
            Call::Rescan => done(proxy.rescan().await),
            Call::Reload => done(proxy.reload().await),
            Call::ResetCurve => done(proxy.reset_curve().await),
            Call::SetToken(Secret(token)) => done(proxy.set_token(&token).await),
            Call::ClearToken => done(proxy.clear_token().await),
            Call::Sensors => proxy.sensors().await.map(Answer::Sensors),
            Call::List => proxy.list().await.map(Answer::List),
            Call::Status => proxy
                .status()
                .await
                .map(|map| Answer::Status(Box::new(StatusInfo::from_map(&map)))),
        };
        result.map_err(|e| map_error(&e))
    }
}
