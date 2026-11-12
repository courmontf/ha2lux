//! Rescan manager (no I/O): when to start a display enumeration.
//!
//! Robustness mechanisms:
//! - 600 ms **debounce** (a burst of events = a single scan);
//! - 30 s **floor** between two scans triggered by a *noisy* hotplug
//!   (a display whose link retrains in a loop); sleep, unlock and
//!   manual request are never throttled;
//! - **one scan at a time**, with a **generation** number: the late
//!   result of a disowned scan is dropped;
//! - 10 s **watchdog**: a stuck scan is disowned; after 3 consecutive
//!   disownments we wait for a new event;
//! - **pending** displays: retries at 2, 5, 10, 20 s then every 60 s with no
//!   end, postponed while the interactive UI is open; a display that has
//!   never answered is not retried (the manager is only notified if there is one).
//!
//! Time is the caller's (monotonic milliseconds): the manager never
//! sleeps, it publishes its next deadline ([`RescanManager::next_deadline`]).

/// Settings (starting values from the spec, to be recalibrated on hardware).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RescanConfig {
    /// Debounce of a burst of events.
    pub debounce_ms: u64,
    /// Cap on how long repeated events can postpone a scan.
    pub max_defer_ms: u64,
    /// Floor between two noisy-hotplug scans.
    pub noisy_floor_ms: u64,
    /// Duration beyond which a scan is disowned.
    pub watchdog_ms: u64,
    /// Consecutive disownments before waiting for a new event.
    pub max_disavowals: u32,
    /// Delay before retrying after a disownment.
    pub disavow_retry_ms: u64,
    /// Retries of pending displays; the last step repeats indefinitely.
    pub waiting_ladder_ms: Vec<u64>,
}

impl Default for RescanConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 600,
            max_defer_ms: 3_000,
            noisy_floor_ms: 30_000,
            watchdog_ms: 10_000,
            max_disavowals: 3,
            disavow_retry_ms: 2_000,
            waiting_ladder_ms: vec![2_000, 5_000, 10_000, 20_000, 60_000],
        }
    }
}

/// Origin of a scan request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Daemon startup: immediate.
    Startup,
    /// User request: immediate, never throttled.
    Manual,
    /// Wake from sleep, unlock, display turned back on, or request from the state
    /// machine: debounced, never throttled by the floor.
    Resume,
    /// Display plugged/unplugged: debounced, throttled if noisy.
    Hotplug,
}

/// Action requested from the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start an enumeration carrying this generation number.
    Start {
        /// Generation number to hand back in [`RescanManager::finished`].
        generation: u64,
    },
}

/// Result of a finished scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanOutcome {
    /// The list was accepted (not rejected as fake).
    pub accepted: bool,
    /// At least one already-known display is unavailable: it must be retried.
    pub waiting: bool,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    first_ms: u64,
    due_ms: u64,
    hotplug_only: bool,
}

#[derive(Clone, Copy, Debug)]
struct Running {
    generation: u64,
    started_ms: u64,
}

/// Rescan manager.
#[derive(Clone, Debug)]
pub struct RescanManager {
    config: RescanConfig,
    pending: Option<Pending>,
    running: Option<Running>,
    generation: u64,
    last_hotplug_scan_ms: Option<u64>,
    ladder_step: usize,
    retry_due_ms: Option<u64>,
    disavowals: u32,
    halted: bool,
    ui_open: bool,
    suspended: bool,
}

impl RescanManager {
    /// Fresh manager.
    pub fn new(config: RescanConfig) -> Self {
        Self {
            config,
            pending: None,
            running: None,
            generation: 0,
            last_hotplug_scan_ms: None,
            ladder_step: 0,
            retry_due_ms: None,
            disavowals: 0,
            halted: false,
            ui_open: false,
            suspended: false,
        }
    }

    /// Records a scan request. Then call [`RescanManager::poll`].
    pub fn trigger(&mut self, now_ms: u64, trigger: Trigger, extra_delay_ms: u64) {
        self.halted = false;
        self.disavowals = 0;
        let (due, hotplug) = match trigger {
            Trigger::Startup => (now_ms, false),
            Trigger::Manual => {
                self.ladder_step = 0;
                (now_ms, false)
            }
            Trigger::Resume => (now_ms + self.config.debounce_ms.max(extra_delay_ms), false),
            Trigger::Hotplug => (now_ms + self.config.debounce_ms, true),
        };
        let immediate = matches!(trigger, Trigger::Startup | Trigger::Manual);
        self.pending = Some(match self.pending {
            None => Pending {
                first_ms: now_ms,
                due_ms: due,
                hotplug_only: hotplug,
            },
            Some(p) => {
                let cap = p.first_ms + self.config.max_defer_ms;
                // "Trailing" debounce (each event pushes the deadline back), capped so
                // that a continuous burst does not delay the scan indefinitely.
                let merged = if immediate {
                    due.min(p.due_ms)
                } else {
                    p.due_ms.max(due.min(cap))
                };
                Pending {
                    first_ms: p.first_ms,
                    due_ms: merged,
                    hotplug_only: p.hotplug_only && hotplug,
                }
            }
        });
    }

