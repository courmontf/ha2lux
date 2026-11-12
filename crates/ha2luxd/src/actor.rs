//! Decision actor: the **sole** owner of the control state.
//!
//! All the rest (D-Bus, timers, display tasks, system events) sends it
//! messages; only it calls the state machine and executes its
//! commands. Intended consequences:
//! - requests are **serialized** (writes never interleave);
//! - **no blocking I/O** in the actor: hardware is driven by
//!   display tasks, files by a dedicated writer, discovery by a
//!   separate task;
//! - a published state (`watch`) that the interface reads without ever locking.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ha2lux_backends::task::{DisplayEvent, DisplayHandle, TaskConfig, spawn_display_task};
use ha2lux_core::config::{Config, DisplayEntry};
use ha2lux_core::hw::{backlight_raw_from_level, raw_from_level};
use ha2lux_core::{
    BaseMode, Clock, Command, Controller, ControllerConfig, DisplayId, DisplayKind, DisplayState,
    Event, Feature, InitialState, Listed, Notice, Persist, Range, RescanAction, RescanConfig,
    RescanManager, RescanTrigger, ScanOutcome, ScheduleError, State,
};
use ha2lux_i18n::wire;
use ha2lux_ipc::{DisplayDto, FEATURE_AUTO, FEATURE_IDLE_DIM, FEATURE_SCHEDULE};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::api::{self, ApiError, Reply, Request, Secret, StatusSnapshot};
use crate::ha::HaDeps;
use crate::hardware::{DiscoverOptions, Found, Hardware};
use crate::paths::Paths;
use crate::store::{self, ConfigLoad, StoreError};
use ha2lux_ha::source::{HaSource, Output};

/// Maximum number of displays remembered in the configuration.
const MAX_REMEMBERED_DISPLAYS: usize = 32;
/// Capacity of the actor's mailbox.
const MAILBOX: usize = 256;

/// Signals to emit to D-Bus clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// The master has changed.
    LevelChanged(u8),
    /// The list or state of the displays has changed.
    DisplaysChanged,
    /// The base source has changed.
    ModeChanged(String),
    /// A write has permanently failed.
    DisplayFailed {
        /// Display concerned.
        id: String,
        /// Human-readable reason.
        reason: String,
    },
}

/// Actor dependencies.
pub struct Deps {
    /// Configuration and state files.
    pub paths: Paths,
    /// Display source.
    pub hardware: Arc<dyn Hardware>,
    /// Clock (real, or driven by tokio in tests).
    pub clock: Arc<dyn Clock + Send + Sync>,
    /// Destination of the D-Bus signals.
    pub signals: mpsc::UnboundedSender<Signal>,
    /// Forced display-task settings (tests); `None`: depends on the display type.
    pub task_config: Option<TaskConfig>,
    /// Schedule evaluation period.
    pub planning_tick: Duration,
    /// Home Assistant (token and measurement source); `None`: auto-brightness unavailable.
    pub ha: Option<HaDeps>,
}

/// Startup failure.
#[derive(Debug, Error)]
pub enum StartError {
    /// Unreadable file or unsupported version.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Invalid schedule (must not happen after validation).
    #[error("invalid schedule: {0}")]
    Schedule(#[from] ScheduleError),
}

/// Event coming from the system (logind, udev...), forwarded to the actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemEvent {
    /// Preparing for sleep: no more hardware writes (DDC during sleep = hang).
    Suspend,
    /// Resume from sleep.
    Resume,
    /// Session locked: nothing to do (like screen blanking).
    Lock,
    /// Session unlocked.
    Unlock,
    /// The session becomes active or inactive (session switch, multi-session).
    SessionActive(bool),
    /// Display plugged or unplugged (DRM, i2c node).
    Hotplug,
}

enum FlushKind {
    State,
    Config,
}

enum Msg {
    Request {
        request: Request,
        reply: oneshot::Sender<Result<Reply, ApiError>>,
    },
    Event(Event),
    Discovered {
        generation: u64,
        found: Vec<Found>,
    },
    RescanTimer,
    System(SystemEvent),
    Flush(FlushKind),
    /// Result of reading the token for this address and this entity.
    Token {
        key: String,
        entity: String,
        token: Option<String>,
    },
    Shutdown(oneshot::Sender<()>),
}

/// Cloneable handle to the actor (D-Bus, system adapters, tests).
#[derive(Clone)]
pub struct DaemonHandle {
    tx: mpsc::Sender<Msg>,
    snapshot: watch::Receiver<Arc<StatusSnapshot>>,
    closed: watch::Receiver<bool>,
}

impl DaemonHandle {
    /// Sends a request and waits for its reply. Requests are serialized.
    pub async fn request(&self, request: Request) -> Result<Reply, ApiError> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(Msg::Request { request, reply })
            .await
            .map_err(|_| ApiError::Stopped)?;
        answer.await.map_err(|_| ApiError::Stopped)?
    }

    /// Injects a system event (wake-up, idle, lux measurement...).
    pub async fn send_event(&self, event: Event) -> Result<(), ApiError> {
        self.tx
            .send(Msg::Event(event))
            .await
            .map_err(|_| ApiError::Stopped)
    }

    /// Injects a system event (logind, udev).
    pub async fn send_system(&self, event: SystemEvent) -> Result<(), ApiError> {
        self.tx
            .send(Msg::System(event))
            .await
            .map_err(|_| ApiError::Stopped)
    }

    /// Latest published state (lock-free read).
    pub fn snapshot(&self) -> Arc<StatusSnapshot> {
        Arc::clone(&self.snapshot.borrow())
    }

    /// Stream of published states.
    pub fn subscribe(&self) -> watch::Receiver<Arc<StatusSnapshot>> {
        self.snapshot.clone()
    }

    /// Waits for the actor to stop (`Quit` request, shutdown or end of the mailbox).
    pub async fn wait_closed(&self) {
        let mut closed = self.closed.clone();
        // An error means the actor is gone: it is indeed stopped.
        let _ = closed.wait_for(|c| *c).await;
    }
}

