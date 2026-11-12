//! Acteur décideur : l'**unique** propriétaire de l'état de pilotage.
//!
//! Tout le reste (D-Bus, minuteries, tâches d'écran, événements système) lui
//! envoie des messages ; lui seul appelle la machine d'états et exécute ses
//! commandes. Conséquences voulues :
//! - les requêtes sont **sérialisées** (jamais d'entrelacement d'écritures) ;
//! - **aucune E/S bloquante** dans l'acteur : le matériel est piloté par des
//!   tâches d'écran, les fichiers par un écrivain dédié, la découverte par une
//!   tâche à part ;
//! - un état publié (`watch`) que l'interface lit sans jamais verrouiller.

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

/// Nombre maximal d'écrans mémorisés dans la configuration.
const MAX_REMEMBERED_DISPLAYS: usize = 32;
/// Capacité de la boîte aux lettres de l'acteur.
const MAILBOX: usize = 256;

/// Signaux à émettre vers les clients D-Bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Le maître a changé.
    LevelChanged(u8),
    /// La liste ou l'état des écrans a changé.
    DisplaysChanged,
    /// La source de base a changé.
    ModeChanged(String),
    /// Une écriture a définitivement échoué.
    DisplayFailed {
        /// Écran concerné.
        id: String,
        /// Raison lisible.
        reason: String,
    },
}

/// Dépendances de l'acteur.
pub struct Deps {
    /// Fichiers de configuration et d'état.
    pub paths: Paths,
    /// Source d'écrans.
    pub hardware: Arc<dyn Hardware>,
    /// Horloge (réelle, ou pilotée par tokio dans les tests).
    pub clock: Arc<dyn Clock + Send + Sync>,
    /// Destination des signaux D-Bus.
    pub signals: mpsc::UnboundedSender<Signal>,
    /// Réglages de tâche d'écran imposés (tests) ; `None` : selon le type d'écran.
    pub task_config: Option<TaskConfig>,
    /// Période d'évaluation du planning.
    pub planning_tick: Duration,
    /// Home Assistant (jeton et source de mesures) ; `None` : auto-luminosité indisponible.
    pub ha: Option<HaDeps>,
}

/// Échec du démarrage.
#[derive(Debug, Error)]
pub enum StartError {
    /// Fichier illisible ou version non prise en charge.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Planning invalide (ne doit pas arriver après la validation).
    #[error("planning invalide : {0}")]
    Schedule(#[from] ScheduleError),
}

/// Événement venu du système (logind, udev…), transmis à l'acteur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemEvent {
    /// Préparation de la veille : plus aucune écriture matérielle (DDC en veille = blocage).
    Suspend,
    /// Retour de veille.
    Resume,
    /// Session verrouillée : rien à faire (comme l'extinction d'écran).
    Lock,
    /// Session déverrouillée.
    Unlock,
    /// La session devient active ou inactive (changement de session, multi-session).
    SessionActive(bool),
    /// Branchement ou débranchement d'écran (DRM, nœud i2c).
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
    /// Résultat de la lecture du jeton pour cette adresse et cette entité.
    Token {
        key: String,
        entity: String,
        token: Option<String>,
    },
    Shutdown(oneshot::Sender<()>),
}

/// Poignée clonable vers l'acteur (D-Bus, adaptateurs système, tests).
#[derive(Clone)]
pub struct DaemonHandle {
    tx: mpsc::Sender<Msg>,
    snapshot: watch::Receiver<Arc<StatusSnapshot>>,
    closed: watch::Receiver<bool>,
}

