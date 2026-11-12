//! Control state machine (no I/O): see `specs/ARCHITECTURE.md` §2.
//!
//! The [`Controller`] receives timestamped [`Event`]s (`Now`) and returns
//! [`Command`]s to be executed by the adapters. It never touches the
//! hardware, never reads the clock and never sleeps: the caller is the one
//! who drives time (virtual clock in tests).
//!
//! Three layers: base mode (auto-lux > schedule > static), manual
//! hold, idle overlay. The overlay never modifies the master.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::backend::DisplayId;
use crate::clock::Now;
use crate::config::{Config, IdleConfig, Tuning};
use crate::curve::{Curve, LearnOutcome, log_scale};
use crate::filter::Filter;
use crate::gate::Gate;
use crate::range::{Range, clamp_percent, reconstruct_master};
use crate::schedule::{Schedule, ScheduleError, Suspension};

/// Retry delays after a fake display list is rejected (ms).
pub const FAKE_LIST_RETRY_MS: [u64; 4] = [2_000, 5_000, 10_000, 20_000];
/// Delay of the second re-apply after a wake (ms).
pub const SECOND_REAPPLY_MS: u64 = 3_000;
/// Window after a wake during which a display whose read level has drifted is rewritten
/// (some monitors finish resetting long after coming back from sleep).
pub const WAKE_DRIFT_WINDOW_MS: u64 = 60_000;
/// Delay of the next check when a drift has just been corrected.
pub const WAKE_DRIFT_RECHECK_MS: u64 = 5_000;
/// Rescan delay after a wake (ms): displays do not respond right away.
pub const WAKE_RESCAN_MS: u64 = 1_500;
/// Consecutive write failures before declaring a display unavailable.
pub const FAILURES_BEFORE_UNAVAILABLE: u8 = 3;
/// Light gap (log scale) that ends the auto pause after a preset.
const PAUSE_BREAK_LOG_DELTA: f64 = 0.3;

/// Settings relevant to the state machine.
#[derive(Clone, Debug, PartialEq)]
pub struct ControllerConfig {
    /// Validated schedule.
    pub schedule: Schedule,
    /// Schedule enabled.
    pub schedule_enabled: bool,
    /// Idle dimming.
    pub idle: IdleConfig,
    /// Auto-brightness requested.
    pub auto_enabled: bool,
    /// URL, token and entity are set.
    pub auto_configured: bool,
    /// Fine settings.
    pub tuning: Tuning,
}

impl ControllerConfig {
    /// Extracts the settings from a validated configuration.
    pub fn from_config(config: &Config, auto_configured: bool) -> Result<Self, ScheduleError> {
        Ok(Self {
            schedule: config.build_schedule()?,
            schedule_enabled: config.schedule.enabled,
            idle: config.idle.clone(),
            auto_enabled: config.auto.enabled,
            auto_configured,
            tuning: config.auto.tuning.clone(),
        })
    }
}

impl Default for ControllerConfig {
    fn default() -> Self {
        let config = Config::default();
        Self {
            schedule: Schedule::default(),
            schedule_enabled: config.schedule.enabled,
            idle: config.idle,
            auto_enabled: false,
            auto_configured: false,
            tuning: config.auto.tuning,
        }
    }
}

/// State restored at startup (state file).
#[derive(Clone, Debug, Default)]
pub struct InitialState {
    /// Last known master.
    pub master: Option<u8>,
    /// Schedule suspension in progress.
    pub suspension: Option<Suspension>,
    /// Learned curve.
    pub curve: Curve,
    /// Ranges of known displays (even unplugged).
    pub ranges: BTreeMap<DisplayId, Range>,
}

/// Health of the Home Assistant link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaHealth {
    /// Healthy link.
    Ok,
    /// Offline (consecutive failures).
    Offline,
    /// Degraded (no valid measurement for too long).
    Degraded,
}

/// Features that can be enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// Schedule.
    Schedule,
    /// Idle dimming.
    IdleDim,
    /// Auto-brightness.
    Auto,
}

/// State of a display as seen by the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayState {
    /// Responds and can be driven.
    Controllable,
    /// Already controllable or known to the config, does not respond: retried.
    Unavailable,
    /// Has never responded: retried only on an event.
    NotControllable,
    /// No longer present in the last accepted list.
    Absent,
}

/// Display coming from an enumeration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// Stable identifier.
    pub id: DisplayId,
    /// The probe succeeded (valid read, maximum > 0).
    pub controllable: bool,
    /// Display level read (0-100), if it was read.
    pub level: Option<u8>,
}

