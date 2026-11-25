//! Rule-content and historical-result comparison contracts (04 §6.3.3).

use crate::{
    CategoryRef, FurtherHidden, LocalDate, PreviewDeadlineMs, PreviewSampleLimit, QueryCursor,
    QueryVersions, RulePreviewMutation, RulePreviewSample, RulesTextPreviewMutation,
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

/// Strictly tagged comparison selection; drafts accept the real receipt-free write body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleComparison {
    /// Compares current results with a validated disposable mutation.
    Draft { mutation: DiffMutation },
    /// Compares own entries and evaluates each revision with its saved ancestors.
    Versions {
        key: String,
        before_version: NonZeroU32,
        after_version: NonZeroU32,
    },
    /// Applies one user revision to its saved and current parent chains.
    Upstream { key: String, version: NonZeroU32 },
}
/// Editor operations and text import share the same disposable draft path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiffMutation {
    /// Receipt-free rule_sets write.
    Editor(RulePreviewMutation),
    /// Receipt-free rules_text.import.
    Text(RulesTextPreviewMutation),
}
/// Read-only comparison over one to seven logical dates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesTextDiffRequest {
    pub comparison: RuleComparison,
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
impl RulesTextDiffRequest {
    /// Checks the same one-to-seven-day half-open range used by rule previews.
    ///
    /// # Errors
    /// Rejects empty, reversed, and longer-than-seven-day ranges.
    pub fn validate_dates(&self) -> Result<(), crate::ValidatePreviewDatesError> {
        super::preview::PreviewDates {
            start: self.start_date,
            end: self.end_date,
        }
        .validate()
    }
    /// Binds the exact comparison digest and evaluation inputs to continuation pages.
    pub fn cursor_parameters(&self, comparison_digest: String) -> crate::CursorQuery {
        crate::CursorQuery::RulesTextDiff {
            comparison_digest,
            start_date: self.start_date,
            end_date: self.end_date,
            expected_versions: self.expected_versions.clone(),
            deadline_ms: self.deadline_ms.unwrap_or_default(),
            sample_limit: self.sample_limit.unwrap_or_default(),
        }
    }
}
/// One qualified rule's canonical field changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDiffChange {
    pub rule: String,
    pub change: RuleDiffChangeKind,
    pub paths: Vec<String>,
}
/// Entry presence or canonical field change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleDiffChangeKind {
    /// The entry exists only on the after side.
    Added,
    /// The entry exists only on the before side.
    Removed,
    /// Both entries exist and their fields or source order differ.
    Changed,
}
/// Final active duration for a category on both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDiffCategoryDelta {
    pub category: CategoryRef,
    pub before_active_ms: i64,
    pub after_active_ms: i64,
}
/// Lists continue on subsequent pages; scalar summaries occur only on page one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDiffImpact {
    pub category_deltas: Vec<RuleDiffCategoryDelta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_changes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub further_hidden: Option<FurtherHidden>,
}
/// A canonical definition, replacement, or disable directive on one comparison side.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDiffEntry {
    pub entry_kind: crate::RuleEntryKind,
    pub content: crate::RuleEntryContent,
}
impl std::fmt::Debug for RuleDiffEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleDiffEntry")
            .field("entry_kind", &self.entry_kind)
            .finish_non_exhaustive()
    }
}
/// Canonical entries of a single member of a parent chain.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDiffSet {
    pub key: String,
    pub version: u32,
    pub entries: Vec<RuleDiffEntry>,
}
impl std::fmt::Debug for RuleDiffSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleDiffSet")
            .field("key", &self.key)
            .field("version", &self.version)
            .field("entries", &self.entries.len())
            .finish()
    }
}
/// Parent-first chains and the selected user overlay revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDiffSides {
    pub saved_parent: Vec<RuleDiffSet>,
    pub current_parent: Vec<RuleDiffSet>,
    pub overlay: Vec<RuleDiffSet>,
}
/// One completed comparison page; diff does not issue saving receipts.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesTextDiffResponse {
    pub changes: Vec<RuleDiffChange>,
    pub impact: RuleDiffImpact,
    pub samples: Vec<RulePreviewSample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sides: Option<RuleDiffSides>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
