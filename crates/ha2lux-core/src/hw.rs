//! Hardware normalization: display level 0-100 ⇄ raw value (spec §4.4).

use thiserror::Error;

/// Reasons why a display is not controllable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum HwError {
    /// The reported maximum is zero: the display is not controllable.
    #[error("the brightness maximum reported by the display is zero")]
    ZeroMax,
}

/// Checks that a hardware maximum is usable (`max > 0`).
pub fn ensure_controllable(max: u32) -> Result<u32, HwError> {
    if max == 0 {
        Err(HwError::ZeroMax)
    } else {
        Ok(max)
    }
}

/// Display level (0-100, clamped) → raw value, rounded to nearest
/// (half up). `100 → max`, `0 → 0`.
pub fn raw_from_level(level: i64, max: u32) -> u32 {
    let level = u64::from(crate::range::clamp_percent(level));
    // level <= 100 so the result is <= max.
    ((u64::from(max) * level + 50) / 100) as u32
}

/// Raw value → display level 0-100, rounded to nearest. A raw value
/// above the maximum is clamped to it; `max = 0` gives 0.
pub fn level_from_raw(raw: u32, max: u32) -> u8 {
    if max == 0 {
        return 0;
    }
    let raw = u64::from(raw.min(max));
    let max = u64::from(max);
    // raw <= max so the result is <= 100.
    crate::range::clamp_percent(((raw * 100 + max / 2) / max) as i64)
}

/// Backlight: level → raw with a configurable floor, so that "0 %"
/// does not turn off a panel that goes dark at 0. The result stays within `0..=max`.
pub fn backlight_raw_from_level(level: i64, max: u32, floor: u32) -> u32 {
    raw_from_level(level, max).max(floor).min(max)
}