/// Input events.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Set the master (0-100, otherwise rejected).
    SetMaster(i64),
    /// Master change (−100..=100).
    Step(i64),
    /// Direct level of a display (0-100 of its level, outside the range).
    SetDisplay {
        /// Targeted display.
        display: DisplayId,
        /// Display level.
        level: i64,
    },
    /// Preset: master level held temporarily, never learned.
    Preset(i64),
    /// New range for a display.
    SetRange {
        /// Targeted display.
        display: DisplayId,
        /// Raw lower bound.
        low: i64,
        /// Raw upper bound.
        high: i64,
    },
    /// Activation of a feature.
    Feature(Feature, bool),
    /// Forget the learned curve.
    ResetCurve,
    /// Undo the last learning.
    UndoLearn,
    /// Configuration reloaded.
    Reload(Box<ControllerConfig>),
    /// Periodic evaluation of the schedule (60 s).
    PlanningTick,
    /// Ramp step requested by [`Command::ArmRampTimer`].
    RampTick,
    /// Valid lux measurement.
    Lux(f64),
    /// Health change of the Home Assistant link.
    Ha(HaHealth),
    /// Auto-brightness becomes configured (URL, entity and token present) or no longer is.
    AutoConfigured(bool),
    /// Idle probe.
    IdleProbe {
        /// Session idle duration (ms).
        idle_ms: u64,
        /// A blocker is active (inhibitor, fullscreen, microphone/camera).
        blocked: bool,
    },
    /// User activity resumed.
    Activity,
    /// Wake from sleep, unlock, hotplug, display turned back on.
    Wake,
    /// Second deferred re-apply after a wake.
    Reapply,
    /// New display list coming from a rescan.
    DisplaysListed(Vec<Listed>),
    /// Result of a write requested by [`Command::Write`].
    WriteResult {
        /// Display concerned.
        display: DisplayId,
        /// Level written.
        level: u8,
        /// Write succeeded.
        ok: bool,
    },
}

/// Data to persist (the daemon debounces).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Persist {
    /// Current master.
    Master,
    /// Learned curve.
    Curve,
    /// Schedule suspension.
    Suspension,
    /// Display ranges.
    Ranges,
}

/// Information intended for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Learning refused: too many contradicted points.
    LearnRefused {
        /// Number of contradicted points.
        contradicted: usize,
    },
    /// Display marked unavailable after repeated failures.
    DisplayUnavailable(DisplayId),
    /// Display list rejected (fake); new attempt scheduled.
    ListRejected {
        /// Delay before the next rescan (ms).
        retry_in_ms: u64,
    },
}

/// Output commands, executed by the adapters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Write the display level (0-100); the executor coalesces and converts to raw.
    Write {
        /// Targeted display.
        display: DisplayId,
        /// Display level 0-100.
        level: u8,
    },
    /// Start a rescan (targeted if `target`), after `delay_ms`.
    Rescan {
        /// Targeted display, or all.
        target: Option<DisplayId>,
        /// Delay before scanning.
        delay_ms: u64,
    },
    /// Emit [`Event::RampTick`] in `in_ms`.
    ArmRampTimer {
        /// Delay in ms.
        in_ms: u64,
    },
    /// Emit [`Event::Reapply`] in `in_ms`.
    ArmReapply {
        /// Delay in ms.
        in_ms: u64,
    },
    /// Bring forward the next Home Assistant poll (confirmation of a jump).
    PollLuxSoon {
        /// Delay in ms.
        in_ms: u64,
    },
    /// Persist a piece of data.
    Persist(Persist),
    /// Information for the interface.
    Notify(Notice),
}

/// Invalid user requests, rejected with no effect.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ControlError {
    /// Value out of bounds.
    #[error("value out of range ({what}): {value}")]
    OutOfRange {
        /// Quantity concerned: `master-level`, `display-level`, `step` or `preset-level`.
        what: &'static str,
        /// Value received.
        value: i64,
    },
    /// Unknown display.
    #[error("unknown display: {0}")]
    UnknownDisplay(DisplayId),
    /// Display known but not controllable at the moment.
    #[error("display unavailable: {0}")]
    DisplayUnavailable(DisplayId),
    /// No controllable display.
    #[error("no controllable display")]
    NoControllableDisplay,
}

/// Source that decides the master (layer 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseMode {
    /// Home Assistant lux sensor.
    AutoLux,
    /// Hourly schedule.
    Planning,
    /// The last master stays.
    Static,
}

/// State of auto-brightness for the status panel.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoInfo {
    /// Lux smoothed by the filter (no measurement: `None`).
    pub smoothed_lux: Option<f64>,
    /// Most recent lux (measurement awaiting confirmation, otherwise smoothed lux).
    pub latest_lux: Option<f64>,
    /// A large gap is awaiting its confirmation.
    pub adjusting: bool,
    /// Points of the curve actually in use (lux, master level).
    pub curve: Vec<(f64, u8)>,
    /// The curve is the default curve (nothing learned).
    pub default_curve: bool,
    /// Target level from the curve for the smoothed lux.
    pub target: Option<u8>,
}

