use super::*;
use serde_json::json;

struct WireSnapshot;
impl WireSnapshot {
    fn round_trip<T: serde::de::DeserializeOwned + Serialize>(expected: serde_json::Value) {
        let decoded: T = serde_json::from_value(expected.clone()).expect("wire snapshot decodes");
        assert_eq!(
            serde_json::to_value(decoded).expect("wire snapshot encodes"),
            expected
        );
    }
    fn rule() -> serde_json::Value {
        json!({"entry_kind":"rule","content":{"id":"code","label":"Code","priority":50,"category":"11111111-1111-4111-8111-111111111111","match":"all","when":[{"field":"app.exe_name","op":"equals","value":"code.exe","case_sensitive":false,"negate":false}]},"provenance":[{"key":"user.local","version":1,"id":"code"}],"state":"effective"})
    }
}
#[test]
fn read_request_json_snapshots() {
    WireSnapshot::round_trip::<RuleSetsTreeRequest>(json!({"kind":"privacy"}));
    WireSnapshot::round_trip::<RuleSetsGetRequest>(json!({"key":"user.local","view":"effective"}));
    WireSnapshot::round_trip::<RuleSetsGetRequest>(json!({"key":"user.local","view":"own"}));
    WireSnapshot::round_trip::<RuleSetsTreeRequest>(
        json!({"kind":"classification","cursor":{"versions":"registry-inputs","query":{"kind":"rule_sets_tree","rule_kind":"classification"},"position":5}}),
    );
    WireSnapshot::round_trip::<RuleSetsGetRequest>(
        json!({"key":"user.local","view":"own","cursor":{"versions":"registry-inputs","query":{"kind":"rule_sets_get","key":"user.local","view":"own"},"position":2}}),
    );
}
#[test]
fn read_response_json_snapshots() {
    WireSnapshot::round_trip::<RuleSetsTreeResponse>(
        json!({"kind":"classification","effective_version":"a".repeat(64),"nodes":[{"key":"user.local","parent":"builtin.classification","version":3,"enabled":true,"sibling_order":2,"source":"user","counts":{"added":1,"overridden":2,"disabled":3},"warnings":[{"code":"target_deleted","target":"builtin.classification/old"},{"code":"conflict","target":"builtin.classification/terminal"}]}],"versions":"registry-inputs","next_cursor":null}),
    );
    WireSnapshot::round_trip::<RuleSetsGetResponse>(
        json!({"key":"user.local","version":1,"parent_version":2,"entries":[WireSnapshot::rule(),{"entry_kind":"override","content":{"target":"builtin.classification/code","label":"Replacement","priority":50,"category":"11111111-1111-4111-8111-111111111111","match":"all","when":[]},"provenance":[{"key":"builtin.classification","version":2,"id":"code"},{"key":"user.local","version":1,"id":"code"}],"state":"overridden"},{"entry_kind":"disable","content":{"target":"builtin.classification/terminal","reason":"Unused"},"provenance":[{"key":"builtin.classification","version":2,"id":"terminal"},{"key":"user.local","version":1,"id":"terminal"}],"state":"disabled"}],"versions":"registry-inputs","next_cursor":null}),
    );
    WireSnapshot::round_trip::<crate::ProtocolError>(
        json!({"code":"rule_set_not_found","key":"user.local"}),
    );
}
#[test]
fn requests_reject_unknown_fields_and_invalid_enums() {
    for body in [
        json!({}),
        json!({"kind":"precedent"}),
        json!({"kind":"privacy","extra":1}),
        json!({"kind":"privacy","cursor":"invalid"}),
    ] {
        assert!(serde_json::from_value::<RuleSetsTreeRequest>(body).is_err());
    }
    for body in [
        json!({"key":"user.local"}),
        json!({"key":"user.local","view":"merged"}),
        json!({"key":"user.local","view":"own","version":1}),
        json!({"key":"user.local","view":"own","extra":true}),
    ] {
        assert!(serde_json::from_value::<RuleSetsGetRequest>(body).is_err());
    }
    assert!(
        serde_json::from_str::<RuleSetsGetRequest>(
            r#"{"key":"user.local","view":"own","view":"effective"}"#
        )
        .is_err()
    );
}
#[test]
fn entries_ignore_additive_fields_and_reject_invalid_names_and_category_references() {
    let mut unknown = WireSnapshot::rule();
    unknown["content"]["extra"] = json!(true);
    assert!(serde_json::from_value::<RuleSetEntry>(unknown).is_ok());
    let mut category = WireSnapshot::rule();
    category["content"]["category"] = json!("builtin:learning");
    assert!(serde_json::from_value::<RuleSetEntry>(category).is_err());
    let mut field = WireSnapshot::rule();
    field["content"]["when"][0]["field"] = json!("app.name");
    assert!(serde_json::from_value::<RuleSetEntry>(field).is_err());
    let mut op = WireSnapshot::rule();
    op["content"]["when"][0]["op"] = json!("similar");
    assert!(serde_json::from_value::<RuleSetEntry>(op).is_err());
}

