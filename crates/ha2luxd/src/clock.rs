//! Horloges du démon : réelle (heure locale) et pilotée par le temps de tokio.

use chrono::Timelike;
use ha2lux_core::{Clock, MinuteOfDay, Now};
use tokio::time::Instant;

fn minute_of_day(minutes: u64) -> MinuteOfDay {
    // `% 1440` garantit une valeur valide.
    MinuteOfDay::new((minutes % 1440) as u16).unwrap_or_default()
}

/// Horloge de production : temps monotone de tokio, heure locale du système.
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    /// Crée l'horloge (origine du temps monotone : maintenant).
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

/// Horloge entièrement dérivée du temps de tokio : sous `start_paused`, le
/// temps (et donc l'heure locale simulée) n'avance que par `tokio::time::advance`.
pub struct TokioClock {
    start: Instant,
    start_minute: u16,
}

impl TokioClock {
    /// Horloge démarrant à l'heure locale `start`.
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
