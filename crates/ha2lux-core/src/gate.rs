//! Hysteresis gate on the target level (spec §8.5).

/// Gate settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateParams {
    /// Gap from which the level is applied immediately.
    pub immediate: u8,
    /// Minimum gap for a decision to count.
    pub minimum: u8,
    /// Number of consecutive decisions in the same direction required.
    pub confirmations: u8,
}

impl Default for GateParams {
    fn default() -> Self {
        Self {
            immediate: 15,
            minimum: 3,
            confirmations: 3,
        }
    }
}

/// Gate: decides whether a target level must be applied.
#[derive(Clone, Debug, Default)]
pub struct Gate {
    params: GateParams,
    last_applied: Option<u8>,
    streak: u8,
    direction: i8,
}

impl Gate {
    /// Gate with the given settings.
    pub fn new(params: GateParams) -> Self {
        Self {
            params,
            ..Self::default()
        }
    }

    /// Forgets the last applied level: the next target will be applied.
    pub fn reset(&mut self) {
        self.last_applied = None;
        self.streak = 0;
        self.direction = 0;
    }

    /// Resets by declaring `level` as the last applied level
    /// (after learning or a forced apply).
    pub fn reset_to(&mut self, level: u8) {
        self.last_applied = Some(level);
        self.streak = 0;
        self.direction = 0;
    }

    /// Last applied level.
    pub fn last_applied(&self) -> Option<u8> {
        self.last_applied
    }

    /// Decides for the target `target`; if so, it becomes the last applied
    /// level. The caller must call [`Gate::reset_to`] again if the hardware
    /// write fails.
    pub fn decide(&mut self, target: u8) -> bool {
        let Some(last) = self.last_applied else {
            self.reset_to(target);
            return true;
        };
        let gap = i16::from(target) - i16::from(last);
        if gap.unsigned_abs() >= u16::from(self.params.immediate) {
            self.reset_to(target);
            return true;
        }
        let reaches_end = (target == 0 || target == 100) && gap != 0;
        if gap.unsigned_abs() < u16::from(self.params.minimum) && !reaches_end {
            self.streak = 0;
            self.direction = 0;
            return false;
        }
        let direction = gap.signum() as i8;
        if direction == self.direction {
            self.streak = self.streak.saturating_add(1);
        } else {
            self.direction = direction;
            self.streak = 1;
        }
        if self.streak >= self.params.confirmations {
            self.reset_to(target);
            true
        } else {
            false
        }
    }
}