/// Snapshot of the state, for the interface and tests.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// Current master.
    pub master: Option<u8>,
    /// Last master confirmed written to all targeted displays.
    pub applied_master: Option<u8>,
    /// Base source.
    pub mode: BaseMode,
    /// Schedule suspension.
    pub suspension: Option<Suspension>,
    /// Idle dimming in progress.
    pub dimmed: bool,
    /// Ramp target in progress.
    pub ramp_target: Option<u8>,
    /// Home Assistant health.
    pub ha_health: HaHealth,
    /// At least one valid measurement received.
    pub has_measure: bool,
    /// Auto paused after a preset.
    pub auto_paused: bool,
    /// Displays: identifier, state, last commanded level, range.
    pub displays: Vec<(DisplayId, DisplayState, Option<u8>, Range)>,
}

#[derive(Clone, Debug)]
struct Disp {
    range: Range,
    state: DisplayState,
    level: Option<u8>,
    failures: u8,
}

#[derive(Clone, Debug)]
struct Pending {
    master: u8,
    waiting: BTreeSet<DisplayId>,
}

#[derive(Clone, Copy, Debug)]
struct Pause {
    until_ms: u64,
    lux: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ManualKind {
    Normal,
    Preset,
}

/// Control state machine.
pub struct Controller {
    cfg: ControllerConfig,
    displays: BTreeMap<DisplayId, Disp>,
    master: Option<u8>,
    applied_master: Option<u8>,
    pending: Option<Pending>,
    suspension: Option<Suspension>,
    dimmed: bool,
    ramp: Option<u8>,
    ramp_armed: bool,
    health: HaHealth,
    has_measure: bool,
    pause: Option<Pause>,
    filter: Filter,
    gate: Gate,
    curve: Curve,
    reapply_on_list: bool,
    /// End of the drift-correction window that follows a wake.
    drift_until_ms: Option<u64>,
    fake_attempts: usize,
}

impl Controller {
    /// Creates the machine from the configuration and the persisted state.
    pub fn new(cfg: ControllerConfig, initial: InitialState) -> Self {
        let displays = initial
            .ranges
            .into_iter()
            .map(|(id, range)| {
                (
                    id,
                    Disp {
                        range,
                        state: DisplayState::Absent,
                        level: None,
                        failures: 0,
                    },
                )
            })
            .collect();
        Self {
            filter: Filter::new(cfg.tuning.filter_params()),
            gate: Gate::new(cfg.tuning.gate_params()),
            cfg,
            displays,
            master: initial.master.map(|m| m.min(100)),
            applied_master: None,
            pending: None,
            suspension: initial.suspension,
            dimmed: false,
            ramp: None,
            ramp_armed: false,
            health: HaHealth::Ok,
            has_measure: false,
            pause: None,
            curve: initial.curve,
            reapply_on_list: false,
            drift_until_ms: None,
            fake_attempts: 0,
        }
    }

    /// Base source: auto-lux > schedule > static.
    pub fn base_mode(&self) -> BaseMode {
        if self.auto_owner() {
            BaseMode::AutoLux
        } else if self.planning_active() {
            BaseMode::Planning
        } else {
            BaseMode::Static
        }
    }

    /// Learned curve.
    pub fn curve(&self) -> &Curve {
        &self.curve
    }

    /// State of auto-brightness (filter, curve, target).
    pub fn auto_info(&self) -> AutoInfo {
        let has_value = self.filter.has_value();
        AutoInfo {
            smoothed_lux: has_value.then(|| self.filter.smoothed_lux()),
            latest_lux: has_value.then(|| self.filter.latest_lux()),
            adjusting: self.filter.is_pending() || self.ramp.is_some(),
            curve: self
                .curve
                .effective_points()
                .iter()
                .map(|p| (p.lux, p.level))
                .collect(),
            default_curve: self.curve.points().is_empty(),
            target: has_value.then(|| self.curve.level_at(self.filter.smoothed_lux())),
        }
    }

    /// Snapshot of the state.
    pub fn status(&self) -> Status {
        Status {
            master: self.master,
            applied_master: self.applied_master,
            mode: self.base_mode(),
            suspension: self.suspension,
            dimmed: self.dimmed,
            ramp_target: self.ramp,
            ha_health: self.health,
            has_measure: self.has_measure,
            auto_paused: self.pause.is_some(),
            displays: self
                .displays
                .iter()
                .map(|(id, d)| (id.clone(), d.state, d.level, d.range))
                .collect(),
        }
    }

