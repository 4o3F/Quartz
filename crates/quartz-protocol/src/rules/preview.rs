//! Bounded disposable rule preview contracts (04 §6.3.2).

use crate::{
    CategoryRef, ClockSegmentId, LocalDate, RuleAction, RuleSetsDeleteRequest, RuleSetsEditRequest,
    RuleSetsSetEnabledRequest, RuleWebAction, TitleExtraction,
};
use serde::{Deserialize, Serialize};

/// Maximum number of half-open logical dates in one preview.
pub const PREVIEW_MAX_DAYS: i64 = 7;
/// Fixed upper bound on redacted samples returned by one preview.
pub const PREVIEW_MAX_SAMPLES: usize = 50;

/// Exact rule-set write or text import operation evaluated by `rules.preview`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "body", deny_unknown_fields)]
pub enum RulePreviewMutation {
    #[serde(rename = "rule_sets.edit")]
    Edit(RuleSetsEditRequest),
    #[serde(rename = "rules_text.import")]
    Import(RuleSetsEditRequest),
    #[serde(rename = "rule_sets.set_enabled")]
    SetEnabled(RuleSetsSetEnabledRequest),
    #[serde(rename = "rule_sets.delete")]
    Delete(RuleSetsDeleteRequest),
}
impl std::fmt::Debug for RulePreviewMutation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let operation = match self {
            Self::Edit(_) => "rule_sets.edit",
            Self::Import(_) => "rules_text.import",
            Self::SetEnabled(_) => "rule_sets.set_enabled",
            Self::Delete(_) => "rule_sets.delete",
        };
        formatter
            .debug_tuple("RulePreviewMutation")
            .field(&operation)
            .finish()
    }
}

/// UI-only bounded preview request over one to seven logical days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesPreviewRequest {
    pub mutation: RulePreviewMutation,
    pub start_date: LocalDate,
    pub end_date: LocalDate,
}
impl RulesPreviewRequest {
    /// Checks the half-open logical date range before work is admitted.
    ///
    /// # Errors
    /// Rejects empty, reversed, or longer-than-seven-day ranges.
    pub fn validate_dates(&self) -> Result<(), ValidatePreviewDatesError> {
        let mut date = self.start_date;
        for _ in 0..PREVIEW_MAX_DAYS {
            if date >= self.end_date {
                break;
            }
            date = date.tomorrow().map_err(|_| ValidatePreviewDatesError)?;
        }
        if self.start_date < self.end_date && date == self.end_date {
            Ok(())
        } else {
            Err(ValidatePreviewDatesError)
        }
    }
}

/// The requested preview date range is empty, reversed, or exceeds seven logical days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
#[snafu(display("preview requires between one and seven logical days"))]
pub struct ValidatePreviewDatesError;

/// Per-rule condition matches and final active contribution over the preview range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulePreviewStatistics {
    pub rule: String,
    pub matched_count: u64,
    pub matched_ms: i64,
    pub effective_ms: i64,
}

/// Category ranking duration with the `query.day` category reference shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewCategoryRank {
    pub category: CategoryRef,
    pub active_ms: i64,
}

/// Active total and category breakdown for one side of the preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulePreviewSideTotals {
    pub active_ms: i64,
    pub categories: Vec<PreviewCategoryRank>,
}

/// Before-and-after category totals matching `query.day` semantics.
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

/// Redacted sample ordered by start timestamp, clock segment, fact identity, and rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulePreviewSample {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub clock_segment_id: ClockSegmentId,
    pub rule: String,
    pub before: RulePreviewOutput,
    pub after: RulePreviewOutput,
}

/// Single-frame preview response; samples are truncated first if needed to fit the frame limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesPreviewResponse {
    pub rules: Vec<RulePreviewStatistics>,
    pub totals: RulePreviewTotals,
    pub document_change_count: u64,
    pub further_hidden_count: u64,
    pub samples: Vec<RulePreviewSample>,
    pub samples_truncated: bool,
}

#[cfg(test)]
mod tests;
