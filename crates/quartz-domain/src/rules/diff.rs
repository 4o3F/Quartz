//! Structural comparisons in canonical rule-format field names.

use super::{Content, EntryPosition, RuleSetKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Structural change to one qualified rule reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryChange {
    /// Definition reference, or ancestor target for an overlay entry.
    pub rule: String,
    /// Whether the entry appeared, disappeared, or changed.
    pub change: EntryChangeKind,
    /// Sorted canonical field paths relative to the entry.
    pub paths: Vec<String>,
}
/// Presence or content change between two revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryChangeKind {
    /// The entry exists only in the after content.
    Added,
    /// The entry exists only in the before content.
    Removed,
    /// Both entries exist and fields or source order differ.
    Changed,
}
struct ComparedEntry {
    fields: Value,
}
impl Content {
    /// Compares definitions, overrides, disables, and their source order deterministically.
    pub fn diff(&self, after: &Self, key: &RuleSetKey) -> Vec<EntryChange> {
        let before = self.compared_entries(key);
        let after = after.compared_entries(key);
        let references: BTreeSet<_> = before.keys().chain(after.keys()).collect();
        references
            .into_iter()
            .filter_map(|reference| {
                let old = before.get(reference);
                let new = after.get(reference);
                let change = match (old, new) {
                    (None, Some(_)) => EntryChangeKind::Added,
                    (Some(_), None) => EntryChangeKind::Removed,
                    _ => EntryChangeKind::Changed,
                };
                let mut paths = BTreeSet::new();
                FieldPaths::compare(
                    old.map(|entry| &entry.fields),
                    new.map(|entry| &entry.fields),
                    "",
                    &mut paths,
                );
                (!paths.is_empty()).then(|| EntryChange {
                    rule: reference.clone(),
                    change,
                    paths: paths.into_iter().collect(),
                })
            })
            .collect()
    }
    fn compared_entries(&self, key: &RuleSetKey) -> BTreeMap<String, ComparedEntry> {
        self.order
            .iter()
            .enumerate()
            .map(|(order, position)| {
                let reference = match *position {
                    EntryPosition::Rule(index) => format!("{key}/{}", self.rule[index].id),
                    EntryPosition::Override(index) => self.r#override[index].target.clone(),
                    EntryPosition::Disable(index) => self.disable[index].target.clone(),
                };
                let mut fields = self.entry_fields(*position);
                let object = fields.as_object_mut().expect("canonical entry object");
                object.insert(
                    "entry_kind".into(),
                    serde_json::to_value(position.kind()).expect("entry kind"),
                );
                object.insert("entry_order".into(), order.into());
                (reference, ComparedEntry { fields })
            })
            .collect()
    }
}
struct FieldPaths;
impl FieldPaths {
    fn compare(
        before: Option<&Value>,
        after: Option<&Value>,
        path: &str,
        paths: &mut BTreeSet<String>,
    ) {
        if before == after {
            return;
        }
        match (before, after) {
            (Some(Value::Object(old)), Some(Value::Object(new))) => {
                let keys: BTreeSet<_> = old.keys().chain(new.keys()).collect();
                for key in keys {
                    let field = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    Self::compare(old.get(key), new.get(key), &field, paths);
                }
            }
            (Some(Value::Array(old)), Some(Value::Array(new))) => {
                for index in 0..old.len().max(new.len()) {
                    Self::compare(
                        old.get(index),
                        new.get(index),
                        &format!("{path}[{index}]"),
                        paths,
                    );
                }
            }
            (None, Some(Value::Object(fields))) | (Some(Value::Object(fields)), None) => {
                for key in fields.keys() {
                    paths.insert(if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    });
                }
            }
            _ => {
                paths.insert(path.to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Disable, ParseContext, ReferenceCatalog, RuleOrigin, RuleSet};

    #[test]
    fn added_removed_and_changed_disable_entries_have_sorted_field_paths() {
        let key = "user.overlay".parse().expect("key");
        let before = Content {
            disable: vec![Disable {
                target: "builtin.privacy/bank".into(),
                reason: Some("old".into()),
            }],
            order: vec![EntryPosition::Disable(0)],
            ..Content::default()
        };
        let mut after = before.clone();
        after.disable[0].reason = Some("new".into());
        assert_eq!(before.diff(&after, &key)[0].paths, ["reason"]);
        assert_eq!(
            Content::default().diff(&after, &key)[0].change,
            EntryChangeKind::Added
        );
        assert_eq!(
            before.diff(&Content::default(), &key)[0].change,
            EntryChangeKind::Removed
        );
        assert!(before.diff(&before, &key).is_empty());
    }
    fn content(entries: &str) -> Content {
        RuleSet::parse("diff.toml", &format!("format='quartz-rules/1'\n[set]\nkey='user.overlay'\nkind='privacy'\nname='Overlay'\nparent='builtin.privacy'\n{entries}"), ParseContext { origin: RuleOrigin::User, references: &ReferenceCatalog::default() }).expect("valid content").content
    }
    #[test]
    fn added_removed_and_changed_rules_use_canonical_nested_field_paths() {
        let key = "user.overlay".parse().expect("key");
        let entry = "[[rule]]\nid='bank'\nlabel='Bank'\npreset='title_hidden'\nwhen=[{field='title.raw',op='contains',value='bank'}]\n";
        let before = content(entry);
        let after = content(
            &entry
                .replace("value='bank'", "value='account'")
                .replace("label='Bank'", "label='Account'"),
        );
        let changes = before.diff(&after, &key);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].rule, "user.overlay/bank");
        assert_eq!(changes[0].paths, ["label", "when[0].value"]);
        assert_eq!(
            Content::default().diff(&before, &key)[0].change,
            EntryChangeKind::Added
        );
        assert_eq!(
            before.diff(&Content::default(), &key)[0].change,
            EntryChangeKind::Removed
        );
    }
    #[test]
    fn override_changes_and_conversion_to_disable_preserve_the_target_reference() {
        let key = "user.overlay".parse().expect("key");
        let entry = "[[override]]\ntarget='builtin.privacy/bank'\nlabel='Bank'\npreset='title_hidden'\nwhen=[{field='title.raw',op='contains',value='bank'}]\n";
        let before = content(entry);
        let after = content(&entry.replace("title_hidden", "excluded"));
        let changes = before.diff(&after, &key);
        assert_eq!(changes[0].rule, "builtin.privacy/bank");
        assert!(changes[0].paths.contains(&"app".into()));
        assert_eq!(
            Content::default().diff(&before, &key)[0].change,
            EntryChangeKind::Added
        );
        assert_eq!(
            before.diff(&Content::default(), &key)[0].change,
            EntryChangeKind::Removed
        );
        let disabled = content("[[disable]]\ntarget='builtin.privacy/bank'\nreason='Removed'\n");
        let changes = before.diff(&disabled, &key);
        assert_eq!(changes[0].change, EntryChangeKind::Changed);
        assert!(changes[0].paths.contains(&"entry_kind".into()));
        assert!(changes[0].paths.contains(&"reason".into()));
    }
    #[test]
    fn entry_order_changes_are_reported_in_rule_reference_order() {
        let key = "user.overlay".parse().expect("key");
        let before = content(
            "[[disable]]\ntarget='builtin.privacy/z'\n[[disable]]\ntarget='builtin.privacy/a'\n",
        );
        let mut after = before.clone();
        after.order.reverse();
        let changes = before.diff(&after, &key);
        assert_eq!(
            changes
                .iter()
                .map(|change| change.rule.as_str())
                .collect::<Vec<_>>(),
            ["builtin.privacy/a", "builtin.privacy/z"]
        );
        assert!(changes.iter().all(|change| change.paths == ["entry_order"]));
    }
    fn family_content(kind: &str, entries: &str) -> Content {
        let mut references = ReferenceCatalog::default();
        for category in crate::categories::BuiltinCategory::ALL {
            references.insert_category(&category.uuid().0, Some(category.key()), None);
        }
        RuleSet::parse("diff.toml", &format!("format='quartz-rules/1'\n[set]\nkey='user.overlay'\nkind='{kind}'\nname='Overlay'\n{entries}"), ParseContext { origin: RuleOrigin::User, references: &references }).expect("valid content").content
    }
    #[test]
    fn classification_changes_use_category_priority_and_match_field_paths() {
        let entry = "[[rule]]\nid='work'\nlabel='Work'\npriority=1\ncategory='builtin:learning'\nmatch='all'\nwhen=[{field='app.exe_name',op='equals',value='code.exe'}]\n";
        let before = family_content("classification", entry);
        let after = family_content(
            "classification",
            &entry
                .replace("priority=1", "priority=2")
                .replace("builtin:learning", "builtin:development")
                .replace("match='all'", "match='any'"),
        );
        let changes = before.diff(&after, &"user.overlay".parse().expect("key"));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].paths, ["category", "match", "priority"]);
    }
    #[test]
    fn extraction_changes_use_app_condition_and_pattern_field_paths() {
        let before = family_content(
            "extraction",
            "[[rule]]\nid='document'\nlabel='Document'\napp=[{field='app.exe_name',op='equals',value='code.exe'}]\npattern='(?P<document>.+)'\n",
        );
        let after = family_content(
            "extraction",
            "[[rule]]\nid='document'\nlabel='Document'\napp=[{field='app.display_name',op='contains',value='Code',case_sensitive=true,negate=true}]\npattern='prefix (?P<document>.+)'\n",
        );
        let changes = before.diff(&after, &"user.overlay".parse().expect("key"));
        assert_eq!(
            changes[0].paths,
            [
                "app[0].case_sensitive",
                "app[0].field",
                "app[0].negate",
                "app[0].op",
                "app[0].value",
                "pattern"
            ]
        );
    }
    #[test]
    fn condition_array_length_changes_report_the_added_and_removed_condition_fields() {
        let one = content(
            "[[rule]]\nid='bank'\nlabel='Bank'\npreset='title_hidden'\nwhen=[{field='title.raw',op='contains',value='bank'}]\n",
        );
        let two = content(
            "[[rule]]\nid='bank'\nlabel='Bank'\npreset='title_hidden'\nwhen=[{field='title.raw',op='contains',value='bank'},{field='app.exe_name',op='equals',value='app.exe'}]\n",
        );
        let key = "user.overlay".parse().expect("key");
        let expected = [
            "when[1].case_sensitive",
            "when[1].field",
            "when[1].negate",
            "when[1].op",
            "when[1].value",
        ];
        assert_eq!(one.diff(&two, &key)[0].paths, expected);
        assert_eq!(two.diff(&one, &key)[0].paths, expected);
    }
}
