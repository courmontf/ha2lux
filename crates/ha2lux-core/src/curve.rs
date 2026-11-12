//! Courbe lux → niveau maître et apprentissage (spec §8.4 et §8.6).

use std::fmt;

use thiserror::Error;

use crate::range::clamp_percent;

/// Nombre maximal de points d'une courbe.
pub const MAX_CURVE_POINTS: usize = 8;
/// Éclairement maximal accepté (lux).
pub const MAX_LUX: f64 = 1_000_000.0;

/// Écart (en échelle log) sous lequel deux points sont « à la même lumière ».
const SAME_LIGHT_DELTA: f64 = 0.15;

/// Erreurs de sérialisation de la courbe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum CurveError {
    /// Le texte dépasse la taille permise ; aucune troncature silencieuse.
    #[error("curve too long for the allowed size ({max} characters)")]
    TooLong {
        /// Taille maximale demandée.
        max: usize,
    },
}

/// Ramène un éclairement dans `[0, 10⁶]` ; `NaN` et `-∞` donnent 0.
pub fn sanitize_lux(lux: f64) -> f64 {
    if lux.is_nan() {
        0.0
    } else {
        lux.clamp(0.0, MAX_LUX)
    }
}

/// Échelle logarithmique décalée : `log10(lux + 10)`.
pub fn log_scale(lux: f64) -> f64 {
    (sanitize_lux(lux) + 10.0).log10()
}

/// Inverse de [`log_scale`] : lux correspondant à une abscisse, borné ≥ 0.
pub fn lux_from_log(x: f64) -> f64 {
    (10f64.powf(x) - 10.0).max(0.0)
}

/// Point de courbe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint {
    /// Éclairement en lux (`0..=10⁶`).
    pub lux: f64,
    /// Niveau maître `0..=100`.
    pub level: u8,
    /// Instant d'apprentissage (secondes Unix), s'il est connu.
    pub learned_at: Option<u64>,
}

/// Garde-fous d'apprentissage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LearnPolicy {
    /// Ignore une observation à moins de `min_delta` points de la valeur
    /// déjà prédite (0 = désactivé).
    pub min_delta: u8,
    /// Nombre maximal de points contredits supprimables ; au-delà,
    /// l'observation est refusée (`None` = illimité).
    pub max_contradicted: Option<usize>,
}

impl LearnPolicy {
    /// Aucun garde-fou : algorithme de base de la spec §8.6.
    pub const UNGUARDED: LearnPolicy = LearnPolicy {
        min_delta: 0,
        max_contradicted: None,
    };
}

impl Default for LearnPolicy {
    /// Garde-fous recommandés : plancher d'écart de 2, au plus 2 contradictions.
    fn default() -> Self {
        Self {
            min_delta: 2,
            max_contradicted: Some(2),
        }
    }
}

/// Résultat d'une tentative d'apprentissage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LearnOutcome {
    /// Le point a été intégré à la courbe.
    Learned,
    /// Niveau trop proche de la valeur déjà prédite : rien appris.
    IgnoredTooClose,
    /// Trop de points contredits : observation refusée, courbe inchangée.
    Refused {
        /// Nombre de points qui auraient dû être supprimés.
        contradicted: usize,
    },
}

/// Courbe lux → niveau maître : au plus 8 points triés par lux croissant, à
/// niveaux non décroissants. Une courbe vide équivaut à la courbe par défaut.
#[derive(Clone, Debug, Default)]
pub struct Curve {
    points: Vec<CurvePoint>,
    undo: Option<Vec<CurvePoint>>,
}

impl PartialEq for Curve {
    /// Deux courbes sont égales si leurs points le sont (l'instantané
    /// d'annulation n'intervient pas).
    fn eq(&self, other: &Self) -> bool {
        self.points == other.points
    }
}

fn default_points() -> Vec<CurvePoint> {
    [(0.0, 20), (10.0, 40), (100.0, 70), (1000.0, 100)]
        .into_iter()
        .map(|(lux, level)| CurvePoint {
            lux,
            level,
            learned_at: None,
        })
        .collect()
}

fn evaluate(points: &[CurvePoint], lux: f64) -> u8 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return 0;
    };
    let lux = sanitize_lux(lux);
    if lux <= first.lux {
        return first.level;
    }
    if lux >= last.lux {
        return last.level;
    }
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if lux <= b.lux {
            let (xa, xb, x) = (log_scale(a.lux), log_scale(b.lux), log_scale(lux));
            let fraction = if xb > xa { (x - xa) / (xb - xa) } else { 1.0 };
            let value = f64::from(a.level) + fraction * (f64::from(b.level) - f64::from(a.level));
            return clamp_percent((value + 0.5).floor() as i64);
        }
    }
    last.level
}

