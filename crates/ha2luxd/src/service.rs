//! Service : connexion au bus, instance unique avec remplacement, cycle de vie.
//!
//! **Jamais deux démons sur un bus DDC.** Un nouveau démon qui trouve le nom
//! déjà pris demande à l'ancien de quitter (`Quit`), attend qu'il libère le nom
//! (au plus [`TAKEOVER_TIMEOUT`]), et **refuse de démarrer** s'il ne le fait pas.
//! L'ancien libère le nom seulement après avoir arrêté ses tâches d'écran et
//! écrit son état : les deux démons ne pilotent donc jamais le matériel en même temps.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use ha2lux_backends::task::TaskConfig;
use ha2lux_core::Clock;
use ha2lux_ipc::{BUS_NAME, DaemonProxy, OBJECT_PATH};
use thiserror::Error;
use tokio::time::Instant;
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};

use crate::actor::{self, Deps, StartError};
use crate::blockers::{BlockerOptions, SystemBlockers};
use crate::dbus::{DaemonInterface, pump_signals};
use crate::dbus_idle::MutterIdleTime;
use crate::hardware::Hardware;
use crate::idle::{PROBE_PERIOD, drive_events, drive_polled, supervise};
use crate::paths::Paths;
use crate::system::{poll_connectors, watch_logind, watch_uevents};
use crate::wayland::WaylandIdle;

/// Délai accordé à l'ancienne instance pour libérer le nom.
pub const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(5);

/// Bus sur lequel se connecter.
#[derive(Clone, Debug)]
pub enum BusKind {
    /// Bus de session de l'utilisateur.
    Session,
    /// Bus système (logind).
    System,
    /// Adresse explicite (bus privé des tests).
    Address(String),
}

/// Sources d'événements système à surveiller.
#[derive(Clone, Debug, Default)]
pub struct SystemOptions {
    /// Bus sur lequel écouter logind (veille, réveil, verrouillage, session active).
    pub logind_bus: Option<BusKind>,
    /// Identifiant de session logind (`XDG_SESSION_ID`).
    pub session_id: Option<String>,
    /// Écouter les événements du noyau (hotplug d'écrans) par netlink.
    pub netlink: bool,
    /// Répertoire DRM sondé périodiquement si netlink est indisponible.
    pub poll_dir: Option<std::path::PathBuf>,
}

impl SystemOptions {
    /// Aucune source (tests).
    pub fn none() -> Self {
        Self::default()
    }