    /// Remembers the range of a display, seen or not (ranges from the
    /// configuration file, including for unplugged displays). No write:
    /// it takes effect at the next application (reload, wake…).
    pub fn remember_range(&mut self, id: DisplayId, range: Range) {
        self.displays
            .entry(id)
            .and_modify(|d| d.range = range)
            .or_insert(Disp {
                range,
                state: DisplayState::Absent,
                level: None,
                failures: 0,
            });
    }

    /// Handles an event and returns the commands to execute.
    ///
    /// An invalid user request returns an error with no effect at all.
    pub fn handle(&mut self, now: Now, event: Event) -> Result<Vec<Command>, ControlError> {
        let mut out = Vec::new();
        match event {
            Event::SetMaster(m) => {
                let m = percent("master-level", m)?;
                self.require_controllable()?;
                self.manual_policy(now, m, ManualKind::Normal, &mut out);
                self.apply_master(m, false, &mut out);
            }
            Event::Step(delta) => {
                if !(-100..=100).contains(&delta) {
                    return Err(ControlError::OutOfRange {
                        what: "step",
                        value: delta,
                    });
                }
                self.require_controllable()?;
                self.ensure_master(&mut out);
                let m = clamp_percent(i64::from(self.master.unwrap_or(50)) + delta);
                self.manual_policy(now, m, ManualKind::Normal, &mut out);
                self.apply_master(m, false, &mut out);
            }
            Event::Preset(level) => {
                let m = percent("preset-level", level)?;
                self.require_controllable()?;
                self.manual_policy(now, m, ManualKind::Preset, &mut out);
                self.apply_master(m, false, &mut out);
            }
            Event::SetDisplay { display, level } => {
                self.set_display(now, display, level, &mut out)?
            }
            Event::SetRange { display, low, high } => {
                self.set_range(display, low, high, &mut out)?
            }
            Event::Feature(feature, on) => self.set_feature(now, feature, on, &mut out),
            Event::ResetCurve => {
                self.curve.reset();
                self.gate.reset();
                out.push(Command::Persist(Persist::Curve));
                if self.auto_owner() && !self.dimmed {
                    self.apply_auto_forced(&mut out);
                }
            }
            Event::UndoLearn => {
                if self.curve.undo() {
                    out.push(Command::Persist(Persist::Curve));
                }
            }
            Event::Reload(cfg) => self.reload(now, *cfg, &mut out),
            Event::PlanningTick => self.planning_tick(now, &mut out),
            Event::RampTick => self.ramp_tick(&mut out),
            Event::Lux(lux) => self.on_lux(now, lux, &mut out),
            Event::Ha(health) => self.on_health(now, health, &mut out),
            Event::AutoConfigured(configured) => self.on_auto_configured(now, configured, &mut out),
            Event::IdleProbe { idle_ms, blocked } => {
                self.on_idle_probe(now, idle_ms, blocked, &mut out)
            }
            Event::Activity => {
                if self.dimmed {
                    self.restore(now, &mut out);
                }
            }
            Event::Wake => {
                self.reapply_on_list = true;
                self.drift_until_ms = Some(now.ms + WAKE_DRIFT_WINDOW_MS);
                out.push(Command::Rescan {
                    target: None,
                    delay_ms: WAKE_RESCAN_MS,
                });
            }
            Event::Reapply => self.reapply(now, &mut out),
            Event::DisplaysListed(list) => self.on_list(now, list, &mut out),
            Event::WriteResult { display, level, ok } => {
                self.on_write_result(&display, level, ok, &mut out)
            }
        }
        Ok(out)
    }

    // ---- Predicates ---------------------------------------------------------

    fn auto_owner(&self) -> bool {
        self.cfg.auto_enabled
            && self.cfg.auto_configured
            && self.health == HaHealth::Ok
            && self.has_measure
    }

    fn planning_active(&self) -> bool {
        self.cfg.schedule_enabled && !self.cfg.schedule.is_empty()
    }

    fn require_controllable(&self) -> Result<(), ControlError> {
        if self
            .displays
            .values()
            .any(|d| d.state == DisplayState::Controllable)
        {
            Ok(())
        } else {
            Err(ControlError::NoControllableDisplay)
        }
    }

    /// Rebuilds the master if it is unknown (spec §4.5).
    fn ensure_master(&mut self, out: &mut Vec<Command>) {
        if self.master.is_none() {
            self.master = Some(self.reconstructed_master());
            out.push(Command::Persist(Persist::Master));
        }
    }

    fn reconstructed_master(&self) -> u8 {
        reconstruct_master(
            self.displays
                .values()
                .filter(|d| d.state == DisplayState::Controllable)
                .filter_map(|d| d.level.map(|l| (d.range, i64::from(l)))),
        )
    }

