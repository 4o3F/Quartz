//! Plain view models the UI reads. A protocol mapping fills these later; nothing here depends on
//! GPUI, the database, or rule evaluation.

mod summary;
mod time;

use std::time::Duration;

pub use summary::{CategoryTotal, DaySummary, HourlyBar, Share};
pub use time::{TimeOfDay, TimeSpan};

/// One of the 14 categorical palette slots of 11 section 3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaletteSlot {
    Cat1,
    Cat2,
    Cat3,
    Cat4,
    Cat5,
    Cat6,
    Cat7,
    Cat8,
    Cat9,
    Cat10,
    Cat11,
    Cat12,
    Cat13,
    Cat14,
}

impl PaletteSlot {
    /// Position of the slot in the palette, starting at zero.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// How a category is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryColor {
    Palette(PaletteSlot),
    /// The neutral status color, used by the unclassified category.
    Neutral,
}

/// Whether time in a category counts toward focus or distraction share (07 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tendency {
    Focus,
    #[default]
    Neutral,
    Distraction,
}

/// Built-in categories whose names are translated (10 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinCategory {
    Development,
    Learning,
    Communication,
    Entertainment,
    System,
    Uncategorized,
}

/// Display name of a category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CategoryName {
    Builtin(BuiltinCategory),
    /// User text shown as is.
    Custom(String),
}

/// Identifies a category within one [`Day`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CategoryId(pub u16);

/// A category with its color and tendency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: CategoryId,
    pub name: CategoryName,
    pub color: CategoryColor,
    pub tendency: Tendency,
}

pub(crate) static FALLBACK_CATEGORY: Category = Category {
    id: CategoryId(u16::MAX),
    name: CategoryName::Builtin(BuiltinCategory::Uncategorized),
    color: CategoryColor::Neutral,
    tendency: Tendency::Neutral,
};

/// Placeholder glyph standing in for an application icon (11 section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Editor,
    Browser,
    Chat,
    Messenger,
    Terminal,
    Document,
    Website,
    Generic,
}

/// Identifies an application within one [`Day`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AppId(pub u16);

/// A tracked application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub id: AppId,
    pub name: String,
    /// Short name used where space is tight, such as swimlane labels.
    pub short_name: String,
    pub exe: String,
    pub glyph: Glyph,
}

pub(crate) static FALLBACK_APP: AppInfo = AppInfo {
    id: AppId(u16::MAX),
    name: String::new(),
    short_name: String::new(),
    exe: String::new(),
    glyph: Glyph::Generic,
};

/// Why a segment has its category (07 section 4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassificationSource {
    /// A matched rule, shown as its condition text.
    Rule {
        condition: String,
    },
    Manual,
    /// No rule matched; the segment is unclassified.
    Unmatched,
}

/// Identifies a foreground segment within one [`Day`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SegmentId(pub u32);

/// One foreground fact: an application in front for a span of time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub id: SegmentId,
    pub span: TimeSpan,
    pub app: AppId,
    pub category: CategoryId,
    pub title: String,
    pub domain: Option<String>,
    pub document: Option<String>,
    pub source: ClassificationSource,
    /// Raw intervals merged into this segment for display (07 section 4.2).
    pub merged_count: u32,
}

/// Non-foreground states drawn only on the category main track (07 section 4.1, 11 section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateKind {
    Idle,
    Locked,
    Sleep,
    CoreStopped,
    NoForeground,
    Excluded,
    CollectionGap,
}

impl StateKind {
    /// Whether the state is drawn as plain subtle fill without text.
    #[must_use]
    pub const fn is_quiet(self) -> bool {
        matches!(self, Self::Locked | Self::Sleep | Self::CoreStopped)
    }
}

/// A non-foreground span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateBlock {
    pub span: TimeSpan,
    pub kind: StateKind,
    /// Category of the preceding activity; idle spans are drawn in its color.
    pub category: Option<CategoryId>,
    /// Idle longer than 15 minutes that still needs a label (07 section 3).
    pub pending_away: bool,
    /// Set for ranges with a clock anomaly (02 section 5).
    pub clock_anomaly: bool,
}

