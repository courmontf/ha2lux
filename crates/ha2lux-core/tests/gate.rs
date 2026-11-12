//! T-GAT: hysteresis gate (spec §8.5).
use ha2lux_core::{Gate, GateParams};

fn gate_at(last: u8) -> Gate {
    let mut g = Gate::new(GateParams::default());
    g.reset_to(last);
    g
}

fn run(g: &mut Gate, seq: &[u8]) -> Vec<bool> {
    seq.iter().map(|&t| g.decide(t)).collect()
}

#[test]
fn t_gat_01_first_applied() {
    let mut g = Gate::new(GateParams::default());
    assert!(g.decide(50));
    assert_eq!(g.last_applied(), Some(50));
}

#[test]
fn t_gat_02_small_gap() {
    assert_eq!(run(&mut gate_at(50), &[52]), [false]);
}

#[test]
fn t_gat_03_third_consecutive() {
    let mut g = gate_at(50);
    assert_eq!(run(&mut g, &[54, 54, 54]), [false, false, true]);
    assert_eq!(g.last_applied(), Some(54));
}

#[test]
fn t_gat_04_direction_change_restarts() {
    assert_eq!(run(&mut gate_at(54), &[58, 50, 58]), [false, false, false]);
}

#[test]
fn t_gat_05_big_gap_immediate() {
    let mut g = gate_at(54);
    assert_eq!(run(&mut g, &[80]), [true]);
    assert_eq!(g.last_applied(), Some(80));
}

#[test]
fn t_gat_06_07_extremes() {
    let mut g = gate_at(98);
    assert_eq!(run(&mut g, &[100, 100, 100]), [false, false, true]);
    assert_eq!(run(&mut g, &[100]), [false]);
}

#[test]
fn t_gat_08_reset() {
    let mut g = gate_at(50);
    g.reset();
    assert_eq!(g.last_applied(), None);
    assert!(g.decide(51));
}

#[test]
fn t_gat_09_insufficient_resets_counter() {
    // 54 counts 1, 55 (gap 1 < 3) resets to zero, then 3 are needed again.
    let mut g = gate_at(50);
    assert_eq!(
        run(&mut g, &[54, 54, 51, 54, 54]),
        [false, false, false, false, false]
    );
    assert_eq!(run(&mut g, &[54]), [true]);
}

#[test]
fn reset_to_makes_next_reading_not_undo_it() {
    let mut g = gate_at(50);
    g.reset_to(70);
    assert_eq!(run(&mut g, &[71]), [false]);
}
