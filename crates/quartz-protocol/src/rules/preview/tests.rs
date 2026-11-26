use super::*;
use crate::{CoreRequest, Op, Role};
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
    round_trip::<RulesPreviewRequest>(fixture["edit"].clone());
    round_trip::<RulesPreviewRequest>(fixture["import"].clone());
    round_trip::<RulesPreviewResponse>(fixture["response"].clone());
    assert_eq!(RulesPreviewRequest::OP, Op::RulesPreview);
    for output in [
        json!({"category":{"kind":"unclassified"}}),
        json!({"extraction":{"document":null,"project":null,"profile":null,"dirty":null,"normalized_title":null,"extractor_rule":null,"status":null}}),
        json!({"app":"drop","window_title":"drop","web":"none"}),
    ] {
        round_trip::<RulePreviewOutput>(output);
    }
}

#[test]
fn preview_requests_reject_unknown_fields_and_accept_all_mutations() {
    for key in ["edit", "import"] {
        for pointer in ["", "/mutation", "/mutation/body"] {
            let mut request = fixture()[key].clone();
            request.pointer_mut(pointer).expect("object")["extra"] = json!(true);
            assert!(serde_json::from_value::<RulesPreviewRequest>(request).is_err());
        }
    }
    for mutation in [
        json!({"op":"rule_sets.edit","body":{"key":"user.x","base_version":0,"text":"text"}}),
        json!({"op":"rules_text.import","body":{"key":"user.x","base_version":0,"text":"text"}}),
        json!({"op":"rule_sets.set_enabled","body":{"key":"user.x","base_version":1,"enabled":false}}),
        json!({"op":"rule_sets.delete","body":{"key":"user.x","base_version":1}}),
    ] {
        round_trip::<RulePreviewMutation>(mutation.clone());
        let mut unknown = mutation;
        unknown["body"]["extra"] = json!(true);
        assert!(serde_json::from_value::<RulePreviewMutation>(unknown).is_err());
    }
    let duplicate =
        r#"{"op":"rule_sets.delete","body":{"key":"user.x","base_version":1,"key":"user.y"}}"#;
    assert!(serde_json::from_str::<RulePreviewMutation>(duplicate).is_err());
}

#[test]
fn preview_logical_dates_have_documented_bounds() {
    assert_eq!(PREVIEW_MAX_SAMPLES, 50);
    let mut request: RulesPreviewRequest =
        serde_json::from_value(fixture()["edit"].clone()).expect("request");
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
fn preview_responses_accept_additive_fields_and_ui_only_role() {
    let mut response = fixture()["response"].clone();
    response["extra"] = json!(true);
    response["rules"][0]["extra"] = json!(true);
    response["totals"]["before"]["extra"] = json!(true);
    response["samples"][0]["extra"] = json!(true);
    response["samples"][0]["after"]["extra"] = json!(true);
    let _: RulesPreviewResponse = serde_json::from_value(response).expect("additive response");
    assert!(Role::Ui.authorize(Op::RulesPreview).is_ok());
    assert!(Role::Cli.authorize(Op::RulesPreview).is_err());
}

#[test]
fn preview_debug_output_omits_submitted_rule_text() {
    for key in ["edit", "import"] {
        let mut request = fixture()[key].clone();
        request["mutation"]["body"]["text"] = json!("private-rule-text-marker");
        let request: RulesPreviewRequest = serde_json::from_value(request).expect("request");
        assert!(!format!("{request:?}").contains("private-rule-text-marker"));
    }
}
