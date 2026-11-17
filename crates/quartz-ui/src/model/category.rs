//! Categories: name, palette color and focus tendency.

use super::palette::{CategoryColor, PaletteSlot};

/// Whether time in a category counts toward focus or distraction share (07 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum Tendency {
    Focus,
    #[default]
    Neutral,
    Distraction,
}

/// Built-in categories, whose names are translated (03 section 5.3, 10 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BuiltinCategory {
    Development,
    Learning,
    Communication,
    Entertainment,
    System,
    Uncategorized,
}

impl BuiltinCategory {
    /// Default color of the built-in category (11 section 3.3).
    #[must_use]
    pub(crate) const fn default_color(self) -> CategoryColor {
        match self {
            Self::Development => CategoryColor::Palette(PaletteSlot::numbered(8)),
            Self::Learning => CategoryColor::Palette(PaletteSlot::numbered(1)),
            Self::Communication => CategoryColor::Palette(PaletteSlot::numbered(3)),
            Self::Entertainment => CategoryColor::Palette(PaletteSlot::numbered(13)),
            Self::System => CategoryColor::Palette(PaletteSlot::numbered(10)),
            Self::Uncategorized => CategoryColor::Neutral,
        }
    }

    /// Default tendency of the built-in category; users can change it (07 section 6).
    #[must_use]
    pub(crate) const fn default_tendency(self) -> Tendency {
        match self {
            Self::Development | Self::Learning => Tendency::Focus,
            Self::Entertainment => Tendency::Distraction,
            Self::Communication | Self::System | Self::Uncategorized => Tendency::Neutral,
        }
    }
}

/// Display name of a category.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum CategoryName {
    Builtin(BuiltinCategory),
    /// User text shown as is.
    Custom(String),
}

/// Identifies a category within one day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CategoryId(u16);

impl CategoryId {
    /// Wraps a category id from the data source.
    #[must_use]
    pub(crate) const fn new(id: u16) -> Self {
        Self(id)
    }
}

/// A category with its color and tendency.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Category {
    pub(crate) id: CategoryId,
    pub(crate) name: CategoryName,
    pub(crate) color: CategoryColor,
    pub(crate) tendency: Tendency,
}

impl Category {
    /// A built-in category with its default color and tendency.
    #[must_use]
    pub(crate) const fn builtin(id: CategoryId, kind: BuiltinCategory) -> Self {
        Self {
            id,
            name: CategoryName::Builtin(kind),
            color: kind.default_color(),
            tendency: kind.default_tendency(),
        }
    }

    /// A user category.
    #[must_use]
    pub(crate) const fn custom(
        id: CategoryId,
        name: String,
        color: CategoryColor,
        tendency: Tendency,
    ) -> Self {
        Self {
            id,
            name: CategoryName::Custom(name),
            color,
            tendency,
        }
    }

    /// Whether this is the category of time no rule matched.
    #[must_use]
    pub(crate) fn is_uncategorized(&self) -> bool {
        self.name == CategoryName::Builtin(BuiltinCategory::Uncategorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_categories_use_the_documented_defaults() {
        let development = Category::builtin(CategoryId::new(1), BuiltinCategory::Development);
        assert_eq!(
            development.color,
            CategoryColor::Palette(PaletteSlot::numbered(8))
        );
        assert_eq!(development.tendency, Tendency::Focus);
        let uncategorized = Category::builtin(CategoryId::new(2), BuiltinCategory::Uncategorized);
        assert_eq!(uncategorized.color, CategoryColor::Neutral);
        assert!(uncategorized.is_uncategorized());
        assert!(!development.is_uncategorized());
    }
}