    // ---- Writes ---------------------------------------------------------

    /// Applies a master to all controllable displays. Without `force`, only
    /// displays whose level changes are written.
    fn apply_master(&mut self, master: u8, force: bool, out: &mut Vec<Command>) {
        self.master = Some(master);
        out.push(Command::Persist(Persist::Master));
        let mut waiting = BTreeSet::new();
        let mut targeted = false;
        for (id, d) in &mut self.displays {
            if d.state != DisplayState::Controllable {
                continue;
            }
            targeted = true;
            let level = d.range.display_level(i64::from(master));
            if force || d.level != Some(level) {
                d.level = Some(level);
                waiting.insert(id.clone());
                out.push(Command::Write {
                    display: id.clone(),
                    level,
                });
            }
        }
        if waiting.is_empty() {
            // Nothing to write: the master is only "applied" if there is at
            // least one display already at the right level (never with no display).
            self.applied_master = targeted.then_some(master);
            self.pending = None;
        } else {
            // Writes in flight: nothing is "applied" until they are confirmed.
            self.applied_master = None;
            self.pending = Some(Pending { master, waiting });
        }
    }

    fn idle_level_for(&self, range: Range) -> u8 {
        if self.cfg.idle.level_is_absolute {
            self.cfg.idle.level.min(100)
        } else {
            range.display_level(i64::from(self.cfg.idle.level))
        }
    }

    fn write_dim(&mut self, force: bool, out: &mut Vec<Command>) {
        let absolute = self.cfg.idle.level_is_absolute;
        let idle = i64::from(self.cfg.idle.level);
        for (id, d) in &mut self.displays {
            if d.state != DisplayState::Controllable {
                continue;
            }
            let level = if absolute {
                clamp_percent(idle)
            } else {
                d.range.display_level(idle)
            };
            if force || d.level != Some(level) {
                d.level = Some(level);
                out.push(Command::Write {
                    display: id.clone(),
                    level,
                });
            }
        }
    }

    fn on_write_result(
        &mut self,
        display: &DisplayId,
        level: u8,
        ok: bool,
        out: &mut Vec<Command>,
    ) {
        let Some(d) = self.displays.get_mut(display) else {
            return;
        };
        if ok {
            d.failures = 0;
            if let Some(pending) = &mut self.pending {
                pending.waiting.remove(display);
                if pending.waiting.is_empty() {
                    self.applied_master = Some(pending.master);
                    self.pending = None;
                }
            }
            return;
        }
        if d.level != Some(level) {
            return; // stale result: a more recent value has been commanded
        }
        // A failure is never remembered as applied.
        d.level = None;
        d.failures = d.failures.saturating_add(1);
        self.pending = None;
        self.applied_master = None;
        if d.failures >= FAILURES_BEFORE_UNAVAILABLE {
            d.state = DisplayState::Unavailable;
            d.failures = 0;
            out.push(Command::Notify(Notice::DisplayUnavailable(display.clone())));
            out.push(Command::Rescan {
                target: Some(display.clone()),
                delay_ms: 0,
            });
        }
    }

    // ---- Manual actions ----------------------------------------------------

    /// Effects of a manual action depending on the base mode (excluding the write).
    fn manual_policy(&mut self, now: Now, master: u8, kind: ManualKind, out: &mut Vec<Command>) {
        self.ramp = None;
        self.dimmed = false;
        match self.base_mode() {
            BaseMode::AutoLux => {
                let lux = self.filter.latest_lux();
                match kind {
                    ManualKind::Normal => {
                        let policy = self.cfg.tuning.learn_policy();
                        match self.curve.learn(lux, i64::from(master), None, &policy) {
                            LearnOutcome::Learned => out.push(Command::Persist(Persist::Curve)),
                            LearnOutcome::Refused { contradicted } => {
                                out.push(Command::Notify(Notice::LearnRefused { contradicted }));
                            }
                            LearnOutcome::IgnoredTooClose => {}
                        }
                    }
                    ManualKind::Preset => {
                        let minutes = u64::from(self.cfg.tuning.preset_pause_minutes);
                        self.pause = Some(Pause {
                            until_ms: now.ms + minutes * 60_000,
                            lux,
                        });
                    }
                }
                self.gate.reset_to(master);
            }
            BaseMode::Planning => {
                self.suspension = Suspension::begin(&self.cfg.schedule, now.minute);
                out.push(Command::Persist(Persist::Suspension));
            }
            BaseMode::Static => {}
        }
    }

