//! T-MAP: master level ⇄ display level (spec §4).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::{Range, reconstruct_master};
use proptest::prelude::*;

fn r(low: i64, high: i64) -> Range {
    Range::normalized(low, high)
}

#[test]
fn t_map_01_to_04() {
    assert_eq!(
        (r(40, 100).display_level(100), r(0, 60).display_level(100)),
        (100, 60)
    );
    assert_eq!(
        (r(40, 100).display_level(75), r(0, 60).display_level(75)),
        (85, 45)
    );
    assert_eq!(
        (r(40, 100).display_level(50), r(0, 60).display_level(50)),
        (70, 30)
    );
    assert_eq!(
        (r(40, 100).display_level(0), r(0, 60).display_level(0)),
        (40, 0)
    );
}

#[test]
fn t_map_05() {
    assert_eq!(r(40, 100).display_level(33), 60);
}

#[test]
fn t_map_06_identity_on_full_range() {
    for m in 0..=100 {
        let level = Range::FULL.display_level(m);
        assert_eq!(i64::from(level), m);
        assert_eq!(i64::from(Range::FULL.master_for_level(i64::from(level))), m);
    }
}

#[test]
fn t_map_07_master_clamped() {
    assert_eq!(r(0, 60).display_level(130), 60);
    assert_eq!(r(40, 100).display_level(-10), 40);
}

#[test]
fn t_map_08_09_inverse() {
    assert_eq!(r(0, 60).master_for_level(80), 100);
    assert_eq!(r(40, 100).master_for_level(10), 0);
    assert_eq!(r(40, 100).master_for_level(55), 25);
}

#[test]
fn t_map_10_to_14_normalize() {
    let t = |a, b| {
        let n = r(a, b);
        (n.low(), n.high())
    };
    assert_eq!(t(70, 80), (60, 80));
    assert_eq!(t(0, 5), (0, 20));
    assert_eq!(t(-10, 140), (0, 100));
    assert_eq!(t(100, 100), (80, 100));
    assert_eq!(t(50, 10), (0, 20));
}

#[test]
fn t_map_15_exhaustive() {
    for low in (0..=80).step_by(5) {
        for high in ((low + 20)..=100).step_by(5) {
            let range = r(low, high);
            assert_eq!(range.display_level(0), low as u8);
            assert_eq!(range.display_level(100), high as u8);
            for m in 1..=100 {
                assert!(range.display_level(m) >= range.display_level(m - 1));
            }
            for m in (5..=100).step_by(5) {
                assert!(range.display_level(m) > range.display_level(m - 5));
            }
        }
    }
}

#[test]
fn t_map_16_exhaustive() {
    for low in (0..=80).step_by(5) {
        for high in ((low + 20)..=100).step_by(5) {
            let range = r(low, high);
            for l in low..=high {
                assert_eq!(
                    i64::from(range.display_level(i64::from(range.master_for_level(l)))),
                    l
                );
            }
            for m in 0..=100i64 {
                let back = i64::from(range.master_for_level(i64::from(range.display_level(m))));
                assert!(
                    (back - m).abs() <= 3,
                    "plage ({low},{high}) m={m} back={back}"
                );
            }
        }
    }
}

proptest! {
    #[test]
    fn t_map_15_16_any_range(low in 0i64..=80, extra in 20i64..=100, m in 0i64..=100) {
        let range = r(low, (low + extra).min(100).max(low + 20));
        prop_assert!(range.width() >= 20);
        prop_assert!(range.display_level(m) >= range.low() && range.display_level(m) <= range.high());
        if m > 0 { prop_assert!(range.display_level(m) >= range.display_level(m - 1)); }
        let l = i64::from(range.display_level(m));
        prop_assert_eq!(i64::from(range.display_level(i64::from(range.master_for_level(l)))), l);
        prop_assert!((i64::from(range.master_for_level(l)) - m).abs() <= 3);
    }

    #[test]
    fn normalized_always_valid(low in any::<i64>(), high in any::<i64>()) {
        let n = r(low, high);
        prop_assert!((20..=100).contains(&n.high()));
        prop_assert!(n.low() + 20 <= n.high());
    }
}

#[test]
fn t_map_17_18_edit_stops_at_limit() {
    let range = r(30, 60);
    let e = range.with_high(40);
    assert_eq!((e.low(), e.high()), (30, 50));
    let e = range.with_low(50);
    assert_eq!((e.low(), e.high()), (40, 60));
}

#[test]
fn t_map_19_reconstruct() {
    assert_eq!(reconstruct_master([(r(40, 100), 70), (r(0, 60), 30)]), 50);
    assert_eq!(reconstruct_master(std::iter::empty()), 50);
}
