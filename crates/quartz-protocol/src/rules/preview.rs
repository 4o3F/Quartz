//! Disposable rule preview contracts (04 §6.3.2).

use crate::{
    ApplicationRank, CategoryRef, ClockSegmentId, DocumentRank, LocalDate, QueryCursor,
    QueryVersions, RuleAction, RuleWebAction, RulesTextValidateRequest, TitleExtraction,
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

/// Maximum number of half-open logical dates in one preview.
pub const PREVIEW_MAX_DAYS: i64 = 7;
/// Default and maximum number of samples selected before pagination.
pub const PREVIEW_MAX_SAMPLES: u32 = 50;
/// Default deadline measured from admission of each page.
pub const PREVIEW_DEFAULT_DEADLINE_MS: u32 = 30_000;
/// Maximum deadline for one page, including queued work.
pub const PREVIEW_MAX_DEADLINE_MS: u32 = 120_000;

/// Validated relative deadline in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct PreviewDeadlineMs(u32);
impl Default for PreviewDeadlineMs {
    fn default() -> Self {
        Self(PREVIEW_DEFAULT_DEADLINE_MS)
    }
}
impl PreviewDeadlineMs {
    pub fn get(self) -> u32 {
        self.0
    }
}
impl TryFrom<u32> for PreviewDeadlineMs {
    type Error = ParsePreviewDeadlineError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if (1..=PREVIEW_MAX_DEADLINE_MS).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ParsePreviewDeadlineError)
        }
    }
}
impl From<PreviewDeadlineMs> for u32 {
    fn from(value: PreviewDeadlineMs) -> Self {
        value.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
#[snafu(display("preview deadline must be between 1 and 120000 milliseconds"))]
pub struct ParsePreviewDeadlineError;

/// First-N sample count; zero requests statistics without samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct PreviewSampleLimit(u32);
impl Default for PreviewSampleLimit {
    fn default() -> Self {
        Self(PREVIEW_MAX_SAMPLES)
    }
}
impl PreviewSampleLimit {
    pub fn get(self) -> u32 {
        self.0
    }
}
impl TryFrom<u32> for PreviewSampleLimit {
    type Error = ParsePreviewSampleLimitError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value <= PREVIEW_MAX_SAMPLES {
            Ok(Self(value))
        } else {
            Err(ParsePreviewSampleLimitError)
        }
    }
}
impl From<PreviewSampleLimit> for u32 {
    fn from(value: PreviewSampleLimit) -> Self {
        value.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
#[snafu(display("preview sample limit must be between 0 and 50"))]
pub struct ParsePreviewSampleLimitError;

/// User-set enablement preview body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewSetEnabled {
    pub key: String,
    pub base_version: NonZeroU32,
    pub enabled: bool,
}
/// Childless user-set deletion preview body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewDelete {
    pub key: String,
    pub base_version: NonZeroU32,
}
/// Exact rule_sets write operation for preview.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "body", deny_unknown_fields)]
pub enum RulePreviewMutation {
    #[serde(rename = "rule_sets.edit")]
    Edit(RulesTextValidateRequest),
    #[serde(rename = "rule_sets.set_enabled")]
    SetEnabled(PreviewSetEnabled),
    #[serde(rename = "rule_sets.delete")]
    Delete(PreviewDelete),
}
impl std::fmt::Debug for RulePreviewMutation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let operation = match self {
            Self::Edit(_) => "rule_sets.edit",
            Self::SetEnabled(_) => "rule_sets.set_enabled",
            Self::Delete(_) => "rule_sets.delete",
        };
        formatter
            .debug_tuple("RulePreviewMutation")
            .field(&operation)
            .finish()
    }
}
/// Text previews accept only an import.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "body", deny_unknown_fields)]
pub enum RulesTextPreviewMutation {
    #[serde(rename = "rules_text.import")]
    Import(RulesTextValidateRequest),
}
impl std::fmt::Debug for RulesTextPreviewMutation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RulesTextPreviewMutation::Import([redacted])")
    }
}
/// Shared request envelope; each operation fixes its accepted mutation family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePreviewRequest<Mutation> {
    pub mutation: Mutation,
    pub start_date: LocalDate,
    pub end_date: LocalDate,
    pub expected_versions: QueryVersions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<PreviewDeadlineMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_limit: Option<PreviewSampleLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<QueryCursor>,
}
impl<Mutation> RulePreviewRequest<Mutation> {
    /// Checks the half-open logical date range before work is admitted.
    ///
    /// # Errors
    /// Rejects empty, reversed, or longer-than-seven-day ranges.
    pub fn validate_dates(&self) -> Result<(), ValidatePreviewDatesError> {
        PreviewDates {
            start: self.start_date,
            end: self.end_date,
        }
        .validate()
    }
}
pub(super) struct PreviewDates {
    pub start: LocalDate,
    pub end: LocalDate,
}
impl PreviewDates {
    pub(super) fn validate(&self) -> Result<(), ValidatePreviewDatesError> {
        let mut date = self.start;
        for _ in 0..PREVIEW_MAX_DAYS {
            if date >= self.end {
                break;
            }
            date = date.tomorrow().map_err(|_| ValidatePreviewDatesError)?;
        }
        if self.start < self.end && date == self.end {
            Ok(())
        } else {
            Err(ValidatePreviewDatesError)
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
#[snafu(display("preview requires between one and seven logical days"))]
pub struct ValidatePreviewDatesError;
/// UI-only preview of a rule_sets write.
pub type RulesPreviewRequest = RulePreviewRequest<RulePreviewMutation>;
/// UI and CLI preview of a rules_text.import.
pub type RulesTextPreviewRequest = RulePreviewRequest<RulesTextPreviewMutation>;

impl RulesPreviewRequest {
    /// Binds the core-computed digest of the exact mutation bytes to pagination.
    pub fn cursor_parameters(&self, mutation_digest: String) -> crate::CursorQuery {
        crate::CursorQuery::RulesPreview {
            mutation_digest,
            start_date: self.start_date,
            end_date: self.end_date,
            expected_versions: self.expected_versions.clone(),
            deadline_ms: self.deadline_ms.unwrap_or_default(),
            sample_limit: self.sample_limit.unwrap_or_default(),
        }
    }
}
impl RulesTextPreviewRequest {
    /// Binds import inputs independently of the UI-only editor operation.
    pub fn cursor_parameters(&self, mutation_digest: String) -> crate::CursorQuery {
        crate::CursorQuery::RulesTextPreview {
            mutation_digest,
            start_date: self.start_date,
            end_date: self.end_date,
            expected_versions: self.expected_versions.clone(),
            deadline_ms: self.deadline_ms.unwrap_or_default(),
            sample_limit: self.sample_limit.unwrap_or_default(),
        }
    }
}

/// Condition matches include idle and shadowed retained fact slices.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleConditionMatch {
    pub segments: u64,
    pub duration_ms: i64,
}
/// Final rule contribution after activity and annotation evaluation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleEffectiveContribution {
    pub segments: u64,
    pub active_ms: i64,
}
/// Additional downgrades of retained facts, including inactive slices.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FurtherHidden {
    pub segments: u64,
    pub fields: u64,
    pub duration_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulePreviewStatistics {
    pub rule: String,
    pub condition_match: RuleConditionMatch,
    pub effective_contribution: RuleEffectiveContribution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub further_hidden: Option<FurtherHidden>,
}
/// Category ranking duration with the query.day category reference shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewCategoryRank {
    pub category: CategoryRef,
    pub active_ms: i64,
}
/// Rankings continue across pages; the scalar total is present on page one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulePreviewSideTotals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_ms: Option<i64>,
    pub categories: Vec<PreviewCategoryRank>,
    pub applications: Vec<ApplicationRank>,
    pub documents: Vec<DocumentRank>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulePreviewTotals {
    pub before: RulePreviewSideTotals,
    pub after: RulePreviewSideTotals,
}
/// Kind-specific sample values after applying both sides' privacy restrictions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RulePreviewOutput {
    Classification {
        category: CategoryRef,
    },
    Extraction {
        extraction: TitleExtraction,
    },
    Privacy {
        app: RuleAction,
        window_title: RuleAction,
        web: RuleWebAction,
    },
}
/// Samples are selected globally before pagination, in fact/rule order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulePreviewSample {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub clock_segment_id: ClockSegmentId,
    pub rule: String,
    pub before: RulePreviewOutput,
    pub after: RulePreviewOutput,
}
/// Completed preview page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesPreviewResponse {
    pub rules: Vec<RulePreviewStatistics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<RulePreviewTotals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub further_hidden: Option<FurtherHidden>,
    pub samples: Vec<RulePreviewSample>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Text and editor previews return the same result shape.
pub type RulesTextPreviewResponse = RulesPreviewResponse;

#[cfg(test)]
mod tests;
