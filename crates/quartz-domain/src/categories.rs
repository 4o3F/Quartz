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

/// Stable category identifier syntax, independent of display names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CategoryIdentifier {
    /// A hexadecimal UUID normalized to lowercase.
    Uuid(CategoryUuid),
    /// A key in the reserved builtin namespace.
    Builtin(String),
}
impl CategoryIdentifier {
    /// Recognizes builtin references and UUIDs in 8-4-4-4-12 form.
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(key) = value.strip_prefix("builtin:") {
            return Some(Self::Builtin(key.to_owned()));
        }
        // UUID text has 32 hexadecimal digits separated into five groups.
        const UUID_TEXT_LEN: usize = 36;
        const UUID_HYPHENS: [usize; 4] = [8, 13, 18, 23];
        if value.len() == UUID_TEXT_LEN
            && value.bytes().enumerate().all(|(index, byte)| {
                if UUID_HYPHENS.contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            Some(Self::Uuid(CategoryUuid(value.to_ascii_lowercase())))
        } else {
            None
        }
    }
    /// Returns the normalized rule-text reference.
    pub fn key(&self) -> String {
        match self {
            Self::Uuid(uuid) => uuid.0.clone(),
            Self::Builtin(key) => format!("builtin:{key}"),
        }
    }
}

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryTendency {
    Focus,
    #[default]
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

/// Maximum user name length in Unicode scalar values (04 §6.3.5).
pub const MAX_CATEGORY_NAME_SCALARS: usize = 256;

/// Trimmed, nonblank user name; absence restores a builtin's localized default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CategoryName(String);
impl CategoryName {
    /// Reads legacy names for display, including names in a reserved namespace.
    ///
    /// # Errors
    /// Rejects blank names and names exceeding the documented length limit.
    pub fn from_stored(value: &str) -> Result<Self, EditCategoryError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(EditCategoryError::BlankName);
        }
        if value.chars().count() > MAX_CATEGORY_NAME_SCALARS {
            return Err(EditCategoryError::NameTooLong);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the validated, trimmed name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl FromStr for CategoryName {
    type Err = EditCategoryError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let name = Self::from_stored(value)?;
        if CategoryIdentifier::parse(name.as_str()).is_some() {
            return Err(EditCategoryError::ReservedName);
        }
        Ok(name)
    }
}
impl TryFrom<String> for CategoryName {
    type Error = EditCategoryError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<CategoryName> for String {
    fn from(value: CategoryName) -> Self {
        value.0
    }
}

/// A tendency outside the three supported values.
#[derive(Debug, snafu::Snafu)]
#[snafu(display("category tendency must be focus, neutral or distraction"))]
pub struct ParseCategoryTendencyError;
impl FromStr for CategoryTendency {
    type Err = ParseCategoryTendencyError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "focus" => Ok(Self::Focus),
            "neutral" => Ok(Self::Neutral),
            "distraction" => Ok(Self::Distraction),
            _ => Err(ParseCategoryTendencyError),
        }
    }
}

/// Whether list results include archived categories.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IncludeArchivedCategories {
    #[default]
    Exclude,
    Include,
}

/// Persisted archive state, independent of the parent and children's states.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CategoryArchiveState {
    #[default]
    Active,
    Archived,
}

/// Complete editable values; builtin identity and ordering have separate owners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryValues {
    pub parent_uuid: Option<CategoryUuid>,
    pub name: Option<CategoryName>,
    pub color: CategoryColor,
    pub tendency: CategoryTendency,
}
impl CategoryValues {
    /// Checks the name requirement after resolving the stored builtin identity.
    ///
    /// # Errors
    /// Rejects unnamed user categories and names in a reserved identifier namespace.
    pub fn validate_for(&self, builtin: Option<BuiltinCategory>) -> Result<(), EditCategoryError> {
        if self
            .name
            .as_ref()
            .is_some_and(|name| CategoryIdentifier::parse(name.as_str()).is_some())
        {
            return Err(EditCategoryError::ReservedName);
        }
        if builtin.is_none() && self.name.is_none() {
            return Err(EditCategoryError::MissingName);
        }
        Ok(())
    }
}

