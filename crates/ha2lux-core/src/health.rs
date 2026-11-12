//! Santé du lien avec le capteur de lumière (spec §8.3) : validité des mesures,
//! péremption, état « hors ligne » et « dégradé ». Sans E/S : le temps est celui de
//! l'appelant (millisecondes monotones).

use crate::machine::HaHealth;

/// Éclairement maximal accepté (lux).
pub const MAX_VALID_LUX: f64 = 1_000_000.0;

/// Une valeur est utilisable si elle est **finie**, **≥ 0** et **≤ 1 000 000 lx**.
pub fn is_valid_lux(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_VALID_LUX).contains(&value)
}

/// Réglages de la santé du lien.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthConfig {
    /// Une mesure plus vieille que cela est ignorée (défaut 10 min).
    pub stale_ms: u64,
    /// Sans mesure valide depuis ce délai, avec Home Assistant joignable : « dégradé » (défaut 15 min).
    pub fallback_ms: u64,
    /// Échecs consécutifs avant « hors ligne » (défaut 6, soit ≈ 90 s à 15 s).
    pub offline_after: u32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            stale_ms: 600_000,
            fallback_ms: 900_000,
            offline_after: 6,
        }
    }
}

/// Résultat de la prise en compte d'une observation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Update {
    /// Santé après l'observation.
    pub health: HaHealth,
    /// La santé a changé.
    pub changed: bool,
    /// Mesure valide et fraîche à transmettre au filtre.
    pub measurement: Option<f64>,
}

/// Suivi de la santé du lien.
#[derive(Clone, Debug)]
pub struct HealthTracker {
    config: HealthConfig,
    health: HaHealth,
    failures: u32,
    /// Dernière mesure valide, ou instant d'activation tant qu'il n'y en a pas eu.
    reference_ms: u64,
    ever_valid: bool,
}

impl HealthTracker {
    /// Suivi neuf, démarré à `now_ms` (activation de l'auto-luminosité).
    pub fn new(config: HealthConfig, now_ms: u64) -> Self {
        Self {
            config,
            health: HaHealth::Ok,
            failures: 0,
            reference_ms: now_ms,
            ever_valid: false,
        }
    }

    /// Santé actuelle.
    pub fn health(&self) -> HaHealth {
        self.health
    }

    /// Au moins une mesure valide a été reçue depuis l'activation.
    pub fn has_valid_measurement(&self) -> bool {
        self.ever_valid
    }

    fn evaluate(&mut self, now_ms: u64) -> Update {
        let next = if self.failures >= self.config.offline_after {
            HaHealth::Offline
        } else if now_ms.saturating_sub(self.reference_ms) >= self.config.fallback_ms {
            HaHealth::Degraded
        } else {
            HaHealth::Ok
        };
        let changed = next != self.health;
        self.health = next;
        Update {
            health: next,
            changed,
            measurement: None,
        }
    }

    /// Réponse réussie portant une valeur numérique. `age_ms` est l'ancienneté de la
    /// mesure d'après l'horodatage de Home Assistant (0 si absent : instant de réception).
    pub fn on_reading(&mut self, now_ms: u64, lux: f64, age_ms: u64) -> Update {
        self.failures = 0;
        if !is_valid_lux(lux) || age_ms > self.config.stale_ms {
            return self.evaluate(now_ms); // joignable, mais mesure inutilisable
        }
        self.reference_ms = now_ms;
        self.ever_valid = true;
        let mut update = self.evaluate(now_ms);
        update.measurement = Some(lux);
        update
    }

    /// Réponse **réussie** sans mesure (`unavailable`, `unknown`, texte) : le lien est sain.
    pub fn on_no_value(&mut self, now_ms: u64) -> Update {
        self.failures = 0;
        self.evaluate(now_ms)
    }

    /// Échec de lecture (connexion, authentification, protocole), ou WebSocket coupé sans repli.
    pub fn on_failure(&mut self, now_ms: u64) -> Update {
        self.failures = self.failures.saturating_add(1);
        self.evaluate(now_ms)
    }

    /// Jeton refusé : le lien est considéré hors ligne **tout de suite** (inutile d'attendre
    /// six échecs espacés de plusieurs minutes), jusqu'à une lecture réussie.
    pub fn on_auth_failure(&mut self, now_ms: u64) -> Update {
        self.failures = self.config.offline_after;
        self.evaluate(now_ms)
    }

    /// Réévaluation périodique (le passage en « dégradé » ne dépend que du temps).
    pub fn tick(&mut self, now_ms: u64) -> Update {
        self.evaluate(now_ms)
    }

    /// Prochain instant où l'état peut changer sans nouvelle observation.
    pub fn next_deadline(&self) -> Option<u64> {
        (self.health == HaHealth::Ok).then(|| self.reference_ms + self.config.fallback_ms)
    }
}
