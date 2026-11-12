//! Tâche d'écran : une tâche asynchrone par écran, qui possède son backend.
//!
//! Garanties :
//! - **aucun appelant n'attend le matériel** : [`DisplayHandle::set_target`]
//!   publie une valeur et rend la main immédiatement ;
//! - **coalescence** : seule la dernière cible compte (`watch`), et la valeur
//!   finale est toujours écrite ;
//! - **cadence** : intervalle minimal entre deux écritures du même écran ;
//! - **timeout** sur chaque opération ; une opération abandonnée est comptée
//!   (`abandoned_ops`) et, répétée, rend la panne définitive ;
//! - **retries** avec backoff, puis **cooldown** après des échecs répétés, au
//!   dégel une seule tentative part, avec la dernière cible ;
//! - **jamais de panique** ni de blocage des autres écrans : chaque écran a
//!   sa tâche.
//!
//! Le verrou par bus I²C (plusieurs écrans partageant un bus) viendra avec le
//! backend DDC (étape 5) : il s'insère autour des appels au backend.

use std::time::Duration;

use ha2lux_core::{BackendError, DisplayBackend, DisplayId, DisplayInfo};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until, timeout};

/// Réglages de la tâche d'écran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskConfig {
    /// Intervalle minimal entre deux écritures (0 = pas de limite).
    pub min_interval: Duration,
    /// Délai maximal d'une opération matérielle.
    pub op_timeout: Duration,
    /// Tentatives par écriture (≥ 1).
    pub attempts: u32,
    /// Attente avant la tentative suivante (la dernière valeur sert ensuite).
    pub backoff: Vec<Duration>,
    /// Échecs consécutifs déclenchant le cooldown.
    pub cooldown_after: u32,
    /// Durée du gel des écritures.
    pub cooldown: Duration,
    /// Timeouts consécutifs rendant la panne définitive (fd « suspect »).
    pub max_abandoned: u32,
}

impl TaskConfig {
    /// Réglages DDC/CI : cadence ≈ 50 ms (à confirmer sur le matériel).
    pub fn ddc() -> Self {
        Self {
            min_interval: Duration::from_millis(50),
            op_timeout: Duration::from_secs(2),
            attempts: 3,
            backoff: vec![
                Duration::from_millis(50),
                Duration::from_millis(100),
                Duration::from_millis(200),
            ],
            cooldown_after: 3,
            cooldown: Duration::from_secs(5),
            max_abandoned: 3,
        }
    }

    /// Réglages backlight : pas de cadence minimale, coalescence identique.
    pub fn backlight() -> Self {
        Self {
            min_interval: Duration::ZERO,
            ..Self::ddc()
        }
    }
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self::ddc()
    }
}

/// Événements émis par une tâche d'écran vers le décideur.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayEvent {
    /// La valeur brute a été écrite avec succès.
    Applied {
        /// Écran concerné.
        display: DisplayId,
        /// Valeur brute écrite.
        raw: u32,
    },
    /// Une écriture a définitivement échoué (retries épuisés ou erreur fatale).
    WriteFailed {
        /// Écran concerné.
        display: DisplayId,
        /// Dernière erreur.
        error: BackendError,
        /// Inutile de réessayer : droits, débranchement, ou délais répétés.
        fatal: bool,
    },
    /// Écritures gelées pendant `duration` (une seule partira au dégel).
    CoolingDown {
        /// Écran concerné.
        display: DisplayId,
        /// Durée du gel.
        duration: Duration,
    },
}

/// Compteurs observables d'un écran (exposés par `Status()`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayStats {
    /// Écritures réussies.
    pub writes_ok: u64,
    /// Écritures définitivement échouées.
    pub writes_failed: u64,
    /// Nouvelles tentatives effectuées.
    pub retries: u64,
    /// Opérations abandonnées après timeout.
    pub abandoned_ops: u32,
    /// Dernière erreur rencontrée.
    pub last_error: Option<String>,
    /// Dernière valeur brute écrite avec succès.
    pub last_raw: Option<u32>,
}

/// Cible publiée vers la tâche : une valeur brute et une « époque ». Une écriture
/// **forcée** incrémente l'époque : la tâche réécrit alors même si la valeur est
/// identique à la dernière écrite (écran qui s'est remis à 100 % au réveil).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    raw: u32,
    epoch: u64,
}

enum Command {
    Read(oneshot::Sender<Result<u32, BackendError>>),
    Probe(oneshot::Sender<Result<DisplayInfo, BackendError>>),
}

/// Poignée vers la tâche d'un écran. Lâcher la poignée arrête la tâche après
/// l'écriture finale éventuellement en attente.
pub struct DisplayHandle {
    info: DisplayInfo,
    target: watch::Sender<Option<Target>>,
    epoch: std::sync::atomic::AtomicU64,
    commands: mpsc::Sender<Command>,
    stats: watch::Receiver<DisplayStats>,
    join: JoinHandle<()>,
}

