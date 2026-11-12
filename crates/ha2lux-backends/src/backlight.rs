//! Backlight backend: reads from `/sys/class/backlight`, writes through
//! `SetBrightness` of logind (no root privileges required).
//!
//! Writing goes through the [`BrightnessSetter`] trait: [`LogindSetter`] in
//! production, a fake in tests. Never a direct write to sysfs.

use std::future::Future;
use std::path::{Path, PathBuf};

use ha2lux_core::{BackendError, DisplayBackend, DisplayId, DisplayInfo, DisplayKind};
use zbus::zvariant::OwnedObjectPath;

/// Standard sysfs directory for backlights.
pub const DEFAULT_SYSFS: &str = "/sys/class/backlight";

/// Controller type declared by the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BacklightType {
    /// Firmware interface (ACPI video): preferred.
    Firmware,
    /// Platform interface.
    Platform,
    /// Direct hardware register.
    Raw,
    /// Type inconnu.
    Other,
}

impl BacklightType {
    fn parse(text: &str) -> Self {
        match text.trim() {
            "firmware" => Self::Firmware,
            "platform" => Self::Platform,
            "raw" => Self::Raw,
            _ => Self::Other,
        }
    }
}

/// A backlight discovered in sysfs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BacklightInfo {
    /// Directory name (e.g. `intel_backlight`), reused by logind.
    pub name: String,
    /// Declared type.
    pub kind: BacklightType,
    /// `max_brightness`.
    pub max: u32,
    /// Sysfs directory.
    pub path: PathBuf,
}