/// A calendar date with its weekday.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarDate {
    pub month: u8,
    pub day: u8,
    pub weekday: Weekday,
}

/// Day of week, Monday first (10 section 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    const ALL: [Self; 7] = [
        Self::Mon,
        Self::Tue,
        Self::Wed,
        Self::Thu,
        Self::Fri,
        Self::Sat,
        Self::Sun,
    ];

    /// The weekday `days` before this one.
    #[must_use]
    pub fn back(self, days: usize) -> Self {
        Self::ALL[(self as usize + 7 - days % 7) % 7]
    }
}

/// Everything the Today page shows for one logical day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    pub date: CalendarDate,
    pub categories: Vec<Category>,
    pub apps: Vec<AppInfo>,
    pub segments: Vec<Segment>,
    pub states: Vec<StateBlock>,
    pub week_totals: Vec<AppWeek>,
    pub explanations: Vec<AppExplanation>,
}

/// Active time of one application for the six days before the day shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppWeek {
    pub app: AppId,
    pub previous_days: [Duration; 6],
}

/// Why an application got its category, as the explain tree shows (07 section 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppExplanation {
    pub app: AppId,
    pub rule_set: String,
    pub rule_name: String,
    pub priority: u32,
    pub field: String,
    pub operator: String,
    pub expected: String,
    pub actual: String,
}

impl Day {
    /// Looks up a category, falling back to "unclassified".
    #[must_use]
    pub fn category(&self, id: CategoryId) -> &Category {
        self.categories
            .iter()
            .find(|category| category.id == id)
            .unwrap_or(&FALLBACK_CATEGORY)
    }

    /// Looks up an application, falling back to an empty placeholder.
    #[must_use]
    pub fn app(&self, id: AppId) -> &AppInfo {
        self.apps
            .iter()
            .find(|app| app.id == id)
            .unwrap_or(&FALLBACK_APP)
    }

    /// Looks up a segment.
    #[must_use]
    pub fn segment(&self, id: SegmentId) -> Option<&Segment> {
        self.segments.iter().find(|segment| segment.id == id)
    }
}

/// One entry of the category main track after merging same-category neighbors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainBlock {
    Active {
        span: TimeSpan,
        category: CategoryId,
    },
    State(StateBlock),
}

impl MainBlock {
    /// Time covered by the block.
    #[must_use]
    pub fn span(&self) -> TimeSpan {
        match self {
            Self::Active { span, .. } => *span,
            Self::State(block) => block.span,
        }
    }
}

/// One application swimlane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    pub app: AppId,
    pub segments: Vec<SegmentId>,
}

/// What the details sidebar is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsSubject {
    Segment(SegmentId),
    App(AppId),
}

/// Ranking tabs of 07 section 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RankingKind {
    #[default]
    Apps,
    Domains,
    Documents,
    Mixed,
}

/// One ranking row with its per-category parts for the share bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankRow {
    pub name: String,
    pub glyph: Glyph,
    pub total: Duration,
    pub parts: Vec<(CategoryId, Duration)>,
    pub subject: DetailsSubject,
}

/// The selected-interval card of the details sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntervalCard {
    pub span: TimeSpan,
    pub title: String,
    pub category: CategoryId,
    pub source: ClassificationSource,
    pub merged_count: u32,
}

/// One bar of the 7-day chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayBar {
    pub weekday: Weekday,
    pub is_today: bool,
    pub total: Duration,
}

/// A common title row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleRow {
    pub title: String,
    pub total: Duration,
}

/// Content of the details sidebar (07 section 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
    pub app: AppInfo,
    pub category: CategoryId,
    pub interval: Option<IntervalCard>,
    pub week: Vec<DayBar>,
    pub titles: Vec<TitleRow>,
    pub explanation: Option<AppExplanation>,
}