impl DisplayHandle {
    /// Écran piloté.
    pub fn info(&self) -> &DisplayInfo {
        &self.info
    }

    /// Publie la valeur brute voulue (bornée au maximum de l'écran).
    /// Ne bloque jamais ; remplace toute cible non encore écrite. Si la valeur
    /// égale la dernière écrite, rien n'est réécrit.
    pub fn set_target(&self, raw: u32) {
        let epoch = self.epoch.load(std::sync::atomic::Ordering::Acquire);
        self.target.send_replace(Some(Target {
            raw: raw.min(self.info.max),
            epoch,
        }));
    }

    /// Comme [`DisplayHandle::set_target`], mais **réécrit** même si la valeur
    /// égale la dernière écrite : à utiliser quand l'écran a pu changer de lui-même
    /// (réveil de veille, retour du lien).
    pub fn force_target(&self, raw: u32) {
        let epoch = self.epoch.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
        self.target.send_replace(Some(Target {
            raw: raw.min(self.info.max),
            epoch,
        }));
    }

    /// Compteurs courants.
    pub fn stats(&self) -> DisplayStats {
        self.stats.borrow().clone()
    }

    /// Lecture matérielle exclusive, sérialisée avec les écritures.
    pub async fn read(&self) -> Result<u32, BackendError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Read(reply)).await?;
        answer.await.map_err(|_| gone())?
    }

    /// Sonde exclusive de l'écran.
    pub async fn probe(&self) -> Result<DisplayInfo, BackendError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Probe(reply)).await?;
        answer.await.map_err(|_| gone())?
    }

    /// Arrête la tâche proprement (l'écriture finale en attente est émise).
    pub async fn shutdown(self) {
        let Self {
            target,
            commands,
            join,
            ..
        } = self;
        drop(target);
        drop(commands);
        // Une tâche déjà arrêtée ou paniquée n'a rien à nous apprendre ici.
        let _ = join.await;
    }

    async fn send(&self, command: Command) -> Result<(), BackendError> {
        self.commands.send(command).await.map_err(|_| gone())
    }
}

fn gone() -> BackendError {
    BackendError::Disconnected("display task stopped".to_owned())
}

/// Lance la tâche d'un écran. Doit être appelée dans un runtime tokio.
pub fn spawn_display_task<B: DisplayBackend>(
    backend: B,
    config: TaskConfig,
    events: mpsc::UnboundedSender<DisplayEvent>,
) -> DisplayHandle {
    let info = backend.info().clone();
    let (target_tx, target_rx) = watch::channel::<Option<Target>>(None);
    let (command_tx, command_rx) = mpsc::channel(8);
    let (stats_tx, stats_rx) = watch::channel(DisplayStats::default());
    let runner = Runner {
        id: info.id.clone(),
        backend,
        config,
        target: target_rx,
        commands: command_rx,
        events,
        stats: stats_tx,
        last_ok: None,
        applied_epoch: 0,
        last_write_at: None,
        frozen_until: None,
        failures: 0,
        timeouts: 0,
    };
    DisplayHandle {
        info,
        target: target_tx,
        epoch: std::sync::atomic::AtomicU64::new(0),
        commands: command_tx,
        stats: stats_rx,
        join: tokio::spawn(runner.run()),
    }
}

struct Runner<B> {
    id: DisplayId,
    backend: B,
    config: TaskConfig,
    target: watch::Receiver<Option<Target>>,
    commands: mpsc::Receiver<Command>,
    events: mpsc::UnboundedSender<DisplayEvent>,
    stats: watch::Sender<DisplayStats>,
    last_ok: Option<u32>,
    applied_epoch: u64,
    last_write_at: Option<Instant>,
    frozen_until: Option<Instant>,
    failures: u32,
    timeouts: u32,
}

impl<B: DisplayBackend> Runner<B> {
    async fn run(mut self) {
        // Vrai quand la poignée a été lâchée : on tente l'écriture finale due
        // (une fois), puis on s'arrête.
        let mut closing = false;
        loop {
            let wanted = *self.target.borrow_and_update();
            let pending = wanted.filter(|w| self.needs_write(*w));
            let wait_until = pending
                .and_then(|_| self.ready_at())
                .filter(|at| *at > Instant::now());
            match (pending, wait_until) {
                (Some(_), wait) if closing => {
                    // Arrêt : une seule écriture finale, après la cadence, mais
                    // jamais en attendant la fin d'un gel (écran déjà en panne).
                    let frozen = self.frozen_until.is_some_and(|t| t > Instant::now());
                    if !frozen {
                        if let Some(at) = wait {
                            sleep_until(at).await;
                        }
                        self.write_op().await;
                    }
                    return;
                }
                (Some(_), None) => self.write_op().await,
                (Some(_), Some(at)) => {
                    tokio::select! {
                        () = sleep_until(at) => {}
                        changed = self.target.changed() => closing = changed.is_err(),
                        command = self.commands.recv() => match command {
                            Some(command) => self.execute(command).await,
                            None => closing = true,
                        },
                    }
                }
                (None, _) if closing => return,
                (None, _) => {
                    tokio::select! {
                        changed = self.target.changed() => closing = changed.is_err(),
                        command = self.commands.recv() => match command {
                            Some(command) => self.execute(command).await,
                            None => closing = true,
                        },
                    }
                }
            }
        }
    }

