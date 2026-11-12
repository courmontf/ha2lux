//! Normalisation matérielle : niveau écran 0-100 ⇄ valeur brute (spec §4.4).

use thiserror::Error;

/// Raisons pour lesquelles un écran n'est pas contrôlable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum HwError {
    /// Le maximum rapporté est nul : l'écran n'est pas contrôlable.
    #[error("le maximum de luminosité rapporté par l'écran est nul")]
    ZeroMax,
}

/// Vérifie qu'un maximum matériel est exploitable (`max > 0`).
pub fn ensure_controllable(max: u32) -> Result<u32, HwError> {
    if max == 0 {
        Err(HwError::ZeroMax)
    } else {
        Ok(max)
    }
}

/// Niveau écran (0-100, clampé) → valeur brute, arrondie au plus proche
/// (demi vers le haut). `100 → max`, `0 → 0`.
pub fn raw_from_level(level: i64, max: u32) -> u32 {
    let level = u64::from(crate::range::clamp_percent(level));
    // level <= 100 donc le résultat est <= max.
    ((u64::from(max) * level + 50) / 100) as u32
}

/// Valeur brute → niveau écran 0-100, arrondi au plus proche. Une valeur
/// brute supérieure au maximum est ramenée à celui-ci ; `max = 0` donne 0.
pub fn level_from_raw(raw: u32, max: u32) -> u8 {
    if max == 0 {
        return 0;
    }
    let raw = u64::from(raw.min(max));
    let max = u64::from(max);
    // raw <= max donc le résultat est <= 100.
    crate::range::clamp_percent(((raw * 100 + max / 2) / max) as i64)
}

/// Backlight : niveau → brut avec un plancher configurable, pour qu'un « 0 % »
/// n'éteigne pas un panneau qui s'éteint à 0. Le résultat reste dans `0..=max`.
pub fn backlight_raw_from_level(level: i64, max: u32, floor: u32) -> u32 {
    raw_from_level(level, max).max(floor).min(max)
}
