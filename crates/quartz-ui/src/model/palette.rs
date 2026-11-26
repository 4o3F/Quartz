//! The 14-slot categorical palette of 11 section 3.3 and the color a category uses.

/// Number of categorical palette slots.
pub(crate) const PALETTE_LEN: usize = 14;

/// One categorical palette slot, `cat.1` to `cat.14`; no other value is representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PaletteSlot(u8);

impl PaletteSlot {
    /// Every slot in palette order, which is also the order new categories take them in.
    pub(crate) const ALL: [Self; PALETTE_LEN] = [
        Self(1),
        Self(2),
        Self(3),
        Self(4),
        Self(5),
        Self(6),
        Self(7),
        Self(8),
        Self(9),
        Self(10),
        Self(11),
        Self(12),
        Self(13),
        Self(14),
    ];

    const TOKEN_KEYS: [&'static str; PALETTE_LEN] = [
        "cat.1", "cat.2", "cat.3", "cat.4", "cat.5", "cat.6", "cat.7", "cat.8", "cat.9", "cat.10",
        "cat.11", "cat.12", "cat.13", "cat.14",
    ];

    /// The slot with the 1-based number of the 11 section 3.3 table.
    ///
    /// # Panics
    /// Panics when `number` is not between 1 and 14. Meant for the constant tables that cite
    /// the design document, where the check runs at compile time.
    #[must_use]
    pub(crate) const fn numbered(number: u8) -> Self {
        let last = Self::ALL[PALETTE_LEN - 1];
        assert!(
            number >= 1 && number <= last.0,
            "palette slots are numbered 1 to 14"
        );
        Self(number)
    }

    /// The theme token and persisted key, such as `cat.8`.
    #[must_use]
    pub(crate) fn token_key(self) -> &'static str {
        Self::TOKEN_KEYS
            .get(self.index())
            .copied()
            .expect("every slot has a token key")
    }

    /// The first slot no color in `used` takes, in palette order (11 section 3.3); `None` when
    /// all 14 are taken.
    #[must_use]
    pub(crate) fn first_unused(used: &[CategoryColor]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|slot| !used.contains(&CategoryColor::Palette(*slot)))
    }

    /// Stable ordinal for high contrast chart patterns (11 section 2).
    #[cfg(windows)]
    pub(crate) const fn ordinal(self) -> u8 {
        self.0
    }

    fn index(self) -> usize {
        usize::from(self.0 - 1)
    }
}

/// One value per palette slot, looked up without a fallback because every slot is present.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PaletteSlotMap<T>([T; PALETTE_LEN]);

impl<T: Copy> PaletteSlotMap<T> {
    /// Uses one system color for all category slots in high contrast (11 section 2).
    #[cfg(windows)]
    pub(crate) fn filled(value: T) -> Self {
        Self([value; PALETTE_LEN])
    }
}

impl<T> PaletteSlotMap<T> {
    /// Builds the map by calling `value` once per slot in palette order, stopping at the first
    /// error.
    ///
    /// # Errors
    /// Returns the first error `value` returns.
    pub(crate) fn try_from_fn<E>(
        mut value: impl FnMut(PaletteSlot) -> Result<T, E>,
    ) -> Result<Self, E> {
        let mut values = Vec::with_capacity(PALETTE_LEN);
        for slot in PaletteSlot::ALL {
            values.push(value(slot)?);
        }
        let Ok(values) = <[T; PALETTE_LEN]>::try_from(values) else {
            unreachable!("one value was built per slot");
        };
        Ok(Self(values))
    }

    /// The value of `slot`.
    #[must_use]
    pub(crate) fn get(&self, slot: PaletteSlot) -> &T {
        self.0
            .get(slot.index())
            .expect("a palette slot always indexes the map")
    }
}