    /// Une écriture est due si la valeur diffère de la dernière écrite, ou si une
    /// écriture forcée plus récente que la dernière appliquée a été demandée.
    fn needs_write(&self, target: Target) -> bool {
        Some(target.raw) != self.last_ok || target.epoch > self.applied_epoch
    }

    /// Instant avant lequel on n'écrit pas (cadence ou gel).
    fn ready_at(&self) -> Option<Instant> {
        let paced = self.last_write_at.map(|t| t + self.config.min_interval);
        match (paced, self.frozen_until) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    async fn execute(&mut self, command: Command) {
        match command {
            Command::Read(reply) => {
                let limit = self.config.op_timeout;
                let outcome = timeout(limit, self.backend.read()).await;
                let result = self.settle(outcome);
                // L'appelant a pu abandonner : rien à faire.
                let _ = reply.send(result);
            }
            Command::Probe(reply) => {
                let limit = self.config.op_timeout;
                let outcome = timeout(limit, self.backend.probe()).await;
                let result = self.settle(outcome);
                let _ = reply.send(result);
            }
        }
    }

    /// Traite le résultat d'une opération sous timeout et tient les compteurs.
    fn settle<T>(
        &mut self,
        outcome: Result<Result<T, BackendError>, tokio::time::error::Elapsed>,
    ) -> Result<T, BackendError> {
        match outcome {
            Ok(result) => {
                self.timeouts = 0;
                result
            }
            Err(_elapsed) => {
                self.timeouts += 1;
                self.stats.send_modify(|s| s.abandoned_ops += 1);
                Err(BackendError::Timeout)
            }
        }
    }

    /// Une écriture de la dernière cible, avec retries et backoff.
    async fn write_op(&mut self) {
        let attempts = if self.failures >= self.config.cooldown_after {
            1 // dégel : une seule tentative d'essai
        } else {
            self.config.attempts.max(1)
        };
        for attempt in 0..attempts {
            // Chaque tentative reprend la valeur la plus récente.
            let Some(target) = *self.target.borrow_and_update() else {
                return;
            };
            if !self.needs_write(target) {
                return;
            }
            let value = target.raw;
            let limit = self.config.op_timeout;
            let outcome = timeout(limit, self.backend.write(value)).await;
            match self.settle(outcome) {
                Ok(()) => return self.on_success(value, target.epoch),
                Err(error) => {
                    self.failures += 1;
                    self.last_write_at = Some(Instant::now());
                    let last = attempt + 1 == attempts;
                    let fatal = !error.is_transient() || self.timeouts >= self.config.max_abandoned;
                    tracing::debug!(display = %self.id, %error, attempt, "write failed");
                    if fatal || last {
                        return self.on_final_failure(error, fatal);
                    }
                    self.stats.send_modify(|s| s.retries += 1);
                    sleep(self.backoff(attempt)).await;
                }
            }
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let index = usize::try_from(attempt).unwrap_or(usize::MAX);
        self.config
            .backoff
            .get(index)
            .or(self.config.backoff.last())
            .copied()
            .unwrap_or(Duration::ZERO)
    }

    fn on_success(&mut self, raw: u32, epoch: u64) {
        self.last_ok = Some(raw);
        self.applied_epoch = epoch;
        self.last_write_at = Some(Instant::now());
        self.failures = 0;
        self.frozen_until = None;
        self.stats.send_modify(|s| {
            s.writes_ok += 1;
            s.last_raw = Some(raw);
        });
        self.emit(DisplayEvent::Applied {
            display: self.id.clone(),
            raw,
        });
    }

    fn on_final_failure(&mut self, error: BackendError, fatal: bool) {
        if fatal {
            self.failures = self.failures.max(self.config.cooldown_after);
        }
        self.stats.send_modify(|s| {
            s.writes_failed += 1;
            s.last_error = Some(error.to_string());
        });
        self.emit(DisplayEvent::WriteFailed {
            display: self.id.clone(),
            error,
            fatal,
        });
        if self.failures >= self.config.cooldown_after {
            if self.frozen_until.is_none_or(|t| t <= Instant::now()) {
                // Un seul avertissement par entrée en cooldown ; la suite reste en debug.
                tracing::warn!(display = %self.id, "writes frozen after repeated failures");
            }
            self.frozen_until = Some(Instant::now() + self.config.cooldown);
            self.emit(DisplayEvent::CoolingDown {
                display: self.id.clone(),
                duration: self.config.cooldown,
            });
        }
    }

    fn emit(&self, event: DisplayEvent) {
        // Le décideur peut avoir disparu pendant l'arrêt : sans importance.
        let _ = self.events.send(event);
    }
}
