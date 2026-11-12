//! T-HW: hardware normalization (spec §4.4).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::hw::{
    HwError, backlight_raw_from_level, ensure_controllable, level_from_raw, raw_from_level,
};

#[test]
fn t_hw_01_identity_m100() {
    for l in 0..=100 {
        assert_eq!(raw_from_level(l, 100), l as u32);
    }
}

#[test]
fn t_hw_02() {
    let raws: Vec<u32> = [100, 0, 50, 1]
        .iter()
        .map(|&l| raw_from_level(l, 255))
        .collect();
    assert_eq!(raws, [255, 0, 128, 3]);
}

#[test]
fn t_hw_03() {
    assert_eq!(raw_from_level(50, 63), 32);
}

#[test]
fn t_hw_04_round_trip() {
    for max in [1u32, 10, 50, 63, 100] {
        for raw in 0..=max {
            assert_eq!(
                raw_from_level(i64::from(level_from_raw(raw, max)), max),
                raw,
                "max={max}"
            );
        }
    }
    for max in [255u32, 65535] {
        let tolerance = max.div_ceil(200);
        for raw in 0..=max {
            let back = raw_from_level(i64::from(level_from_raw(raw, max)), max);
            assert!(
                back.abs_diff(raw) <= tolerance,
                "max={max} raw={raw} back={back}"
            );
        }
    }
}

#[test]
fn t_hw_05() {
    assert_eq!(level_from_raw(128, 255), 50);
}

#[test]
fn t_hw_06_zero_max_not_controllable() {
    assert_eq!(ensure_controllable(0), Err(HwError::ZeroMax));
    assert_eq!(ensure_controllable(100), Ok(100));
    assert_eq!(level_from_raw(5, 0), 0);
}

#[test]
fn t_hw_07_08_backlight_floor() {
    assert_eq!(backlight_raw_from_level(0, 1000, 1), 1);
    assert_eq!(backlight_raw_from_level(100, 7, 1), 7);
    assert_eq!(backlight_raw_from_level(0, 7, 1), 1);
    assert_eq!(backlight_raw_from_level(0, 7, 0), 0);
    assert_eq!(
        backlight_raw_from_level(50, 7, 99),
        7,
        "plancher borné au max"
    );
}

#[test]
fn raw_never_exceeds_max_and_out_of_range_level_is_clamped() {
    assert_eq!(raw_from_level(500, 255), 255);
    assert_eq!(raw_from_level(-5, 255), 0);
    assert_eq!(level_from_raw(999, 100), 100);
}
