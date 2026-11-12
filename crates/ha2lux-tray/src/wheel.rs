//! Molette : les crans sont **comptés**, puis appliqués en un seul message.
//!
//! Sans cela, une rafale de crans produirait autant de messages que de crans et la
//! luminosité continuerait de bouger après l'arrêt de la molette.

use std::time::Duration;

/// Délai de regroupement des crans avant envoi.
pub const FLUSH_AFTER: Duration = Duration::from_millis(60);

/// Accumulateur de crans.
#[derive(Debug, Default)]
pub struct Wheel {
    notches: i32,
}

impl Wheel {
    /// Enregistre un cran (positif : plus lumineux).
    pub fn push(&mut self, notches: i32) {
        self.notches = self.notches.saturating_add(notches);
    }

    /// Des crans attendent d'être appliqués.
    pub fn pending(&self) -> bool {
        self.notches != 0
    }

    /// Retire les crans accumulés et renvoie la variation totale du maître, en
    /// pourcentage (`crans × pas`), bornée à ±100 ; `None` s'ils s'annulent.
    pub fn take(&mut self, step: u8) -> Option<i32> {
        let notches = std::mem::take(&mut self.notches);
        let delta = notches.saturating_mul(i32::from(step)).clamp(-100, 100);
        (delta != 0).then_some(delta)
    }
}
