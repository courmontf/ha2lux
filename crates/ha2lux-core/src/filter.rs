//! Smoothing of lux measurements with confirmation of large changes (spec §8.5).

use crate::curve::{log_scale, lux_from_log, sanitize_lux};

/// Filter settings (exposed in configuration, `auto.tuning.*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterParams {
    /// Minimum gap on the log scale for a "large" change.
    pub big_log_delta: f64,
    /// Minimum gap in lux for a "large" change.
    pub big_lux_delta: f64,
    /// Slow smoothing coefficient (`state += alpha × gap`).
    pub alpha: f64,
    /// Closeness (log scale) of two measurements to confirm a jump.
    pub confirm_close: f64,
}

impl Default for FilterParams {
    fn default() -> Self {
        Self {
            big_log_delta: 0.2,
            big_lux_delta: 15.0,
            alpha: 0.08,
            confirm_close: 0.1,
        }
    }
}

/// Lux filter: exponential smoothing on the log scale, with hold then
/// confirmation of large gaps.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    params: FilterParams,
    state: Option<f64>,
    pending: Option<f64>,
}

impl Filter {
    /// Filter with the given settings.
    pub fn new(params: FilterParams) -> Self {
        Self {
            params,
            state: None,
            pending: None,
        }
    }

    /// Resets the state (HA back online, configuration change).
    pub fn reset(&mut self) {
        self.state = None;
        self.pending = None;
    }

    /// Integrates a measurement (clamped to `[0, 10⁶]`; `NaN` counts as 0).
    pub fn push(&mut self, lux: f64) {
        let lux = sanitize_lux(lux);
        let x = log_scale(lux);
        let Some(state) = self.state else {
            self.state = Some(x);
            self.pending = None;
            return;
        };
        let gap = x - state;
        let big = gap.abs() >= self.params.big_log_delta
            && (lux - lux_from_log(state)).abs() >= self.params.big_lux_delta;
        if !big {
            self.pending = None;
            self.state = Some(state + self.params.alpha * gap);
            return;
        }
        match self.pending {
            Some(p) if (p - state) * gap > 0.0 && (x - p).abs() < self.params.confirm_close => {
                self.state = Some(x);
                self.pending = None;
            }
            _ => self.pending = Some(x),
        }
    }

    /// Smoothed lux (0 until a measurement has been received).
    pub fn smoothed_lux(&self) -> f64 {
        self.state.map_or(0.0, lux_from_log)
    }

    /// True if a large gap is held pending confirmation.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Most recent lux: the pending measurement if there is one, otherwise the smoothed lux.
    pub fn latest_lux(&self) -> f64 {
        self.pending
            .map_or_else(|| self.smoothed_lux(), lux_from_log)
    }

    /// True if at least one measurement has been integrated.
    pub fn has_value(&self) -> bool {
        self.state.is_some()
    }
}
