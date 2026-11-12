//! Horloge injectée : le cœur ne lit jamais l'heure système.

use crate::schedule::MinuteOfDay;

/// Instant courant vu par la machine d'états.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    /// Temps monotone en millisecondes (origine arbitraire).
    pub ms: u64,
    /// Heure locale, en minutes depuis minuit (pour le planning).
    pub minute: MinuteOfDay,
}

/// Source de temps.
pub trait Clock {
    /// Instant courant.
    fn now(&self) -> Now;
}

/// Horloge virtuelle : le temps n'avance que lorsqu'on le demande
/// (aucun `sleep` réel dans les tests).
#[derive(Clone, Debug)]
pub struct VirtualClock {
    start_minute: u16,
    elapsed_ms: u64,
}

impl VirtualClock {
    /// Horloge démarrant à l'heure locale `start`.
    pub fn new(start: MinuteOfDay) -> Self {
        Self {
            start_minute: start.minutes(),
            elapsed_ms: 0,
        }
    }

    /// Fait avancer le temps.
    pub fn advance_ms(&mut self, ms: u64) {
        self.elapsed_ms += ms;
    }

    /// Place le temps monotone à `ms` (jamais en arrière).
    pub fn advance_to_ms(&mut self, ms: u64) {
        self.elapsed_ms = self.elapsed_ms.max(ms);
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> Now {
        let total_minutes = (u64::from(self.start_minute) * 60_000 + self.elapsed_ms) / 60_000;
        // `% 1440` garantit une minute valide ; repli sur minuit sinon.
        let minute = MinuteOfDay::new((total_minutes % 1440) as u16).unwrap_or_default();
        Now {
            ms: self.elapsed_ms,
            minute,
        }
    }
}
