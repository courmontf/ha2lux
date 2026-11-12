//! Niveau maître, plage par écran et conversions (spec §4.1-4.3).

/// Largeur minimale d'une plage (`haut - bas`).
pub const MIN_RANGE_WIDTH: u8 = 20;

/// Ramène un entier quelconque dans `0..=100`.
pub fn clamp_percent(value: i64) -> u8 {
    // Après `clamp`, la valeur tient dans un `u8`.
    value.clamp(0, 100) as u8
}

/// Plage `(bas, haut)` d'un écran : niveaux réels de l'écran quand le maître
/// vaut 0 et 100.
///
/// Invariants garantis par construction : `haut ∈ [20, 100]` et
/// `bas ∈ [0, haut − 20]`.
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
    /// Plage par défaut d'un écran nouveau : `(0, 100)`.
    pub const FULL: Range = Range { low: 0, high: 100 };

    /// Construit une plage en normalisant des bornes arbitraires : `haut` est
    /// ramené dans `[20, 100]`, puis `bas` dans `[0, haut − 20]`.
    pub fn normalized(low: i64, high: i64) -> Self {
        let min_width = i64::from(MIN_RANGE_WIDTH);
        let high = high.clamp(min_width, 100);
        let low = low.clamp(0, high - min_width);
        Self {
            low: clamp_percent(low),
            high: clamp_percent(high),
        }
    }

    /// Borne basse.
    pub fn low(self) -> u8 {
        self.low
    }

    /// Borne haute.
    pub fn high(self) -> u8 {
        self.high
    }

    /// Largeur `haut − bas` (toujours ≥ 20).
    pub fn width(self) -> u8 {
        self.high - self.low
    }

    /// Édite la borne haute : elle s'arrête à `bas + 20`, sans pousser le bas.
    pub fn with_high(self, high: i64) -> Self {
        let floor = i64::from(self.low) + i64::from(MIN_RANGE_WIDTH);
        Self {
            low: self.low,
            high: clamp_percent(high.clamp(floor, 100)),
        }
    }

    /// Édite la borne basse : elle s'arrête à `haut − 20`, sans pousser le haut.
    pub fn with_low(self, low: i64) -> Self {
        let ceiling = i64::from(self.high) - i64::from(MIN_RANGE_WIDTH);
        Self {
            low: clamp_percent(low.clamp(0, ceiling)),
            high: self.high,
        }
    }

    /// Niveau écran (0-100) correspondant à un niveau maître.
    ///
    /// `bas + arrondi_au_plus_proche((haut − bas) × clamp(maître) / 100)`,
    /// arrondi « demi vers le haut », en arithmétique entière.
    pub fn display_level(self, master: i64) -> u8 {
        let master = u32::from(clamp_percent(master));
        let offset = (u32::from(self.width()) * master + 50) / 100;
        // offset <= width, donc low + offset <= high <= 100.
        clamp_percent(i64::from(self.low) + i64::from(offset))
    }

    /// Niveau maître correspondant à un niveau écran (inverse de
    /// [`Range::display_level`]), le niveau étant d'abord clampé dans la plage.
    pub fn master_for_level(self, level: i64) -> u8 {
        let level = level.clamp(i64::from(self.low), i64::from(self.high));
        let width = i64::from(self.width());
        let above = level - i64::from(self.low);
        clamp_percent((above * 100 * 2 + width) / (2 * width))
    }
}

/// Reconstruit un maître en moyennant l'inverse du mappage des niveaux lus
/// sur les écrans contrôlables (spec §4.5). Sans écran : 50.
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
