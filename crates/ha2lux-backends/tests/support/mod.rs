//! Test bench: virtual clock + mock displays + command executor.
//!
//! No real `sleep`: time only advances through [`World::advance`], which
//! fires, in order, the timers requested by the state machine and
//! the schedule evaluation every 60 s.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use ha2lux_backends::mock::{MockBackend, MockControl};
use ha2lux_core::hw::raw_from_level;
use ha2lux_core::{
    BackendError, Clock, Command, ControlError, Controller, ControllerConfig, DisplayBackend,
    DisplayId, DisplayKind, Event, InitialState, Listed, MinuteOfDay, Range, VirtualClock,
};

/// Drives a future that completes immediately (mock backend).
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("le backend factice doit se terminer immédiatement"),
    }
}

pub mod monitor;

pub fn t(text: &str) -> MinuteOfDay {
    MinuteOfDay::parse(text).unwrap()
}

pub fn id(name: &str) -> DisplayId {
    DisplayId::new(name)
}

struct Sim {
    backend: MockBackend,
    control: MockControl,
    max: u32,
    present: bool,
    responds: bool,
}

/// A write observed on a mock display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    pub at_ms: u64,
    pub display: DisplayId,
    pub level: u8,
    pub raw: u32,
    pub ok: bool,
}

pub struct World {
    pub clock: VirtualClock,
    pub ctl: Controller,
    sims: BTreeMap<DisplayId, Sim>,
    pub log: Vec<Written>,
    pub rescans: Vec<(Option<DisplayId>, u64)>,
    pub notices: Vec<ha2lux_core::Notice>,
    pub persisted: Vec<ha2lux_core::Persist>,
    pub poll_soon: Vec<u64>,
    ramp_due: Option<u64>,
    reapply_due: Option<u64>,
    next_planning_tick: u64,
    pub planning_ticks: bool,
    /// Schedule suspension lifts: instant and suspension lifted.
    pub resumes: Vec<(ha2lux_core::Now, ha2lux_core::Suspension)>,
}

impl World {
    pub fn new(start: &str, cfg: ControllerConfig, initial: InitialState) -> Self {
        let clock = VirtualClock::new(t(start));
        let now = clock.now().ms;
        Self {
            clock,
            ctl: Controller::new(cfg, initial),
            sims: BTreeMap::new(),
            log: Vec::new(),
            rescans: Vec::new(),
            notices: Vec::new(),
            persisted: Vec::new(),
            poll_soon: Vec::new(),
            ramp_due: None,
            reapply_due: None,
            next_planning_tick: now.div_ceil(60_000) * 60_000,
            planning_ticks: true,
            resumes: Vec::new(),
        }
    }

    /// Plugs in a mock display (max 100: raw = level) at an initial level.
    pub fn plug(&mut self, name: &str, level: u8) -> MockControl {
        self.plug_with_max(name, 100, level)
    }

    pub fn plug_with_max(&mut self, name: &str, max: u32, level: u8) -> MockControl {
        let raw = raw_from_level(i64::from(level), max);
        let (backend, control) = MockBackend::new(name, DisplayKind::External, max, raw);
        self.sims.insert(
            id(name),
            Sim {
                backend,
                control: control.clone(),
                max,
                present: true,
                responds: true,
            },
        );
        control
    }

    pub fn unplug(&mut self, name: &str) {
        if let Some(s) = self.sims.get_mut(&id(name)) {
            s.present = false;
        }
    }

    pub fn replug(&mut self, name: &str) {
        if let Some(s) = self.sims.get_mut(&id(name)) {
            s.present = true;
        }
    }

    pub fn set_responding(&mut self, name: &str, responds: bool) {
        if let Some(s) = self.sims.get_mut(&id(name)) {
            s.responds = responds;
        }
    }

    /// Current display level (0-100) of the mock display, from its raw value.
    pub fn level_of(&self, name: &str) -> u8 {
        let s = &self.sims[&id(name)];
        ha2lux_core::hw::level_from_raw(s.control.raw(), s.max)
    }

    pub fn writes_to(&self, name: &str) -> Vec<u8> {
        self.log
            .iter()
            .filter(|w| w.ok && w.display == id(name))
            .map(|w| w.level)
            .collect()
    }

