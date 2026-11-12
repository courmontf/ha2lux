//! Reconnection with capped exponential delay and jitter (spec §8.2: 1 s → 60 s).

use std::time::Duration;

/// Reconnection delay.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    /// Delay of 1 s, doubling up to 60 s.
    pub fn standard() -> Self {
        Self::new(Duration::from_secs(1), Duration::from_secs(60))
    }

    /// Delay of `min`, doubling up to `max`.
    pub fn new(min: Duration, max: Duration) -> Self {
        Self {
            min,
            max,
            attempt: 0,
        }
    }

    /// Next delay. `jitter` ∈ [0, 1[: the base delay is multiplied by 0.8 to 1.2.
    pub fn next_delay(&mut self, jitter: f64) -> Duration {
        let base = self
            .min
            .saturating_mul(1u32.checked_shl(self.attempt).unwrap_or(u32::MAX))
            .min(self.max);
        self.attempt = self.attempt.saturating_add(1).min(30);
        let factor = 0.8 + 0.4 * jitter.clamp(0.0, 0.999_999);
        base.mul_f64(factor)
    }

    /// Restarts from the minimum delay (connection succeeded).
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// Small pseudo-random generator (xorshift) for the jitter: no dependency, no cryptographic need.
#[derive(Clone, Debug)]
pub struct Jitter(u64);

impl Jitter {
    /// Arbitrary non-zero seed.
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Value in [0, 1[.
    pub fn sample(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}
