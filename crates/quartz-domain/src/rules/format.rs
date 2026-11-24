use super::{CaseSensitivity, Condition, Content, EntryPosition, Rule, RuleBody};
use serde_json::{Map, Value};

/// Maximum canonical entry encoding accepted by the IPC rule contract (04 §6.3.1).
pub const MAX_ENTRY_BYTES: usize = 128 * 1024;

impl Content {
    /// Projects one entry into rule-format fields, keeping resolved category UUIDs.
    pub fn entry_fields(&self, position: EntryPosition) -> Value {
        match position {
            EntryPosition::Rule(index) => self.rule[index].format_fields(),
            EntryPosition::Override(index) => {
                let entry = &self.r#override[index];
                let mut fields = entry.rule.format_fields();
                let object = fields.as_object_mut().expect("rule fields form an object");
                object.remove("id");
                object.insert("target".into(), Value::String(entry.target.clone()));
                fields
            }
            EntryPosition::Disable(index) => {
                let entry = &self.disable[index];
                let mut fields =
                    Map::from_iter([("target".into(), Value::String(entry.target.clone()))]);
                if let Some(reason) = &entry.reason {
                    fields.insert("reason".into(), Value::String(reason.clone()));
                }
                Value::Object(fields)
            }
        }
    }
}
impl Rule {
    fn format_fields(&self) -> Value {
        let mut fields = Map::from_iter([
            ("id".into(), Value::String(self.id.clone())),
            ("priority".into(), self.priority.into()),
        ]);
        if let Some(label) = &self.label {
            fields.insert("label".into(), label.clone().into());
        }
        if let Some(builtin) = &self.builtin {
            let value = serde_json::to_value(builtin).expect("builtin metadata is JSON-safe");
            fields.extend(
                value
                    .as_object()
                    .expect("metadata object")
                    .iter()
                    .filter(|item| !item.1.is_null())
                    .map(|item| (item.0.clone(), item.1.clone())),
            );
            if builtin.languages.is_empty() {
                fields.remove("languages");
            }
        }
        match &self.body {
            RuleBody::Classification {
                category,
                mode,
                when,
            } => {
                fields.insert("category".into(), category.uuid.0.clone().into());
                fields.insert(
                    "match".into(),
                    serde_json::to_value(mode).expect("mode is JSON-safe"),
                );
                fields.insert("when".into(), Condition::format_list(when));
            }
            RuleBody::Extraction { app, pattern } => {
                fields.insert("app".into(), Condition::format_list(app));
                fields.insert("pattern".into(), pattern.clone().into());
            }
            RuleBody::Privacy { action, mode, when } => {
                fields.insert(
                    "match".into(),
                    serde_json::to_value(mode).expect("mode is JSON-safe"),
                );
                fields.insert("when".into(), Condition::format_list(when));
                fields.insert(
                    "app".into(),
                    serde_json::to_value(action.app).expect("action is JSON-safe"),
                );
                fields.insert(
                    "window_title".into(),
                    serde_json::to_value(action.window_title).expect("action is JSON-safe"),
                );
                fields.insert(
                    "web".into(),
                    serde_json::to_value(action.web).expect("action is JSON-safe"),
                );
            }
        }
        Value::Object(fields)
    }
}
impl Condition {
    fn format_list(conditions: &[Self]) -> Value {
        Value::Array(conditions.iter().map(|condition| serde_json::json!({
            "field": condition.field, "op": condition.op, "value": condition.value,
            "case_sensitive": condition.case == CaseSensitivity::Sensitive, "negate": condition.negate,
        })).collect())
    }
}

impl super::ResolvedEntry {
    /// Projects the complete IPC entry, including the definition and overlay chain.
    pub fn format_entry(&self, content: &Content) -> Value {
        let fields = content.entry_fields(self.position);
        serde_json::json!({"entry_kind": self.position.kind(), "content": fields, "provenance": self.provenance, "state": self.state})
    }
}
