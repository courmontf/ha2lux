//! Daemon clocks: real (local time) and driven by tokio time.

use chrono::Timelike;
use ha2lux_core::{Clock, MinuteOfDay, Now};
use tokio::time::Instant;

fn minute_of_day(minutes: u64) -> MinuteOfDay {
    // `% 1440` guarantees a valid value.
    MinuteOfDay::new((minutes % 1440) as u16).unwrap_or_default()
}

/// Production clock: tokio monotonic time, system local time.
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    /// Creates the clock (origin of the monotonic time: now).
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Now {
        let local = chrono::Local::now();
        Now {
            ms: u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX),
            minute: minute_of_day(u64::from(local.hour()) * 60 + u64::from(local.minute())),
        }
    }
}

/// Clock entirely derived from tokio time: under `start_paused`, the
/// time (and thus the simulated local time) only advances through `tokio::time::advance`.
pub struct TokioClock {
    start: Instant,
    start_minute: u16,
}

impl TokioClock {
    /// Clock starting at local time `start`.
    pub fn new(start: MinuteOfDay) -> Self {
        Self {
            start: Instant::now(),
            start_minute: start.minutes(),
        }
    }
}

impl Clock for TokioClock {
    fn now(&self) -> Now {
        let elapsed_ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let total = (u64::from(self.start_minute) * 60_000 + elapsed_ms) / 60_000;
        Now {
            ms: elapsed_ms,
            minute: minute_of_day(total),
        }
    }
}
