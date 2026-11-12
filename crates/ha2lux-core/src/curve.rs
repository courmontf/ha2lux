//! lux → master level curve and learning (spec §8.4 and §8.6).

use std::fmt;

use thiserror::Error;

use crate::range::clamp_percent;

/// Maximum number of points in a curve.
pub const MAX_CURVE_POINTS: usize = 8;
/// Maximum accepted illuminance (lux).
pub const MAX_LUX: f64 = 1_000_000.0;

/// Gap (on the log scale) below which two points are "at the same light".
const SAME_LIGHT_DELTA: f64 = 0.15;

/// Curve serialization errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum CurveError {
    /// The text exceeds the allowed size; no silent truncation.
    #[error("curve too long for the allowed size ({max} characters)")]
    TooLong {
        /// Maximum size requested.
        max: usize,
    },
}

/// Clamps an illuminance into `[0, 10⁶]`; `NaN` and `-∞` give 0.
pub fn sanitize_lux(lux: f64) -> f64 {
    if lux.is_nan() {
        0.0
    } else {
        lux.clamp(0.0, MAX_LUX)
    }
}

/// Shifted logarithmic scale: `log10(lux + 10)`.
pub fn log_scale(lux: f64) -> f64 {
    (sanitize_lux(lux) + 10.0).log10()
}

/// Inverse of [`log_scale`]: lux corresponding to an abscissa, clamped to ≥ 0.
pub fn lux_from_log(x: f64) -> f64 {
    (10f64.powf(x) - 10.0).max(0.0)
}

/// Curve point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint {
    /// Illuminance in lux (`0..=10⁶`).
    pub lux: f64,
    /// Master level `0..=100`.
    pub level: u8,
    /// Learning instant (Unix seconds), if known.
    pub learned_at: Option<u64>,
}

/// Learning safeguards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LearnPolicy {
    /// Ignores an observation within `min_delta` points of the value
    /// already predicted (0 = disabled).
    pub min_delta: u8,
    /// Maximum number of contradicted points that may be removed; beyond that,
    /// the observation is rejected (`None` = unlimited).
    pub max_contradicted: Option<usize>,
}

impl LearnPolicy {
    /// No safeguards: the base algorithm of spec §8.6.
    pub const UNGUARDED: LearnPolicy = LearnPolicy {
        min_delta: 0,
        max_contradicted: None,
    };
}

impl Default for LearnPolicy {
    /// Recommended safeguards: gap floor of 2, at most 2 contradictions.
    fn default() -> Self {
        Self {
            min_delta: 2,
            max_contradicted: Some(2),
        }
    }
}

/// Result of a learning attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LearnOutcome {
    /// The point was merged into the curve.
    Learned,
    /// Level too close to the already predicted value: nothing learned.
    IgnoredTooClose,
    /// Too many contradicted points: observation rejected, curve unchanged.
    Refused {
        /// Number of points that would have had to be removed.
        contradicted: usize,
    },
}

/// lux → master level curve: at most 8 points sorted by increasing lux, with
/// non-decreasing levels. An empty curve is equivalent to the default curve.
#[derive(Clone, Debug, Default)]
pub struct Curve {
    points: Vec<CurvePoint>,
    undo: Option<Vec<CurvePoint>>,
}

impl PartialEq for Curve {
    /// Two curves are equal if their points are (the undo
    /// snapshot does not count).
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

/// Steps 2 to 5 of the update (without safeguards): removes points at the
/// same light, contradicted points, evicts if full, inserts keeping order.
/// Returns the number of contradicted points removed.
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
    /// Empty curve (equivalent to the default curve when evaluated).
    pub fn new() -> Self {
        Self::default()
    }

    /// Explicit points (empty = default curve).
    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// Points actually used for evaluation.
    pub fn effective_points(&self) -> Vec<CurvePoint> {
        if self.points.is_empty() {
            default_points()
        } else {
            self.points.clone()
        }
    }

    /// Master level 0-100 for an illuminance.
    ///
    /// Flat outside the endpoints; linear interpolation in `log10(lux + 10)`
    /// between two points, rounded to the nearest integer.
    pub fn level_at(&self, lux: f64) -> u8 {
        if self.points.is_empty() {
            evaluate(&default_points(), lux)
        } else {
            evaluate(&self.points, lux)
        }
    }

    /// Learns the observation `(lux, chosen level)`.
    ///
    /// Inputs are clamped (lux `[0, 10⁶]`, level `[0, 100]`). An empty
    /// curve starts from the default curve. The previous snapshot is
    /// kept for [`Curve::undo`] only if the curve changes.
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

    /// Undoes the last learning. Returns `false` if there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        match self.undo.take() {
            Some(previous) => {
                self.points = previous;
                true
            }
            None => false,
        }
    }

    /// Forgets the learned curve (back to the default curve).
    pub fn reset(&mut self) {
        self.undo = None;
        self.points.clear();
    }

    /// Removes points learned before `now - max_age_secs` (points without a
    /// timestamp are kept). Returns the number of points removed.
    pub fn prune_older_than(&mut self, now: u64, max_age_secs: u64) -> usize {
        let before = self.points.len();
        self.points.retain(|p| {
            p.learned_at
                .is_none_or(|t| now.saturating_sub(t) <= max_age_secs)
        });
        before - self.points.len()
    }

    /// Reads back a `lux:level,lux:level,…` curve. Unreadable or out-of-range
    /// elements are ignored; the others go through the insertion algorithm
    /// (sorting, contradictions, 8-point cap).
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

    /// Text `lux:level,…`; fails instead of truncating if longer than `max`.
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