    /// Sources de production : logind sur le bus système, netlink, repli par sondage de sysfs.
    pub fn production() -> Self {
        Self {
            logind_bus: Some(BusKind::System),
            session_id: std::env::var("XDG_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            netlink: true,
            poll_dir: Some(std::path::PathBuf::from(
                ha2lux_backends::drm::DEFAULT_DRM_DIR,
            )),
        }
    }
}

/// Échecs du service.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// Erreur D-Bus.
    #[error("D-Bus : {0}")]
    Bus(#[from] zbus::Error),
    /// Erreur D-Bus de bas niveau.
    #[error("D-Bus : {0}")]
    Fdo(#[from] zbus::fdo::Error),
    /// Impossible de prendre la place de l'ancienne instance.
    #[error("{0}")]
    Takeover(String),
    /// Démarrage du démon impossible.
    #[error("démarrage impossible : {0}")]
    Start(#[from] StartError),
}

/// Sources de l'atténuation à l'inactivité.
#[derive(Clone, Debug, Default)]
pub struct IdleOptions {
    /// Utiliser le protocole Wayland `ext-idle-notify-v1`.
    pub wayland: bool,
    /// Repli : lire l'inactivité chez GNOME Mutter par le bus de session.
    pub mutter: bool,
    /// Bus système pour les inhibiteurs logind.
    pub system_bus: Option<BusKind>,
    /// Interroger les inhibiteurs du bureau (bus de session).
    pub desktop_inhibitors: bool,
    /// Interroger PipeWire (`pw-dump`) : microphone et caméra.
    pub pipewire: bool,
    /// Répertoire `/proc` parcouru pour les caméras V4L2 ouvertes.
    pub proc_dir: Option<std::path::PathBuf>,
    /// Programme `pw-dump` (tests).
    pub pw_dump: Option<std::path::PathBuf>,
}

impl IdleOptions {
    /// Aucune source : l'atténuation n'a pas de quoi fonctionner (tests).
    pub fn none() -> Self {
        Self::default()
    }

    /// Sources de production.
    pub fn production() -> Self {
        Self {
            wayland: std::env::var_os("WAYLAND_DISPLAY").is_some(),
            mutter: true,
            system_bus: Some(BusKind::System),
            desktop_inhibitors: true,
            pipewire: true,
            proc_dir: Some(std::path::PathBuf::from("/proc")),
            pw_dump: None,
        }
    }

    fn any_source(&self) -> bool {
        self.wayland || self.mutter
    }
}

/// Réglages du service.
pub struct ServiceOptions {
    /// Bus à utiliser.
    pub bus: BusKind,
    /// Fichiers de configuration et d'état.
    pub paths: Paths,
    /// Source d'écrans.
    pub hardware: Arc<dyn Hardware>,
    /// Horloge.
    pub clock: Arc<dyn Clock + Send + Sync>,
    /// Délai de prise de relais.
    pub takeover_timeout: Duration,
    /// Honorer `Quit` (faux : test d'une ancienne instance qui ne libère pas le nom).
    pub honor_quit: bool,
    /// Réglages de tâche d'écran imposés (tests).
    pub task_config: Option<TaskConfig>,
    /// Période d'évaluation du planning.
    pub planning_tick: Duration,
    /// Home Assistant (jeton et source) ; `None` : auto-luminosité indisponible.
    pub ha: Option<crate::ha::HaDeps>,
    /// Sources d'événements système.
    pub system: SystemOptions,
    /// Sources de l'atténuation à l'inactivité.
    pub idle: IdleOptions,
}

impl ServiceOptions {
    /// Réglages de production.
    pub fn production(
        paths: Paths,
        hardware: Arc<dyn Hardware>,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Self {
        Self {
            bus: BusKind::Session,
            paths,
            hardware,
            clock,
            takeover_timeout: TAKEOVER_TIMEOUT,
            honor_quit: true,
            task_config: None,
            planning_tick: Duration::from_secs(60),
            ha: None,
            system: SystemOptions::production(),
            idle: IdleOptions::production(),
        }
    }
}

async fn connect(bus: &BusKind) -> Result<zbus::Connection, ServiceError> {
    Ok(match bus {
        BusKind::Session => zbus::connection::Builder::session()?.build().await?,
        BusKind::System => zbus::connection::Builder::system()?.build().await?,
        BusKind::Address(address) => {
            zbus::connection::Builder::address(address.as_str())?
                .build()
                .await?
        }
    })
}

/// Obtient le droit d'être l'unique instance : demande à l'ancienne de quitter et
/// attend la libération du nom, ou échoue.
async fn make_room(connection: &zbus::Connection, timeout: Duration) -> Result<(), ServiceError> {
    let dbus = DBusProxy::new(connection).await?;
    let name = zbus::names::BusName::try_from(BUS_NAME).map_err(zbus::Error::from)?;
    if !dbus.name_has_owner(name.clone()).await? {
        return Ok(());
    }
    tracing::info!("une autre instance est active : demande de sortie");
    let proxy = DaemonProxy::new(connection).await?;
    // L'ancienne instance peut être figée ou d'une autre version : l'appel est borné.
    match tokio::time::timeout(timeout, proxy.quit()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "la demande de sortie a échoué"),
        Err(_) => tracing::warn!("la demande de sortie est restée sans réponse"),
    }
    let deadline = Instant::now() + timeout;
    while dbus.name_has_owner(name.clone()).await? {
        if Instant::now() >= deadline {
            return Err(ServiceError::Takeover(format!(
                "l'instance en cours n'a pas libéré {BUS_NAME} en {} s : démarrage refusé (jamais deux démons sur un bus DDC)",
                timeout.as_secs_f32()
            )));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

/// Fait vivre le démon jusqu'à `shutdown` ou jusqu'à une demande `Quit`.
pub async fn run(
    options: ServiceOptions,
    shutdown: impl Future<Output = ()>,
) -> Result<(), ServiceError> {
    let connection = connect(&options.bus).await?;
    make_room(&connection, options.takeover_timeout).await?;

    let (signals_tx, signals_rx) = tokio::sync::mpsc::unbounded_channel();
    let daemon = actor::start(Deps {
        paths: options.paths,
        hardware: options.hardware,
        clock: options.clock,
        signals: signals_tx,
        task_config: options.task_config,
        planning_tick: options.planning_tick,
        ha: options.ha,
    })
    .await?;
    let handle = daemon.handle();

    connection
        .object_server()
        .at(
            OBJECT_PATH,
            DaemonInterface::new(handle.clone(), options.honor_quit),
        )
        .await?;
    let reply = connection
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await?;
    if !matches!(
        reply,
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner
    ) {
        daemon.shutdown().await;
        return Err(ServiceError::Takeover(format!(
            "{BUS_NAME} a été pris par une autre instance pendant le démarrage ({reply})"
        )));
    }
    tracing::info!(name = BUS_NAME, "service D-Bus prêt");

    let pump = tokio::spawn(pump_signals(connection.clone(), signals_rx));
    let mut watchers = spawn_system_sources(&options.system, &handle).await;
    if options.idle.any_source() {
        watchers.push(spawn_idle(&options.idle, &connection, &handle).await);
    }
    tokio::select! {
        () = shutdown => tracing::info!("arrêt demandé"),
        () = handle.wait_closed() => tracing::info!("arrêt demandé par D-Bus (Quit)"),
    }
    // État écrit et tâches d'écran arrêtées AVANT de libérer le nom : le successeur ne
    // démarre qu'ensuite, donc jamais deux démons sur le matériel.
    daemon.shutdown().await;
    if let Err(error) = connection.release_name(BUS_NAME).await {
        tracing::warn!(%error, "libération du nom D-Bus");
    }
    pump.abort();
    for watcher in watchers {
        watcher.abort();
    }
    Ok(())
}

/// Lance les surveillances système demandées ; une source indisponible est signalée
/// dans le journal sans empêcher le démon de fonctionner.
async fn spawn_system_sources(
    system: &SystemOptions,
    handle: &crate::actor::DaemonHandle,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut tasks = Vec::new();
    if let Some(bus) = &system.logind_bus {
        match connect(bus).await {
            Ok(connection) => {
                let (handle, session_id) = (handle.clone(), system.session_id.clone());
                tasks.push(tokio::spawn(async move {
                    if let Err(error) = watch_logind(connection, session_id, handle).await {
                        tracing::warn!(%error, "surveillance logind arrêtée");
                    }
                }));
            }
            Err(error) => {
                tracing::warn!(%error, "logind inaccessible : veille et verrouillage non surveillés")
            }
        }
    }
    if system.netlink {
        let (handle, poll_dir) = (handle.clone(), system.poll_dir.clone());
        tasks.push(tokio::spawn(async move {
            if let Err(error) = watch_uevents(handle.clone()).await {
                tracing::warn!(%error, "netlink indisponible : repli par sondage de sysfs toutes les 3 s");
                if let Some(dir) = poll_dir {
                    poll_connectors(dir, handle, Duration::from_secs(3)).await;
                }
            }
        }));
    }
    tasks
}

/// Lance la supervision de l'atténuation : une source (Wayland, sinon Mutter) est ouverte
/// quand la fonction est activée et relancée si le délai change ; désactivée, rien n'est ouvert.
async fn spawn_idle(
    idle: &IdleOptions,
    session: &zbus::Connection,
    handle: &crate::actor::DaemonHandle,
) -> tokio::task::JoinHandle<()> {
    let system = match &idle.system_bus {
        Some(bus) => connect(bus).await.map_err(|error| tracing::warn!(%error, "bus système inaccessible : inhibiteurs logind ignorés")).ok(),
        None => None,
    };
    let blockers = BlockerOptions {
        system,
        session: idle.desktop_inhibitors.then(|| session.clone()),
        pipewire: idle.pipewire,
        proc_dir: idle.proc_dir.clone(),
        pw_dump: idle.pw_dump.clone(),
    };
    let (wayland, mutter) = (idle.wayland, idle.mutter.then(|| session.clone()));
    let driver_handle = handle.clone();
    tokio::spawn(supervise(handle.clone(), move |threshold_ms| {
        let (blockers, mutter, handle) = (blockers.clone(), mutter.clone(), driver_handle.clone());
        async move {
            let mut blockers = SystemBlockers::new(blockers);
            if wayland {
                let timeout = u32::try_from(threshold_ms).unwrap_or(u32::MAX);
                match tokio::task::spawn_blocking(move || WaylandIdle::connect(timeout)).await {
                    Ok(Ok(mut source)) => {
                        tracing::info!(
                            threshold_ms,
                            "atténuation : source Wayland ext-idle-notify-v1"
                        );
                        drive_events(
                            &handle,
                            &mut source,
                            &mut blockers,
                            threshold_ms,
                            PROBE_PERIOD,
                        )
                        .await;
                        return;
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "source d'inactivité Wayland indisponible")
                    }
                    Err(error) => tracing::warn!(%error, "source d'inactivité Wayland interrompue"),
                }
            }
            if let Some(connection) = mutter {
                tracing::info!(
                    threshold_ms,
                    "atténuation : repli sur GNOME Mutter (sondage)"
                );
                let mut time = MutterIdleTime::new(connection);
                drive_polled(
                    &handle,
                    &mut time,
                    &mut blockers,
                    threshold_ms,
                    PROBE_PERIOD,
                )
                .await;
                return;
            }
            tracing::warn!(
                "aucune source d'inactivité disponible : l'atténuation ne fonctionnera pas"
            );
        }
    }))
}
