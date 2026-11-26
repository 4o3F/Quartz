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
pub(crate) mod rule_editor;
pub(crate) mod rule_import;
pub(crate) mod rules;
mod segment;
mod state;
mod summary;
mod time;
mod track;
pub(crate) mod unclassified;

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
pub(crate) use rule_editor::DiagnosticCodeKind;
#[cfg(windows)]
pub(crate) use rule_editor::{
    ClassificationRulePrefill, DirtyState, EditorLoadState, LeaveConfirmation, LeaveDecision,
    LeaveTarget, PreviewState, RuleEditor, RuleEditorEffect, RuleEditorMode, RuleEditorSession,
    SaveOutcome, StaleNotice, VALIDATE_DEBOUNCE_DELAY, ValidateRuleSetKeyError, ValidationState,
    valid_parents_for_kind,
};
#[cfg(windows)]
pub(crate) use rule_import::{
    ExtractImportKeyError, ImportAction, ImportOutcome, ImportTarget, RuleImport, RuleImportEffect,
    RuleImportSession,
};
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
pub(crate) use unclassified::UnclassifiedEntityLabel;
#[cfg(windows)]
pub(crate) use unclassified::{
    UNCLASSIFIED_DEBOUNCE_DELAY, UnclassifiedEffect, UnclassifiedEntityKind, UnclassifiedList,
    UnclassifiedQueue, UnclassifiedRow, ValidateUnclassifiedSearchError,
    unclassified_last_seven_days,
};

#[cfg(test)]
pub(crate) mod fixture;
