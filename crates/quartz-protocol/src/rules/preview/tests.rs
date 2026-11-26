use super::*;
use crate::{Capability, CoreRequest, Op, QueryCursor, Role};
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/rule-preview.json"))
        .expect("preview fixture")
}
fn round_trip<T: serde::de::DeserializeOwned + Serialize>(value: Value) {
    let decoded: T = serde_json::from_value(value.clone()).expect("fixture decodes");
    assert_eq!(
        serde_json::to_value(decoded).expect("fixture encodes"),
        value
    );
}
#[test]
fn preview_requests_and_responses_preserve_wire_shapes() {
    let fixture = fixture();
    round_trip::<RulesPreviewRequest>(fixture["editor"].clone());
    round_trip::<RulesTextPreviewRequest>(fixture["text"].clone());
    round_trip::<RulesPreviewResponse>(fixture["response"].clone());
    round_trip::<RulesTextPreviewResponse>(fixture["continuation"].clone());
    assert_eq!(RulesPreviewRequest::OP, Op::RulesPreview);
    assert_eq!(RulesTextPreviewRequest::OP, Op::RulesTextPreview);
    for output in [
        json!({"category":{"kind":"unclassified"}}),
        json!({"extraction":{"document":null,"project":null,"profile":null,"dirty":null,"normalized_title":null,"extractor_rule":null,"status":null}}),
        json!({"app":"drop","window_title":"drop","web":"none"}),
    ] {
        round_trip::<RulePreviewOutput>(output);
    }
}
#[test]
fn preview_requests_reject_unknown_fields_at_every_mutation_level() {
    for pointer in ["", "/mutation", "/mutation/body"] {
        let mut request = fixture()["editor"].clone();
        request.pointer_mut(pointer).expect("object")["extra"] = json!(true);
        assert!(serde_json::from_value::<RulesPreviewRequest>(request).is_err());
        let mut request = fixture()["text"].clone();
        request.pointer_mut(pointer).expect("object")["extra"] = json!(true);
        assert!(serde_json::from_value::<RulesTextPreviewRequest>(request).is_err());
    }
    assert!(serde_json::from_value::<RulesPreviewRequest>(fixture()["text"].clone()).is_err());
    assert!(
        serde_json::from_value::<RulesTextPreviewRequest>(fixture()["editor"].clone()).is_err()
    );
    let duplicate =
        r#"{"op":"rule_sets.delete","body":{"key":"user.x","base_version":1,"key":"user.y"}}"#;
    assert!(serde_json::from_str::<RulePreviewMutation>(duplicate).is_err());
}
#[test]
fn preview_accepts_all_rule_set_write_shapes() {
    for mutation in [
        json!({"op":"rule_sets.edit","body":{"key":"user.x","base_version":0,"text":"text"}}),
        json!({"op":"rule_sets.set_enabled","body":{"key":"user.x","base_version":1,"enabled":false}}),
        json!({"op":"rule_sets.delete","body":{"key":"user.x","base_version":1}}),
    ] {
        round_trip::<RulePreviewMutation>(mutation.clone());
        let mut unknown = mutation;
        unknown["body"]["extra"] = json!(true);
        assert!(serde_json::from_value::<RulePreviewMutation>(unknown).is_err());
    }
    for removed in [
        json!({"op":"rule_sets.reorder","body":{"siblings":[{"key":"user.x","base_version":1}]}}),
        json!({"op":"rule_sets.revert","body":{"key":"user.x","base_version":1,"version":1}}),
    ] {
        assert!(serde_json::from_value::<RulePreviewMutation>(removed).is_err());
    }
}
#[test]
fn preview_deadlines_samples_and_logical_dates_have_documented_bounds() {
    assert_eq!(PreviewDeadlineMs::default().get(), 30_000);
    assert_eq!(PreviewSampleLimit::default().get(), 50);
    for value in [1, 120_000] {
        assert_eq!(
            PreviewDeadlineMs::try_from(value)
                .expect("valid deadline")
                .get(),
            value
        );
    }
    for value in [0, 120_001, u32::MAX] {
        assert!(PreviewDeadlineMs::try_from(value).is_err());
    }
    for value in [0, 50] {
        assert_eq!(
            PreviewSampleLimit::try_from(value)
                .expect("valid samples")
                .get(),
            value
        );
    }
    for value in [51, u32::MAX] {
        assert!(PreviewSampleLimit::try_from(value).is_err());
    }
    for value in [json!(-1), json!(1.5), json!(true), json!("50")] {
        assert!(serde_json::from_value::<PreviewDeadlineMs>(value.clone()).is_err());
        assert!(serde_json::from_value::<PreviewSampleLimit>(value).is_err());
    }
    let mut request: RulesPreviewRequest =
        serde_json::from_value(fixture()["editor"].clone()).expect("request");
    for end in ["2026-11-17", "2026-11-23"] {
        request.end_date = end.parse().expect("end");
        request.validate_dates().expect("one through seven days");
    }
    for end in ["2026-11-15", "2026-11-16", "2026-11-24"] {
        request.end_date = end.parse().expect("end");
        assert!(request.validate_dates().is_err());
    }
}
#[test]
fn preview_responses_accept_additive_fields() {
    let mut response = fixture()["response"].clone();
    response["extra"] = json!(true);
    response["rules"][0]["extra"] = json!(true);
    response["totals"]["before"]["extra"] = json!(true);
    response["samples"][0]["extra"] = json!(true);
    response["samples"][0]["after"]["extra"] = json!(true);
    let _: RulesPreviewResponse = serde_json::from_value(response).expect("additive response");
}
#[test]
fn preview_roles_and_capability_match_the_contract() {
    assert!(Role::Ui.authorize(Op::RulesPreview).is_ok());
    assert!(Role::Ui.authorize(Op::RulesTextPreview).is_ok());
    assert!(Role::Cli.authorize(Op::RulesTextPreview).is_ok());
    assert!(Role::Cli.authorize(Op::RulesPreview).is_err());
    assert!(Role::Host.authorize(Op::RulesPreview).is_err());
    assert!(Role::Host.authorize(Op::RulesTextPreview).is_err());
    assert_eq!(
        serde_json::to_value(Capability::RulesPreviewV1).expect("capability"),
        json!("rules.preview.v1")
    );
}
#[test]
fn preview_cursors_bind_mutation_operation_dates_versions_and_limits() {
    let request: RulesPreviewRequest =
        serde_json::from_value(fixture()["editor"].clone()).expect("request");
    let parameters = request.cursor_parameters("mutation-digest".into());
    let cursor = QueryCursor::new(QueryVersions("inputs".into()), parameters.clone(), 1);
    cursor.validate_query(&parameters).expect("same parameters");
    assert!(
        cursor
            .validate_query(&request.cursor_parameters("another-mutation".into()))
            .is_err()
    );
    let text: RulesTextPreviewRequest =
        serde_json::from_value(fixture()["text"].clone()).expect("request");
    assert!(
        cursor
            .validate_query(&text.cursor_parameters("mutation-digest".into()))
            .is_err()
    );
    let mut changed = request.clone();
    changed.expected_versions = QueryVersions("other".into());
    assert!(
        cursor
            .validate_query(&changed.cursor_parameters("mutation-digest".into()))
            .is_err()
    );
    changed = request.clone();
    changed.end_date = "2026-11-22".parse().expect("date");
    assert!(
        cursor
            .validate_query(&changed.cursor_parameters("mutation-digest".into()))
            .is_err()
    );
    changed = request.clone();
    changed.sample_limit = Some(PreviewSampleLimit::try_from(0).expect("no samples"));
    assert!(
        cursor
            .validate_query(&changed.cursor_parameters("mutation-digest".into()))
            .is_err()
    );
    changed = request;
    changed.deadline_ms = Some(PreviewDeadlineMs::try_from(1).expect("short deadline"));
    assert!(
        cursor
            .validate_query(&changed.cursor_parameters("mutation-digest".into()))
            .is_err()
    );
}
#[test]
fn preview_debug_output_omits_submitted_rule_text() {
    let mut request = fixture()["editor"].clone();
    request["mutation"]["body"]["text"] = json!("private-rule-text-marker");
    let request: RulesPreviewRequest = serde_json::from_value(request).expect("request");
    assert!(!format!("{request:?}").contains("private-rule-text-marker"));
    let mut request = fixture()["text"].clone();
    request["mutation"]["body"]["text"] = json!("private-rule-text-marker");
    let request: RulesTextPreviewRequest = serde_json::from_value(request).expect("request");
    assert!(!format!("{request:?}").contains("private-rule-text-marker"));
}
#[test]
fn preview_errors_require_the_current_digest_and_preserve_stable_codes() {
    for wire in [
        json!({"code":"stale_inputs","current_versions":"current-range-inputs"}),
        json!({"code":"stale_cursor"}),
        json!({"code":"deadline_exceeded"}),
        json!({"code":"cancelled"}),
    ] {
        round_trip::<crate::ProtocolError>(wire);
    }
    assert!(
        serde_json::from_value::<crate::ProtocolError>(json!({"code":"stale_inputs"})).is_err()
    );
}

#[test]
fn cancellation_requests_are_strict_and_exclude_host_roles() {
    round_trip::<crate::CancelRequest>(json!({"request_id":17}));
    assert!(
        serde_json::from_value::<crate::CancelRequest>(json!({"request_id":17,"extra":true}))
            .is_err()
    );
    assert!(serde_json::from_value::<crate::CancelRequest>(json!({"request_id":-1})).is_err());
    assert!(Role::Ui.authorize(Op::RequestsCancel).is_ok());
    assert!(Role::Cli.authorize(Op::RequestsCancel).is_ok());
    assert!(Role::Host.authorize(Op::RequestsCancel).is_err());
    assert_eq!(
        serde_json::to_value(Capability::RequestsCancelV1).expect("capability"),
        json!("requests.cancel.v1")
    );
}

#[test]
fn privacy_preview_fixture_preserves_stored_retention_totals() {
    let totals = fixture()["response"]["totals"].clone();
    assert_eq!(totals["before"], totals["after"]);
}
