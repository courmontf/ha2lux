//! Reconnexion à délai exponentiel plafonné, avec gigue (spec §8.2 : 1 s → 60 s).

use std::time::Duration;

/// Délai de reconnexion.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    /// Délai de 1 s doublant jusqu'à 60 s.
    pub fn standard() -> Self {
        Self::new(Duration::from_secs(1), Duration::from_secs(60))
    }

    /// Délai de `min` doublant jusqu'à `max`.
    pub fn new(min: Duration, max: Duration) -> Self {
        Self {
            min,
            max,
            attempt: 0,
        }
    }

    /// Prochain délai. `jitter` ∈ [0, 1[ : le délai de base est multiplié par 0,8 à 1,2.
    pub fn next_delay(&mut self, jitter: f64) -> Duration {
        let base = self
            .min
            .saturating_mul(1u32.checked_shl(self.attempt).unwrap_or(u32::MAX))
            .min(self.max);
        self.attempt = self.attempt.saturating_add(1).min(30);
        let factor = 0.8 + 0.4 * jitter.clamp(0.0, 0.999_999);
        base.mul_f64(factor)
    }

    /// Repart du délai minimal (connexion réussie).
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// Petit générateur pseudo-aléatoire (xorshift) pour la gigue : pas de dépendance, pas de besoin cryptographique.
#[derive(Clone, Debug)]
pub struct Jitter(u64);

impl Jitter {
    /// Graine arbitraire non nulle.
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Valeur dans [0, 1[.
    pub fn sample(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}
