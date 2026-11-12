//! Accès au démon : un trait minuscule (pour tester sans bus) et son
//! implémentation D-Bus, avec classement des échecs en codes de sortie.

use std::collections::HashMap;
use std::future::Future;

use ha2lux_ipc::{DaemonProxy, DisplayDto, ErrorKind, classify};
use serde::Serialize;
use zbus::zvariant::OwnedValue;

/// Jeton : son `Debug` ne le montre jamais.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Appel adressé au démon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// Enregistre le jeton Home Assistant.
    SetToken(Secret),
    /// Supprime le jeton Home Assistant.
    ClearToken,
    /// Capteurs d'éclairement de Home Assistant.
    Sensors,
    /// Niveau maître.
    SetMaster(u32),
    /// Variation du maître.
    StepMaster(i32),
    /// Niveau direct d'un écran.
    SetDisplay {
        /// Identité de l'écran.
        id: String,
        /// Niveau 0-100.
        level: u32,
    },
    /// Preset par nom.
    ApplyPreset(String),
    /// Fonction activable.
    SetMode {
        /// `schedule`, `idle-dim` ou `auto`.
        feature: &'static str,
        /// Nouvel état.
        on: bool,
    },
    /// Relance l'énumération.
    Rescan,
    /// Recharge la configuration.
    Reload,
    /// Oublie la courbe apprise.
    ResetCurve,
    /// Liste des écrans.
    List,
    /// État du démon.
    Status,
}

/// Réponse du démon.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// Commande exécutée.
    Done,
    /// Liste des écrans.
    List(Vec<DisplayDto>),
    /// Capteurs d'éclairement : (entité, nom, pièce).
    Sensors(Vec<(String, String, String)>),
    /// État du démon.
    Status(Box<StatusInfo>),
}

/// Échec d'un appel, classé selon le code de sortie à renvoyer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// Démon absent (nom D-Bus sans propriétaire, pas de bus) : code 2.
    Absent(String),
    /// Démon sans réponse dans le délai : code 4.
    NoReply(String),
    /// Argument invalide ou hors bornes, refusé par le démon : code 1.
    Usage(String),
    /// Commande refusée (écran ou preset inconnu, indisponible…) : code 3.
    Failed(String),
    /// Autre anomalie de transport : code 4.
    Other(String),
}

impl CallError {
    /// Code de sortie.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 1,
            Self::Absent(_) => 2,
            Self::Failed(_) => 3,
            Self::NoReply(_) | Self::Other(_) => 4,
        }
    }

    /// Message à l'attention de l'utilisateur.
    pub fn message(&self) -> String {
        match self {
            Self::Absent(detail) => format!(
                "le démon ha2luxd est absent ({detail}) ; lancez-le : systemctl --user start ha2luxd"
            ),
            Self::NoReply(detail) => format!("le démon ha2luxd ne répond pas ({detail})"),
            Self::Usage(m) | Self::Failed(m) | Self::Other(m) => m.clone(),
        }
    }
}

/// État du démon, tel que renvoyé par `Status()`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StatusInfo {
    /// Version du démon.
    pub version: String,
    /// Niveau maître.
    pub master: Option<u8>,
    /// Source de base (`auto-lux`, `planning`, `static`).
    pub mode: String,
    /// Pas par défaut de `up`/`down`.
    pub step: u8,
    /// Planning activé.
    pub schedule_enabled: bool,
    /// Nombre de points du planning.
    pub schedule_points: u32,
    /// Heure de reprise du planning suspendu.
    pub schedule_resume: Option<String>,
    /// Atténuation d'inactivité activée.
    pub idle_enabled: bool,
    /// Niveau d'atténuation.
    pub idle_level: u8,
    /// Délai d'inactivité (minutes).
    pub idle_minutes: u16,
    /// Atténuation en cours.
    pub dimmed: bool,
    /// Auto-luminosité activée.
    pub auto_enabled: bool,
    /// Auto-luminosité configurée.
    pub auto_configured: bool,
    /// Santé du lien Home Assistant.
    pub ha_health: String,
    /// Configuration illisible.
    pub config_error: Option<String>,
    /// Dernier état brut lu sur Home Assistant.
    pub ha_raw: Option<String>,
    /// Flux WebSocket actif.
    pub ha_websocket: bool,
    /// Jeton refusé par Home Assistant.
    pub ha_auth_rejected: bool,
    /// Jeton enregistré.
    pub ha_token_present: bool,
    /// Lux lissé.
    pub lux_smoothed: Option<f64>,
    /// Lux le plus récent.
    pub lux_latest: Option<f64>,
    /// Ajustement en cours.
    pub auto_adjusting: bool,
    /// Niveau visé par la courbe.
    pub auto_target: Option<u8>,
    /// Courbe (lux, niveau).
    pub curve: Vec<(f64, u8)>,
    /// Courbe par défaut (rien d'appris).
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
    /// Lit le dictionnaire `a{sv}` du démon ; les clés absentes prennent leur valeur par défaut.
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

/// Le démon vu par la CLI.
pub trait Daemon {
    /// Exécute un appel.
    fn call(&self, call: Call) -> impl Future<Output = Result<Answer, CallError>>;
}

/// Client D-Bus réel.
pub struct ZbusDaemon {
    connection: zbus::Connection,
}

fn map_error(error: &zbus::Error) -> CallError {
    if let zbus::Error::MethodError(name, detail, _) = error {
        let detail = detail.clone().unwrap_or_default();
        if let Some(kind) = classify(error) {
            return match kind {
                ErrorKind::Usage => CallError::Usage(detail),
                ErrorKind::Failed => CallError::Failed(detail),
            };
        }
        return match name.as_str() {
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner" => {
                CallError::Absent("aucun propriétaire pour org.ha2lux.Daemon".to_owned())
            }
            "org.freedesktop.DBus.Error.NoReply"
            | "org.freedesktop.DBus.Error.Timeout"
            | "org.freedesktop.DBus.Error.TimedOut" => {
                CallError::NoReply("délai dépassé".to_owned())
            }
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
    /// Se connecte au bus de session. Toute erreur de connexion (pas de bus, adresse
    /// invalide, socket absent) revient à ne pas pouvoir joindre le démon : « absent ».
    pub async fn connect_session() -> Result<Self, CallError> {
        let connection = zbus::Connection::session()
            .await
            .map_err(|e| CallError::Absent(e.to_string()))?;
        Ok(Self { connection })
    }

    /// Se connecte à un bus explicite (bus privé des tests).
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