/// The running actor.
pub struct Daemon {
    handle: DaemonHandle,
    join: JoinHandle<()>,
}

impl Daemon {
    /// Cloneable handle.
    pub fn handle(&self) -> DaemonHandle {
        self.handle.clone()
    }

    /// Clean shutdown: state written, final hardware writes done, tasks stopped.
    pub async fn shutdown(self) {
        let (done, finished) = oneshot::channel();
        if self.handle.tx.send(Msg::Shutdown(done)).await.is_ok() {
            // The actor may have stopped in the meantime (Quit): irrelevant.
            let _ = finished.await;
        }
        let _ = self.join.await;
    }
}

struct Persister {
    tx: mpsc::UnboundedSender<WriteJob>,
    join: JoinHandle<()>,
}

struct WriteJob {
    path: PathBuf,
    bytes: Vec<u8>,
    backup_first: bool,
}

impl Persister {
    fn spawn() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<WriteJob>();
        let join = tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let path = job.path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    if job.backup_first {
                        store::backup(&job.path)?;
                    }
                    store::write_atomic(&job.path, &job.bytes)
                })
                .await;
                match result {
                    Ok(Ok(())) => tracing::debug!(path = %path.display(), "file written"),
                    Ok(Err(error)) => {
                        tracing::error!(path = %path.display(), %error, "write failed")
                    }
                    Err(error) => {
                        tracing::error!(path = %path.display(), %error, "write interrupted")
                    }
                }
            }
        });
        Self { tx, join }
    }

    fn write(&self, job: WriteJob) {
        // The receiver only goes away at shutdown.
        let _ = self.tx.send(job);
    }

    async fn finish(self) {
        drop(self.tx);
        let _ = self.join.await;
    }
}

struct Known {
    name: String,
    kind: DisplayKind,
    max: u32,
}

struct Runtime {
    handle: DisplayHandle,
    kind: DisplayKind,
    max: u32,
    bus: Option<u32>,
    floor: u32,
    /// Last commanded level and its raw value.
    last: Option<(u8, u32)>,
    /// Last raw value confirmed as written.
    confirmed_raw: Option<u32>,
}

struct Actor {
    tx: mpsc::Sender<Msg>,
    display_tx: mpsc::UnboundedSender<DisplayEvent>,
    deps: Deps,
    ctl: Controller,
    config: Config,
    config_error: Option<String>,
    config_backed_up: bool,
    queue: VecDeque<Event>,
    runtime: BTreeMap<DisplayId, Runtime>,
    known: BTreeMap<DisplayId, Known>,
    last_display: Option<String>,
    persister: Option<Persister>,
    state_due: Option<Instant>,
    config_due: Option<Instant>,
    rescan: RescanManager,
    armed_deadline: Option<u64>,
    /// Sleep or inactive session: no hardware writes.
    suspended: bool,
    first_discovery_done: bool,
    /// Cumulative counters of display tasks already stopped (they do not vanish with the display).
    retired: ha2lux_backends::task::DisplayStats,
    /// Writes in progress are re-applications (wake-up, accepted list): forced.
    force_writes: bool,
    snapshot_tx: watch::Sender<Arc<StatusSnapshot>>,
    previous: Arc<StatusSnapshot>,
    ha: HaState,
}

/// State of the Home Assistant link on the daemon side.
#[derive(Default)]
struct HaState {
    source: Option<HaSource>,
    /// Address and entity of the current source.
    source_key: Option<(String, String)>,
    token_present: bool,
    raw: Option<String>,
    websocket: bool,
    auth_rejected: bool,
}

fn mode_name(mode: BaseMode) -> &'static str {
    match mode {
        BaseMode::AutoLux => "auto-lux",
        BaseMode::Planning => "planning",
        BaseMode::Static => "static",
    }
}

fn state_name(state: DisplayState) -> &'static str {
    match state {
        DisplayState::Controllable => "controllable",
        DisplayState::Unavailable => "unavailable",
        DisplayState::NotControllable => "not-controllable",
        DisplayState::Absent => "absent",
    }
}

fn controller_config(
    config: &Config,
    auto_configured: bool,
) -> Result<ControllerConfig, ScheduleError> {
    ControllerConfig::from_config(config, auto_configured)
}