impl DaemonHandle {
    /// Envoie une requête et attend sa réponse. Les requêtes sont sérialisées.
    pub async fn request(&self, request: Request) -> Result<Reply, ApiError> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(Msg::Request { request, reply })
            .await
            .map_err(|_| ApiError::Stopped)?;
        answer.await.map_err(|_| ApiError::Stopped)?
    }

    /// Injecte un événement système (réveil, inactivité, mesure de lux…).
    pub async fn send_event(&self, event: Event) -> Result<(), ApiError> {
        self.tx
            .send(Msg::Event(event))
            .await
            .map_err(|_| ApiError::Stopped)
    }

    /// Injecte un événement système (logind, udev).
    pub async fn send_system(&self, event: SystemEvent) -> Result<(), ApiError> {
        self.tx
            .send(Msg::System(event))
            .await
            .map_err(|_| ApiError::Stopped)
    }

    /// Dernier état publié (lecture sans verrou).
    pub fn snapshot(&self) -> Arc<StatusSnapshot> {
        Arc::clone(&self.snapshot.borrow())
    }

    /// Flux des états publiés.
    pub fn subscribe(&self) -> watch::Receiver<Arc<StatusSnapshot>> {
        self.snapshot.clone()
    }

    /// Attend l'arrêt de l'acteur (demande `Quit`, arrêt ou fin de la boîte aux lettres).
    pub async fn wait_closed(&self) {
        let mut closed = self.closed.clone();
        // Une erreur signifie que l'acteur a disparu : il est bien arrêté.
        let _ = closed.wait_for(|c| *c).await;
    }
}

/// L'acteur en cours d'exécution.
pub struct Daemon {
    handle: DaemonHandle,
    join: JoinHandle<()>,
}

impl Daemon {
    /// Poignée clonable.
    pub fn handle(&self) -> DaemonHandle {
        self.handle.clone()
    }

    /// Arrêt propre : état écrit, écritures matérielles finales faites, tâches arrêtées.
    pub async fn shutdown(self) {
        let (done, finished) = oneshot::channel();
        if self.handle.tx.send(Msg::Shutdown(done)).await.is_ok() {
            // L'acteur peut s'être arrêté entre-temps (Quit) : sans importance.
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
                    Ok(Ok(())) => tracing::debug!(path = %path.display(), "fichier écrit"),
                    Ok(Err(error)) => {
                        tracing::error!(path = %path.display(), %error, "écriture impossible")
                    }
                    Err(error) => {
                        tracing::error!(path = %path.display(), %error, "écriture interrompue")
                    }
                }
            }
        });
        Self { tx, join }
    }

    fn write(&self, job: WriteJob) {
        // Le récepteur ne disparaît qu'à l'arrêt.
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
    /// Dernier niveau commandé et sa valeur brute.
    last: Option<(u8, u32)>,
    /// Dernière valeur brute confirmée écrite.
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
    /// Veille ou session inactive : aucune écriture matérielle.
    suspended: bool,
    first_discovery_done: bool,
    /// Compteurs cumulés des tâches d'écran déjà arrêtées (ils ne disparaissent pas avec l'écran).
    retired: ha2lux_backends::task::DisplayStats,
    /// Les écritures en cours sont des ré-applications (réveil, liste acceptée) : forcées.
    force_writes: bool,
    snapshot_tx: watch::Sender<Arc<StatusSnapshot>>,
    previous: Arc<StatusSnapshot>,
    ha: HaState,
}

