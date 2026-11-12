//! T-AMB: lux → master curve (spec §8.4).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::Curve;
use proptest::prelude::*;

fn pairs(c: &Curve) -> Vec<(f64, u8)> {
    c.points().iter().map(|p| (p.lux, p.level)).collect()
}

#[test]
fn t_amb_01_default_points() {
    let c = Curve::new();
    let got: Vec<u8> = [0.0, 10.0, 100.0, 1000.0]
        .iter()
        .map(|&l| c.level_at(l))
        .collect();
    assert_eq!(got, [20, 40, 70, 100]);
}

#[test]
fn t_amb_02_flat_outside() {
    let c = Curve::new();
    assert_eq!(c.level_at(50_000.0), 100);
    assert_eq!(c.level_at(-5.0), 20);
}

#[test]
fn t_amb_03_interpolation() {
    let c = Curve::new();
    let lux = [1.0, 5.0, 20.0, 30.0, 50.0, 77.0, 300.0, 500.0];
    let got: Vec<u8> = lux.iter().map(|&l| c.level_at(l)).collect();
    assert_eq!(got, [23, 32, 47, 52, 59, 66, 84, 91]);
}

#[test]
fn t_amb_04_empty_is_default() {
    assert_eq!(Curve::new().level_at(10.0), 40);
}

#[test]
fn t_amb_05_monotonic_sweep() {
    let c = Curve::new();
    let mut lux = 0.0f64;
    let mut previous = 0;
    while lux < 30_000.0 {
        let level = c.level_at(lux);
        assert!(level >= previous, "décroît à {lux} lx");
        previous = level;
        lux = lux * 1.1 + 0.1;
    }
}

#[test]
fn t_amb_06_log_scale_offset() {
    assert!(Curve::new().level_at(5.0) > 30);
}

#[test]
fn t_amb_07_round_trip() {
    let text = "0:20,5:30,100:70,1000:100";
    let c = Curve::parse(text);
    assert_eq!(c.to_string(), text);
    assert_eq!(Curve::parse(&c.to_string()), c);
}

#[test]
fn t_amb_08_unreadable() {
    assert!(Curve::parse("").points().is_empty());
    assert!(Curve::parse("garbage").points().is_empty());
}

#[test]
fn t_amb_09_sorted() {
    assert_eq!(
        pairs(&Curve::parse("100:90,10:20")),
        [(10.0, 20), (100.0, 90)]
    );
}

#[test]
fn t_amb_10_contradiction() {
    assert_eq!(pairs(&Curve::parse("10:80,100:20")), [(100.0, 20)]);
}

#[test]
fn t_amb_11_out_of_bounds_dropped() {
    assert_eq!(
        pairs(&Curve::parse("10:20,-1:5,20:200,30:40")),
        [(10.0, 20), (30.0, 40)]
    );
}

#[test]
fn t_amb_12_fractional_lux() {
    assert_eq!(pairs(&Curve::parse("2.5:15")), [(2.5, 15)]);
    assert_eq!(Curve::parse("2.5:15").to_string(), "2.5:15");
}

#[test]
fn t_amb_13_bounded_format() {
    let c = Curve::parse("0:20,5:30,100:70,1000:100");
    assert!(c.to_text_bounded(25).is_ok());
    assert!(c.to_text_bounded(24).is_err());
}

#[test]
fn parse_ignores_non_finite_and_huge() {
    assert!(
        Curve::parse("nan:5,inf:5,2000000:5,1e999:5")
            .points()
            .is_empty()
    );
}

proptest! {
    #[test]
    fn parse_never_panics_and_keeps_invariants(text in ".{0,80}") {
        let c = Curve::parse(&text);
        prop_assert!(c.points().len() <= 8);
        for w in c.points().windows(2) {
            prop_assert!(w[0].lux < w[1].lux && w[0].level <= w[1].level);
        }
    }

    #[test]
    fn level_at_is_monotonic(a in 0.0f64..2_000_000.0, b in 0.0f64..2_000_000.0) {
        let c = Curve::new();
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        prop_assert!(c.level_at(lo) <= c.level_at(hi));
    }
}
