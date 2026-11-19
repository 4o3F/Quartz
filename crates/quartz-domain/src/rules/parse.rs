use super::{
    diagnostic::{Diagnostic, DiagnosticCode, SourceLocation},
    model::{
        Action, BuiltinMetadata, CaseSensitivity, CategoryReference, Condition, Content, Disable,
        EntryPosition, Field, MAX_TEXT_UTF16, MatchMode, Operator, Override, PrivacyAction,
        REGEX_SIZE_LIMIT, Rule, RuleBody, RuleKind, RuleOrigin, RuleSet, SetHeader, WebAction,
    },
};
use crate::domain_name::DomainName;
use regex::RegexBuilder;
use serde::{
    Deserialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use snafu::Snafu;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};
use toml::Spanned;

/// Rule priority when the optional field is absent.
const DEFAULT_PRIORITY: u16 = 50;
/// Largest rule priority accepted by the published format.
const MAX_PRIORITY: u16 = 1000;

/// Names and builtin keys mapped to durable category UUIDs by the caller.
#[derive(Debug, Clone, Default)]
pub struct ReferenceCatalog {
    /// Accepted category names, builtin keys, and UUIDs mapped to durable category UUIDs.
    pub categories: BTreeMap<String, String>,
}
/// Import authority and known references; user files cannot grant themselves builtin authority.
#[derive(Debug, Clone, Copy)]
pub struct ParseContext<'a> {
    /// Import authority controlling reserved keys and builtin-only metadata.
    pub origin: RuleOrigin,
    /// Existing references against which category values are validated and resolved.
    pub references: &'a ReferenceCatalog,
}

/// A non-empty collection of located syntax or validation diagnostics for one rule file.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
#[snafu(display("rule set has {} validation errors", diagnostics.len()))]
pub struct ParseRuleSetError {
    diagnostics: Vec<Diagnostic>,
}

impl ParseRuleSetError {
    fn new(first: Diagnostic, remaining: impl IntoIterator<Item = Diagnostic>) -> Self {
        Self {
            diagnostics: std::iter::once(first).chain(remaining).collect(),
        }
    }

    /// Borrows every diagnostic in validation order; the slice always contains at least one.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Takes ownership of all diagnostics while preserving their validation order.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }
}