/// Starts the daemon: loads configuration and state, launches the actor, the
/// first enumeration and the periodic schedule evaluation.
pub async fn start(deps: Deps) -> Result<Daemon, StartError> {
    let paths = deps.paths.clone();
    let (config, config_error) = {
        let path = paths.config_file.clone();
        let loaded = tokio::task::spawn_blocking(move || store::load_or_create_config(&path))
            .await
            .map_err(|e| StoreError::Io {
                path: paths.config_file.clone(),
                source: std::io::Error::other(e),
            })??;
        match loaded {
            ConfigLoad::Created(l) | ConfigLoad::Loaded(l) => {
                for issue in &l.issues {
                    tracing::warn!(key = %issue.path, "configuration: {}", issue.message);
                }
                (l.config, None)
            }
            ConfigLoad::Invalid { error } => {
                tracing::error!(%error, "unreadable configuration: defaults used, file preserved and never rewritten");
                (Config::default(), Some(error.to_string()))
            }
        }
    };
    let state_path = paths.state_file.clone();
    let loaded_state = tokio::task::spawn_blocking(move || store::load_state(&state_path))
        .await
        .unwrap_or_default();
    for issue in &loaded_state.issues {
        tracing::warn!(key = %issue.path, "state: {}", issue.message);
    }

    let ranges: BTreeMap<DisplayId, Range> = config
        .displays
        .iter()
        .map(|d| (DisplayId::new(d.id.clone()), d.range))
        .collect();
    let ctl = Controller::new(
        controller_config(&config, false)?,
        InitialState {
            master: loaded_state.state.master,
            suspension: loaded_state.state.suspension,
            curve: loaded_state.state.curve,
            ranges,
        },
    );

    let (tx, rx) = mpsc::channel(MAILBOX);
    let (display_tx, display_rx) = mpsc::unbounded_channel();
    let initial = Arc::new(StatusSnapshot::default());
    let (snapshot_tx, snapshot_rx) = watch::channel(Arc::clone(&initial));
    let (closed_tx, closed_rx) = watch::channel(false);
    let planning_tick = deps.planning_tick;
    let mut actor = Actor {
        tx: tx.clone(),
        display_tx,
        deps,
        ctl,
        config,
        config_error,
        config_backed_up: false,
        queue: VecDeque::new(),
        runtime: BTreeMap::new(),
        known: BTreeMap::new(),
        last_display: loaded_state.state.last_display,
        persister: Some(Persister::spawn()),
        state_due: None,
        config_due: None,
        rescan: RescanManager::new(RescanConfig::default()),
        armed_deadline: None,
        suspended: false,
        first_discovery_done: false,
        retired: ha2lux_backends::task::DisplayStats::default(),
        force_writes: false,
        snapshot_tx,
        previous: initial,
        ha: HaState::default(),
    };
    actor.refresh_token();
    actor.request_scan(RescanTrigger::Startup, 0);
    actor.publish();

    // Periodic schedule evaluation (the first one, right after the first enumeration).
    let ticker = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(Instant::now() + planning_tick, planning_tick);
        loop {
            interval.tick().await;
            if ticker.send(Msg::Event(Event::PlanningTick)).await.is_err() {
                break;
            }
        }
    });

    let join = tokio::spawn(actor.run(rx, display_rx, closed_tx));
    Ok(Daemon {
        handle: DaemonHandle {
            tx,
            snapshot: snapshot_rx,
            closed: closed_rx,
        },
        join,
    })
}

