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
}
#[test]
fn read_response_json_snapshots() {
    WireSnapshot::round_trip::<RuleSetsTreeResponse>(
        json!({"kind":"classification","effective_version":"a".repeat(64),"nodes":[{"key":"user.local","parent":"builtin.classification","version":3,"enabled":true,"sibling_order":2,"source":"user","counts":{"added":1,"overridden":2,"disabled":3},"warnings":[{"code":"target_deleted","target":"builtin.classification/old"},{"code":"conflict","target":"builtin.classification/terminal"}]}],"versions":"registry-inputs","truncated":false}),
    );
    WireSnapshot::round_trip::<RuleSetsGetResponse>(
        json!({"key":"user.local","version":1,"parent_version":2,"entries":[WireSnapshot::rule(),{"entry_kind":"override","content":{"target":"builtin.classification/code","label":"Replacement","priority":50,"category":"11111111-1111-4111-8111-111111111111","match":"all","when":[]},"provenance":[{"key":"builtin.classification","version":2,"id":"code"},{"key":"user.local","version":1,"id":"code"}],"state":"overridden"},{"entry_kind":"disable","content":{"target":"builtin.classification/terminal","reason":"Unused"},"provenance":[{"key":"builtin.classification","version":2,"id":"terminal"},{"key":"user.local","version":1,"id":"terminal"}],"state":"disabled"}],"versions":"registry-inputs","truncated":false}),
    );
    WireSnapshot::round_trip::<crate::ProtocolError>(
        json!({"code":"rule_set_not_found","key":"user.local"}),
    );
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

#[test]
fn rule_text_contracts_preserve_fixtures_and_accept_additive_responses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/rule-text.json")).expect("fixture");
    WireSnapshot::round_trip::<RulesTextValidateRequest>(fixture["validate"].clone());
    WireSnapshot::round_trip::<RulesTextExportRequest>(fixture["export"].clone());
    WireSnapshot::round_trip::<RulesTextValidateResponse>(fixture["valid"].clone());
    WireSnapshot::round_trip::<RulesTextValidateResponse>(fixture["invalid"].clone());
    WireSnapshot::round_trip::<RulesTextExportResponse>(fixture["page"].clone());
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
