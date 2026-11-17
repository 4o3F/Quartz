//! Classification rules as the explain tree shows them (07 section 6).

/// Identifies a rule within one day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RuleId(u32);

impl RuleId {
    /// Wraps a rule id from the data source.
    #[must_use]
    pub(crate) const fn new(id: u32) -> Self {
        Self(id)
    }
}

/// A condition field of the rule format (03 section 2.6). The protocol mapping adds the others
/// as the core starts sending them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RuleField {
    AppExeName,
    WebDomain,
}

impl RuleField {
    /// The field as written in rule text, such as `app.exe_name`.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::AppExeName => "app.exe_name",
            Self::WebDomain => "web.domain",
        }
    }
}

/// A condition operator of the rule format (03 section 2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RuleOperator {
    Equals,
}

impl RuleOperator {
    /// The operator as written in rule text, such as `equals`.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Equals => "equals",
        }
    }
}

/// One condition: a field, an operator and the expected value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RuleCondition {
    pub(crate) field: RuleField,
    pub(crate) operator: RuleOperator,
    pub(crate) value: String,
}

/// A classification rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Rule {
    pub(crate) id: RuleId,
    /// Key of the rule set that defines the rule, such as `builtin.classification`.
    pub(crate) rule_set: String,
    pub(crate) name: String,
    pub(crate) priority: u32,
    pub(crate) condition: RuleCondition,
}

/// Why a segment got its category: the rule and the field value it matched.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Explanation {
    pub(crate) rule: Rule,
    /// The value the segment had in the rule's field.
    pub(crate) actual: String,
}
