//! Health of the link with the light sensor (spec §8.3): measurement validity,
//! staleness, "offline" and "degraded" states. No I/O: time is the
//! caller's (monotonic milliseconds).

use crate::machine::HaHealth;

/// Maximum accepted illuminance (lux).
pub const MAX_VALID_LUX: f64 = 1_000_000.0;

/// A value is usable if it is **finite**, **≥ 0** and **≤ 1,000,000 lx**.
pub fn is_valid_lux(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_VALID_LUX).contains(&value)
}

/// Link health settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthConfig {
    /// A measurement older than this is ignored (default 10 min).
    pub stale_ms: u64,
    /// With no valid measurement for this long, with Home Assistant reachable: "degraded" (default 15 min).
    pub fallback_ms: u64,
    /// Consecutive failures before "offline" (default 6, i.e. ≈ 90 s at 15 s).
    pub offline_after: u32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            stale_ms: 600_000,
            fallback_ms: 900_000,
            offline_after: 6,
        }
    }
}

/// Result of taking an observation into account.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Update {
    /// Health after the observation.
    pub health: HaHealth,
    /// The health has changed.
    pub changed: bool,
    /// Valid, fresh measurement to pass to the filter.
    pub measurement: Option<f64>,
}

/// Link health tracking.
#[derive(Clone, Debug)]
pub struct HealthTracker {
    config: HealthConfig,
    health: HaHealth,
    failures: u32,
    /// Last valid measurement, or activation instant as long as there has been none.
    reference_ms: u64,
    ever_valid: bool,
}

impl HealthTracker {
    /// Fresh tracker, started at `now_ms` (auto-brightness activation).
    pub fn new(config: HealthConfig, now_ms: u64) -> Self {
        Self {
            config,
            health: HaHealth::Ok,
            failures: 0,
            reference_ms: now_ms,
            ever_valid: false,
        }
    }

    /// Current health.
    pub fn health(&self) -> HaHealth {
        self.health
    }

    /// At least one valid measurement has been received since activation.
    pub fn has_valid_measurement(&self) -> bool {
        self.ever_valid
    }

    fn evaluate(&mut self, now_ms: u64) -> Update {
        let next = if self.failures >= self.config.offline_after {
            HaHealth::Offline
        } else if now_ms.saturating_sub(self.reference_ms) >= self.config.fallback_ms {
            HaHealth::Degraded
        } else {
            HaHealth::Ok
        };
        let changed = next != self.health;
        self.health = next;
        Update {
            health: next,
            changed,
            measurement: None,
        }
    }

    /// Successful response carrying a numeric value. `age_ms` is the age of the
    /// measurement according to Home Assistant's timestamp (0 if absent: reception instant).
    pub fn on_reading(&mut self, now_ms: u64, lux: f64, age_ms: u64) -> Update {
        self.failures = 0;
        if !is_valid_lux(lux) || age_ms > self.config.stale_ms {
            return self.evaluate(now_ms); // reachable, but the measurement is unusable
        }
        self.reference_ms = now_ms;
        self.ever_valid = true;
        let mut update = self.evaluate(now_ms);
        update.measurement = Some(lux);
        update
    }

    /// **Successful** response with no measurement (`unavailable`, `unknown`, text): the link is healthy.
    pub fn on_no_value(&mut self, now_ms: u64) -> Update {
        self.failures = 0;
        self.evaluate(now_ms)
    }

    /// Read failure (connection, authentication, protocol), or WebSocket closed with no fallback.
    pub fn on_failure(&mut self, now_ms: u64) -> Update {
        self.failures = self.failures.saturating_add(1);
        self.evaluate(now_ms)
    }

    /// Token rejected: the link is considered offline **immediately** (no point waiting for
    /// six failures spaced several minutes apart), until a successful read.
    pub fn on_auth_failure(&mut self, now_ms: u64) -> Update {
        self.failures = self.config.offline_after;
        self.evaluate(now_ms)
    }

    /// Periodic re-evaluation (the switch to "degraded" depends only on time).
    pub fn tick(&mut self, now_ms: u64) -> Update {
        self.evaluate(now_ms)
    }

    /// Next instant at which the state may change without a new observation.
    pub fn next_deadline(&self) -> Option<u64> {
        (self.health == HaHealth::Ok).then(|| self.reference_ms + self.config.fallback_ms)
    }
}
