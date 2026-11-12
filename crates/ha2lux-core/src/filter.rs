//! Lissage des mesures de lux avec confirmation des grands changements (spec §8.5).

use crate::curve::{log_scale, lux_from_log, sanitize_lux};

/// Réglages du filtre (exposés en configuration, `auto.tuning.*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterParams {
    /// Écart minimal en échelle log pour un « grand » changement.
    pub big_log_delta: f64,
    /// Écart minimal en lux pour un « grand » changement.
    pub big_lux_delta: f64,
    /// Coefficient du lissage lent (`état += alpha × écart`).
    pub alpha: f64,
    /// Proximité (échelle log) de deux mesures pour confirmer un saut.
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

/// Filtre de lux : lissage exponentiel dans l'échelle log, avec retenue puis
/// confirmation des grands écarts.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    params: FilterParams,
    state: Option<f64>,
    pending: Option<f64>,
}

impl Filter {
    /// Filtre avec les réglages donnés.
    pub fn new(params: FilterParams) -> Self {
        Self {
            params,
            state: None,
            pending: None,
        }
    }

    /// Réinitialise l'état (retour de HA, changement de configuration).
    pub fn reset(&mut self) {
        self.state = None;
        self.pending = None;
    }

    /// Intègre une mesure (bornée à `[0, 10⁶]` ; `NaN` compte pour 0).
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

    /// Lux lissé (0 tant qu'aucune mesure n'a été reçue).
    pub fn smoothed_lux(&self) -> f64 {
        self.state.map_or(0.0, lux_from_log)
    }

    /// Vrai si un grand écart est retenu en attendant confirmation.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Lux le plus récent : la mesure en attente si elle existe, sinon le lux lissé.
    pub fn latest_lux(&self) -> f64 {
        self.pending
            .map_or_else(|| self.smoothed_lux(), lux_from_log)
    }

    /// Vrai si au moins une mesure a été intégrée.
    pub fn has_value(&self) -> bool {
        self.state.is_some()
    }
}
