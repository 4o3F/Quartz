//! Deterministic TOML rendering of validated rule-format fields.

use super::{Content, EntryPosition, RuleSet, SetHeader};
use serde_json::Value;
use std::fmt::Write;

impl RuleSet {
    /// Returns canonical quartz-rules/1 text, retaining the header and source entry order.
    pub fn canonical_text(&self) -> String {
        self.content.canonical_text(&self.header)
    }
}
impl Content {
    /// Renders stored contents with their saved set header and UUID category references.
    pub fn canonical_text(&self, header: &SetHeader) -> String {
        let mut output = String::from("format = \"quartz-rules/1\"\n\n[set]\n");
        let header = serde_json::to_value(header).expect("set header is JSON-safe");
        TextWriter::fields(&mut output, &header);
        for position in &self.order {
            let section = match position {
                EntryPosition::Rule(_) => "rule",
                EntryPosition::Override(_) => "override",
                EntryPosition::Disable(_) => "disable",
            };
            write!(output, "\n[[{section}]]\n").expect("string writer");
            TextWriter::fields(&mut output, &self.entry_fields(*position));
        }
        output
    }
}
struct TextWriter;
impl TextWriter {
    fn fields(output: &mut String, value: &Value) {
        let fields = value.as_object().expect("format fields are an object");
        for key in Self::sorted_keys(fields) {
            let value = &fields[key];
            if !value.is_null() {
                write!(output, "{key} = ").expect("string writer");
                Self::value(output, value);
                output.push('\n');
            }
        }
    }
    fn sorted_keys(fields: &serde_json::Map<String, Value>) -> Vec<&str> {
        let mut keys: Vec<_> = fields.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }
    fn value(output: &mut String, value: &Value) {
        match value {
            // JSON basic strings use the same escapes as TOML basic strings.
            Value::String(_) => {
                output.push_str(&value.to_string().replace('\u{7f}', "\\u007f"));
            }
            Value::Number(_) | Value::Bool(_) => {
                write!(output, "{value}").expect("string writer");
            }
            Value::Array(items) => {
                output.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index != 0 {
                        output.push_str(", ");
                    }
                    Self::value(output, item);
                }
                output.push(']');
            }
            Value::Object(fields) => {
                output.push_str("{ ");
                for (index, key) in Self::sorted_keys(fields).into_iter().enumerate() {
                    let value = &fields[key];
                    if index != 0 {
                        output.push_str(", ");
                    }
                    write!(output, "{key} = ").expect("string writer");
                    Self::value(output, value);
                }
                output.push_str(" }");
            }
            Value::Null => unreachable!("canonical format omits absent fields"),
        }
    }
}