/// How a category is colored; persisted as `cat.1` to `cat.14` or `neutral` (11 section 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CategoryColor {
    Palette(PaletteSlot),
    /// The neutral status color, used by the unclassified category.
    Neutral,
}

impl CategoryColor {
    /// All selectable category colors: the 14 palette slots followed by neutral.
    pub(crate) const ALL: [Self; PALETTE_LEN + 1] = [
        Self::Palette(PaletteSlot::numbered(1)),
        Self::Palette(PaletteSlot::numbered(2)),
        Self::Palette(PaletteSlot::numbered(3)),
        Self::Palette(PaletteSlot::numbered(4)),
        Self::Palette(PaletteSlot::numbered(5)),
        Self::Palette(PaletteSlot::numbered(6)),
        Self::Palette(PaletteSlot::numbered(7)),
        Self::Palette(PaletteSlot::numbered(8)),
        Self::Palette(PaletteSlot::numbered(9)),
        Self::Palette(PaletteSlot::numbered(10)),
        Self::Palette(PaletteSlot::numbered(11)),
        Self::Palette(PaletteSlot::numbered(12)),
        Self::Palette(PaletteSlot::numbered(13)),
        Self::Palette(PaletteSlot::numbered(14)),
        Self::Neutral,
    ];

    /// The persisted wire token key (`cat.1` through `cat.14`, or `neutral`).
    #[must_use]
    pub(crate) fn token_key(self) -> &'static str {
        match self {
            Self::Palette(slot) => slot.token_key(),
            Self::Neutral => "neutral",
        }
    }

    /// Parses a canonical wire color token (`cat.1` through `cat.14`, or `neutral`).
    #[must_use]
    pub(crate) fn parse_wire(value: &str) -> Option<Self> {
        if value == "neutral" {
            return Some(Self::Neutral);
        }
        PaletteSlot::ALL
            .into_iter()
            .find(|slot| slot.token_key() == value)
            .map(Self::Palette)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_number_their_token_keys() {
        assert_eq!(PaletteSlot::numbered(1).token_key(), "cat.1");
        assert_eq!(PaletteSlot::numbered(14).token_key(), "cat.14");
        let keys: Vec<&str> = PaletteSlot::ALL
            .iter()
            .map(|slot| slot.token_key())
            .collect();
        assert_eq!(keys.len(), PALETTE_LEN);
    }

    #[test]
    #[should_panic(expected = "palette slots are numbered 1 to 14")]
    fn slot_fifteen_does_not_exist() {
        let _ = PaletteSlot::numbered(15);
    }

    #[test]
    fn first_unused_skips_taken_slots_and_ignores_neutral() {
        let used = [
            CategoryColor::Palette(PaletteSlot::numbered(1)),
            CategoryColor::Neutral,
            CategoryColor::Palette(PaletteSlot::numbered(3)),
        ];
        assert_eq!(
            PaletteSlot::first_unused(&used),
            Some(PaletteSlot::numbered(2))
        );
        let all = PaletteSlot::ALL.map(CategoryColor::Palette);
        assert_eq!(PaletteSlot::first_unused(&all), None);
    }

    #[test]
    fn map_returns_the_value_built_for_each_slot() {
        let map =
            PaletteSlotMap::try_from_fn(|slot| Ok::<_, std::convert::Infallible>(slot.token_key()))
                .expect("infallible");
        assert_eq!(*map.get(PaletteSlot::numbered(8)), "cat.8");
    }

    #[test]
    fn category_color_round_trips_canonical_wire_tokens() {
        assert_eq!(CategoryColor::ALL.len(), PALETTE_LEN + 1);
        for color in CategoryColor::ALL {
            assert_eq!(CategoryColor::parse_wire(color.token_key()), Some(color));
        }
        assert_eq!(CategoryColor::parse_wire("cat.0"), None);
        assert_eq!(CategoryColor::parse_wire("cat.15"), None);
        assert_eq!(CategoryColor::parse_wire(""), None);
    }
}