    /// Simulates displays that reset to 100% (wake from sleep).
    pub fn reset_hardware(&mut self) {
        for sim in self.sims.values() {
            sim.control.set_raw(sim.max);
        }
    }

    pub fn clear_log(&mut self) {
        self.log.clear();
    }

    /// Enumeration: probes each present display through the mock backend.
    pub fn rescan(&mut self) -> Result<(), ControlError> {
        let mut listed = Vec::new();
        for (display, sim) in &mut self.sims {
            if !sim.present {
                continue;
            }
            let controllable = sim.responds && block_on(sim.backend.probe()).is_ok();
            let level = if controllable {
                block_on(sim.backend.read())
                    .ok()
                    .map(|raw| ha2lux_core::hw::level_from_raw(raw, sim.max))
            } else {
                None
            };
            listed.push(Listed {
                id: display.clone(),
                controllable,
                level,
            });
        }
        self.send(Event::DisplaysListed(listed))
    }

    /// Sends an event and executes its commands, step by step.
    pub fn send(&mut self, event: Event) -> Result<(), ControlError> {
        let now = self.clock.now();
        let before = self.ctl.status().suspension;
        let timed = matches!(event, Event::PlanningTick | Event::Reapply);
        let commands = self.ctl.handle(now, event)?;
        if timed && let (Some(s), None) = (before, self.ctl.status().suspension) {
            self.resumes.push((now, s));
        }
        self.run(commands);
        Ok(())
    }

    pub fn send_ok(&mut self, event: Event) {
        self.send(event).unwrap();
    }

    fn run(&mut self, commands: Vec<Command>) {
        let mut queue: VecDeque<Command> = commands.into();
        while let Some(command) = queue.pop_front() {
            let now = self.clock.now();
            match command {
                Command::Write { display, level } => {
                    let sim = self
                        .sims
                        .get_mut(&display)
                        .expect("écriture sur un écran inconnu");
                    let raw = raw_from_level(i64::from(level), sim.max);
                    let ok = sim.responds && block_on(sim.backend.write(raw)).is_ok();
                    self.log.push(Written {
                        at_ms: now.ms,
                        display: display.clone(),
                        level,
                        raw,
                        ok,
                    });
                    let more = self
                        .ctl
                        .handle(now, Event::WriteResult { display, level, ok })
                        .unwrap();
                    queue.extend(more);
                }
                Command::Rescan { target, delay_ms } => self.rescans.push((target, delay_ms)),
                Command::ArmRampTimer { in_ms } => self.ramp_due = Some(now.ms + in_ms),
                Command::ArmReapply { in_ms } => self.reapply_due = Some(now.ms + in_ms),
                Command::PollLuxSoon { in_ms } => self.poll_soon.push(in_ms),
                Command::Persist(p) => self.persisted.push(p),
                Command::Notify(n) => self.notices.push(n),
            }
        }
    }

    /// Advances virtual time by firing, in order, the
    /// expired timers (including those falling exactly on the target).
    pub fn advance(&mut self, ms: u64) {
        let target = self.clock.now().ms + ms;
        loop {
            let planning = self.planning_ticks.then_some(self.next_planning_tick);
            let due = [(self.ramp_due, 0), (self.reapply_due, 1), (planning, 2)]
                .into_iter()
                .filter_map(|(at, kind)| at.map(|at| (at, kind)))
                .filter(|(at, _)| *at <= target)
                .min();
            let Some((at, kind)) = due else {
                self.clock.advance_to_ms(target);
                return;
            };
            self.clock.advance_to_ms(at);
            let event = match kind {
                0 => {
                    self.ramp_due = None;
                    Event::RampTick
                }
                1 => {
                    self.reapply_due = None;
                    Event::Reapply
                }
                _ => {
                    self.next_planning_tick += 60_000;
                    Event::PlanningTick
                }
            };
            self.send_ok(event);
        }
    }

    pub fn advance_minutes(&mut self, minutes: u64) {
        self.advance(minutes * 60_000);
    }

    pub fn status(&self) -> ha2lux_core::Status {
        self.ctl.status()
    }

    pub fn master(&self) -> Option<u8> {
        self.ctl.status().master
    }
}

pub fn range(low: i64, high: i64) -> Range {
    Range::normalized(low, high)
}

/// Avoids an `unused` warning if a test does not use this error type.
pub fn transient() -> BackendError {
    BackendError::Transient("test".to_owned())
}