impl Actor {
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<Msg>,
        mut display_rx: mpsc::UnboundedReceiver<DisplayEvent>,
        closed: watch::Sender<bool>,
    ) {
        loop {
            tokio::select! {
                message = rx.recv() => {
                    let Some(message) = message else { break };
                    match message {
                        Msg::Shutdown(done) => {
                            self.stop().await;
                            // The requester may have given up waiting.
                            let _ = done.send(());
                            break;
                        }
                        Msg::Request { request: Request::Quit, reply } => {
                            let _ = reply.send(Ok(Reply::Done));
                            self.stop().await;
                            break;
                        }
                        Msg::Request { request: request @ (Request::SetToken(_) | Request::ClearToken | Request::ListSensors), reply } => {
                            self.on_token_request(request, reply);
                        }
                        Msg::Request { request, reply } => {
                            let result = self.on_request(request);
                            let _ = reply.send(result);
                        }
                        Msg::Event(event) => {
                            let forced = matches!(event, Event::Reapply);
                            self.force_writes = forced;
                            let result = self.dispatch(event);
                            self.force_writes = false;
                            if let Err(error) = result {
                                tracing::debug!(%error, "event had no effect");
                            }
                        }
                        Msg::Discovered { generation, found } => self.on_discovered(generation, found),
                        Msg::RescanTimer => {
                            self.armed_deadline = None;
                            self.poll_rescan();
                        }
                        Msg::System(event) => self.on_system(event),
                        Msg::Flush(kind) => self.on_flush(&kind),
                        Msg::Token { key, entity, token } => self.on_token(&key, &entity, token),
                    }
                }
                Some(event) = display_rx.recv() => self.on_display_event(event),
                Some(output) = next_output(&mut self.ha.source) => self.on_ha_output(output),
            }
            self.publish();
        }
        let _ = closed.send(true);
    }

    fn after(&self, delay: Duration, message: Msg) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // The actor may have stopped in the meantime.
            let _ = tx.send(message).await;
        });
    }

    // ---- State machine ---------------------------------------------------------

    fn dispatch(&mut self, event: Event) -> Result<(), ApiError> {
        let now = self.deps.clock.now();
        let commands = self.ctl.handle(now, event).map_err(ApiError::from)?;
        self.execute(commands);
        self.drain_queue();
        Ok(())
    }

    fn drain_queue(&mut self) {
        while let Some(event) = self.queue.pop_front() {
            let now = self.deps.clock.now();
            match self.ctl.handle(now, event) {
                Ok(commands) => self.execute(commands),
                Err(error) => tracing::debug!(%error, "internal event refused"),
            }
        }
    }

    fn execute(&mut self, commands: Vec<Command>) {
        for command in commands {
            match command {
                Command::Write { display, level } => self.write_display(display, level),
                Command::Rescan { delay_ms, .. } => {
                    self.request_scan(RescanTrigger::Resume, delay_ms)
                }
                Command::ArmRampTimer { in_ms } => {
                    self.after(Duration::from_millis(in_ms), Msg::Event(Event::RampTick));
                }
                Command::ArmReapply { in_ms } => {
                    self.after(Duration::from_millis(in_ms), Msg::Event(Event::Reapply));
                }
                Command::PollLuxSoon { .. } => {
                    if let Some(source) = &self.ha.source {
                        source.poll_soon();
                    }
                }
                Command::Persist(kind) => self.mark_dirty(kind),
                Command::Notify(notice) => self.on_notice(notice),
            }
        }
    }

    fn on_notice(&self, notice: Notice) {
        match notice {
            Notice::LearnRefused { contradicted } => {
                tracing::info!(
                    contradicted,
                    "learning refused: too many contradicted points"
                );
            }
            Notice::DisplayUnavailable(id) => {
                tracing::warn!(display = %id, "display unavailable after repeated failures");
                self.signal(Signal::DisplayFailed {
                    id: id.to_string(),
                    reason: "display unavailable".to_owned(),
                });
            }
            Notice::ListRejected { retry_in_ms } => {
                tracing::info!(retry_in_ms, "fake display list rejected");
            }
        }
    }

    fn signal(&self, signal: Signal) {
        // No more receiver: the D-Bus service is stopped.
        let _ = self.deps.signals.send(signal);
    }

    // ---- Hardware ---------------------------------------------------------------

    fn write_display(&mut self, display: DisplayId, level: u8) {
        if self.suspended {
            // No writes during sleep; the wake-up re-applies by priority.
            return;
        }
        let Some(runtime) = self.runtime.get_mut(&display) else {
            self.queue.push_back(Event::WriteResult {
                display,
                level,
                ok: false,
            });
            return;
        };
        let raw = match runtime.kind {
            DisplayKind::Internal => {
                backlight_raw_from_level(i64::from(level), runtime.max, runtime.floor)
            }
            DisplayKind::External => raw_from_level(i64::from(level), runtime.max),
        };
        runtime.last = Some((level, raw));
        let id_text = display.to_string();
        tracing::debug!(
            screen = %id_text,
            level,
            raw,
            forced = self.force_writes,
            "write requested"
        );
        if self.force_writes {
            // Re-application: the display may have changed on its own, so even an identical value is rewritten.
            runtime.handle.force_target(raw);
            return;
        }
        // Publish the target even if it equals the last written value: the task
        // writes nothing but cancels a pending target.
        runtime.handle.set_target(raw);
        if runtime.confirmed_raw == Some(raw) {
            self.queue.push_back(Event::WriteResult {
                display,
                level,
                ok: true,
            });
        }
    }

    fn on_display_event(&mut self, event: DisplayEvent) {
        match event {
            DisplayEvent::Applied { display: id, raw } => {
                if let Some(runtime) = self.runtime.get_mut(&id) {
                    runtime.confirmed_raw = Some(raw);
                    if let Some((level, last_raw)) = runtime.last
                        && last_raw == raw
                    {
                        self.queue.push_back(Event::WriteResult {
                            display: id,
                            level,
                            ok: true,
                        });
                    }
                }
            }
            DisplayEvent::WriteFailed {
                display: id,
                error,
                fatal,
            } => {
                tracing::warn!(screen = %id, error = %error, fatal, "hardware write failed");
                self.signal(Signal::DisplayFailed {
                    id: id.to_string(),
                    reason: error.to_string(),
                });
                if let Some((level, _)) = self.runtime.get(&id).and_then(|r| r.last) {
                    self.queue.push_back(Event::WriteResult {
                        display: id,
                        level,
                        ok: false,
                    });
                }
            }
            DisplayEvent::CoolingDown {
                display: id,
                duration,
            } => {
                tracing::debug!(screen = %id, ?duration, "writes frozen");
            }
        }
        self.drain_queue();
    }

    fn discover_options(&self) -> DiscoverOptions {
        DiscoverOptions {
            edid_bus_matching: self.config.general.edid_bus_matching,
            floors: self
                .config
                .displays
                .iter()
                .map(|d| (DisplayId::new(d.id.clone()), d.floor))
                .collect(),
        }
    }

    /// Requests a scan from the manager (debounce, floor, one at a time).
    fn request_scan(&mut self, trigger: RescanTrigger, delay_ms: u64) {
        let now = self.deps.clock.now().ms;
        self.rescan.trigger(now, trigger, delay_ms);
        self.poll_rescan();
    }

    /// Launches the scans that are due and schedules the manager's next deadline.
    fn poll_rescan(&mut self) {
        let now = self.deps.clock.now().ms;
        for RescanAction::Start { generation } in self.rescan.poll(now) {
            let hardware = Arc::clone(&self.deps.hardware);
            let options = self.discover_options();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let found = hardware.discover(options).await;
                // The actor may have stopped in the meantime.
                let _ = tx.send(Msg::Discovered { generation, found }).await;
            });
        }
        match self.rescan.next_deadline() {
            Some(deadline) if self.armed_deadline != Some(deadline) => {
                self.armed_deadline = Some(deadline);
                self.after(
                    Duration::from_millis(deadline.saturating_sub(now)),
                    Msg::RescanTimer,
                );
            }
            None => self.armed_deadline = None,
            Some(_) => {}
        }
    }

    /// System event: sleep, wake-up, lock, hotplug.
    fn on_system(&mut self, event: SystemEvent) {
        tracing::debug!(?event, "system event");
        match event {
            SystemEvent::Suspend | SystemEvent::SessionActive(false) => {
                self.suspended = true;
                self.rescan.set_suspended(true);
            }
            SystemEvent::Resume | SystemEvent::Unlock | SystemEvent::SessionActive(true) => {
                self.suspended = false;
                self.rescan.set_suspended(false);
                // Re-application by priority after a debounced rescan (the 1.5 s delay comes from the state machine).
                if let Err(error) = self.dispatch(Event::Wake) {
                    tracing::debug!(%error, "wake-up had no effect");
                }
            }
            SystemEvent::Lock => {}
            SystemEvent::Hotplug => self.request_scan(RescanTrigger::Hotplug, 0),
        }
        self.poll_rescan();
    }

    fn on_discovered(&mut self, generation: u64, found: Vec<Found>) {
        if !self.rescan.is_current(generation) {
            tracing::debug!(
                generation,
                "stale enumeration result discarded (scan disowned)"
            );
            return;
        }
        let mut accepted = true;
        let listed: Vec<Listed> = found
            .iter()
            .map(|f| Listed {
                id: f.info.id.clone(),
                controllable: f.backend.is_some(),
                level: f.level,
            })
            .collect();
        for entry in &listed {
            tracing::debug!(
                screen = %entry.id,
                controllable = entry.controllable,
                level_read = ?entry.level,
                "enumeration: level read"
            );
        }
        let now = self.deps.clock.now();
        // The state machine decides first (rejection of a bogus list): the current
        // hardware is only touched if the list is accepted.
        match self.ctl.handle(now, Event::DisplaysListed(listed)) {
            Ok(commands) => {
                let rejected = commands
                    .iter()
                    .any(|c| matches!(c, Command::Notify(Notice::ListRejected { .. })));
                accepted = !rejected;
                if accepted {
                    self.reconcile(found);
                }
                // An accepted list (wake-up, a display coming back) re-applies: forced writes.
                self.force_writes = true;
                self.execute(commands);
                self.drain_queue();
                self.force_writes = false;
            }
            Err(error) => tracing::error!(%error, "display list refused"),
        }
        if !self.first_discovery_done {
            self.first_discovery_done = true;
            // Schedule: value for the current hour right away, without waiting for the next minute.
            if let Err(error) = self.dispatch(Event::PlanningTick) {
                tracing::debug!(%error, "initial schedule evaluation had no effect");
            }
        }
        let waiting = self
            .ctl
            .status()
            .displays
            .iter()
            .any(|d| d.1 == DisplayState::Unavailable);
        let now = self.deps.clock.now().ms;
        self.rescan
            .finished(now, generation, ScanOutcome { accepted, waiting });
        self.poll_rescan();
    }

    /// Aligns the display tasks with the accepted list.
    fn reconcile(&mut self, found: Vec<Found>) {
        let was_controllable: BTreeSet<DisplayId> = self
            .ctl
            .status()
            .displays
            .iter()
            .filter(|d| d.1 == DisplayState::Controllable)
            .map(|d| d.0.clone())
            .collect();
        let mut seen = BTreeSet::new();
        for f in found {
            let id = f.info.id.clone();
            seen.insert(id.clone());
            self.known.insert(
                id.clone(),
                Known {
                    name: f.info.name.clone(),
                    kind: f.info.kind,
                    max: f.info.max,
                },
            );
            match f.backend {
                Some(backend) => {
                    // An existing task whose bus has not changed is kept (same EDID identity).
                    let keep = self.runtime.get(&id).is_some_and(|r| {
                        was_controllable.contains(&id) && r.bus == f.bus && r.max == f.info.max
                    });
                    if keep {
                        continue;
                    }
                    self.stop_runtime(&id);
                    let config =
                        self.deps
                            .task_config
                            .clone()
                            .unwrap_or_else(|| match f.info.kind {
                                DisplayKind::Internal => TaskConfig::backlight(),
                                DisplayKind::External => TaskConfig::ddc(),
                            });
                    let handle = spawn_display_task(backend, config, self.display_tx.clone());
                    self.runtime.insert(
                        id.clone(),
                        Runtime {
                            handle,
                            kind: f.info.kind,
                            max: f.info.max,
                            bus: f.bus,
                            floor: f.floor,
                            last: None,
                            confirmed_raw: None,
                        },
                    );
                    self.last_display = Some(id.to_string());
                }
                None => self.stop_runtime(&id),
            }
        }
        let gone: Vec<DisplayId> = self
            .runtime
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            self.stop_runtime(&id);
        }
    }

    fn stop_runtime(&mut self, id: &DisplayId) {
        if let Some(runtime) = self.runtime.remove(id) {
            let stats = runtime.handle.stats();
            self.retired.writes_ok += stats.writes_ok;
            self.retired.writes_failed += stats.writes_failed;
            self.retired.retries += stats.retries;
            self.retired.abandoned_ops += stats.abandoned_ops;
            if stats.last_error.is_some() {
                self.retired.last_error = stats.last_error;
            }
            // Shutdown in a background task: the actor never waits for the hardware.
            tokio::spawn(runtime.handle.shutdown());
        }
    }

    // ---- Persistence -------------------------------------------------------------

    fn mark_dirty(&mut self, kind: Persist) {
        let (is_state, delay_ms) = match kind {
            Persist::Master => (true, 1_000),
            Persist::Curve => (true, 3_000),
            Persist::Suspension => (true, 500),
            Persist::Ranges => (false, 500),
        };
        let delay = Duration::from_millis(delay_ms);
        let due = Instant::now() + delay;
        if is_state {
            self.state_due = Some(due);
            self.after(delay, Msg::Flush(FlushKind::State));
        } else {
            self.config_due = Some(due);
            self.after(delay, Msg::Flush(FlushKind::Config));
        }
    }

    fn on_flush(&mut self, kind: &FlushKind) {
        let now = Instant::now();
        match kind {
            FlushKind::State if self.state_due.is_some_and(|d| now >= d) => self.write_state(),
            FlushKind::Config if self.config_due.is_some_and(|d| now >= d) => self.write_config(),
            _ => {} // a more recent write is already scheduled
        }
    }

    fn write_state(&mut self) {
        self.state_due = None;
        let status = self.ctl.status();
        let state = State {
            master: status.master,
            suspension: status.suspension,
            curve: self.ctl.curve().clone(),
            last_display: self.last_display.clone(),
        };
        match (state.to_toml(), &self.persister) {
            (Ok(text), Some(persister)) => persister.write(WriteJob {
                path: self.deps.paths.state_file.clone(),
                bytes: text.into_bytes(),
                backup_first: false,
            }),
            (Err(error), _) => tracing::error!(%error, "state not serializable"),
            (_, None) => {}
        }
    }

    fn write_config(&mut self) {
        self.config_due = None;
        if let Some(error) = &self.config_error {
            tracing::warn!(%error, "unreadable configuration: settings not saved until it is fixed and reloaded");
            return;
        }
        match (self.config.to_toml(), &self.persister) {
            (Ok(text), Some(persister)) => {
                // Rewriting the file loses the comments: keep a copy of the original, once.
                let backup_first = !std::mem::replace(&mut self.config_backed_up, true);
                persister.write(WriteJob {
                    path: self.deps.paths.config_file.clone(),
                    bytes: text.into_bytes(),
                    backup_first,
                });
            }
            (Err(error), _) => tracing::error!(%error, "configuration not serializable"),
            (_, None) => {}
        }
    }

    async fn stop(&mut self) {
        if self.state_due.is_some() {
            self.write_state();
        }
        if self.config_due.is_some() {
            self.write_config();
        }
        let ids: Vec<DisplayId> = self.runtime.keys().cloned().collect();
        for id in ids {
            if let Some(runtime) = self.runtime.remove(&id) {
                runtime.handle.shutdown().await;
            }
        }
        if let Some(persister) = self.persister.take() {
            persister.finish().await;
        }
    }

    // ---- Requests ----------------------------------------------------------------

    /// URL, entity and token present (the token was found in the keyring).
    fn auto_configured(&self) -> bool {
        self.ha.token_present
            && !self.config.auto.url.is_empty()
            && !self.config.auto.entity.is_empty()
    }

    /// Keyring key for the configured URL.
    fn token_key(&self) -> Option<String> {
        ha2lux_core::url::authority_key(&self.config.auto.url)
    }

    /// Re-reads the token from the keyring (in a background task) then (re)starts or stops the source.
    fn refresh_token(&self) {
        let (Some(ha), Some(key)) = (self.deps.ha.clone(), self.token_key()) else {
            let _ = self.tx.try_send(Msg::Token {
                key: String::new(),
                entity: self.config.auto.entity.clone(),
                token: None,
            });
            return;
        };
        let tx = self.tx.clone();
        let entity = self.config.auto.entity.clone();
        tokio::spawn(async move {
            let token = match ha.tokens.get(&key).await {
                Ok(token) => token,
                Err(error) => {
                    tracing::warn!(%error, "Home Assistant token unreadable");
                    None
                }
            };
            let _ = tx.send(Msg::Token { key, entity, token }).await;
        });
    }

    fn on_token(&mut self, key: &str, entity: &str, token: Option<String>) {
        // Stale result: the configuration changed in the meantime.
        if self.token_key().as_deref().unwrap_or("") != key || self.config.auto.entity != entity {
            return;
        }
        let present = token.is_some() && !entity.is_empty();
        let wanted = present.then(|| (self.config.auto.url.clone(), entity.to_owned()));
        if wanted != self.ha.source_key {
            self.ha.source = None;
            self.ha.source_key = None;
            self.ha.raw = None;
            self.ha.websocket = false;
            self.ha.auth_rejected = false;
            if let (Some(token), Some(deps)) = (token, self.deps.ha.clone())
                && let Some((url, entity)) = &wanted
            {
                match deps.sources.start(url, entity, token) {
                    Ok(source) => {
                        self.ha.source = Some(source);
                        self.ha.source_key = wanted;
                    }
                    Err(error) => tracing::warn!(%error, "Home Assistant source not started"),
                }
            }
        }
        self.ha.token_present = self.ha.source.is_some();
        let configured = self.auto_configured();
        if let Err(error) = self.dispatch(Event::AutoConfigured(configured)) {
            tracing::debug!(%error, "configuration auto sans effet");
        }
    }

    fn on_ha_output(&mut self, output: Output) {
        match output {
            Output::Measurement { lux } => {
                let _ = self.dispatch(Event::Lux(lux));
            }
            Output::Health(health) => {
                let _ = self.dispatch(Event::Ha(health));
            }
            Output::Raw(raw) => self.ha.raw = Some(raw),
            Output::Websocket(active) => self.ha.websocket = active,
            Output::AuthRejected(rejected) => self.ha.auth_rejected = rejected,
        }
    }

    /// Requests that touch the token or the network: handled outside the actor, deferred reply.
    fn on_token_request(&self, request: Request, reply: oneshot::Sender<Result<Reply, ApiError>>) {
        let (Some(ha), Some(key)) = (self.deps.ha.clone(), self.token_key()) else {
            let _ = reply.send(Err(ApiError::NotConfigured(wire::encode(
                "err-no-ha-url",
                &[],
            ))));
            return;
        };
        let (tx, url) = (self.tx.clone(), self.config.auto.url.clone());
        let entity = self.config.auto.entity.clone();
        tokio::spawn(async move {
            let result = match request {
                Request::SetToken(Secret(token)) => {
                    if token.trim().is_empty() {
                        Err(ApiError::InvalidArgs(wire::encode("err-token-empty", &[])))
                    } else {
                        ha.tokens
                            .set(&key, token.trim())
                            .await
                            .map(|()| Reply::Done)
                            .map_err(ApiError::Failed)
                    }
                }
                Request::ClearToken => ha
                    .tokens
                    .delete(&key)
                    .await
                    .map(|_| Reply::Done)
                    .map_err(ApiError::Failed),
                _ => match ha.tokens.get(&key).await {
                    Ok(Some(token)) => ha
                        .sources
                        .sensors(&url, token)
                        .await
                        .map(|list| {
                            Reply::Sensors(
                                list.into_iter()
                                    .map(|s| (s.entity, s.name, s.area))
                                    .collect(),
                            )
                        })
                        .map_err(ApiError::Failed),
                    Ok(None) => Err(ApiError::NotConfigured(wire::encode("err-no-token", &[]))),
                    Err(error) => Err(ApiError::Failed(error)),
                },
            };
            let changed = result.is_ok() && !matches!(result, Ok(Reply::Sensors(_)));
            let _ = reply.send(result);
            if changed {
                let token = ha.tokens.get(&key).await.ok().flatten();
                let _ = tx.send(Msg::Token { key, entity, token }).await;
            }
        });
    }

    fn on_request(&mut self, request: Request) -> Result<Reply, ApiError> {
        match request {
            Request::SetMaster(level) => {
                let level = api::percent("master-level", level)?;
                self.dispatch(Event::SetMaster(level))?;
            }
            Request::StepMaster(delta) => {
                if delta == 0 || delta.unsigned_abs() > 100 {
                    return Err(ApiError::OutOfRange(wire::encode(
                        "err-step-out-of-range",
                        &[("delta", &delta)],
                    )));
                }
                self.dispatch(Event::Step(i64::from(delta)))?;
            }
            Request::SetDisplay { id, level } => {
                let display = api::display_id(&id)?;
                let level = api::percent("display-level", level)?;
                self.dispatch(Event::SetDisplay { display, level })?;
            }
            Request::SetRange { id, low, high } => {
                let display = api::display_id(&id)?;
                if low > 100 || high > 100 {
                    return Err(ApiError::OutOfRange(wire::encode(
                        "err-range-out-of-range",
                        &[("low", &low), ("high", &high)],
                    )));
                }
                self.dispatch(Event::SetRange {
                    display: display.clone(),
                    low: i64::from(low),
                    high: i64::from(high),
                })?;
                self.remember_range(&display);
            }
            Request::ApplyPreset(name) => {
                let name = api::preset_name(&name)?;
                let Some(level) = self.config.preset(name).map(|p| p.level) else {
                    let available: Vec<String> = self
                        .config
                        .presets
                        .iter()
                        .map(|p| format!("{}={}", p.name, p.level))
                        .collect();
                    return Err(ApiError::UnknownPreset(wire::encode(
                        "err-unknown-preset",
                        &[("name", &name), ("available", &available.join(", "))],
                    )));
                };
                self.dispatch(Event::Preset(i64::from(level)))?;
            }
            Request::SetMode { feature, on } => self.set_mode(&feature, on)?,
            Request::Rescan => self.request_scan(RescanTrigger::Manual, 0),
            Request::SetUiOpen(open) => {
                self.rescan.set_ui_open(open);
                self.poll_rescan();
            }
            Request::Reload => self.reload()?,
            Request::ResetCurve => self.dispatch(Event::ResetCurve)?,
            Request::UndoLearn => self.dispatch(Event::UndoLearn)?,
            Request::Presets => {
                return Ok(Reply::Presets(
                    self.config
                        .presets
                        .iter()
                        .map(|p| (p.name.clone(), u32::from(p.level)))
                        .collect(),
                ));
            }
            Request::List => return Ok(Reply::List(self.display_dtos())),
            Request::Status => return Ok(Reply::Status(Box::new(self.snapshot()))),
            // Handled by the main loop (asynchronous access to the keyring and the network).
            Request::SetToken(_) | Request::ClearToken | Request::ListSensors => {}
            Request::Quit => {} // handled by the main loop
        }
        Ok(Reply::Done)
    }

    fn remember_range(&mut self, display: &DisplayId) {
        let Some(range) = self
            .ctl
            .status()
            .displays
            .iter()
            .find(|d| &d.0 == display)
            .map(|d| d.3)
        else {
            return;
        };
        if let Some(entry) = self
            .config
            .displays
            .iter_mut()
            .find(|d| d.id == display.as_str())
        {
            entry.range = range;
        } else {
            if self.config.displays.len() >= MAX_REMEMBERED_DISPLAYS {
                // More than 32 displays remembered: forget the oldest one that is not plugged in.
                let present: BTreeSet<&str> = self.runtime.keys().map(DisplayId::as_str).collect();
                if let Some(pos) = self
                    .config
                    .displays
                    .iter()
                    .position(|d| !present.contains(d.id.as_str()))
                {
                    self.config.displays.remove(pos);
                }
            }
            self.config.displays.push(DisplayEntry {
                id: display.to_string(),
                range,
                floor: 1,
            });
        }
    }

    fn set_mode(&mut self, feature: &str, on: bool) -> Result<(), ApiError> {
        let feature = match feature {
            FEATURE_SCHEDULE => Feature::Schedule,
            FEATURE_IDLE_DIM => Feature::IdleDim,
            FEATURE_AUTO => Feature::Auto,
            other => {
                return Err(ApiError::InvalidArgs(wire::encode(
                    "err-unknown-feature",
                    &[
                        ("feature", &other),
                        ("first", &FEATURE_SCHEDULE),
                        ("second", &FEATURE_IDLE_DIM),
                        ("third", &FEATURE_AUTO),
                    ],
                )));
            }
        };
        if feature == Feature::Auto && on && !self.auto_configured() {
            return Err(ApiError::NotConfigured(wire::encode(
                "err-auto-needs-config",
                &[],
            )));
        }
        self.dispatch(Event::Feature(feature, on))?;
        match feature {
            Feature::Schedule => self.config.schedule.enabled = on,
            Feature::IdleDim => self.config.idle.enabled = on,
            Feature::Auto => self.config.auto.enabled = on,
        }
        self.mark_dirty(Persist::Ranges);
        Ok(())
    }

    /// Reloads the configuration. On error, the old one is kept (T-CFG-11).
    fn reload(&mut self) -> Result<(), ApiError> {
        let path = self.deps.paths.config_file.clone();
        let loaded = match store::read_config(&path) {
            Ok(loaded) => loaded,
            Err(error) => {
                let message = error.to_string();
                tracing::error!(%message, "reload refused: previous configuration kept");
                return Err(ApiError::ConfigInvalid(message));
            }
        };
        let cfg = controller_config(&loaded.config, self.auto_configured())
            .map_err(|e| ApiError::ConfigInvalid(e.to_string()))?;
        for issue in &loaded.issues {
            tracing::warn!(key = %issue.path, "configuration: {}", issue.message);
        }
        // The daemon's messages also follow the language of the reloaded file.
        ha2lux_i18n::set_preferred(&loaded.config.general.language);
        self.config = loaded.config;
        self.config_error = None;
        for entry in &self.config.displays {
            self.ctl
                .remember_range(DisplayId::new(entry.id.clone()), entry.range);
        }
        // Reloading never rewrites the file: we read, we do not write.
        self.config_due = None;
        self.dispatch(Event::Reload(Box::new(cfg)))?;
        self.config_due = None;
        self.refresh_token();
        Ok(())
    }

    // ---- Published state ---------------------------------------------------------------

    fn display_dtos(&self) -> Vec<DisplayDto> {
        self.ctl
            .status()
            .displays
            .into_iter()
            .filter(|d| d.1 != DisplayState::Absent)
            .map(|(id, state, level, range)| {
                let known = self.known.get(&id);
                DisplayDto {
                    name: known.map_or_else(|| id.to_string(), |k| k.name.clone()),
                    kind: match known.map(|k| k.kind) {
                        Some(DisplayKind::Internal) => "internal",
                        _ => "external",
                    }
                    .to_owned(),
                    state: state_name(state).to_owned(),
                    level: level.map_or(-1, i32::from),
                    low: u32::from(range.low()),
                    high: u32::from(range.high()),
                    max_raw: known.map_or(0, |k| k.max),
                    id: id.to_string(),
                }
            })
            .collect()
    }

    fn snapshot(&self) -> StatusSnapshot {
        let status = self.ctl.status();
        let auto = self.ctl.auto_info();
        let mut snapshot = StatusSnapshot {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            master: status.master,
            mode: mode_name(status.mode).to_owned(),
            step: self.config.general.step,
            schedule_enabled: self.config.schedule.enabled,
            schedule_points: u32::try_from(self.config.schedule.points.len()).unwrap_or(u32::MAX),
            schedule_resume: status.suspension.map(|s| s.resume().to_string()),
            idle_enabled: self.config.idle.enabled,
            idle_level: self.config.idle.level,
            idle_minutes: self.config.idle.minutes,
            dimmed: status.dimmed,
            auto_enabled: self.config.auto.enabled,
            auto_configured: self.auto_configured(),
            ha_health: format!("{:?}", status.ha_health).to_lowercase(),
            ramp_target: status.ramp_target,
            ha_raw: self.ha.raw.clone(),
            ha_websocket: self.ha.websocket,
            ha_auth_rejected: self.ha.auth_rejected,
            ha_token_present: self.ha.token_present,
            lux_smoothed: auto.smoothed_lux,
            lux_latest: auto.latest_lux,
            auto_adjusting: auto.adjusting,
            auto_target: auto.target,
            curve: auto.curve,
            curve_default: auto.default_curve,
            config_error: self.config_error.clone(),
            displays: self.display_dtos(),
            ..StatusSnapshot::default()
        };
        snapshot.writes_ok = self.retired.writes_ok;
        snapshot.writes_failed = self.retired.writes_failed;
        snapshot.retries = self.retired.retries;
        snapshot.abandoned_ops = self.retired.abandoned_ops;
        snapshot.last_error.clone_from(&self.retired.last_error);
        for runtime in self.runtime.values() {
            let stats = runtime.handle.stats();
            snapshot.writes_ok += stats.writes_ok;
            snapshot.writes_failed += stats.writes_failed;
            snapshot.retries += stats.retries;
            snapshot.abandoned_ops += stats.abandoned_ops;
            if stats.last_error.is_some() {
                snapshot.last_error = stats.last_error;
            }
        }
        snapshot
    }

    /// Publishes the state and emits the signals matching the changes.
    fn publish(&mut self) {
        let snapshot = Arc::new(self.snapshot());
        let previous = Arc::clone(&self.previous);
        if snapshot.master != previous.master
            && let Some(master) = snapshot.master
        {
            self.signal(Signal::LevelChanged(master));
        }
        if snapshot.mode != previous.mode {
            self.signal(Signal::ModeChanged(snapshot.mode.clone()));
        }
        let shape = |s: &StatusSnapshot| -> Vec<(String, String, u32, u32)> {
            s.displays
                .iter()
                .map(|d| (d.id.clone(), d.state.clone(), d.low, d.high))
                .collect()
        };
        if shape(&snapshot) != shape(&previous) {
            self.signal(Signal::DisplaysChanged);
        }
        if *snapshot != *previous {
            self.snapshot_tx.send_replace(Arc::clone(&snapshot));
            self.previous = snapshot;
        }
    }
}

/// Next output of the source; waits forever if there is none.
async fn next_output(source: &mut Option<HaSource>) -> Option<Output> {
    match source {
        Some(source) => source.outputs.recv().await,
        None => std::future::pending().await,
    }
}
