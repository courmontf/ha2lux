//! Display task: one async task per display, which owns its backend.
//!
//! Garanties :
//! - **no caller waits for the hardware**: [`DisplayHandle::set_target`]
//!   publishes a value and returns immediately;
//! - **coalescing**: only the latest target counts (`watch`), and the final
//!   value is always written;
//! - **pacing**: minimum interval between two writes to the same display;
//! - **timeout** on each operation; an abandoned operation is counted
//!   (`abandoned_ops`) and, when repeated, makes the failure permanent;
//! - **retries** with backoff, then **cooldown** after repeated failures; at
//!   thaw a single attempt goes out, with the latest target;
//! - **never a panic** nor blocking of the other displays: each display has
//!   its own task.
//!
//! The per-I²C-bus lock (several displays sharing a bus) will come with the
//! DDC backend (step 5): it fits around the backend calls.

use std::time::Duration;

use ha2lux_core::{BackendError, DisplayBackend, DisplayId, DisplayInfo};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until, timeout};

/// Display task settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskConfig {
    /// Minimum interval between two writes (0 = no limit).
    pub min_interval: Duration,
    /// Maximum delay of a hardware operation.
    pub op_timeout: Duration,
    /// Attempts per write (>= 1).
    pub attempts: u32,
    /// Wait before the next attempt (the last value is then reused).
    pub backoff: Vec<Duration>,
    /// Consecutive failures that trigger the cooldown.
    pub cooldown_after: u32,
    /// Duration of the write freeze.
    pub cooldown: Duration,
    /// Consecutive timeouts that make the failure permanent ("suspect" fd).
    pub max_abandoned: u32,
}

impl TaskConfig {
    /// DDC/CI settings: pacing of about 50 ms (to be confirmed on real hardware).
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

    /// Backlight settings: no minimum pacing, same coalescing.
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

/// Events emitted by a display task towards the decision maker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayEvent {
    /// The raw value was written successfully.
    Applied {
        /// Display concerned.
        display: DisplayId,
        /// Raw value written.
        raw: u32,
    },
    /// A write failed permanently (retries exhausted or fatal error).
    WriteFailed {
        /// Display concerned.
        display: DisplayId,
        /// Last error.
        error: BackendError,
        /// No point retrying: permissions, unplugging, or repeated timeouts.
        fatal: bool,
    },
    /// Writes frozen for `duration` (a single one will go out at thaw).
    CoolingDown {
        /// Display concerned.
        display: DisplayId,
        /// Freeze duration.
        duration: Duration,
    },
}

/// Observable counters of a display (exposed by `Status()`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayStats {
    /// Successful writes.
    pub writes_ok: u64,
    /// Permanently failed writes.
    pub writes_failed: u64,
    /// Retries performed.
    pub retries: u64,
    /// Operations abandoned after timeout.
    pub abandoned_ops: u32,
    /// Last error encountered.
    pub last_error: Option<String>,
    /// Last raw value written successfully.
    pub last_raw: Option<u32>,
}

/// Target published to the task: a raw value and an "epoch". A **forced**
/// write increments the epoch: the task then rewrites even if the value is
/// identical to the last one written (display that reset to 100% on wake-up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    raw: u32,
    epoch: u64,
}

enum Command {
    Read(oneshot::Sender<Result<u32, BackendError>>),
    Probe(oneshot::Sender<Result<DisplayInfo, BackendError>>),
}

/// Handle to a display's task. Dropping the handle stops the task after
/// the final pending write, if any.
pub struct DisplayHandle {
    info: DisplayInfo,
    target: watch::Sender<Option<Target>>,
    epoch: std::sync::atomic::AtomicU64,
    commands: mpsc::Sender<Command>,
    stats: watch::Receiver<DisplayStats>,
    join: JoinHandle<()>,
}

impl DisplayHandle {
    /// Display driven.
    pub fn info(&self) -> &DisplayInfo {
        &self.info
    }

    /// Publishes the desired raw value (clamped to the display's maximum).
    /// Never blocks; replaces any target not yet written. If the value
    /// equals the last one written, nothing is rewritten.
    pub fn set_target(&self, raw: u32) {
        let epoch = self.epoch.load(std::sync::atomic::Ordering::Acquire);
        self.target.send_replace(Some(Target {
            raw: raw.min(self.info.max),
            epoch,
        }));
    }

    /// Like [`DisplayHandle::set_target`], but **rewrites** even if the value
    /// equals the last one written: use when the display may have changed by itself
    /// (wake from sleep, link coming back).
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

    /// Exclusive hardware read, serialized with writes.
    pub async fn read(&self) -> Result<u32, BackendError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Read(reply)).await?;
        answer.await.map_err(|_| gone())?
    }

    /// Exclusive probe of the display.
    pub async fn probe(&self) -> Result<DisplayInfo, BackendError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Probe(reply)).await?;
        answer.await.map_err(|_| gone())?
    }

    /// Stops the task cleanly (the pending final write is emitted).
    pub async fn shutdown(self) {
        let Self {
            target,
            commands,
            join,
            ..
        } = self;
        drop(target);
        drop(commands);
        // A task already stopped or panicked has nothing to tell us here.
        let _ = join.await;
    }

    async fn send(&self, command: Command) -> Result<(), BackendError> {
        self.commands.send(command).await.map_err(|_| gone())
    }
}

fn gone() -> BackendError {
    BackendError::Disconnected("display task stopped".to_owned())
}

/// Starts a display's task. Must be called within a tokio runtime.
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
        // True when the handle has been dropped: we try the final write due
        // (once), then stop.
        let mut closing = false;
        loop {
            let wanted = *self.target.borrow_and_update();
            let pending = wanted.filter(|w| self.needs_write(*w));
            let wait_until = pending
                .and_then(|_| self.ready_at())
                .filter(|at| *at > Instant::now());
            match (pending, wait_until) {
                (Some(_), wait) if closing => {
                    // Stop: a single final write, after the pacing, but
                    // never while waiting out a freeze (display already failing).
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

    /// A write is due if the value differs from the last one written, or if a
    /// forced write more recent than the last one applied was requested.
    fn needs_write(&self, target: Target) -> bool {
        Some(target.raw) != self.last_ok || target.epoch > self.applied_epoch
    }

    /// Instant before which we do not write (pacing or freeze).
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
                // The caller may have given up: nothing to do.
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

    /// Handles the result of an operation under timeout and maintains the counters.
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

    /// One write of the latest target, with retries and backoff.
    async fn write_op(&mut self) {
        let attempts = if self.failures >= self.config.cooldown_after {
            1 // thaw: a single trial attempt
        } else {
            self.config.attempts.max(1)
        };
        for attempt in 0..attempts {
            // Each attempt takes the most recent value.
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
                // A single warning per cooldown entry; the rest stays at debug level.
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
        // The decision maker may have disappeared during shutdown: unimportant.
        let _ = self.events.send(event);
    }
}