/// Create a user category, or return the original entity for an existing UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCategory {
    pub uuid: CategoryUuid,
    pub values: CategoryValues,
}

/// Replace all editable values of an existing category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCategory {
    pub uuid: CategoryUuid,
    pub values: CategoryValues,
}

/// Complete final sibling order, including archived children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReorderCategories {
    pub parent_uuid: Option<CategoryUuid>,
    pub uuids: Vec<CategoryUuid>,
}

/// Set one category's final archive state without cascading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveCategory {
    pub uuid: CategoryUuid,
    pub state: CategoryArchiveState,
}

/// Complete stored category metadata for editing and deterministic list traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryEntity {
    pub id: CategoryId,
    pub uuid: CategoryUuid,
    pub builtin_key: Option<BuiltinCategory>,
    pub values: CategoryValues,
    pub sort_order: i64,
    pub archive_state: CategoryArchiveState,
}

/// A rejected category edit; variants are independent of error message wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
#[snafu(module)]
pub enum EditCategoryError {
    #[snafu(display("category does not exist"))]
    NotFound,
    #[snafu(display("category name is blank"))]
    BlankName,
    #[snafu(display("category name exceeds 256 Unicode scalars"))]
    NameTooLong,
    #[snafu(display("category name uses a reserved canonical reference"))]
    ReservedName,
    #[snafu(display("user category requires a name"))]
    MissingName,
    #[snafu(display("category parent does not exist"))]
    InvalidParent,
    #[snafu(display("category hierarchy would contain a cycle"))]
    Cycle,
    #[snafu(display("client cannot create a builtin category identity"))]
    BuiltinIdentityViolation,
    #[snafu(display("reorder must contain every sibling exactly once"))]
    ReorderSetMismatch,
    #[snafu(display("category sibling order exceeds the supported range"))]
    OrderOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_reject_canonical_category_references() {
        for name in [
            "builtin:development",
            "builtin:future",
            "71f5a800-423a-4000-8000-000000000001",
            "71F5A800-423A-4000-8000-000000000001",
        ] {
            assert!(name.parse::<CategoryName>().is_err(), "{name}");
        }
    }

    #[test]
    fn names_trim_and_count_unicode_scalars() {
        let maximum = "🦀".repeat(MAX_CATEGORY_NAME_SCALARS);
        let name: CategoryName = format!(" \t{maximum}\n").parse().expect("maximum name");
        assert_eq!(name.as_str(), maximum);
        for value in ["", " \t\n", "\u{2003}"] {
            assert_eq!(
                value.parse::<CategoryName>(),
                Err(EditCategoryError::BlankName)
            );
        }
        assert_eq!(
            format!("{maximum}x").parse::<CategoryName>(),
            Err(EditCategoryError::NameTooLong)
        );
        // Combining marks count separately from the preceding character.
        assert!(
            "a\u{301}"
                .repeat(MAX_CATEGORY_NAME_SCALARS / 2)
                .parse::<CategoryName>()
                .is_ok()
        );
        assert_eq!(
            "a\u{301}"
                .repeat(MAX_CATEGORY_NAME_SCALARS)
                .parse::<CategoryName>(),
            Err(EditCategoryError::NameTooLong)
        );
        assert!(serde_json::from_value::<CategoryName>(serde_json::json!("  ")).is_err());
        assert_eq!(serde_json::to_value(name).expect("name JSON"), maximum);
    }

    #[test]
    fn tendencies_accept_only_the_three_exact_values_and_default_to_neutral() {
        assert_eq!(CategoryTendency::default(), CategoryTendency::Neutral);
        for value in ["focus", "neutral", "distraction"] {
            let tendency: CategoryTendency = value.parse().expect("valid tendency");
            assert_eq!(
                serde_json::to_value(tendency).expect("tendency JSON"),
                value
            );
            assert_eq!(
                serde_json::from_value::<CategoryTendency>(serde_json::json!(value))
                    .expect("tendency round trip"),
                tendency
            );
        }
        for value in ["", "Focus", "focus ", "other", "1"] {
            assert!(value.parse::<CategoryTendency>().is_err());
            assert!(serde_json::from_value::<CategoryTendency>(serde_json::json!(value)).is_err());
        }
    }

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
