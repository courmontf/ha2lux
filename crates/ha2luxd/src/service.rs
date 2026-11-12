//! Service: bus connection, single instance with takeover, lifecycle.
//!
//! **Never two daemons on a DDC bus.** A new daemon that finds the name
//! already taken asks the old one to quit (`Quit`), waits for it to release the name
//! (at most [`TAKEOVER_TIMEOUT`]), and **refuses to start** if it does not.
//! The old one releases the name only after stopping its display tasks and
//! writing its state: the two daemons therefore never drive the hardware at the same time.

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

/// Time granted to the old instance to release the name.
pub const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(5);

/// Bus to connect to.
#[derive(Clone, Debug)]
pub enum BusKind {
    /// The user's session bus.
    Session,
    /// System bus (logind).
    System,
    /// Explicit address (private bus for tests).
    Address(String),
}

/// System event sources to watch.
#[derive(Clone, Debug, Default)]
pub struct SystemOptions {
    /// Bus on which to listen to logind (sleep, wake-up, lock, active session).
    pub logind_bus: Option<BusKind>,
    /// logind session identifier (`XDG_SESSION_ID`).
    pub session_id: Option<String>,
    /// Listen to kernel events (display hotplug) through netlink.
    pub netlink: bool,
    /// DRM directory polled periodically if netlink is unavailable.
    pub poll_dir: Option<std::path::PathBuf>,
}

impl SystemOptions {
    /// No source (tests).
    pub fn none() -> Self {
        Self::default()
    }

