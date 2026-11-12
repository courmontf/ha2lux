//! T-FLT : lissage (spec §8.5) et simulations T-FLT-13/14.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::{Curve, Filter, FilterParams, Gate, GateParams};

fn run(seq: &[f64]) -> Filter {
    let mut f = Filter::new(FilterParams::default());
    for &lux in seq {
        f.push(lux);
    }
    f
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn t_flt_01() {
    assert_eq!(run(&[0.0]).smoothed_lux(), 0.0);
}

#[test]
fn t_flt_02_big_gap_held() {
    let f = run(&[8.0, 77.0]);
    assert!(close(f.smoothed_lux(), 8.0, 0.01));
    assert!(f.is_pending());
}

#[test]
fn t_flt_03_confirmation() {
    let f = run(&[8.0, 77.0, 77.0]);
    assert!(close(f.smoothed_lux(), 77.0, 0.01));
    assert!(!f.is_pending());
}

#[test]
fn t_flt_04_stays_on_lamp() {
    let mut seq = vec![8.0, 77.0];
    seq.extend(std::iter::repeat_n(77.0, 60));
    let lux = run(&seq).smoothed_lux();
    assert!((75.0..=79.0).contains(&lux), "{lux}");
}

#[test]
fn t_flt_05_isolated_spike_ignored() {
    assert!(close(run(&[8.0, 300.0, 8.0]).smoothed_lux(), 8.0, 0.01));
}

#[test]
fn t_flt_06_opposite_does_not_confirm() {
    assert!(run(&[8.0, 300.0, 0.5]).smoothed_lux() < 10.0);
}

#[test]
fn t_flt_07_renewed_hold() {
    let f = run(&[8.0, 50.0, 77.0]);
    assert!(f.is_pending());
    assert!(close(
        run(&[8.0, 50.0, 77.0, 77.0]).smoothed_lux(),
        77.0,
        0.01
    ));
}

#[test]
fn t_flt_08_dark_room_noise_not_big() {
    let mut seq = vec![0.0; 20];
    seq.push(8.0);
    assert!(!run(&seq).is_pending());
}

#[test]
fn t_flt_09_spike_after_noise() {
    let mut seq = vec![0.0; 20];
    seq.extend([8.0, 300.0, 8.0]);
    assert!(run(&seq).smoothed_lux() < 10.0);
}

#[test]
fn t_flt_10_medium_step() {
    let mut seq = vec![77.0; 20];
    let before = run(&seq).smoothed_lux();
    assert!(close(before, 77.0, 0.01));
    seq.extend([130.0, 130.0]);
    assert!(close(run(&seq).smoothed_lux(), 130.0, 5.0));
}

#[test]
fn t_flt_11_slow_smoothing() {
    let f = |n: usize| {
        let mut seq = vec![0.0];
        seq.extend(std::iter::repeat_n(8.0, n));
        run(&seq).smoothed_lux()
    };
    assert!(close(f(1), 0.48, 0.02));
    assert!(close(f(2), 0.95, 0.02));
    assert!(close(f(3), 1.39, 0.02));
}

#[test]
fn t_flt_12_latest_lux() {
    let held = run(&[8.0, 300.0]);
    assert!(close(held.latest_lux(), 300.0, 10.0));
    let free = run(&[8.0, 8.0]);
    assert!(close(free.latest_lux(), free.smoothed_lux(), 1e-9));
}

#[test]
fn non_finite_input_is_safe() {
    let mut f = Filter::new(FilterParams::default());
    for lux in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -4.0, 1e12] {
        f.push(lux);
        assert!(f.smoothed_lux().is_finite() && (0.0..=1_000_000.5).contains(&f.smoothed_lux()));
    }
}

#[test]
fn reset_forgets_everything() {
    let mut f = run(&[8.0, 300.0]);
    f.reset();
    assert!(!f.has_value() && !f.is_pending());
}

/// PRNG xorshift : la reproductibilité suffit, les graines ne sont pas sacrées.
struct Rng(u64);
impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Simule `polls` sondages et renvoie la suite des niveaux appliqués.
fn simulate(polls: usize, mut noise: impl FnMut(&mut Rng) -> f64) -> Vec<u8> {
    let (curve, mut filter, mut gate) = (
        Curve::new(),
        Filter::new(FilterParams::default()),
        Gate::new(GateParams::default()),
    );
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut applied = Vec::new();
    for _ in 0..polls {
        filter.push(noise(&mut rng));
        let target = curve.level_at(filter.smoothed_lux());
        if gate.decide(target) {
            applied.push(target);
        }
    }
    applied
}

#[test]
fn t_flt_13_noisy_dark_room_is_stable() {
    let applied = simulate(3_840, |r| if r.next_f64() < 1.0 / 3.0 { 5.0 } else { 0.0 });
    let changes = applied.windows(2).skip(40).filter(|w| w[0] != w[1]).count();
    assert!(changes <= 35, "{changes} changements");
}

#[test]
fn t_flt_14_no_big_jump_with_small_noise() {
    let applied = simulate(3_840, |r| {
        let v = r.next_f64();
        if v < 1.0 / 3.0 {
            0.0
        } else if v < 2.0 / 3.0 {
            5.0
        } else {
            8.0
        }
    });
    for w in applied.windows(2) {
        assert!(w[0].abs_diff(w[1]) < 15, "saut {} -> {}", w[0], w[1]);
    }
}

#[test]
fn lamp_is_followed_quickly() {
    // Lampe 8 → 77 lx, sondages à 15 s : suivie en au plus 2 sondages (≈ 30 s,
    // ou ≈ 18 s avec le sondage de confirmation rapproché).
    let mut f = Filter::new(FilterParams::default());
    f.push(8.0);
    f.push(77.0);
    f.push(77.0);
    assert!(close(f.smoothed_lux(), 77.0, 0.01));
}
