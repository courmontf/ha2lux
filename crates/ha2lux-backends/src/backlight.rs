//! Backend backlight : lecture dans `/sys/class/backlight`, écriture par
//! `SetBrightness` de logind (aucun droit root requis).
//!
//! L'écriture passe par le trait [`BrightnessSetter`] : [`LogindSetter`] en
//! production, un faux dans les tests. Jamais d'écriture directe dans sysfs.

use std::future::Future;
use std::path::{Path, PathBuf};

use ha2lux_core::{BackendError, DisplayBackend, DisplayId, DisplayInfo, DisplayKind};
use zbus::zvariant::OwnedObjectPath;

/// Répertoire sysfs standard des rétroéclairages.
pub const DEFAULT_SYSFS: &str = "/sys/class/backlight";

/// Type de contrôleur déclaré par le noyau.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BacklightType {
    /// Interface firmware (ACPI vidéo) : à préférer.
    Firmware,
    /// Interface de la plateforme.
    Platform,
    /// Registre matériel direct.
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

/// Un rétroéclairage découvert dans sysfs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BacklightInfo {
    /// Nom du répertoire (ex. `intel_backlight`), repris par logind.
    pub name: String,
    /// Type déclaré.
    pub kind: BacklightType,
    /// `max_brightness`.
    pub max: u32,
    /// Répertoire sysfs.
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

/// Énumère les rétroéclairages de `root`. Un répertoire absent donne une liste
/// vide ; les entrées illisibles ou de maximum nul sont ignorées.
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
            tracing::debug!(%name, "rétroéclairage ignoré : max_brightness illisible ou nul");
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

/// Rétroéclairage à préférer : firmware, puis platform, puis raw (ordre
/// recommandé par le noyau), à égalité l'ordre alphabétique.
pub fn preferred(list: &[BacklightInfo]) -> Option<&BacklightInfo> {
    list.iter().min_by_key(|b| (b.kind, &b.name))
}

/// Écriture de la luminosité d'un rétroéclairage.
pub trait BrightnessSetter: Send + 'static {
    /// Applique la valeur brute au rétroéclairage `name`.
    fn set(
        &mut self,
        name: &str,
        value: u32,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;
}

/// Backend d'un panneau interne.
pub struct BacklightBackend<S> {
    info: DisplayInfo,
    path: PathBuf,
    name: String,
    setter: S,
}

impl<S: BrightnessSetter> BacklightBackend<S> {
    /// Crée le backend d'un rétroéclairage découvert par [`scan`].
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
        // `actual_brightness` reflète l'état réel quand il existe.
        for file in ["actual_brightness", "brightness"] {
            if let Some(value) = read_u32(&self.path.join(file)).await {
                return Ok(value);
            }
        }
        Err(BackendError::Disconnected(
            "luminosité illisible".to_owned(),
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

/// Classe une erreur D-Bus par son nom (fonction pure, testable).
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

/// Écriture via `org.freedesktop.login1.Session.SetBrightness`.
///
/// La connexion et la session sont résolues paresseusement au premier
/// `set` : une simple lecture ne touche jamais au bus.
#[derive(Default)]
pub struct LogindSetter {
    connection: Option<zbus::Connection>,
    session: Option<OwnedObjectPath>,
}

/// Résout le chemin de l'objet logind de la session de l'utilisateur : la session
/// `session_id` si elle est donnée (variable `XDG_SESSION_ID`), sinon la session
/// graphique de l'utilisateur (`User.Display`), valable aussi depuis un service
/// `systemd --user`, qui n'est pas dans un scope de session.
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
    /// Crée un écrivain non connecté.
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
            .ok_or_else(|| BackendError::Transient("bus système indisponible".to_owned()))?;
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
                // La session a pu changer (reconnexion) : on la résoudra de nouveau.
                self.session = None;
                Err(map_zbus_error(&error))
            }
        }
    }
}