#[test]
fn mutation_bodies_and_results_match_wire_snapshots() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/rule-mutations.json"))
            .expect("fixture");
    WireSnapshot::round_trip::<RuleSetsEditRequest>(fixture["edit"].clone());
    WireSnapshot::round_trip::<RuleSetsSetEnabledRequest>(fixture["set_enabled"].clone());
    WireSnapshot::round_trip::<RuleSetsDeleteRequest>(fixture["delete"].clone());
    WireSnapshot::round_trip::<RuleSetsWriteResponse>(fixture["response"].clone());
    let mut future = fixture["response"].clone();
    future["extra"] = json!(true);
    future["sets"][0]["extra"] = json!(true);
    assert!(serde_json::from_value::<RuleSetsWriteResponse>(future).is_ok());
    for body in [
        json!({"code":"rule_set_read_only","key":"builtin.privacy"}),
        json!({"code":"rule_set_has_children","key":"user.parent"}),
        json!({"code":"rule_set_identity","key":"user.local","field":"kind"}),
        json!({"code":"rule_set_identity","key":"user.local","field":"parent"}),
        json!({"code":"rule_validation_failed","diagnostics":[{"code":"E-RULE-SET","path":"set.kind","location":{"line":3,"column":1},"params":{}}],"truncated":false}),
    ] {
        WireSnapshot::round_trip::<crate::ProtocolError>(body);
    }
}
impl WireSnapshot {
    fn strict_mutation<T: serde::de::DeserializeOwned>(mut body: serde_json::Value, zero: bool) {
        assert!(serde_json::from_value::<T>(body.clone()).is_ok());
        body["extra"] = json!(true);
        assert!(serde_json::from_value::<T>(body.clone()).is_err());
        body.as_object_mut().expect("body").remove("extra");
        for invalid in [
            json!(-1),
            json!(u64::from(u32::MAX) + 1),
            json!(1.5),
            json!(true),
            json!("1"),
        ] {
            body["base_version"] = invalid;
            assert!(serde_json::from_value::<T>(body.clone()).is_err());
        }
        body["base_version"] = json!(0);
        assert_eq!(serde_json::from_value::<T>(body.clone()).is_ok(), zero);
        body["base_version"] = json!(u32::MAX);
        assert!(serde_json::from_value::<T>(body).is_ok());
    }
}
#[test]
fn mutations_reject_unknown_fields_and_invalid_revision_numbers() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/rule-mutations.json"))
            .expect("fixture");
    WireSnapshot::strict_mutation::<RuleSetsEditRequest>(fixture["edit"].clone(), true);
    WireSnapshot::strict_mutation::<RuleSetsSetEnabledRequest>(
        fixture["set_enabled"].clone(),
        false,
    );
    WireSnapshot::strict_mutation::<RuleSetsDeleteRequest>(fixture["delete"].clone(), false);
    assert!(
        serde_json::from_str::<RuleSetsEditRequest>(
            r#"{"key":"user.a","base_version":1,"base_version":2,"text":""}"#
        )
        .is_err()
    );
}

#[test]
fn rule_text_contracts_preserve_fixtures_and_accept_additive_responses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/rule-text.json")).expect("fixture");
    WireSnapshot::round_trip::<RulesTextValidateRequest>(fixture["validate"].clone());
    WireSnapshot::round_trip::<RulesTextExportRequest>(fixture["export"].clone());
    WireSnapshot::round_trip::<RulesTextValidateResponse>(fixture["valid"].clone());
    WireSnapshot::round_trip::<RulesTextValidateResponse>(fixture["invalid"].clone());
    WireSnapshot::round_trip::<RulesTextExportResponse>(fixture["page"].clone());
    WireSnapshot::strict_mutation::<RulesTextValidateRequest>(fixture["validate"].clone(), true);
    let mut export = fixture["export"].clone();
    export["extra"] = json!(true);
    assert!(serde_json::from_value::<RulesTextExportRequest>(export).is_err());
    let mut versioned = fixture["export"].clone();
    versioned["version"] = json!(1);
    assert!(serde_json::from_value::<RulesTextExportRequest>(versioned).is_err());
    assert!(
        serde_json::from_str::<RulesTextValidateRequest>(
            r#"{"key":"user.text","base_version":0,"text":"","text":""}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<RulesTextExportRequest>(r#"{"key":"user.text","key":"user.other"}"#)
            .is_err()
    );
    let mut valid = fixture["valid"].clone();
    valid["future"] = json!(true);
    assert!(serde_json::from_value::<RulesTextValidateResponse>(valid).is_ok());
    let mut page = fixture["page"].clone();
    page["future"] = json!(true);
    assert!(serde_json::from_value::<RulesTextExportResponse>(page).is_ok());
    for role in [crate::Role::Ui, crate::Role::Cli] {
        assert!(role.authorize(crate::Op::RulesTextValidate).is_ok());
        assert!(role.authorize(crate::Op::RulesTextExport).is_ok());
    }
    assert!(
        crate::Role::Host
            .authorize(crate::Op::RulesTextValidate)
            .is_err()
    );
    assert!(
        crate::Role::Host
            .authorize(crate::Op::RulesTextExport)
            .is_err()
    );
}

#[test]
fn rule_write_debug_redacts_source_text() {
    let values = [
        format!(
            "{:?}",
            serde_json::from_value::<RuleSetsEditRequest>(
                json!({"key":"user.debug","base_version":0,"text":"private-text-marker"})
            )
            .expect("edit")
        ),
        format!(
            "{:?}",
            RulesTextValidateRequest {
                key: "user.debug".into(),
                base_version: 0,
                text: "private-text-marker".into()
            }
        ),
    ];
    for debug in values {
        assert!(!debug.contains("private-text-marker"));
    }
}