#[derive(Debug)]
enum Node {
    Table(BTreeMap<Spanned<String>, Spanned<Node>>),
    Array(Vec<Spanned<Node>>),
    Scalar(Value),
}
impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NodeVisitor;
        impl<'de> Visitor<'de> for NodeVisitor {
            type Value = Node;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a TOML value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
                let mut table = BTreeMap::new();
                while let Some((key, value)) = map.next_entry()? {
                    table.insert(key, value);
                }
                Ok(Node::Table(table))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
                let mut array = Vec::new();
                while let Some(value) = seq.next_element()? {
                    array.push(value);
                }
                Ok(Node::Array(array))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Node, E> {
                Ok(Node::Scalar(value.into()))
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Node, E> {
                Ok(Node::Scalar(value.into()))
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Node, E> {
                Ok(Node::Scalar(value.into()))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Node, E> {
                Ok(Node::Scalar(value.into()))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Node, E> {
                Ok(Node::Scalar(Value::from(value)))
            }
        }
        deserializer.deserialize_any(NodeVisitor)
    }
}
struct SourceMap<'source> {
    file: &'source str,
    source: &'source str,
    locations: BTreeMap<String, SourceLocation>,
}
impl<'source> SourceMap<'source> {
    fn new(file: &'source str, source: &'source str) -> Self {
        Self {
            file,
            source,
            locations: BTreeMap::new(),
        }
    }
    fn flatten(&mut self, node: Spanned<Node>, path: &str) -> Value {
        self.locations.insert(
            path.into(),
            SourceLocation::new(self.file, self.source, node.span()),
        );
        match node.into_inner() {
            Node::Scalar(value) => value,
            Node::Array(array) => Value::Array(
                array
                    .into_iter()
                    .enumerate()
                    .map(|(i, node)| self.flatten(node, &format!("{path}[{i}]")))
                    .collect(),
            ),
            Node::Table(table) => Value::Object(
                table
                    .into_iter()
                    .map(|(key, value)| {
                        let child = if path.is_empty() {
                            key.get_ref().clone()
                        } else {
                            format!("{path}.{}", key.get_ref())
                        };
                        self.locations.insert(
                            format!("{child}#key"),
                            SourceLocation::new(self.file, self.source, key.span()),
                        );
                        (key.into_inner(), self.flatten(value, &child))
                    })
                    .collect(),
            ),
        }
    }
}

impl RuleSet {
    /// Parses and validates every independent entry, returning all semantic diagnostics.
    ///
    /// # Errors
    /// Returns [`ParseRuleSetError`] containing all located diagnostics for invalid syntax, keys, references, conditions, or unsupported M7 families.
    pub fn parse(
        file: &str,
        source: &str,
        context: ParseContext<'_>,
    ) -> Result<Self, ParseRuleSetError> {
        let root: Spanned<Node> = match toml::from_str(source) {
            Ok(root) => root,
            Err(error) => {
                return Err(ParseRuleSetError::new(
                    Diagnostic {
                        code: DiagnosticCode::Format,
                        set_key: file.into(),
                        path: "".into(),
                        // A parser error without a span is anchored at the source's beginning.
                        location: SourceLocation::new(file, source, error.span().unwrap_or(0..0)),
                        message: error.message().into(),
                    },
                    std::iter::empty(),
                ));
            }
        };
        let mut source_map = SourceMap::new(file, source);
        let root = source_map.flatten(root, "");
        let locations = source_map.locations;
        // Diagnostics use the source file label until a set key is available.
        let key = root
            .pointer("/set/key")
            .and_then(Value::as_str)
            .unwrap_or(file)
            .to_owned();
        let mut check = Check {
            context,
            key,
            locations,
            diagnostics: Vec::new(),
        };
        check.keys(&root, "", &["format", "set", "rule", "override", "disable"]);
        if root.get("format").and_then(Value::as_str) != Some("quartz-rules/1") {
            check.error(
                DiagnosticCode::Format,
                "format",
                "expected format = \"quartz-rules/1\"",
            );
        }
        // Missing headers enter the non-table diagnostic path while other fields are checked.
        let header = root.get("set").unwrap_or(&Value::Null);
        check.keys(
            header,
            "set",
            &["key", "kind", "name", "parent", "description", "chain"],
        );
        let key = check.required_string(header, "key", "set");
        if key.as_deref().is_none_or(|key| {
            key.is_empty()
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_".contains(&b))
                || (context.origin == RuleOrigin::User && key.starts_with("builtin."))
        }) {
            check.error(
                DiagnosticCode::Set,
                "set.key",
                "invalid or reserved rule-set key",
            );
        }
        let kind = check
            .required_string(header, "kind", "set")
            .as_deref()
            .and_then(RuleKind::parse);
        // Classification grammar collects the remaining diagnostics when the kind is invalid.
        let kind = kind.unwrap_or_else(|| {
            check.error(DiagnosticCode::Set, "set.kind", "unknown rule kind");
            RuleKind::Classification
        });
        if matches!(kind, RuleKind::Precedent | RuleKind::PolicyCheck) {
            check.error(
                DiagnosticCode::Set,
                "set.kind",
                "this rule kind requires milestone M7 and is not supported yet",
            );
        }
        let name = check.required_string(header, "name", "set");
        let parent = check.string(header, "parent", "set", Required::No);
        let description = check.string(header, "description", "set", Required::No);
        let chain = check.string(
            header,
            "chain",
            "set",
            if kind == RuleKind::Precedent {
                Required::Yes
            } else {
                Required::No
            },
        );
        if chain.is_some() && kind != RuleKind::Precedent {
            check.error(
                DiagnosticCode::Set,
                "set.chain",
                "chain is only valid for precedent sets",
            );
        }
        let mut content = Content::default();
        let mut ids = BTreeSet::new();
        for section in ["rule", "override", "disable"] {
            let Some(entries) = root.get(section) else {
                continue;
            };
            let Some(entries) = entries.as_array() else {
                check.error(DiagnosticCode::Key, section, "expected an array of tables");
                continue;
            };
            if kind == RuleKind::Precedent && section != "rule" && !entries.is_empty() {
                check.error(
                    DiagnosticCode::Immutable,
                    section,
                    "precedent sets only permit appended rules",
                );
            }
            let mut targets = BTreeSet::new();
            for (index, entry) in entries.iter().enumerate() {
                let path = format!("{section}[{index}]");
                if section == "disable" {
                    check.keys(entry, &path, &["target", "reason"]);
                    let target = check.required_string(entry, "target", &path);
                    match &target {
                        Some(target) => check.target(target, &path),
                        None => check.invalid_target(&path),
                    }
                    let reason = check.string(entry, "reason", &path, Required::No);
                    if let Some(target) = target {
                        content.disable.push(Disable { target, reason });
                    }
                    continue;
                }
                let target = if section == "override" {
                    check.required_string(entry, "target", &path)
                } else {
                    None
                };
                if let Some(target) = &target {
                    check.target(target, &path);
                }
                if let Some(target) = &target
                    && !targets.insert(target.clone())
                {
                    check.error(
                        DiagnosticCode::Conflict,
                        &format!("{path}.target"),
                        "target is modified more than once",
                    );
                }
                let id = if section == "rule" {
                    check.required_string(entry, "id", &path)
                } else {
                    target
                        .as_deref()
                        .and_then(|t| t.split_once('/'))
                        .map(|(_, id)| id.to_owned())
                };
                if section == "rule"
                    && id.as_deref().is_none_or(|id| {
                        id.is_empty()
                            || !id
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    })
                {
                    check.error(
                        DiagnosticCode::Value,
                        &format!("{path}.id"),
                        "invalid rule identifier",
                    );
                }
                // Missing and empty IDs share an incomplete-ID marker for duplicate diagnostics.
                if section == "rule"
                    && !ids.insert(id.as_ref().filter(|id| !id.is_empty()).cloned())
                {
                    check.error(
                        DiagnosticCode::Dup,
                        &format!("{path}.id"),
                        "duplicate rule identifier",
                    );
                }
                if let Some(rule) = check.rule(entry, &path, kind, id, section) {
                    if let Some(target) = target {
                        content.r#override.push(Override { target, rule });
                    } else {
                        content.rule.push(rule);
                    }
                }
            }
        }
        let mut order: Vec<_> =
            content
                .rule
                .iter()
                .enumerate()
                .map(|(i, _)| (format!("rule[{i}].id"), EntryPosition::Rule(i)))
                .chain(
                    content.r#override.iter().enumerate().map(|(i, _)| {
                        (format!("override[{i}].target"), EntryPosition::Override(i))
                    }),
                )
                .chain(
                    content
                        .disable
                        .iter()
                        .enumerate()
                        .map(|(i, _)| (format!("disable[{i}].target"), EntryPosition::Disable(i))),
                )
                .collect();
        order.sort_by_key(|(path, _)| {
            check
                .locations
                .get(path)
                .map_or(0, |location| location.bytes.start)
        });
        content.order = order.into_iter().map(|(_, position)| position).collect();
        let mut diagnostics = check.diagnostics.into_iter();
        if let Some(first) = diagnostics.next() {
            Err(ParseRuleSetError::new(first, diagnostics))
        } else {
            Ok(RuleSet {
                header: SetHeader {
                    key: key.expect("required set key exists when validation has no diagnostics"),
                    kind,
                    name: name
                        .expect("required set name exists when validation has no diagnostics"),
                    parent,
                    description,
                    chain,
                },
                content,
                locations: check.locations,
            })
        }
    }
}

