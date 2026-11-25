//! Explanations of retained facts using the current rule snapshot.
use crate::{
    AnnotationId, CategoryRef, ClockSegmentId, CursorQuery, PreviewDeadlineMs, QueryCursor,
    QueryVersions, RuleField, RuleKind, RuleProvenance, TitleExtraction,
};
use serde::{Deserialize, Serialize};

/// Strict alternatives keep instant and range selectors mutually exclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RulesExplainRequest {
    Instant(ExplainInstant),
    Range(ExplainRange),
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplainInstant {
    pub at_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_segment_id: Option<ClockSegmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<QueryCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_versions: Option<QueryVersions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<PreviewDeadlineMs>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplainRange {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_segment_id: Option<ClockSegmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<QueryCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_versions: Option<QueryVersions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<PreviewDeadlineMs>,
}
impl std::fmt::Debug for ExplainInstant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExplainInstant")
            .field("at_utc_ms", &self.at_utc_ms)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for ExplainRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExplainRange")
            .field("start_utc_ms", &self.start_utc_ms)
            .field("end_utc_ms", &self.end_utc_ms)
            .finish_non_exhaustive()
    }
}
/// Selection stored in a continuation, without request control fields or a nested cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExplainSelection {
    Instant { at_utc_ms: i64 },
    Range { start_utc_ms: i64, end_utc_ms: i64 },
}
impl RulesExplainRequest {
    pub fn parameters(&self) -> CursorQuery {
        match self {
            Self::Instant(value) => CursorQuery::RulesExplain {
                selection: ExplainSelection::Instant {
                    at_utc_ms: value.at_utc_ms,
                },
                clock_segment_id: value.clock_segment_id,
                expected_versions: value.expected_versions.clone(),
                deadline_ms: value.deadline_ms.unwrap_or_default(),
            },
            Self::Range(value) => CursorQuery::RulesExplain {
                selection: ExplainSelection::Range {
                    start_utc_ms: value.start_utc_ms,
                    end_utc_ms: value.end_utc_ms,
                },
                clock_segment_id: value.clock_segment_id,
                expected_versions: value.expected_versions.clone(),
                deadline_ms: value.deadline_ms.unwrap_or_default(),
            },
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainNodeKind {
    Set,
    Rule,
    Condition,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainState {
    Effective,
    Shadowed,
    Disabled,
    Overridden,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExplainReason {
    DidNotMatch,
    Selected,
    Shadowed {
        by_rule: String,
        order: ExplainShadowOrder,
    },
    SetDisabled,
    Disabled {
        by_set: String,
    },
    Overridden {
        by_set: String,
    },
    Annotation {
        annotation: AnnotationId,
    },
    TargetMissing,
    StageUnavailable,
    CategoryUnavailable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainShadowOrder {
    Priority,
    Depth,
    Order,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainSet {
    pub key: String,
    pub version: u32,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainNode {
    pub path: Vec<usize>,
    pub set: Option<ExplainSet>,
    pub kind: ExplainNodeKind,
    pub rule: Option<String>,
    pub condition_index: Option<usize>,
    pub field: Option<RuleField>,
    pub value: Option<String>,
    pub missing: Option<bool>,
    pub matched: Option<bool>,
    pub state: ExplainState,
    pub stage: RuleKind,
    pub reason: Option<ExplainReason>,
    pub provenance: Vec<RuleProvenance>,
    pub pattern_matched: Option<bool>,
}
impl std::fmt::Debug for ExplainNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExplainNode")
            .field("path", &self.path)
            .field("kind", &self.kind)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExplainCategorySource {
    Rule {
        rule: String,
    },
    Annotation {
        annotation: AnnotationId,
        otherwise_rule: Option<String>,
    },
    Unclassified,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainPrivacy {
    pub rule: Option<String>,
    pub label_key: Option<String>,
    pub label: Option<String>,
}
impl std::fmt::Debug for ExplainPrivacy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExplainPrivacy { .. }")
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainFragment {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_segment_id: Option<ClockSegmentId>,
    pub category: CategoryRef,
    pub category_source: ExplainCategorySource,
    pub extraction: Option<TitleExtraction>,
    pub privacy: Option<ExplainPrivacy>,
    pub nodes: Vec<ExplainNode>,
}
impl std::fmt::Debug for ExplainFragment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExplainFragment")
            .field("start_utc_ms", &self.start_utc_ms)
            .field("end_utc_ms", &self.end_utc_ms)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesExplainResponse {
    pub fragments: Vec<ExplainFragment>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CoreRequest;
    use serde_json::{Value, json};
    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/rule-explain.json"))
            .expect("fixture")
    }
    #[test]
    fn instant_and_range_selectors_are_strict_and_bind_the_explanation_operation() {
        let fixture = fixture();
        for name in ["instant", "range"] {
            let request: RulesExplainRequest =
                serde_json::from_value(fixture[name].clone()).expect("request");
            assert_eq!(RulesExplainRequest::OP, crate::Op::RulesExplain);
            assert_eq!(
                RulesExplainRequest::response(fixture["response"].clone())
                    .expect("response")
                    .fragments
                    .len(),
                1
            );
            let mut invalid = fixture[name].clone();
            invalid["extra"] = true.into();
            assert!(serde_json::from_value::<RulesExplainRequest>(invalid).is_err());
            let mut invalid = fixture[name].clone();
            invalid["at_utc_ms"] = 1.into();
            invalid["start_utc_ms"] = 0.into();
            invalid["end_utc_ms"] = 2.into();
            assert!(serde_json::from_value::<RulesExplainRequest>(invalid).is_err());
            let mut invalid = fixture[name].clone();
            invalid["cursor"] = json!([]);
            assert!(serde_json::from_value::<RulesExplainRequest>(invalid).is_err());
            let mut invalid = fixture[name].clone();
            invalid["deadline_ms"] = json!(0);
            assert!(serde_json::from_value::<RulesExplainRequest>(invalid).is_err());
            let cursor = QueryCursor::new(QueryVersions("v".into()), request.parameters(), 1);
            let mut continuation = request.body().expect("body");
            continuation["cursor"] = serde_json::to_value(cursor).expect("cursor");
            let continued: RulesExplainRequest =
                serde_json::from_value(continuation).expect("continuation");
            assert_eq!(continued.parameters(), request.parameters());
        }
    }
    #[test]
    fn explanation_debug_redacts_values_extraction_labels_and_category_names() {
        let mut response: RulesExplainResponse =
            serde_json::from_value(fixture()["response"].clone()).expect("response");
        response.fragments[0].extraction = Some(TitleExtraction {
            document: Some("retained-private-document".into()),
            normalized_title: Some("retained-private-title".into()),
            ..Default::default()
        });
        response.fragments[0].category = serde_json::from_value(json!({"kind":"classified","id":1,"uuid":"00000000-0000-4000-8000-000000000001","builtin_key":null,"name":"retained-private-category","color":"cat.1","tendency":"neutral"})).expect("category");
        for debug in [
            format!("{response:?}"),
            format!("{:?}", response.fragments),
            format!("{:?}", response.fragments[0].nodes),
            format!("{:?}", response.fragments[0].privacy),
        ] {
            assert!(!debug.contains("retained-private-value"));
            assert!(!debug.contains("retained-private-label"));
            assert!(!debug.contains("retained-private-document"));
            assert!(!debug.contains("retained-private-title"));
            assert!(!debug.contains("retained-private-category"));
        }
    }
    #[test]
    fn explanation_roles_and_capability_allow_ui_and_read_only_cli() {
        assert!(crate::Role::Ui.authorize(crate::Op::RulesExplain).is_ok());
        assert!(crate::Role::Cli.authorize(crate::Op::RulesExplain).is_ok());
        assert!(
            crate::Role::Host
                .authorize(crate::Op::RulesExplain)
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(crate::Capability::RulesExplainV1).expect("capability"),
            "rules.explain.v1"
        );
    }
    #[test]
    fn explanation_responses_accept_additive_fields() {
        let mut response = fixture()["response"].clone();
        response["future"] = true.into();
        response["fragments"][0]["future"] = true.into();
        response["fragments"][0]["nodes"][0]["future"] = true.into();
        RulesExplainRequest::response(response).expect("additive response fields");
    }
    #[test]
    fn cursor_parameters_are_nonrecursive_and_normalize_default_deadlines() {
        let fixture = fixture();
        let request: RulesExplainRequest =
            serde_json::from_value(fixture["instant"].clone()).expect("request");
        assert_eq!(
            serde_json::to_value(request.parameters()).expect("parameters"),
            fixture["cursor_query"]
        );
        assert_eq!(
            request.body().expect("optional fields omitted"),
            fixture["instant"]
        );
        let mut explicit = fixture["instant"].clone();
        explicit["deadline_ms"] = crate::PREVIEW_DEFAULT_DEADLINE_MS.into();
        let explicit: RulesExplainRequest =
            serde_json::from_value(explicit).expect("explicit deadline");
        assert_eq!(request.parameters(), explicit.parameters());
        let wire = serde_json::to_string(&request.parameters()).expect("JSON");
        assert!(!wire.contains("cursor"));
    }
}
