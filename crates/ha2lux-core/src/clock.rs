//! Injected clock: the core never reads the system time.

use crate::schedule::MinuteOfDay;

/// Current instant as seen by the state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    /// Monotonic time in milliseconds (arbitrary origin).
    pub ms: u64,
    /// Local time, in minutes since midnight (for the schedule).
    pub minute: MinuteOfDay,
}

/// Time source.
pub trait Clock {
    /// Current instant.
    fn now(&self) -> Now;
}

/// Virtual clock: time only advances when asked to
/// (no real `sleep` in tests).
#[derive(Clone, Debug)]
pub struct VirtualClock {
    start_minute: u16,
    elapsed_ms: u64,
}

impl VirtualClock {
    /// Clock starting at local time `start`.
    pub fn new(start: MinuteOfDay) -> Self {
        Self {
            start_minute: start.minutes(),
            elapsed_ms: 0,
        }
    }

    /// Advances time.
    pub fn advance_ms(&mut self, ms: u64) {
        self.elapsed_ms += ms;
    }

    /// Sets the monotonic time to `ms` (never backwards).
    pub fn advance_to_ms(&mut self, ms: u64) {
        self.elapsed_ms = self.elapsed_ms.max(ms);
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> Now {
        let total_minutes = (u64::from(self.start_minute) * 60_000 + self.elapsed_ms) / 60_000;
        // `% 1440` guarantees a valid minute; falls back to midnight otherwise.
        let minute = MinuteOfDay::new((total_minutes % 1440) as u16).unwrap_or_default();
        Now {
            ms: self.elapsed_ms,
            minute,
        }
    }
}
