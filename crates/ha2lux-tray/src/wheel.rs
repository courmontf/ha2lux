//! Wheel: notches are **counted**, then applied in a single message.
//!
//! Without this, a burst of notches would produce as many messages as notches and the
//! brightness would keep moving after the wheel stops.

use std::time::Duration;

/// Delay for batching notches before sending.
pub const FLUSH_AFTER: Duration = Duration::from_millis(60);

/// Accumulateur de crans.
#[derive(Debug, Default)]
pub struct Wheel {
    notches: i32,
}

impl Wheel {
    /// Records a notch (positive: brighter).
    pub fn push(&mut self, notches: i32) {
        self.notches = self.notches.saturating_add(notches);
    }

    /// Notches are waiting to be applied.
    pub fn pending(&self) -> bool {
        self.notches != 0
    }

    /// Takes the accumulated notches and returns the total master change, as a
    /// percentage (`notches × step`), clamped to ±100; `None` if they cancel out.
    pub fn take(&mut self, step: u8) -> Option<i32> {
        let notches = std::mem::take(&mut self.notches);
        let delta = notches.saturating_mul(i32::from(step)).clamp(-100, 100);
        (delta != 0).then_some(delta)
    }
}