/// Étapes 2 à 5 de la mise à jour (sans garde-fous) : retire les points à la
/// même lumière, les points contredits, évince si plein, insère en triant.
/// Renvoie le nombre de points contredits supprimés.
fn insert_point(points: &mut Vec<CurvePoint>, new: CurvePoint) -> usize {
    let x_new = log_scale(new.lux);
    points.retain(|p| (log_scale(p.lux) - x_new).abs() >= SAME_LIGHT_DELTA);
    let before = points.len();
    points.retain(|p| {
        let contradicted =
            (p.lux < new.lux && p.level > new.level) || (p.lux > new.lux && p.level < new.level);
        !contradicted
    });
    let contradicted = before - points.len();
    if points.len() >= MAX_CURVE_POINTS {
        let closest = points
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (log_scale(a.lux) - x_new).abs();
                let db = (log_scale(b.lux) - x_new).abs();
                da.total_cmp(&db)
            })
            .map(|(i, _)| i);
        if let Some(i) = closest {
            points.remove(i);
        }
    }
    let position = points.partition_point(|p| p.lux < new.lux);
    points.insert(position, new);
    contradicted
}

impl Curve {
    /// Courbe vide (équivalente à la courbe par défaut à l'évaluation).
    pub fn new() -> Self {
        Self::default()
    }

    /// Points explicites (vide = courbe par défaut).
    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// Points effectivement utilisés pour l'évaluation.
    pub fn effective_points(&self) -> Vec<CurvePoint> {
        if self.points.is_empty() {
            default_points()
        } else {
            self.points.clone()
        }
    }

    /// Niveau maître 0-100 pour un éclairement.
    ///
    /// Plat hors des extrémités ; interpolation linéaire en `log10(lux + 10)`
    /// entre deux points, arrondie à l'entier le plus proche.
    pub fn level_at(&self, lux: f64) -> u8 {
        if self.points.is_empty() {
            evaluate(&default_points(), lux)
        } else {
            evaluate(&self.points, lux)
        }
    }

    /// Apprend l'observation `(lux, niveau choisi)`.
    ///
    /// Les entrées sont bornées (lux `[0, 10⁶]`, niveau `[0, 100]`). Une
    /// courbe vide part de la courbe par défaut. L'instantané précédent est
    /// conservé pour [`Curve::undo`] seulement si la courbe change.
    pub fn learn(
        &mut self,
        lux: f64,
        level: i64,
        at: Option<u64>,
        policy: &LearnPolicy,
    ) -> LearnOutcome {
        let new = CurvePoint {
            lux: sanitize_lux(lux),
            level: clamp_percent(level),
            learned_at: at,
        };
        let mut working = self.effective_points();
        let predicted = evaluate(&working, new.lux);
        if policy.min_delta > 0 && new.level.abs_diff(predicted) < policy.min_delta {
            return LearnOutcome::IgnoredTooClose;
        }
        let contradicted = insert_point(&mut working, new);
        if let Some(max) = policy.max_contradicted
            && contradicted > max
        {
            return LearnOutcome::Refused { contradicted };
        }
        self.undo = Some(std::mem::replace(&mut self.points, working));
        LearnOutcome::Learned
    }

    /// Annule le dernier apprentissage. Renvoie `false` s'il n'y a rien à annuler.
    pub fn undo(&mut self) -> bool {
        match self.undo.take() {
            Some(previous) => {
                self.points = previous;
                true
            }
            None => false,
        }
    }

    /// Oublie la courbe apprise (retour à la courbe par défaut).
    pub fn reset(&mut self) {
        self.undo = None;
        self.points.clear();
    }

    /// Supprime les points appris avant `now - max_age_secs` (les points sans
    /// horodatage sont conservés). Renvoie le nombre de points supprimés.
    pub fn prune_older_than(&mut self, now: u64, max_age_secs: u64) -> usize {
        let before = self.points.len();
        self.points.retain(|p| {
            p.learned_at
                .is_none_or(|t| now.saturating_sub(t) <= max_age_secs)
        });
        before - self.points.len()
    }

    /// Relit une courbe `lux:niveau,lux:niveau,…`. Les éléments illisibles ou
    /// hors bornes sont ignorés ; les autres passent par l'algorithme
    /// d'insertion (tri, contradictions, plafond de 8 points).
    pub fn parse(text: &str) -> Self {
        let mut points = Vec::new();
        for token in text.split(',') {
            let Some((lux, level)) = token.trim().split_once(':') else {
                continue;
            };
            let (Ok(lux), Ok(level)) = (lux.trim().parse::<f64>(), level.trim().parse::<u8>())
            else {
                continue;
            };
            if !lux.is_finite() || !(0.0..=MAX_LUX).contains(&lux) || level > 100 {
                continue;
            }
            insert_point(
                &mut points,
                CurvePoint {
                    lux,
                    level,
                    learned_at: None,
                },
            );
        }
        Self { points, undo: None }
    }

    /// Texte `lux:niveau,…` ; échoue au lieu de tronquer si plus long que `max`.
    pub fn to_text_bounded(&self, max: usize) -> Result<String, CurveError> {
        let text = self.to_string();
        if text.len() > max {
            Err(CurveError::TooLong { max })
        } else {
            Ok(text)
        }
    }
}

impl fmt::Display for Curve {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, p) in self.points.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{}:{}", p.lux, p.level)?;
        }
        Ok(())
    }
}
