//! Stable category metadata shared by storage and query projections.

use serde::{Deserialize, Serialize};

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
    pub const fn color(self) -> &'static str {
        match self {
            Self::Development => "cat.8",
            Self::Learning => "cat.1",
            Self::Communication => "cat.3",
            Self::Entertainment => "cat.13",
            Self::System => "cat.10",
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
    pub color: String,
    pub tendency: CategoryTendency,
}
