//! Stable category metadata shared by storage and query projections.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// Database identity of a category row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryId(pub i64);

/// Durable category identity resolved when importing a rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryUuid(pub String);

/// Number of category palette slots specified by design system section 3.3.
pub const PALETTE_SLOT_COUNT: u8 = 14;

/// A validated slot in the design system's category palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteSlot(u8);
impl TryFrom<u8> for PaletteSlot {
    type Error = ParseCategoryColorError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if !(1..=PALETTE_SLOT_COUNT).contains(&value) {
            return Err(ParseCategoryColorError);
        }
        Ok(Self(value))
    }
}
impl PaletteSlot {
    /// Returns the validated palette index.
    pub const fn get(self) -> u8 {
        self.0
    }
}
/// A category uses neutral gray or one of the fourteen palette slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum CategoryColor {
    Neutral,
    Palette(PaletteSlot),
}
/// The color is outside the fixed category palette or has a noncanonical spelling.
#[derive(Debug, snafu::Snafu)]
#[snafu(display("category color must be neutral or cat.1 through cat.14"))]
pub struct ParseCategoryColorError;
impl FromStr for CategoryColor {
    type Err = ParseCategoryColorError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "neutral" {
            return Ok(Self::Neutral);
        }
        let slot = value.strip_prefix("cat.").ok_or(ParseCategoryColorError)?;
        let number: u8 = slot.parse().map_err(|_| ParseCategoryColorError)?;
        if number.to_string() != slot {
            return Err(ParseCategoryColorError);
        }
        Ok(Self::Palette(PaletteSlot::try_from(number)?))
    }
}
impl fmt::Display for CategoryColor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Neutral => formatter.write_str("neutral"),
            Self::Palette(slot) => write!(formatter, "cat.{}", slot.get()),
        }
    }
}
impl TryFrom<String> for CategoryColor {
    type Error = ParseCategoryColorError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<CategoryColor> for String {
    fn from(value: CategoryColor) -> Self {
        value.to_string()
    }
}

/// Stable builtin key used for localization and rule references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinCategory {
    Development,
    Learning,
    Communication,
    Entertainment,
    System,
}
impl BuiltinCategory {
    /// Default display order, excluding the virtual unclassified result.
    pub const ALL: [Self; 5] = [
        Self::Development,
        Self::Learning,
        Self::Communication,
        Self::Entertainment,
        Self::System,
    ];

    /// Returns the database and translation-key suffix.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Learning => "learning",
            Self::Communication => "communication",
            Self::Entertainment => "entertainment",
            Self::System => "system",
        }
    }
    /// Parses a persisted builtin key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|category| category.key() == key)
    }
    /// Returns the stable UUID assigned to a newly inserted builtin category.
    pub fn uuid(self) -> CategoryUuid {
        let value = match self {
            Self::Development => "71f5a800-423a-4000-8000-000000000001",
            Self::Learning => "71f5a800-423a-4000-8000-000000000002",
            Self::Communication => "71f5a800-423a-4000-8000-000000000003",
            Self::Entertainment => "71f5a800-423a-4000-8000-000000000004",
            Self::System => "71f5a800-423a-4000-8000-000000000005",
        };
        CategoryUuid(value.into())
    }
    /// Initial tendency; subsequent user edits are retained.
    pub const fn tendency(self) -> CategoryTendency {
        match self {
            Self::Development | Self::Learning => CategoryTendency::Focus,
            Self::Communication | Self::System => CategoryTendency::Neutral,
            Self::Entertainment => CategoryTendency::Distraction,
        }
    }
    /// Initial palette slot from design system section 3.3.
    pub const fn color(self) -> CategoryColor {
        match self {
            Self::Development => CategoryColor::Palette(PaletteSlot(8)),
            Self::Learning => CategoryColor::Palette(PaletteSlot(1)),
            Self::Communication => CategoryColor::Palette(PaletteSlot(3)),
            Self::Entertainment => CategoryColor::Palette(PaletteSlot(13)),
            Self::System => CategoryColor::Palette(PaletteSlot(10)),
        }
    }
}

/// Tendency used to compute focus and distraction shares of active time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryTendency {
    Focus,
    Neutral,
    Distraction,
}

/// One category's presentation metadata, without resolving the interface language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: CategoryId,
    pub uuid: CategoryUuid,
    pub builtin_key: Option<BuiltinCategory>,
    /// User name, including a renamed builtin; otherwise localize the builtin key.
    pub name: Option<String>,
    /// Palette key `cat.1` through `cat.14`, or `neutral`.
    pub color: CategoryColor,
    pub tendency: CategoryTendency,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_accept_only_canonical_palette_strings_and_round_trip_json() {
        for value in std::iter::once("neutral".to_owned())
            .chain((1..=PALETTE_SLOT_COUNT).map(|slot| format!("cat.{slot}")))
        {
            let color: CategoryColor = value.parse().expect("valid category color");
            assert_eq!(color.to_string(), value);
            let json = serde_json::to_value(color).expect("color JSON");
            assert_eq!(json, value);
            assert_eq!(
                serde_json::from_value::<CategoryColor>(json).expect("color round trip"),
                color
            );
        }
        for value in [
            "", "cat.0", "cat.15", "cat.01", "cat.+1", "cat.256", "Neutral", "cat.1 ",
        ] {
            assert!(value.parse::<CategoryColor>().is_err(), "{value}");
            assert!(
                serde_json::from_value::<CategoryColor>(serde_json::json!(value)).is_err(),
                "{value}"
            );
        }
    }
}
