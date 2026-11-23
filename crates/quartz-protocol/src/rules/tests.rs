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
    WireSnapshot::round_trip::<RuleSetsGetRequest>(
        json!({"key":"user.local","version":4,"view":"effective"}),
    );
    WireSnapshot::round_trip::<RuleSetsGetRequest>(json!({"key":"user.local","view":"own"}));
    WireSnapshot::round_trip::<RuleSetsVersionsRequest>(json!({"key":"user.local"}));
    WireSnapshot::round_trip::<RuleSetsTreeRequest>(
        json!({"kind":"classification","cursor":{"versions":"registry-inputs","query":{"kind":"rule_sets_tree","rule_kind":"classification"},"position":5}}),
    );
    WireSnapshot::round_trip::<RuleSetsGetRequest>(
        json!({"key":"user.local","view":"own","version":1,"cursor":{"versions":"registry-inputs","query":{"kind":"rule_sets_get","key":"user.local","version":1,"view":"own"},"position":2}}),
    );
    WireSnapshot::round_trip::<RuleSetsVersionsRequest>(
        json!({"key":"user.local","cursor":{"versions":"registry-inputs","query":{"kind":"rule_sets_versions","key":"user.local"},"position":3}}),
    );
}
#[test]
fn read_response_json_snapshots() {
    WireSnapshot::round_trip::<RuleSetsTreeResponse>(
        json!({"kind":"classification","effective_version":"a".repeat(64),"nodes":[{"key":"user.local","parent":"builtin.classification","version":3,"enabled":true,"sibling_order":2,"source":"user","counts":{"added":1,"overridden":2,"disabled":3},"warnings":[{"code":"target_deleted","target":"builtin.classification/old"},{"code":"upstream_changed","target":"builtin.classification/code"},{"code":"conflict","target":"builtin.classification/terminal"}]}],"versions":"registry-inputs","next_cursor":null}),
    );
    WireSnapshot::round_trip::<RuleSetsGetResponse>(
        json!({"key":"user.local","version":1,"parent_version":2,"entries":[WireSnapshot::rule(),{"entry_kind":"override","content":{"target":"builtin.classification/code","label":"Replacement","priority":50,"category":"11111111-1111-4111-8111-111111111111","match":"all","when":[]},"provenance":[{"key":"builtin.classification","version":2,"id":"code"},{"key":"user.local","version":1,"id":"code"}],"state":"overridden"},{"entry_kind":"disable","content":{"target":"builtin.classification/terminal","reason":"Unused"},"provenance":[{"key":"builtin.classification","version":2,"id":"terminal"},{"key":"user.local","version":1,"id":"terminal"}],"state":"disabled"}],"versions":"registry-inputs","next_cursor":null}),
    );
    WireSnapshot::round_trip::<RuleSetsVersionsResponse>(
        json!({"revisions":[{"version":3,"parent_version":2,"created_utc_ms":123456,"note":"rule-set state changed"},{"version":1,"parent_version":1,"created_utc_ms":0,"note":null}],"versions":"registry-inputs","next_cursor":null}),
    );
    WireSnapshot::round_trip::<crate::ProtocolError>(
        json!({"code":"rule_set_not_found","key":"user.local","version":null}),
    );
}
#[test]
fn requests_reject_unknown_fields_invalid_enums_and_invalid_revision_numbers() {
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
        json!({"key":"user.local","view":"own","version":0}),
        json!({"key":"user.local","view":"own","version":-1}),
        json!({"key":"user.local","view":"own","version":4_294_967_296_u64}),
        json!({"key":"user.local","view":"own","version":1.5}),
        json!({"key":"user.local","view":"own","version":true}),
        json!({"key":"user.local","view":"own","extra":true}),
    ] {
        assert!(serde_json::from_value::<RuleSetsGetRequest>(body).is_err());
    }
    assert!(
        serde_json::from_value::<RuleSetsVersionsRequest>(json!({"key":"user.local","extra":true}))
            .is_err()
    );
    assert!(
        serde_json::from_str::<RuleSetsGetRequest>(
            r#"{"key":"user.local","view":"own","view":"effective"}"#
        )
        .is_err()
    );
    WireSnapshot::round_trip::<RuleSetsGetRequest>(
        json!({"key":"user.local","view":"own","version":u32::MAX}),
    );
}
#[test]
fn entries_reject_unknown_fields_names_and_non_uuid_category_references() {
    let mut unknown = WireSnapshot::rule();
    unknown["content"]["extra"] = json!(true);
    assert!(serde_json::from_value::<RuleSetEntry>(unknown).is_err());
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
