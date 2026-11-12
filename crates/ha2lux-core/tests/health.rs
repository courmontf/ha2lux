//! Santé du lien capteur : T-HA-03/04 (validité), T-HA-11..13 (péremption, hors ligne, dégradé).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::{HaHealth, HealthConfig, HealthTracker, is_valid_lux};
use proptest::prelude::*;

const MIN: u64 = 60_000;

fn tracker() -> HealthTracker {
    HealthTracker::new(HealthConfig::default(), 0)
}

#[test]
fn t_ha_03_04_validity_of_a_value() {
    for ok in [0.0, 12.5, 1_000_000.0, 0.0001] {
        assert!(is_valid_lux(ok), "{ok}");
    }
    for bad in [
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -3.0,
        1_000_001.0,
        -0.0001,
    ] {
        assert!(!is_valid_lux(bad), "{bad}");
    }
}

#[test]
fn a_valid_fresh_reading_is_a_measurement_and_keeps_the_link_ok() {
    let mut t = tracker();
    let update = t.on_reading(1_000, 12.5, 0);
    assert_eq!(
        (update.health, update.measurement, update.changed),
        (HaHealth::Ok, Some(12.5), false)
    );
    assert!(t.has_valid_measurement());
}

#[test]
fn invalid_values_are_not_measurements_but_the_link_stays_ok() {
    let mut t = tracker();
    for bad in [f64::NAN, f64::INFINITY, -3.0, 1_000_001.0] {
        let update = t.on_reading(1_000, bad, 0);
        assert_eq!(
            (update.measurement, update.health),
            (None, HaHealth::Ok),
            "{bad}"
        );
    }
    assert!(!t.has_valid_measurement());
}

#[test]
fn t_ha_11_stale_measurements_are_ignored() {
    let mut t = tracker();
    assert_eq!(
        t.on_reading(0, 50.0, 10 * MIN).measurement,
        Some(50.0),
        "exactement à la limite : utilisable"
    );
    assert_eq!(
        t.on_reading(0, 50.0, 10 * MIN + 1).measurement,
        None,
        "plus vieille que la péremption"
    );
    assert_eq!(t.on_reading(0, 50.0, 3 * 60 * MIN).measurement, None);
}

#[test]
fn t_ha_12_six_consecutive_failures_mean_offline_and_one_success_restores() {
    let mut t = tracker();
    for i in 1..=5 {
        assert_eq!(t.on_failure(i * 15_000).health, HaHealth::Ok, "échec n°{i}");
    }
    let sixth = t.on_failure(90_000);
    assert_eq!((sixth.health, sixth.changed), (HaHealth::Offline, true));
    assert_eq!(t.on_failure(105_000).health, HaHealth::Offline);
    let back = t.on_reading(120_000, 30.0, 0);
    assert_eq!(
        (back.health, back.changed, back.measurement),
        (HaHealth::Ok, true, Some(30.0)),
        "une lecture réussie suffit"
    );
}

#[test]
fn a_success_without_value_also_restores_the_link() {
    let mut t = tracker();
    for i in 0..6 {
        t.on_failure(i * 1_000);
    }
    assert_eq!(t.health(), HaHealth::Offline);
    assert_eq!(
        t.on_no_value(10_000).health,
        HaHealth::Ok,
        "HA répond (« unavailable ») : il est joignable"
    );
}

#[test]
fn failures_must_be_consecutive() {
    let mut t = tracker();
    for round in 0..10 {
        for i in 0..5 {
            t.on_failure(round * 100_000 + i);
        }
        t.on_reading(round * 100_000 + 50, 10.0, 0); // une réponse réussie interrompt la série d'échecs
    }
    assert_eq!(t.health(), HaHealth::Ok);
}

#[test]
fn t_ha_13_unavailable_beyond_the_fallback_delay_is_degraded_then_recovers() {
    let mut t = tracker();
    t.on_reading(0, 20.0, 0);
    assert_eq!(t.on_no_value(14 * MIN).health, HaHealth::Ok);
    let degraded = t.on_no_value(15 * MIN);
    assert_eq!(
        (degraded.health, degraded.changed),
        (HaHealth::Degraded, true)
    );
    assert_eq!(t.tick(30 * MIN).health, HaHealth::Degraded);
    let healed = t.on_reading(31 * MIN, 25.0, 0);
    assert_eq!(
        (healed.health, healed.measurement),
        (HaHealth::Ok, Some(25.0))
    );
}

#[test]
fn degraded_also_applies_when_no_valid_measurement_ever_arrived() {
    let mut t = HealthTracker::new(HealthConfig::default(), 5 * MIN);
    assert_eq!(t.on_no_value(5 * MIN + 14 * MIN).health, HaHealth::Ok);
    assert_eq!(t.tick(5 * MIN + 15 * MIN).health, HaHealth::Degraded);
    assert!(!t.has_valid_measurement());
}

#[test]
fn stale_readings_do_not_prevent_degradation() {
    let mut t = tracker();
    for minute in 0..=15 {
        t.on_reading(minute * MIN, 40.0, 11 * MIN); // HA répond, mais avec une mesure périmée
    }
    assert_eq!(t.health(), HaHealth::Degraded);
}

#[test]
fn offline_wins_over_degraded() {
    let mut t = tracker();
    for i in 0..6 {
        t.on_failure(20 * MIN + i);
    }
    assert_eq!(t.health(), HaHealth::Offline);
    // Joignable de nouveau mais sans mesure depuis 20 min : dégradé, pas sain.
    assert_eq!(t.on_no_value(21 * MIN).health, HaHealth::Degraded);
}

#[test]
fn next_deadline_only_while_healthy() {
    let mut t = tracker();
    assert_eq!(t.next_deadline(), Some(15 * MIN));
    t.on_reading(4 * MIN, 10.0, 0);
    assert_eq!(t.next_deadline(), Some(19 * MIN));
    t.tick(19 * MIN);
    assert_eq!(t.next_deadline(), None);
}

proptest! {
    #[test]
    fn measurements_are_always_valid_and_fresh(
        steps in proptest::collection::vec((0u8..4, any::<f64>(), 0u64..3_000_000), 1..60)
    ) {
        let mut t = tracker();
        let mut now = 0u64;
        for (kind, value, age) in steps {
            now += 30_000;
            let update = match kind {
                0 => t.on_reading(now, value, age),
                1 => t.on_no_value(now),
                2 => t.on_failure(now),
                _ => t.tick(now),
            };
            if let Some(lux) = update.measurement {
                prop_assert!(is_valid_lux(lux) && age <= 600_000);
            }
            prop_assert_eq!(update.health, t.health());
        }
    }
}
