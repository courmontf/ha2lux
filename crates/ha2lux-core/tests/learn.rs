//! T-LRN : apprentissage et garde-fous (spec §8.6).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::{Curve, LearnOutcome, LearnPolicy};
use proptest::prelude::*;

const RAW: LearnPolicy = LearnPolicy::UNGUARDED;

fn pairs(c: &Curve) -> Vec<(f64, u8)> {
    c.points().iter().map(|p| (p.lux, p.level)).collect()
}

fn learned(lux: f64, level: i64) -> Curve {
    let mut c = Curve::new();
    assert_eq!(c.learn(lux, level, None, &RAW), LearnOutcome::Learned);
    c
}

#[test]
fn t_lrn_01() {
    let c = learned(5.0, 30);
    assert_eq!(
        pairs(&c),
        [(0.0, 20), (5.0, 30), (100.0, 70), (1000.0, 100)]
    );
    assert_eq!(c.level_at(5.0), 30);
}

#[test]
fn t_lrn_02() {
    let mut c = learned(5.0, 30);
    c.learn(5.5, 35, None, &RAW);
    assert_eq!(c.points().len(), 4);
    assert!(pairs(&c).contains(&(5.5, 35)));
    assert!(!pairs(&c).contains(&(5.0, 30)));
}

#[test]
fn t_lrn_03() {
    let c = learned(10.0, 80);
    assert_eq!(pairs(&c), [(0.0, 20), (10.0, 80), (1000.0, 100)]);
    assert!(c.level_at(1000.0) >= 80);
}

#[test]
fn t_lrn_04_flat_without_guard() {
    let mut c = learned(10.0, 80);
    c.learn(1000.0, 10, None, &RAW);
    assert_eq!(pairs(&c), [(1000.0, 10)]);
}

#[test]
fn t_lrn_05() {
    assert_eq!(
        pairs(&learned(3.0, 25)),
        [(3.0, 25), (10.0, 40), (100.0, 70), (1000.0, 100)]
    );
}

#[test]
fn t_lrn_06_eviction() {
    // La courbe de la spec (0:10, 2:25, …) est incohérente avec la règle « même
    // lumière » (écart log 0,079 < 0,15) : elle serait fusionnée au chargement.
    // On utilise ici une courbe de 8 points valide, même scénario d'éviction.
    let mut c = Curve::parse("0:10,6:25,15:40,30:50,100:70,300:85,600:92,1000:100");
    assert_eq!(c.points().len(), 8);
    c.learn(50.0, 60, None, &RAW);
    let p = pairs(&c);
    assert_eq!(p.len(), 8);
    assert!(!p.contains(&(30.0, 50)));
    assert!(p.contains(&(50.0, 60)));
}

#[test]
fn t_lrn_07_inputs_clamped() {
    let c = learned(-5.0, 150);
    assert!(pairs(&c).contains(&(0.0, 100)));
}

#[test]
fn t_lrn_08_lux_clamped() {
    let c = learned(5e6, 90);
    assert!(pairs(&c).contains(&(1_000_000.0, 90)));
    assert_eq!(Curve::parse(&c.to_string()), c);
}

proptest! {
    #[test]
    fn t_lrn_09_invariants(obs in proptest::collection::vec((0.0f64..2_000_000.0, -20i64..130), 40)) {
        let mut c = Curve::new();
        for (lux, level) in obs {
            c.learn(lux, level, None, &RAW);
            let p = c.points();
            prop_assert!(p.len() <= 8);
            for w in p.windows(2) {
                prop_assert!(w[0].lux < w[1].lux);
                prop_assert!(w[0].level <= w[1].level);
            }
            let want = (lux.clamp(0.0, 1e6), level.clamp(0, 100) as u8);
            prop_assert!(p.iter().any(|q| q.lux == want.0 && q.level == want.1), "dernier point absent");
        }
    }
}

#[test]
fn t_lrn_10_non_finite_lux() {
    let mut c = Curve::new();
    c.learn(f64::NAN, 50, None, &RAW);
    c.learn(f64::INFINITY, 90, None, &RAW);
    assert!(
        c.points()
            .iter()
            .all(|p| p.lux.is_finite() && (0.0..=1e6).contains(&p.lux))
    );
}

#[test]
fn t_lrn_g1_refuses_flattening() {
    let mut c = Curve::new();
    let outcome = c.learn(1000.0, 10, None, &LearnPolicy::default());
    assert_eq!(outcome, LearnOutcome::Refused { contradicted: 3 });
    assert!(c.points().is_empty(), "courbe inchangée");
    assert!(!c.undo(), "rien à annuler après un refus");
}

#[test]
fn t_lrn_g2_ignores_small_delta() {
    let mut c = Curve::new();
    // Valeur prédite à 10 lx : 40.
    assert_eq!(
        c.learn(10.0, 41, None, &LearnPolicy::default()),
        LearnOutcome::IgnoredTooClose
    );
    assert_eq!(
        c.learn(10.0, 39, None, &LearnPolicy::default()),
        LearnOutcome::IgnoredTooClose
    );
    assert!(c.points().is_empty());
    // Un écart de 2 est appris (T-LRN-01 : 32 prédit, 30 voulu).
    assert_eq!(
        c.learn(5.0, 30, None, &LearnPolicy::default()),
        LearnOutcome::Learned
    );
}

#[test]
fn t_lrn_g3_undo() {
    let mut c = Curve::new();
    c.learn(5.0, 30, None, &LearnPolicy::default());
    let after_first = c.clone();
    c.learn(100.0, 90, None, &LearnPolicy::default());
    assert_ne!(c, after_first);
    assert!(c.undo());
    assert_eq!(c, after_first);
    assert!(!c.undo());
}

#[test]
fn t_lrn_g3_undo_restores_empty() {
    let mut c = Curve::new();
    c.learn(5.0, 30, None, &LearnPolicy::default());
    assert!(c.undo());
    assert!(c.points().is_empty());
}

#[test]
fn reset_forgets_curve() {
    let mut c = learned(5.0, 30);
    c.reset();
    assert!(c.points().is_empty());
    assert_eq!(c.level_at(10.0), 40);
}

#[test]
fn age_pruning() {
    let mut c = Curve::new();
    c.learn(5.0, 30, Some(1_000), &RAW);
    c.learn(500.0, 95, Some(9_000), &RAW);
    let removed = c.prune_older_than(10_000, 2_000);
    assert!(removed >= 1);
    assert!(
        c.points()
            .iter()
            .all(|p| p.learned_at.is_none_or(|t| t >= 8_000))
    );
}