async fn read_u32(path: &Path) -> Option<u32> {
    tokio::fs::read_to_string(path)
        .await
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Enumerates the backlights under `root`. A missing directory yields an empty
/// list; unreadable entries or entries with a zero maximum are ignored.
pub async fn scan(root: &Path) -> Vec<BacklightInfo> {
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return Vec::new();
    };
    let mut found = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(max) = read_u32(&path.join("max_brightness"))
            .await
            .filter(|m| *m > 0)
        else {
            tracing::debug!(%name, "backlight ignored: max_brightness unreadable or zero");
            continue;
        };
        let kind = tokio::fs::read_to_string(path.join("type"))
            .await
            .map_or(BacklightType::Other, |t| BacklightType::parse(&t));
        found.push(BacklightInfo {
            name,
            kind,
            max,
            path,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Preferred backlight: firmware, then platform, then raw (order
/// recommended by the kernel), ties broken alphabetically.
pub fn preferred(list: &[BacklightInfo]) -> Option<&BacklightInfo> {
    list.iter().min_by_key(|b| (b.kind, &b.name))
}

/// Writing the brightness of a backlight.
pub trait BrightnessSetter: Send + 'static {
    /// Applies the raw value to the backlight `name`.
    fn set(
        &mut self,
        name: &str,
        value: u32,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;
}

/// Backend for an internal panel.
pub struct BacklightBackend<S> {
    info: DisplayInfo,
    path: PathBuf,
    name: String,
    setter: S,
}

impl<S: BrightnessSetter> BacklightBackend<S> {
    /// Creates the backend for a backlight discovered by [`scan`].
    pub fn new(device: &BacklightInfo, setter: S) -> Self {
        Self {
            info: DisplayInfo {
                id: DisplayId::new(format!("backlight:{}", device.name)),
                name: device.name.clone(),
                kind: DisplayKind::Internal,
                max: device.max,
            },
            path: device.path.clone(),
            name: device.name.clone(),
            setter,
        }
    }
}

impl<S: BrightnessSetter> DisplayBackend for BacklightBackend<S> {
    fn info(&self) -> &DisplayInfo {
        &self.info
    }

    async fn probe(&mut self) -> Result<DisplayInfo, BackendError> {
        let max = read_u32(&self.path.join("max_brightness"))
            .await
            .ok_or_else(|| BackendError::Disconnected("max_brightness illisible".to_owned()))?;
        if max == 0 {
            return Err(BackendError::Invalid("max_brightness nul".to_owned()));
        }
        self.info.max = max;
        Ok(self.info.clone())
    }

    async fn read(&mut self) -> Result<u32, BackendError> {
        // `actual_brightness` reflects the real state when it exists.
        for file in ["actual_brightness", "brightness"] {
            if let Some(value) = read_u32(&self.path.join(file)).await {
                return Ok(value);
            }
        }
        Err(BackendError::Disconnected(
            "brightness unreadable".to_owned(),
        ))
    }

    async fn write(&mut self, raw: u32) -> Result<(), BackendError> {
        if raw > self.info.max {
            return Err(BackendError::Invalid(format!(
                "valeur {raw} au-dessus du maximum {}",
                self.info.max
            )));
        }
        self.setter.set(&self.name, raw).await
    }
}

// ---- logind ----------------------------------------------------------------

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    fn get_session(&self, id: &str) -> zbus::Result<OwnedObjectPath>;
    fn get_user(&self, uid: u32) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1"
)]
trait User {
    #[zbus(property)]
    fn display(&self) -> zbus::Result<(String, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Session {
    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> zbus::Result<()>;
}

/// Classifies a D-Bus error by its name (pure function, testable).
pub fn classify_dbus_error(name: &str) -> fn(String) -> BackendError {
    match name {
        "org.freedesktop.DBus.Error.AccessDenied"
        | "org.freedesktop.DBus.Error.AuthFailed"
        | "org.freedesktop.PolicyKit1.Error.NotAuthorized" => BackendError::PermissionDenied,
        "org.freedesktop.DBus.Error.InvalidArgs"
        | "org.freedesktop.DBus.Error.UnknownObject"
        | "org.freedesktop.DBus.Error.UnknownMethod" => BackendError::Invalid,
        _ => BackendError::Transient,
    }
}

fn map_zbus_error(error: &zbus::Error) -> BackendError {
    match error {
        zbus::Error::MethodError(name, detail, _) => {
            let detail = detail.clone().unwrap_or_default();
            classify_dbus_error(name.as_str())(format!("{name} {detail}").trim().to_owned())
        }
        other => BackendError::Transient(other.to_string()),
    }
}

/// Write through `org.freedesktop.login1.Session.SetBrightness`.
///
/// The connection and the session are resolved lazily on the first
/// `set`: a plain read never touches the bus.
#[derive(Default)]
pub struct LogindSetter {
    connection: Option<zbus::Connection>,
    session: Option<OwnedObjectPath>,
}

/// Resolves the path of the logind object for the user's session: the session
/// `session_id` if given (`XDG_SESSION_ID` variable), otherwise the user's
/// graphical session (`User.Display`), which also works from a
/// `systemd --user` service that is not in a session scope.
pub async fn resolve_session_path(
    connection: &zbus::Connection,
    session_id: Option<&str>,
) -> Result<OwnedObjectPath, BackendError> {
    let manager = ManagerProxy::new(connection)
        .await
        .map_err(|e| map_zbus_error(&e))?;
    match session_id.filter(|id| !id.is_empty()) {
        Some(id) => manager
            .get_session(id)
            .await
            .map_err(|e| map_zbus_error(&e)),
        None => {
            let uid = current_uid()?;
            let user_path = manager
                .get_user(uid)
                .await
                .map_err(|e| map_zbus_error(&e))?;
            let user = UserProxy::builder(connection)
                .path(user_path)
                .map_err(|e| map_zbus_error(&e))?
                .build()
                .await
                .map_err(|e| map_zbus_error(&e))?;
            Ok(user.display().await.map_err(|e| map_zbus_error(&e))?.1)
        }
    }
}

impl LogindSetter {
    /// Creates an unconnected writer.
    pub fn new() -> Self {
        Self::default()
    }

    async fn session_path(&mut self) -> Result<(zbus::Connection, OwnedObjectPath), BackendError> {
        if self.connection.is_none() {
            let connection = zbus::Connection::system()
                .await
                .map_err(|e| BackendError::Transient(format!("bus système : {e}")))?;
            self.connection = Some(connection);
        }
        let connection = self
            .connection
            .clone()
            .ok_or_else(|| BackendError::Transient("system bus unavailable".to_owned()))?;
        if let Some(path) = &self.session {
            return Ok((connection, path.clone()));
        }
        let session_id = std::env::var("XDG_SESSION_ID").ok();
        let path = resolve_session_path(&connection, session_id.as_deref()).await?;
        self.session = Some(path.clone());
        Ok((connection, path))
    }
}

fn current_uid() -> Result<u32, BackendError> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .map_err(|e| BackendError::Invalid(format!("uid introuvable : {e}")))
}

impl BrightnessSetter for LogindSetter {
    async fn set(&mut self, name: &str, value: u32) -> Result<(), BackendError> {
        let (connection, path) = self.session_path().await?;
        let session = SessionProxy::builder(&connection)
            .path(path)
            .map_err(|e| map_zbus_error(&e))?
            .build()
            .await
            .map_err(|e| map_zbus_error(&e))?;
        match session.set_brightness("backlight", name, value).await {
            Ok(()) => Ok(()),
            Err(error) => {
                // The session may have changed (reconnection): it will be resolved again.
                self.session = None;
                Err(map_zbus_error(&error))
            }
        }
    }
}