    fn set_display(
        &mut self,
        now: Now,
        display: DisplayId,
        level: i64,
        out: &mut Vec<Command>,
    ) -> Result<(), ControlError> {
        let level = percent("display-level", level)?;
        match self.displays.get(&display).map(|d| d.state) {
            None => return Err(ControlError::UnknownDisplay(display)),
            Some(DisplayState::Controllable) => {}
            Some(_) => return Err(ControlError::DisplayUnavailable(display)),
        }
        if let Some(d) = self.displays.get_mut(&display) {
            d.level = Some(level);
        }
        let master = self.reconstructed_master();
        self.manual_policy(now, master, ManualKind::Normal, out);
        self.master = Some(master);
        self.applied_master = None;
        self.pending = None;
        out.push(Command::Persist(Persist::Master));
        out.push(Command::Write { display, level });
        Ok(())
    }

    fn set_range(
        &mut self,
        display: DisplayId,
        low: i64,
        high: i64,
        out: &mut Vec<Command>,
    ) -> Result<(), ControlError> {
        if !self.displays.contains_key(&display) {
            return Err(ControlError::UnknownDisplay(display));
        }
        // The unknown master is rebuilt with the old range.
        self.ensure_master(out);
        if let Some(d) = self.displays.get_mut(&display) {
            d.range = Range::normalized(low, high);
        }
        out.push(Command::Persist(Persist::Ranges));
        if !self.dimmed
            && let Some(m) = self.master
        {
            self.apply_master(m, true, out);
        }
        Ok(())
    }

    // ---- Features and configuration ----------------------------------------------

    fn set_feature(&mut self, now: Now, feature: Feature, on: bool, out: &mut Vec<Command>) {
        match feature {
            Feature::Schedule => {
                self.cfg.schedule_enabled = on;
                self.suspension = None;
                out.push(Command::Persist(Persist::Suspension));
                if on && !self.dimmed && self.base_mode() == BaseMode::Planning {
                    if let Some(v) = self.cfg.schedule.value_at(now.minute) {
                        self.apply_master(v, true, out);
                    }
                }
            }
            Feature::IdleDim => {
                self.cfg.idle.enabled = on;
                if !on && self.dimmed {
                    self.restore(now, out);
                }
            }
            Feature::Auto => {
                self.cfg.auto_enabled = on;
                self.ramp = None;
                self.gate.reset();
                if !self.dimmed {
                    if on && self.auto_owner() {
                        self.apply_auto_forced(out);
                    } else if !on {
                        self.reapply(now, out);
                    }
                }
            }
        }
    }

    fn reload(&mut self, now: Now, cfg: ControllerConfig, out: &mut Vec<Command>) {
        if cfg.tuning != self.cfg.tuning {
            self.filter = Filter::new(cfg.tuning.filter_params());
            self.gate = Gate::new(cfg.tuning.gate_params());
            self.has_measure = false;
        }
        self.cfg = cfg;
        self.suspension = None;
        out.push(Command::Persist(Persist::Suspension));
        if !self.dimmed {
            self.reapply(now, out);
        } else {
            self.write_dim(true, out);
        }
    }

    // ---- Schedule --------------------------------------------------------------

    fn planning_tick(&mut self, now: Now, out: &mut Vec<Command>) {
        if self.dimmed || self.auto_owner() || !self.planning_active() {
            return;
        }
        if let Some(s) = self.suspension {
            if !s.should_resume(now.minute) {
                return;
            }
            self.suspension = None;
            out.push(Command::Persist(Persist::Suspension));
        }
        let Some(value) = self.cfg.schedule.value_at(now.minute) else {
            return;
        };
        let in_flight = self.pending.as_ref().is_some_and(|p| p.master == value);
        if self.applied_master != Some(value) && !in_flight {
            self.apply_master(value, false, out);
        }
    }

    // ---- Auto-brightness --------------------------------------------------------

    fn apply_auto_forced(&mut self, out: &mut Vec<Command>) {
        let target = self.curve.level_at(self.filter.smoothed_lux());
        self.gate.reset_to(target);
        self.ramp = None;
        self.apply_master(target, true, out);
    }

    fn start_ramp(&mut self, target: u8, out: &mut Vec<Command>) {
        match self.master {
            Some(m) if m == target => self.ramp = None,
            None => self.apply_master(target, true, out),
            Some(_) => {
                self.ramp = Some(target);
                if !self.ramp_armed {
                    self.ramp_armed = true;
                    out.push(Command::ArmRampTimer {
                        in_ms: u64::from(self.cfg.tuning.ramp_step_ms),
                    });
                }
            }
        }
    }

