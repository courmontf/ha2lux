//! Reconnection: capped exponential delays with jitter.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::time::Duration;

use ha2lux_ha::backoff::{Backoff, Jitter};
use proptest::prelude::*;

#[test]
fn doubles_from_one_to_sixty_seconds_without_jitter() {
    let mut b = Backoff::standard();
    let seq: Vec<f64> = (0..10).map(|_| b.next_delay(0.5).as_secs_f64()).collect();
    assert_eq!(
        seq,
        [1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 60.0, 60.0, 60.0, 60.0]
    );
}

#[test]
fn reset_restarts_at_the_minimum() {
    let mut b = Backoff::standard();
    for _ in 0..5 {
        b.next_delay(0.5);
    }
    b.reset();
    assert_eq!(b.next_delay(0.5), Duration::from_secs(1));
}

#[test]
fn jitter_stays_within_twenty_percent() {
    for jitter in [0.0, 0.25, 0.5, 0.999] {
        let mut b = Backoff::standard();
        let delay = b.next_delay(jitter).as_secs_f64();
        assert!((0.8..=1.2).contains(&delay), "{jitter} → {delay}");
    }
    let mut b = Backoff::standard();
    assert!(b.next_delay(-5.0).as_secs_f64() >= 0.8);
    assert!(b.next_delay(5.0).as_secs_f64() <= 2.4);
}

#[test]
fn very_long_outages_do_not_overflow() {
    let mut b = Backoff::standard();
    for _ in 0..10_000 {
        assert!(b.next_delay(0.5) <= Duration::from_secs(72));
    }
}

proptest! {
    #[test]
    fn delays_are_bounded_and_the_generator_is_in_range(seed in any::<u64>()) {
        let mut jitter = Jitter::new(seed);
        let mut b = Backoff::standard();
        for _ in 0..50 {
            let j = jitter.sample();
            prop_assert!((0.0..1.0).contains(&j));
            let d = b.next_delay(j);
            prop_assert!(d >= Duration::from_millis(800) && d <= Duration::from_millis(72_000));
        }
    }
}