impl std::fmt::Debug for RulesTextDiffResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RulesTextDiffResponse")
            .field("changes", &self.changes.len())
            .field("samples", &self.samples.len())
            .field("versions", &self.versions)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comparisons_reject_unknown_nested_fields_and_receipts() {
        let valid = serde_json::json!({"comparison":{"kind":"draft","mutation":{"op":"rule_sets.edit","body":{"key":"user.test","base_version":0,"text":""}}},"start_date":"2025-01-02","end_date":"2025-01-03","expected_versions":"opaque"});
        assert!(serde_json::from_value::<RulesTextDiffRequest>(valid.clone()).is_ok());
        for path in ["comparison", "body"] {
            let mut invalid = valid.clone();
            if path == "comparison" {
                invalid["comparison"]["extra"] = true.into();
            } else {
                invalid["comparison"]["mutation"]["body"]["preview_receipt"] = "token".into();
            }
            assert!(serde_json::from_value::<RulesTextDiffRequest>(invalid).is_err());
        }
    }
    #[test]
    fn comparison_fixtures_preserve_shapes_and_accept_additive_response_fields() {
        use crate::CoreRequest;
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/rule-diff.json"))
                .expect("fixture");
        for name in ["draft", "versions", "upstream"] {
            let request: RulesTextDiffRequest =
                serde_json::from_value(fixture[name].clone()).expect("request");
            assert_eq!(
                serde_json::to_value(request).expect("request encoding"),
                fixture[name]
            );
        }
        for name in ["response", "continuation"] {
            let response: RulesTextDiffResponse =
                serde_json::from_value(fixture[name].clone()).expect("response");
            assert_eq!(
                serde_json::to_value(response).expect("response encoding"),
                fixture[name]
            );
        }
        let mut response = fixture["response"].clone();
        response["extra"] = true.into();
        response["impact"]["extra"] = true.into();
        response["changes"][0]["extra"] = true.into();
        response["sides"]["saved_parent"][0]["extra"] = true.into();
        serde_json::from_value::<RulesTextDiffResponse>(response).expect("additive fields");
        assert_eq!(RulesTextDiffRequest::OP, crate::Op::RulesTextDiff);
    }
    #[test]
    fn comparison_roles_and_capability_follow_the_read_only_text_contract() {
        assert!(crate::Role::Ui.authorize(crate::Op::RulesTextDiff).is_ok());
        assert!(crate::Role::Cli.authorize(crate::Op::RulesTextDiff).is_ok());
        assert!(
            crate::Role::Host
                .authorize(crate::Op::RulesTextDiff)
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(crate::Capability::RulesTextV1).expect("capability"),
            "rules_text.v1"
        );
    }
    #[test]
    fn comparisons_reject_unknown_fields_for_every_selection_and_invalid_revision_numbers() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/rule-diff.json"))
                .expect("fixture");
        for name in ["draft", "versions", "upstream"] {
            for pointer in ["", "/comparison"] {
                let mut request = fixture[name].clone();
                request.pointer_mut(pointer).expect("object")["extra"] = true.into();
                assert!(serde_json::from_value::<RulesTextDiffRequest>(request).is_err());
            }
        }
        for value in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(4294967296_u64),
        ] {
            let mut request = fixture["versions"].clone();
            request["comparison"]["before_version"] = value;
            assert!(serde_json::from_value::<RulesTextDiffRequest>(request).is_err());
        }
    }
    #[test]
    fn comparison_dates_deadlines_and_sample_limits_share_preview_bounds() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/rule-diff.json"))
                .expect("fixture");
        let mut request: RulesTextDiffRequest =
            serde_json::from_value(fixture["versions"].clone()).expect("request");
        request.validate_dates().expect("seven dates");
        for end in ["2026-11-15", "2026-11-16", "2026-11-24"] {
            request.end_date = end.parse().expect("date");
            assert!(request.validate_dates().is_err());
        }
        for field in ["deadline_ms", "sample_limit"] {
            let mut invalid = fixture["versions"].clone();
            invalid[field] = serde_json::json!(120001);
            assert!(serde_json::from_value::<RulesTextDiffRequest>(invalid).is_err());
        }
        let mut zero_samples = fixture["versions"].clone();
        zero_samples["sample_limit"] = serde_json::json!(0);
        serde_json::from_value::<RulesTextDiffRequest>(zero_samples).expect("statistics only");
    }
    #[test]
    fn entry_debug_prints_the_kind_without_rule_content() {
        let entry: RuleDiffEntry = serde_json::from_value(serde_json::json!({"entry_kind":"disable","content":{"target":"user.secret/rule","reason":"private-marker"}})).expect("entry");
        let debug = format!("{entry:?}");
        assert!(debug.contains("Disable"));
        assert!(!debug.contains("user.secret"));
        assert!(!debug.contains("private-marker"));
    }
}