    fn ramp_tick(&mut self, out: &mut Vec<Command>) {
        self.ramp_armed = false;
        let Some(target) = self.ramp else {
            return;
        };
        if self.dimmed || !self.auto_owner() {
            self.ramp = None;
            return;
        }
        let current = self.master.unwrap_or(target);
        let next = match current.cmp(&target) {
            std::cmp::Ordering::Less => current + 1,
            std::cmp::Ordering::Greater => current - 1,
            std::cmp::Ordering::Equal => current,
        };
        self.apply_master(next, false, out);
        if next == target {
            self.ramp = None;
        } else {
            self.ramp_armed = true;
            out.push(Command::ArmRampTimer {
                in_ms: u64::from(self.cfg.tuning.ramp_step_ms),
            });
        }
    }

    fn on_lux(&mut self, now: Now, lux: f64, out: &mut Vec<Command>) {
        self.filter.push(lux);
        let first = !self.has_measure;
        self.has_measure = true;
        if !(self.cfg.auto_enabled && self.cfg.auto_configured)
            || self.health != HaHealth::Ok
            || self.dimmed
        {
            return;
        }
        if let Some(pause) = self.pause {
            let moved = (log_scale(self.filter.smoothed_lux()) - log_scale(pause.lux)).abs()
                >= PAUSE_BREAK_LOG_DELTA;
            if now.ms < pause.until_ms && !moved {
                return;
            }
            self.pause = None;
        }
        if first {
            self.apply_auto_forced(out);
        } else {
            let target = self.curve.level_at(self.filter.smoothed_lux());
            if self.gate.decide(target) {
                self.start_ramp(target, out);
            }
        }
        if self.filter.is_pending() {
            out.push(Command::PollLuxSoon { in_ms: 3_000 });
        }
    }

    fn on_health(&mut self, now: Now, health: HaHealth, out: &mut Vec<Command>) {
        let previous = std::mem::replace(&mut self.health, health);
        if !(self.cfg.auto_enabled && self.cfg.auto_configured) || previous == health {
            return;
        }
        match health {
            HaHealth::Ok => {
                self.gate.reset();
                if self.auto_owner() && !self.dimmed {
                    let target = self.curve.level_at(self.filter.smoothed_lux());
                    if self.gate.decide(target) {
                        self.start_ramp(target, out);
                    }
                }
            }
            HaHealth::Offline | HaHealth::Degraded => {
                self.ramp = None;
                if !self.dimmed && previous == HaHealth::Ok {
                    // The schedule (or the last master) resumes, value forced.
                    self.reapply(now, out);
                }
            }
        }
    }

    fn on_auto_configured(&mut self, now: Now, configured: bool, out: &mut Vec<Command>) {
        if self.cfg.auto_configured == configured {
            return;
        }
        let was_owner = self.auto_owner();
        self.cfg.auto_configured = configured;
        if !configured {
            // No link possible anymore: start over from zero, the schedule or the last master resumes.
            self.has_measure = false;
            self.filter.reset();
            self.gate.reset();
            self.pause = None;
            if was_owner && !self.dimmed {
                self.reapply(now, out);
            }
        }
    }

    // ---- Idle -------------------------------------------------------------

    fn on_idle_probe(&mut self, now: Now, idle_ms: u64, blocked: bool, out: &mut Vec<Command>) {
        if !self.cfg.idle.enabled {
            return;
        }
        let threshold = u64::from(self.cfg.idle.minutes) * 60_000;
        if self.dimmed {
            if idle_ms < threshold || blocked {
                self.restore(now, out);
            }
        } else if idle_ms >= threshold && !blocked {
            self.dim(out);
        }
    }

    fn dim(&mut self, out: &mut Vec<Command>) {
        self.ramp = None;
        self.ensure_master(out);
        self.dimmed = true;
        self.applied_master = None;
        self.pending = None;
        self.write_dim(true, out);
    }

    fn restore(&mut self, now: Now, out: &mut Vec<Command>) {
        self.dimmed = false;
        self.reapply(now, out);
    }

    // ---- Priority re-apply ---------------------------------------------

    /// Priority re-apply: dimmed > auto (forced) > schedule
    /// (forced, unless suspension not yet expired) > last master.
    fn reapply(&mut self, now: Now, out: &mut Vec<Command>) {
        self.ramp = None;
        if self.dimmed {
            self.write_dim(true, out);
            return;
        }
        if let Some(s) = self.suspension
            && s.should_resume(now.minute)
        {
            self.suspension = None;
            out.push(Command::Persist(Persist::Suspension));
        }
        if self.auto_owner() {
            self.apply_auto_forced(out);
        } else if self.planning_active() && self.suspension.is_none() {
            match self.cfg.schedule.value_at(now.minute) {
                Some(v) => self.apply_master(v, true, out),
                None => self.write_master_if_known(out),
            }
        } else {
            self.write_master_if_known(out);
        }
    }