#[derive(Clone, Copy)]
enum Required {
    Yes,
    No,
}
struct Check<'a> {
    context: ParseContext<'a>,
    key: String,
    locations: BTreeMap<String, SourceLocation>,
    diagnostics: Vec<Diagnostic>,
}
impl Check<'_> {
    fn target(&mut self, target: &str, path: &str) {
        if !target.split_once('/').is_some_and(|(set, id)| {
            !set.is_empty()
                && !id.is_empty()
                && set
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_".contains(&b))
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        }) {
            self.invalid_target(path);
        }
    }
    fn invalid_target(&mut self, path: &str) {
        self.error(
            DiagnosticCode::Target,
            &format!("{path}.target"),
            "expected a target in set-key/rule-id format",
        );
    }
    fn error(&mut self, code: DiagnosticCode, path: &str, message: impl Into<String>) {
        let mut ancestor = path.to_owned();
        let location = loop {
            if let Some(location) = self.locations.get(&ancestor) {
                break location.clone();
            }
            if let Some(index) = ancestor.rfind(['.', '[']) {
                ancestor.truncate(index);
            } else {
                ancestor.clear();
            }
        };
        self.diagnostics.push(Diagnostic {
            code,
            set_key: self.key.clone(),
            path: path.into(),
            location,
            message: message.into(),
        });
    }
    fn keys(&mut self, value: &Value, path: &str, allowed: &[&str]) {
        let Some(table) = value.as_object() else {
            self.error(DiagnosticCode::Key, path, "expected a table");
            return;
        };
        for key in table.keys().filter(|key| !allowed.contains(&key.as_str())) {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            self.error(DiagnosticCode::Key, &child, format!("unknown key {key:?}"));
            if let Some(span) = self.locations.get(&format!("{child}#key")) {
                self.diagnostics
                    .last_mut()
                    .expect("just added diagnostic")
                    .location = span.clone();
            }
        }
    }
    /// Records missing or mistyped fields and returns None so other fields can still be checked.
    fn required_string(&mut self, table: &Value, key: &str, path: &str) -> Option<String> {
        self.string(table, key, path, Required::Yes)
    }
    fn string(
        &mut self,
        table: &Value,
        key: &str,
        path: &str,
        required: Required,
    ) -> Option<String> {
        let path = format!("{path}.{key}");
        match table.get(key) {
            None if matches!(required, Required::No) => None,
            None => {
                self.error(DiagnosticCode::Key, &path, "missing required key");
                None
            }
            Some(Value::String(value)) => {
                if value.is_empty() {
                    self.error(DiagnosticCode::Value, &path, "value must not be empty");
                }
                if key == "value" && value.encode_utf16().count() > MAX_TEXT_UTF16 {
                    self.error(
                        DiagnosticCode::Value,
                        &path,
                        "condition value exceeds 4096 UTF-16 units",
                    );
                }
                Some(value.clone())
            }
            Some(_) => {
                self.error(DiagnosticCode::Value, &path, "expected a string");
                None
            }
        }
    }
    fn boolean(&mut self, table: &Value, key: &str, path: &str) -> bool {
        match table.get(key) {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                self.error(
                    DiagnosticCode::Value,
                    &format!("{path}.{key}"),
                    "expected a boolean",
                );
                false
            }
        }
    }
    fn regex(&mut self, pattern: &str, path: &str, kind: RuleKind) {
        match RegexBuilder::new(pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_SIZE_LIMIT)
            .build()
        {
            Err(error) => self.error(DiagnosticCode::Regex, path, error.to_string()),
            Ok(regex) if kind == RuleKind::Extraction => {
                for name in regex.capture_names().flatten() {
                    if !["document", "project", "profile", "dirty"].contains(&name) {
                        self.error(
                            DiagnosticCode::Regex,
                            path,
                            format!("unknown extraction capture {name:?}"),
                        );
                    }
                }
            }
            Ok(_) => {}
        }
    }
    fn conditions(
        &mut self,
        table: &Value,
        key: &str,
        path: &str,
        kind: RuleKind,
    ) -> Vec<Condition> {
        let path = format!("{path}.{key}");
        let Some(entries) = table.get(key).and_then(Value::as_array) else {
            self.error(DiagnosticCode::Key, &path, "expected a condition array");
            return Vec::new();
        };
        if entries.is_empty() || entries.len() > 32 {
            self.error(DiagnosticCode::Value, &path, "expected 1 to 32 conditions");
        }
        let mut conditions = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let path = format!("{path}[{index}]");
            self.keys(
                entry,
                &path,
                &["field", "op", "value", "case_sensitive", "negate"],
            );
            let field = self
                .string(entry, "field", &path, Required::Yes)
                .and_then(|value| {
                    let field = Field::parse(&value);
                    if field.is_none_or(|f| !f.allows(kind)) {
                        self.error(
                            DiagnosticCode::Field,
                            &format!("{path}.field"),
                            "unknown or unavailable field",
                        );
                    }
                    field
                });
            let op = self
                .string(entry, "op", &path, Required::Yes)
                .and_then(|value| {
                    let op = Operator::parse(&value);
                    if op.is_none() {
                        self.error(
                            DiagnosticCode::Op,
                            &format!("{path}.op"),
                            "unknown operator",
                        );
                    }
                    op
                });
            let mut value = self.string(entry, "value", &path, Required::Yes);
            let case = if self.boolean(entry, "case_sensitive", &path) {
                CaseSensitivity::Sensitive
            } else {
                CaseSensitivity::Folded
            };
            let negate = self.boolean(entry, "negate", &path);
            if let (Some(field), Some(op), Some(value)) = (field, op, value.as_mut()) {
                if (op == Operator::DomainSuffix && field != Field::Domain)
                    || (matches!(field, Field::Category | Field::Tendency | Field::Activity)
                        && op != Operator::Equals)
                {
                    self.error(
                        DiagnosticCode::Op,
                        &format!("{path}.op"),
                        "operator is not valid for this field",
                    );
                }
                if op == Operator::Regex {
                    self.regex(value, &format!("{path}.value"), RuleKind::Classification);
                }
                if field == Field::Domain && op != Operator::Regex {
                    match DomainName::parse(value) {
                        Ok(domain) => *value = domain.as_ascii().to_owned(),
                        Err(_) => self.error(
                            DiagnosticCode::Value,
                            &format!("{path}.value"),
                            "invalid domain name",
                        ),
                    }
                }
                if field == Field::Tendency
                    && !["focus", "neutral", "distraction"].contains(&value.as_str())
                    || field == Field::Activity
                        && !["active", "idle", "locked", "suspended", "no_foreground"]
                            .contains(&value.as_str())
                {
                    self.error(
                        DiagnosticCode::Value,
                        &format!("{path}.value"),
                        "unknown enum value",
                    );
                }
                if field == Field::Category {
                    match self.context.references.categories.get(value) {
                        Some(uuid) => *value = uuid.clone(),
                        None => self.error(
                            DiagnosticCode::Ref,
                            &format!("{path}.value"),
                            "category does not exist",
                        ),
                    }
                }
                conditions.push(Condition {
                    field,
                    op,
                    value: value.clone(),
                    case,
                    negate,
                });
            }
        }
        conditions
    }
    fn rule(
        &mut self,
        entry: &Value,
        path: &str,
        kind: RuleKind,
        id: Option<String>,
        section: &str,
    ) -> Option<Rule> {
        let mut keys = vec!["label", "priority"];
        keys.push(if section == "rule" { "id" } else { "target" });
        match kind {
            RuleKind::Classification => keys.extend(["category", "match", "when"]),
            RuleKind::Extraction => keys.extend(["app", "pattern"]),
            RuleKind::Privacy => {
                keys.extend(["match", "when", "preset", "app", "window_title", "web"])
            }
            RuleKind::Precedent => {
                keys.extend(["match", "when", "max_continuous", "max_total_per_session"])
            }
            RuleKind::PolicyCheck => keys.extend([
                "policy", "kind", "window", "max", "within", "time", "match", "when",
            ]),
        }
        if self.context.origin == RuleOrigin::Builtin {
            keys.extend(["detector", "languages", "verified", "label_key"]);
        }
        self.keys(entry, path, &keys);
        let label_key = if self.context.origin == RuleOrigin::Builtin {
            self.string(entry, "label_key", path, Required::No)
        } else {
            None
        };
        let label = self.string(
            entry,
            "label",
            path,
            if label_key.is_some() {
                Required::No
            } else {
                Required::Yes
            },
        );
        let builtin = if self.context.origin == RuleOrigin::Builtin {
            let detector = self.string(entry, "detector", path, Required::No);
            let verified = entry
                .get("verified")
                .map(|_| self.boolean(entry, "verified", path));
            let mut languages = Vec::new();
            if let Some(value) = entry.get("languages") {
                if let Some(array) = value.as_array() {
                    for value in array {
                        if let Some(language) = value.as_str().filter(|s| !s.is_empty()) {
                            languages.push(language.into());
                        } else {
                            self.error(
                                DiagnosticCode::Value,
                                &format!("{path}.languages"),
                                "expected nonempty language strings",
                            );
                        }
                    }
                } else {
                    self.error(
                        DiagnosticCode::Value,
                        &format!("{path}.languages"),
                        "expected a language array",
                    );
                }
            }
            Some(BuiltinMetadata {
                detector,
                languages,
                verified,
                label_key,
            })
        } else {
            None
        };
        let priority = match entry.get("priority") {
            None => DEFAULT_PRIORITY,
            Some(value) => match value
                .as_u64()
                .filter(|value| *value <= u64::from(MAX_PRIORITY))
            {
                Some(value) => u16::try_from(value).expect("checked priority range fits in u16"),
                None => {
                    self.error(
                        DiagnosticCode::Value,
                        &format!("{path}.priority"),
                        "priority must be an integer from 0 to 1000",
                    );
                    // The set is rejected; this value only permits collecting other diagnostics.
                    DEFAULT_PRIORITY
                }
            },
        };
        let mode =
            self.string(entry, "match", path, Required::No)
                .map_or(MatchMode::All, |value| {
                    // Invalid modes are diagnosed before checking the remaining rule fields.
                    MatchMode::parse(&value).unwrap_or_else(|| {
                        self.error(
                            DiagnosticCode::Value,
                            &format!("{path}.match"),
                            "match must be all or any",
                        );
                        MatchMode::All
                    })
                });
        let body = match kind {
            RuleKind::Classification => {
                let category = self.required_string(entry, "category", path);
                let uuid = category
                    .as_ref()
                    .and_then(|category| self.context.references.categories.get(category).cloned());
                if uuid.is_none() {
                    self.error(
                        DiagnosticCode::Ref,
                        &format!("{path}.category"),
                        "category does not exist",
                    );
                }
                let when = self.conditions(entry, "when", path, kind);
                RuleBody::Classification {
                    category: CategoryReference { uuid: uuid? },
                    mode,
                    when,
                }
            }
            RuleKind::Extraction => {
                let app = self.conditions(entry, "app", path, kind);
                let pattern = self.required_string(entry, "pattern", path)?;
                self.regex(&pattern, &format!("{path}.pattern"), kind);
                RuleBody::Extraction { app, pattern }
            }
            RuleKind::Privacy => {
                let when = self.conditions(entry, "when", path, kind);
                let mut action = PrivacyAction::default();
                if let Some(preset) = self.string(entry, "preset", path, Required::No) {
                    if ["app", "window_title", "web"]
                        .iter()
                        .any(|key| entry.get(key).is_some())
                    {
                        self.error(
                            DiagnosticCode::Key,
                            &format!("{path}.preset"),
                            "preset cannot be combined with field actions",
                        );
                    }
                    action = match preset.as_str() {
                        "title_hidden" => PrivacyAction {
                            window_title: Action::Drop,
                            ..action
                        },
                        "private_window" => PrivacyAction {
                            window_title: Action::Drop,
                            web: WebAction::None,
                            ..action
                        },
                        "excluded" => PrivacyAction {
                            app: Action::Drop,
                            window_title: Action::Drop,
                            web: WebAction::None,
                        },
                        _ => {
                            self.error(
                                DiagnosticCode::Value,
                                &format!("{path}.preset"),
                                "unknown privacy preset",
                            );
                            action
                        }
                    };
                } else {
                    if !["app", "window_title", "web"]
                        .iter()
                        .any(|key| entry.get(key).is_some())
                    {
                        self.error(
                            DiagnosticCode::Key,
                            path,
                            "privacy rule requires a preset or field action",
                        );
                    }
                    for (key, destination) in [
                        ("app", &mut action.app),
                        ("window_title", &mut action.window_title),
                    ] {
                        if let Some(value) = self.string(entry, key, path, Required::No) {
                            if let Some(value) = Action::parse(&value) {
                                *destination = value;
                            } else {
                                self.error(
                                    DiagnosticCode::Value,
                                    &format!("{path}.{key}"),
                                    "expected keep or drop",
                                );
                            }
                        }
                    }
                    if let Some(value) = self.string(entry, "web", path, Required::No) {
                        if let Some(value) = WebAction::parse(&value) {
                            action.web = value;
                        } else {
                            self.error(
                                DiagnosticCode::Value,
                                &format!("{path}.web"),
                                "unknown web action",
                            );
                        }
                    }
                }
                RuleBody::Privacy { action, mode, when }
            }
            RuleKind::Precedent | RuleKind::PolicyCheck => {
                if entry.get("when").is_some() {
                    self.conditions(entry, "when", path, kind);
                }
                return None;
            }
        };
        Some(Rule {
            id: id?,
            label,
            priority,
            builtin,
            body,
        })
    }
}
