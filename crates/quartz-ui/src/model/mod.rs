//! View models mapped from core responses or preview fixtures. They do not depend on GPUI,
//! the database, or rule evaluation.

mod application;
mod category;
mod day;
mod details;
pub(crate) mod live_details;
mod palette;
mod ranking;
mod rule;
pub(crate) mod rules;
mod segment;
mod state;
mod summary;
mod time;
mod track;

pub(crate) use application::{AppId, AppInfo, Glyph};
pub(crate) use category::{BuiltinCategory, Category, CategoryId, CategoryName, Tendency};
#[cfg(windows)]
pub(crate) use day::PendingAwaySpan;
pub(crate) use day::{Day, DaySource};
#[cfg(any(test, feature = "preview"))]
pub(crate) use day::{DayParts, EARLIER_DAYS, EarlierDays};
pub(crate) use details::DetailsSubject;
#[cfg(windows)]
pub(crate) use details::{BarDay, Details, IntervalCard};
#[cfg(windows)]
pub(crate) use palette::PaletteSlotMap;
pub(crate) use palette::{CategoryColor, PaletteSlot};
#[cfg(windows)]
pub(crate) use ranking::RankKey;
pub(crate) use ranking::{RankRow, RankingKind};
#[cfg(windows)]
pub(crate) use rule::Explanation;
pub(crate) use rule::RuleCondition;
#[cfg(any(test, feature = "preview"))]
pub(crate) use rule::{Rule, RuleField, RuleId, RuleOperator};
#[cfg(windows)]
pub(crate) use rules::{
    ArchiveConfirmation, ArchivedVisibility, CategoryEditorDraft, CategoryEditorMode, CategoryItem,
    CategoryMutation, CategoryOrigin, DeleteConfirmation, FlatTreeRow, LoadState, MutationActivity,
    NodeEnablement, ReorderDirection, RuleEntryDetail, RuleSetDetail, RuleSetEnablement, RuleTree,
    RuleTreeNode, RulesEffect, RulesKeyAction, RulesMutation, RulesPage, RulesTab, TreeDirection,
    ValidateCategoryNameError,
};
pub(crate) use rules::{
    CategoryCatalog, ConditionItem, ConditionPolarity, EntryConditionSummary, PrivacyEffectSummary,
    ResolvedCategory, RuleEntryLabel,
};
pub(crate) use segment::{ClassificationSource, Segment, SegmentId};
pub(crate) use state::{AwayLabel, ClockAnomaly, StateBlock, StateKind};
pub(crate) use summary::{DaySummary, HourlyBar, Share};
pub(crate) use time::{
    MILLIS_PER_SECOND, MINUTES_PER_HOUR, SECONDS_PER_MINUTE, TimeOfDay, TimeSpan,
};
#[cfg(windows)]
pub(crate) use track::Lane;
pub(crate) use track::MainBlock;

#[cfg(test)]
pub(crate) mod fixture;