    fn write_master_if_known(&mut self, out: &mut Vec<Command>) {
        if let Some(m) = self.master {
            self.apply_master(m, true, out);
        }
    }

    // ---- Display list -------------------------------------------------------------

    fn on_list(&mut self, now: Now, list: Vec<Listed>, out: &mut Vec<Command>) {
        let had_controllable = self
            .displays
            .values()
            .any(|d| d.state == DisplayState::Controllable);
        let has_controllable = list.iter().any(|l| l.controllable);
        if had_controllable && !has_controllable {
            if let Some(&delay) = FAKE_LIST_RETRY_MS.get(self.fake_attempts) {
                self.fake_attempts += 1;
                out.push(Command::Notify(Notice::ListRejected { retry_in_ms: delay }));
                out.push(Command::Rescan {
                    target: None,
                    delay_ms: delay,
                });
                return; // fake list: keep the old one
            }
        }
        self.fake_attempts = 0;

        let listed: BTreeSet<&DisplayId> = list.iter().map(|l| &l.id).collect();
        for (id, d) in &mut self.displays {
            if !listed.contains(id) {
                d.state = DisplayState::Absent;
                d.level = None;
            }
        }
        let mut returning = Vec::new();
        for l in &list {
            let known = self.displays.contains_key(&l.id);
            let d = self.displays.entry(l.id.clone()).or_insert(Disp {
                range: Range::FULL,
                state: DisplayState::Absent,
                level: None,
                failures: 0,
            });
            let was = d.state;
            d.failures = 0;
            d.level = if l.controllable { l.level } else { None };
            d.state = if l.controllable {
                DisplayState::Controllable
            } else if matches!(was, DisplayState::Controllable | DisplayState::Unavailable) || known
            {
                DisplayState::Unavailable
            } else {
                DisplayState::NotControllable
            };
            if l.controllable && was != DisplayState::Controllable {
                returning.push(l.id.clone());
            }
        }

        if self.master.is_none() && has_controllable {
            // Startup without state: rebuild the master without writing anything.
            self.master = Some(self.reconstructed_master());
            out.push(Command::Persist(Persist::Master));
            self.reapply_on_list = false;
            return;
        }
        if self.reapply_on_list {
            self.reapply_on_list = false;
            self.reapply(now, out);
            out.push(Command::ArmReapply {
                in_ms: SECOND_REAPPLY_MS,
            });
        } else if !returning.is_empty() {
            self.write_to(&returning, out);
        } else if self.drift_until_ms.is_some_and(|until| now.ms < until) {
            self.correct_drift(&list, out);
        }
        // As long as the window that follows a wake is open, we re-check: the late
        // reset of a display is not always accompanied by a plug event.
        if self
            .drift_until_ms
            .is_some_and(|until| now.ms + WAKE_DRIFT_RECHECK_MS < until)
        {
            out.push(Command::Rescan {
                target: None,
                delay_ms: WAKE_DRIFT_RECHECK_MS,
            });
        }
    }

    /// Rewrites the displays whose read level differs from the expected one (late reset
    /// after a wake), then schedules a check. Only used after a wake: outside
    /// this window, a setting made with the monitor's buttons is never overwritten.
    fn correct_drift(&mut self, list: &[Listed], out: &mut Vec<Command>) {
        let mut drifted = Vec::new();
        for l in list.iter().filter(|l| l.controllable) {
            let Some(range) = self.displays.get(&l.id).map(|d| d.range) else {
                continue;
            };
            let expected = if self.dimmed {
                Some(self.idle_level_for(range))
            } else {
                self.master.map(|m| range.display_level(i64::from(m)))
            };
            if expected.is_some() && l.level.is_some() && l.level != expected {
                drifted.push(l.id.clone());
            }
        }
        if !drifted.is_empty() {
            self.write_to(&drifted, out);
        }
    }

    /// Sends the current level (idle or master) only to the given displays.
    fn write_to(&mut self, ids: &[DisplayId], out: &mut Vec<Command>) {
        for id in ids {
            let Some(range) = self.displays.get(id).map(|d| d.range) else {
                continue;
            };
            let level = if self.dimmed {
                Some(self.idle_level_for(range))
            } else {
                self.master.map(|m| range.display_level(i64::from(m)))
            };
            if let (Some(level), Some(d)) = (level, self.displays.get_mut(id)) {
                d.level = Some(level);
                out.push(Command::Write {
                    display: id.clone(),
                    level,
                });
            }
        }
    }
}

fn percent(what: &'static str, value: i64) -> Result<u8, ControlError> {
    if (0..=100).contains(&value) {
        Ok(clamp_percent(value))
    } else {
        Err(ControlError::OutOfRange { what, value })
    }
}
