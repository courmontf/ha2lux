//! Porte d'hystérésis sur le niveau cible (spec §8.5).

/// Réglages de la porte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateParams {
    /// Écart à partir duquel on applique immédiatement.
    pub immediate: u8,
    /// Écart minimal pour compter une décision.
    pub minimum: u8,
    /// Nombre de décisions consécutives de même sens requises.
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

/// Porte : décide si un niveau cible doit être appliqué.
#[derive(Clone, Debug, Default)]
pub struct Gate {
    params: GateParams,
    last_applied: Option<u8>,
    streak: u8,
    direction: i8,
}

impl Gate {
    /// Porte avec les réglages donnés.
    pub fn new(params: GateParams) -> Self {
        Self {
            params,
            ..Self::default()
        }
    }

    /// Oublie le dernier niveau appliqué : la prochaine cible sera appliquée.
    pub fn reset(&mut self) {
        self.last_applied = None;
        self.streak = 0;
        self.direction = 0;
    }

    /// Réinitialise en déclarant `level` comme dernier niveau appliqué
    /// (après un apprentissage ou une application forcée).
    pub fn reset_to(&mut self, level: u8) {
        self.last_applied = Some(level);
        self.streak = 0;
        self.direction = 0;
    }

    /// Dernier niveau appliqué.
    pub fn last_applied(&self) -> Option<u8> {
        self.last_applied
    }

    /// Décide pour la cible `target` ; si oui, elle devient le dernier niveau
    /// appliqué. L'appelant doit rappeler [`Gate::reset_to`] si l'écriture
    /// matérielle échoue.
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
