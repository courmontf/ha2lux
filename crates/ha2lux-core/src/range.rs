//! Master level, per-display range and conversions (spec §4.1-4.3).

/// Minimum width of a range (`high - low`).
pub const MIN_RANGE_WIDTH: u8 = 20;

/// Clamps any integer into `0..=100`.
pub fn clamp_percent(value: i64) -> u8 {
    // After `clamp`, the value fits in a `u8`.
    value.clamp(0, 100) as u8
}

/// `(low, high)` range of a display: the display's actual levels when the master
/// is 0 and 100.
///
/// Invariants guaranteed by construction: `high ∈ [20, 100]` and
/// `low ∈ [0, high − 20]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Range {
    low: u8,
    high: u8,
}

impl Default for Range {
    fn default() -> Self {
        Self::FULL
    }
}

impl Range {
    /// Default range of a new display: `(0, 100)`.
    pub const FULL: Range = Range { low: 0, high: 100 };

    /// Builds a range by normalizing arbitrary bounds: `high` is
    /// clamped into `[20, 100]`, then `low` into `[0, high − 20]`.
    pub fn normalized(low: i64, high: i64) -> Self {
        let min_width = i64::from(MIN_RANGE_WIDTH);
        let high = high.clamp(min_width, 100);
        let low = low.clamp(0, high - min_width);
        Self {
            low: clamp_percent(low),
            high: clamp_percent(high),
        }
    }

    /// Lower bound.
    pub fn low(self) -> u8 {
        self.low
    }

    /// Upper bound.
    pub fn high(self) -> u8 {
        self.high
    }

    /// Width `high − low` (always ≥ 20).
    pub fn width(self) -> u8 {
        self.high - self.low
    }

    /// Edits the upper bound: it stops at `low + 20`, without pushing the low.
    pub fn with_high(self, high: i64) -> Self {
        let floor = i64::from(self.low) + i64::from(MIN_RANGE_WIDTH);
        Self {
            low: self.low,
            high: clamp_percent(high.clamp(floor, 100)),
        }
    }

    /// Edits the lower bound: it stops at `high − 20`, without pushing the high.
    pub fn with_low(self, low: i64) -> Self {
        let ceiling = i64::from(self.high) - i64::from(MIN_RANGE_WIDTH);
        Self {
            low: clamp_percent(low.clamp(0, ceiling)),
            high: self.high,
        }
    }

    /// Display level (0-100) corresponding to a master level.
    ///
    /// `low + round_to_nearest((high − low) × clamp(master) / 100)`,
    /// rounded "half up", using integer arithmetic.
    pub fn display_level(self, master: i64) -> u8 {
        let master = u32::from(clamp_percent(master));
        let offset = (u32::from(self.width()) * master + 50) / 100;
        // offset <= width, so low + offset <= high <= 100.
        clamp_percent(i64::from(self.low) + i64::from(offset))
    }

    /// Master level corresponding to a display level (inverse of
    /// [`Range::display_level`]), the level first being clamped into the range.
    pub fn master_for_level(self, level: i64) -> u8 {
        let level = level.clamp(i64::from(self.low), i64::from(self.high));
        let width = i64::from(self.width());
        let above = level - i64::from(self.low);
        clamp_percent((above * 100 * 2 + width) / (2 * width))
    }
}

/// Rebuilds a master by averaging the inverse of the level mapping read
/// from the controllable displays (spec §4.5). With no display: 50.
pub fn reconstruct_master(displays: impl IntoIterator<Item = (Range, i64)>) -> u8 {
    let (sum, count) = displays
        .into_iter()
        .fold((0u32, 0u32), |(sum, count), (range, level)| {
            (sum + u32::from(range.master_for_level(level)), count + 1)
        });
    if count == 0 {
        return 50;
    }
    clamp_percent(i64::from((sum * 2 + count) / (2 * count)))
}