    /// Suspends (sleep, inactive session) or resumes starting scans.
    /// Requests received during the suspension are kept.
    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
    }

    /// The interactive UI (popup) is open: periodic retries are
    /// postponed (a re-creation would close it).
    pub fn set_ui_open(&mut self, open: bool) {
        self.ui_open = open;
    }

    fn effective_due(&self, pending: &Pending) -> u64 {
        let floor = match (pending.hotplug_only, self.last_hotplug_scan_ms) {
            (true, Some(last)) => last + self.config.noisy_floor_ms,
            _ => 0,
        };
        pending.due_ms.max(floor)
    }

    /// Evaluates deadlines at instant `now_ms` and returns the scans to start.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Action> {
        if let Some(running) = self.running
            && now_ms >= running.started_ms + self.config.watchdog_ms
        {
            // Stuck scan: disowned, its late result will be dropped.
            self.running = None;
            self.disavowals += 1;
            if self.disavowals >= self.config.max_disavowals {
                self.halted = true;
            } else {
                self.pending = Some(Pending {
                    first_ms: now_ms,
                    due_ms: now_ms + self.config.disavow_retry_ms,
                    hotplug_only: false,
                });
            }
        }
        if self.running.is_none()
            && !self.halted
            && !self.suspended
            && !self.ui_open
            && self.retry_due_ms.is_some_and(|due| now_ms >= due)
        {
            self.retry_due_ms = None;
            self.pending = self.pending.or(Some(Pending {
                first_ms: now_ms,
                due_ms: now_ms,
                hotplug_only: false,
            }));
        }
        if self.running.is_some() || self.halted || self.suspended {
            return Vec::new();
        }
        let Some(pending) = self.pending else {
            return Vec::new();
        };
        if now_ms < self.effective_due(&pending) {
            return Vec::new();
        }
        self.pending = None;
        self.generation += 1;
        self.running = Some(Running {
            generation: self.generation,
            started_ms: now_ms,
        });
        if pending.hotplug_only {
            self.last_hotplug_scan_ms = Some(now_ms);
        }
        vec![Action::Start {
            generation: self.generation,
        }]
    }

    /// Reports the end of a scan. Returns `false` if its result must be **dropped**
    /// (disowned or outdated scan); otherwise updates the retries.
    pub fn finished(&mut self, now_ms: u64, generation: u64, outcome: ScanOutcome) -> bool {
        match self.running {
            Some(running) if running.generation == generation => self.running = None,
            _ => return false,
        }
        self.disavowals = 0;
        if outcome.accepted {
            if outcome.waiting {
                let ladder = &self.config.waiting_ladder_ms;
                let step = self.ladder_step.min(ladder.len().saturating_sub(1));
                let delay = ladder.get(step).copied().unwrap_or(60_000);
                self.ladder_step += 1;
                self.retry_due_ms = Some(now_ms + delay);
            } else {
                self.ladder_step = 0;
                self.retry_due_ms = None;
            }
        }
        true
    }

    /// Next time at which to call [`RescanManager::poll`], if any.
    pub fn next_deadline(&self) -> Option<u64> {
        let mut deadlines = Vec::new();
        if let Some(running) = self.running {
            deadlines.push(running.started_ms + self.config.watchdog_ms);
        } else if !self.halted && !self.suspended {
            if let Some(pending) = self.pending {
                deadlines.push(self.effective_due(&pending));
            }
            if let (Some(due), false) = (self.retry_due_ms, self.ui_open) {
                deadlines.push(due);
            }
        }
        deadlines.into_iter().min()
    }

    /// This generation number is the one of the running scan (otherwise its result is stale).
    pub fn is_current(&self, generation: u64) -> bool {
        self.running.is_some_and(|r| r.generation == generation)
    }

    /// A scan is running.
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Automatic scans are stopped (3 disownments) until the next event.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// A request is waiting for its deadline.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}