    /// Production sources: logind on the system bus, netlink, fallback by polling sysfs.
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

/// Service failures.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// D-Bus error.
    #[error("D-Bus : {0}")]
    Bus(#[from] zbus::Error),
    /// Low-level D-Bus error.
    #[error("D-Bus : {0}")]
    Fdo(#[from] zbus::fdo::Error),
    /// Unable to take the old instance's place.
    #[error("{0}")]
    Takeover(String),
    /// Unable to start the daemon.
    #[error("startup failed: {0}")]
    Start(#[from] StartError),
}

/// Sources for idle dimming.
#[derive(Clone, Debug, Default)]
pub struct IdleOptions {
    /// Use the Wayland protocol `ext-idle-notify-v1`.
    pub wayland: bool,
    /// Fallback: read idle time from GNOME Mutter over the session bus.
    pub mutter: bool,
    /// System bus for the logind inhibitors.
    pub system_bus: Option<BusKind>,
    /// Query the desktop inhibitors (session bus).
    pub desktop_inhibitors: bool,
    /// Query PipeWire (`pw-dump`): microphone and camera.
    pub pipewire: bool,
    /// `/proc` directory scanned for open V4L2 cameras.
    pub proc_dir: Option<std::path::PathBuf>,
    /// `pw-dump` program (tests).
    pub pw_dump: Option<std::path::PathBuf>,
}

impl IdleOptions {
    /// No source: dimming has nothing to work with (tests).
    pub fn none() -> Self {
        Self::default()
    }

    /// Production sources.
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

/// Service settings.
pub struct ServiceOptions {
    /// Bus to use.
    pub bus: BusKind,
    /// Configuration and state files.
    pub paths: Paths,
    /// Display source.
    pub hardware: Arc<dyn Hardware>,
    /// Clock.
    pub clock: Arc<dyn Clock + Send + Sync>,
    /// Takeover timeout.
    pub takeover_timeout: Duration,
    /// Honor `Quit` (false: test of an old instance that does not release the name).
    pub honor_quit: bool,
    /// Forced display-task settings (tests).
    pub task_config: Option<TaskConfig>,
    /// Schedule evaluation period.
    pub planning_tick: Duration,
    /// Home Assistant (token and source); `None`: auto-brightness unavailable.
    pub ha: Option<crate::ha::HaDeps>,
    /// System event sources.
    pub system: SystemOptions,
    /// Sources for idle dimming.
    pub idle: IdleOptions,
}

impl ServiceOptions {
    /// Production settings.
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

/// Obtains the right to be the single instance: asks the old one to quit and
/// waits for the name to be released, or fails.
async fn make_room(connection: &zbus::Connection, timeout: Duration) -> Result<(), ServiceError> {
    let dbus = DBusProxy::new(connection).await?;
    let name = zbus::names::BusName::try_from(BUS_NAME).map_err(zbus::Error::from)?;
    if !dbus.name_has_owner(name.clone()).await? {
        return Ok(());
    }
    tracing::info!("another instance is running: asking it to exit");
    let proxy = DaemonProxy::new(connection).await?;
    // The old instance may be frozen or of another version: the call is bounded.
    match tokio::time::timeout(timeout, proxy.quit()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the exit request failed"),
        Err(_) => tracing::warn!("the exit request got no reply"),
    }
    let deadline = Instant::now() + timeout;
    while dbus.name_has_owner(name.clone()).await? {
        if Instant::now() >= deadline {
            return Err(ServiceError::Takeover(format!(
                "the running instance did not release {BUS_NAME} within {} s: startup refused (never two daemons on a DDC bus)",
                timeout.as_secs_f32()
            )));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

/// Runs the daemon until `shutdown` or until a `Quit` request.
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
            "{BUS_NAME} was taken by another instance during startup ({reply})"
        )));
    }
    tracing::info!(name = BUS_NAME, "D-Bus service ready");

    let pump = tokio::spawn(pump_signals(connection.clone(), signals_rx));
    let mut watchers = spawn_system_sources(&options.system, &handle).await;
    if options.idle.any_source() {
        watchers.push(spawn_idle(&options.idle, &connection, &handle).await);
    }
    tokio::select! {
        () = shutdown => tracing::info!("shutdown requested"),
        () = handle.wait_closed() => tracing::info!("shutdown requested over D-Bus (Quit)"),
    }
    // State written and display tasks stopped BEFORE releasing the name: the successor
    // only starts afterwards, hence never two daemons on the hardware.
    daemon.shutdown().await;
    if let Err(error) = connection.release_name(BUS_NAME).await {
        tracing::warn!(%error, "releasing the D-Bus name");
    }
    pump.abort();
    for watcher in watchers {
        watcher.abort();
    }
    Ok(())
}

/// Launches the requested system watchers; an unavailable source is reported
/// in the log without preventing the daemon from working.
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
                        tracing::warn!(%error, "logind monitoring stopped");
                    }
                }));
            }
            Err(error) => {
                tracing::warn!(%error, "logind unreachable: sleep and lock not monitored")
            }
        }
    }
    if system.netlink {
        let (handle, poll_dir) = (handle.clone(), system.poll_dir.clone());
        tasks.push(tokio::spawn(async move {
            if let Err(error) = watch_uevents(handle.clone()).await {
                tracing::warn!(%error, "netlink unavailable: falling back to polling sysfs every 3 s");
                if let Some(dir) = poll_dir {
                    poll_connectors(dir, handle, Duration::from_secs(3)).await;
                }
            }
        }));
    }
    tasks
}

/// Launches the dimming supervision: a source (Wayland, otherwise Mutter) is opened
/// when the feature is enabled and restarted if the delay changes; when disabled, nothing is opened.
async fn spawn_idle(
    idle: &IdleOptions,
    session: &zbus::Connection,
    handle: &crate::actor::DaemonHandle,
) -> tokio::task::JoinHandle<()> {
    let system = match &idle.system_bus {
        Some(bus) => connect(bus)
            .await
            .map_err(
                |error| tracing::warn!(%error, "system bus unreachable: logind inhibitors ignored"),
            )
            .ok(),
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
                        tracing::info!(threshold_ms, "dimming: Wayland source ext-idle-notify-v1");
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
                        tracing::warn!(%error, "Wayland idle source unavailable")
                    }
                    Err(error) => tracing::warn!(%error, "Wayland idle source interrupted"),
                }
            }
            if let Some(connection) = mutter {
                tracing::info!(
                    threshold_ms,
                    "dimming: falling back to GNOME Mutter (polling)"
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
            tracing::warn!("no idle source available: dimming will not work");
        }
    }))
}