/// État de la liaison Home Assistant côté démon.
#[derive(Default)]
struct HaState {
    source: Option<HaSource>,
    /// Adresse et entité de la source en cours.
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

/// Démarre le démon : charge configuration et état, lance l'acteur, la
/// première énumération et l'évaluation périodique du planning.
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
                    tracing::warn!(key = %issue.path, "configuration : {}", issue.message);
                }
                (l.config, None)
            }
            ConfigLoad::Invalid { error } => {
                tracing::error!(%error, "configuration illisible : valeurs par défaut, fichier préservé et jamais réécrit");
                (Config::default(), Some(error.to_string()))
            }
        }
    };
    let state_path = paths.state_file.clone();
    let loaded_state = tokio::task::spawn_blocking(move || store::load_state(&state_path))
        .await
        .unwrap_or_default();
    for issue in &loaded_state.issues {
        tracing::warn!(key = %issue.path, "état : {}", issue.message);
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

    // Évaluation périodique du planning (la première, juste après la première énumération).
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
                            // Le demandeur peut avoir abandonné l'attente.
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
                                tracing::debug!(%error, "événement sans effet");
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
            // L'acteur peut s'être arrêté entre-temps.
            let _ = tx.send(message).await;
        });
    }

    // ---- Machine d'états ---------------------------------------------------------

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
                Err(error) => tracing::debug!(%error, "événement interne refusé"),
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
                    "apprentissage refusé : trop de points contredits"
                );
            }
            Notice::DisplayUnavailable(id) => {
                tracing::warn!(display = %id, "écran indisponible après des échecs répétés");
                self.signal(Signal::DisplayFailed {
                    id: id.to_string(),
                    reason: "écran indisponible".to_owned(),
                });
            }
            Notice::ListRejected { retry_in_ms } => {
                tracing::info!(retry_in_ms, "liste d'écrans factice rejetée");
            }
        }
    }

    fn signal(&self, signal: Signal) {
        // Plus de récepteur : le service D-Bus est arrêté.
        let _ = self.deps.signals.send(signal);
    }

    // ---- Matériel ---------------------------------------------------------------

    fn write_display(&mut self, display: DisplayId, level: u8) {
        if self.suspended {
            // Aucune écriture pendant la veille ; le réveil ré-applique par priorité.
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
            "écriture demandée"
        );
        if self.force_writes {
            // Ré-application : l'écran a pu changer de lui-même, on réécrit même une valeur identique.
            runtime.handle.force_target(raw);
            return;
        }
        // Publier la cible même si elle égale la dernière valeur écrite : la tâche
        // n'écrit rien mais annule une cible en attente.
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
                tracing::warn!(screen = %id, error = %error, fatal, "écriture matérielle en échec");
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
                tracing::debug!(screen = %id, ?duration, "écritures gelées");
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

    /// Demande un scan au gestionnaire (débounce, plancher, un seul à la fois).
    fn request_scan(&mut self, trigger: RescanTrigger, delay_ms: u64) {
        let now = self.deps.clock.now().ms;
        self.rescan.trigger(now, trigger, delay_ms);
        self.poll_rescan();
    }

    /// Lance les scans dus et programme la prochaine échéance du gestionnaire.
    fn poll_rescan(&mut self) {
        let now = self.deps.clock.now().ms;
        for RescanAction::Start { generation } in self.rescan.poll(now) {
            let hardware = Arc::clone(&self.deps.hardware);
            let options = self.discover_options();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let found = hardware.discover(options).await;
                // L'acteur peut s'être arrêté entre-temps.
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

    /// Événement système : veille, réveil, verrouillage, hotplug.
    fn on_system(&mut self, event: SystemEvent) {
        tracing::debug!(?event, "événement système");
        match event {
            SystemEvent::Suspend | SystemEvent::SessionActive(false) => {
                self.suspended = true;
                self.rescan.set_suspended(true);
            }
            SystemEvent::Resume | SystemEvent::Unlock | SystemEvent::SessionActive(true) => {
                self.suspended = false;
                self.rescan.set_suspended(false);
                // Ré-application par priorité après rescan débouncé (le délai de 1,5 s vient de la machine d'états).
                if let Err(error) = self.dispatch(Event::Wake) {
                    tracing::debug!(%error, "réveil sans effet");
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
                "résultat d'énumération périmé jeté (scan désavoué)"
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
                "énumération : niveau lu"
            );
        }
        let now = self.deps.clock.now();
        // La machine d'états décide d'abord (rejet d'une liste factice) : le matériel
        // en cours n'est touché que si la liste est acceptée.
        match self.ctl.handle(now, Event::DisplaysListed(listed)) {
            Ok(commands) => {
                let rejected = commands
                    .iter()
                    .any(|c| matches!(c, Command::Notify(Notice::ListRejected { .. })));
                accepted = !rejected;
                if accepted {
                    self.reconcile(found);
                }
                // Une liste acceptée (réveil, retour d'un écran) ré-applique : écritures forcées.
                self.force_writes = true;
                self.execute(commands);
                self.drain_queue();
                self.force_writes = false;
            }
            Err(error) => tracing::error!(%error, "liste d'écrans refusée"),
        }
        if !self.first_discovery_done {
            self.first_discovery_done = true;
            // Planning : valeur de l'heure courante tout de suite, sans attendre la minute suivante.
            if let Err(error) = self.dispatch(Event::PlanningTick) {
                tracing::debug!(%error, "évaluation initiale du planning sans effet");
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

    /// Aligne les tâches d'écran sur la liste acceptée.
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
                    // Une tâche existante dont le bus n'a pas changé est conservée (même identité EDID).
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
            // Arrêt en tâche de fond : l'acteur n'attend jamais le matériel.
            tokio::spawn(runtime.handle.shutdown());
        }
    }

    // ---- Persistance -------------------------------------------------------------

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
            _ => {} // une écriture plus récente est déjà programmée
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
            (Err(error), _) => tracing::error!(%error, "état non sérialisable"),
            (_, None) => {}
        }
    }

    fn write_config(&mut self) {
        self.config_due = None;
        if let Some(error) = &self.config_error {
            tracing::warn!(%error, "configuration illisible : réglages non enregistrés tant qu'elle n'est pas corrigée puis rechargée");
            return;
        }
        match (self.config.to_toml(), &self.persister) {
            (Ok(text), Some(persister)) => {
                // Réécrire le fichier perd les commentaires : on garde une copie de l'original, une fois.
                let backup_first = !std::mem::replace(&mut self.config_backed_up, true);
                persister.write(WriteJob {
                    path: self.deps.paths.config_file.clone(),
                    bytes: text.into_bytes(),
                    backup_first,
                });
            }
            (Err(error), _) => tracing::error!(%error, "configuration non sérialisable"),
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

    // ---- Requêtes ----------------------------------------------------------------

    /// URL, entité et jeton présents (le jeton a été retrouvé dans le trousseau).
    fn auto_configured(&self) -> bool {
        self.ha.token_present
            && !self.config.auto.url.is_empty()
            && !self.config.auto.entity.is_empty()
    }

    /// Clé du trousseau de l'URL configurée.
    fn token_key(&self) -> Option<String> {
        ha2lux_core::url::authority_key(&self.config.auto.url)
    }

    /// Relit le jeton dans le trousseau (en tâche de fond) puis (re)démarre ou arrête la source.
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
                    tracing::warn!(%error, "jeton Home Assistant illisible");
                    None
                }
            };
            let _ = tx.send(Msg::Token { key, entity, token }).await;
        });
    }

    fn on_token(&mut self, key: &str, entity: &str, token: Option<String>) {
        // Résultat périmé : la configuration a changé entre-temps.
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
                    Err(error) => tracing::warn!(%error, "source Home Assistant non démarrée"),
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

    /// Requêtes qui touchent au jeton ou au réseau : traitées hors de l'acteur, réponse différée.
    fn on_token_request(&self, request: Request, reply: oneshot::Sender<Result<Reply, ApiError>>) {
        let (Some(ha), Some(key)) = (self.deps.ha.clone(), self.token_key()) else {
            let _ = reply.send(Err(ApiError::NotConfigured(
                "aucune URL Home Assistant valide dans la configuration ([auto] url)".to_owned(),
            )));
            return;
        };
        let (tx, url) = (self.tx.clone(), self.config.auto.url.clone());
        let entity = self.config.auto.entity.clone();
        tokio::spawn(async move {
            let result = match request {
                Request::SetToken(Secret(token)) => {
                    if token.trim().is_empty() {
                        Err(ApiError::InvalidArgs("jeton vide".to_owned()))
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
                    Ok(None) => Err(ApiError::NotConfigured(
                        "aucun jeton enregistré pour cette adresse (ha2lux auto token)".to_owned(),
                    )),
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
                let level = api::percent("niveau maître", level)?;
                self.dispatch(Event::SetMaster(level))?;
            }
            Request::StepMaster(delta) => {
                if delta == 0 || delta.unsigned_abs() > 100 {
                    return Err(ApiError::OutOfRange(format!(
                        "pas hors bornes : {delta} (attendu 1 à 100, positif ou négatif)"
                    )));
                }
                self.dispatch(Event::Step(i64::from(delta)))?;
            }
            Request::SetDisplay { id, level } => {
                let display = api::display_id(&id)?;
                let level = api::percent("niveau écran", level)?;
                self.dispatch(Event::SetDisplay { display, level })?;
            }
            Request::SetRange { id, low, high } => {
                let display = api::display_id(&id)?;
                if low > 100 || high > 100 {
                    return Err(ApiError::OutOfRange(format!(
                        "plage hors bornes : ({low}, {high}) (attendu 0 à 100)"
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
                    return Err(ApiError::UnknownPreset(format!(
                        "preset inconnu : {name} ; disponibles : {}",
                        available.join(", ")
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
            // Traitées par la boucle principale (accès asynchrone au trousseau et au réseau).
            Request::SetToken(_) | Request::ClearToken | Request::ListSensors => {}
            Request::Quit => {} // traité par la boucle principale
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
                // Plus de 32 écrans mémorisés : on oublie le plus ancien qui n'est pas branché.
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
                return Err(ApiError::InvalidArgs(format!(
                    "fonction inconnue : {other} (attendu : {FEATURE_SCHEDULE}, {FEATURE_IDLE_DIM} ou {FEATURE_AUTO})"
                )));
            }
        };
        if feature == Feature::Auto && on && !self.auto_configured() {
            return Err(ApiError::NotConfigured(
                "l'auto-luminosité exige l'URL de Home Assistant, l'entité du capteur et un jeton"
                    .to_owned(),
            ));
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

    /// Recharge la configuration. En cas d'erreur, l'ancienne est conservée (T-CFG-11).
    fn reload(&mut self) -> Result<(), ApiError> {
        let path = self.deps.paths.config_file.clone();
        let loaded = match store::read_config(&path) {
            Ok(loaded) => loaded,
            Err(error) => {
                let message = error.to_string();
                tracing::error!(%message, "rechargement refusé : configuration précédente conservée");
                return Err(ApiError::ConfigInvalid(message));
            }
        };
        let cfg = controller_config(&loaded.config, self.auto_configured())
            .map_err(|e| ApiError::ConfigInvalid(e.to_string()))?;
        for issue in &loaded.issues {
            tracing::warn!(key = %issue.path, "configuration : {}", issue.message);
        }
        self.config = loaded.config;
        self.config_error = None;
        for entry in &self.config.displays {
            self.ctl
                .remember_range(DisplayId::new(entry.id.clone()), entry.range);
        }
        // Le rechargement ne réécrit jamais le fichier : on lit, on n'écrit pas.
        self.config_due = None;
        self.dispatch(Event::Reload(Box::new(cfg)))?;
        self.config_due = None;
        self.refresh_token();
        Ok(())
    }

    // ---- État publié ---------------------------------------------------------------

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

    /// Publie l'état et émet les signaux correspondant aux changements.
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

/// Prochaine sortie de la source ; en attente perpétuelle s'il n'y en a pas.
async fn next_output(source: &mut Option<HaSource>) -> Option<Output> {
    match source {
        Some(source) => source.outputs.recv().await,
        None => std::future::pending().await,
    }
}
