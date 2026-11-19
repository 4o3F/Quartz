use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::{fmt, str::FromStr};

/// Stable rule-set key using the rule format's lowercase ASCII vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RuleSetKey(String);

/// A rule-set key is empty or contains characters outside its fixed vocabulary.
#[derive(Debug, Snafu)]
#[snafu(display("invalid rule-set key"))]
pub struct ParseRuleSetKeyError;

impl FromStr for RuleSetKey {
    type Err = ParseRuleSetKeyError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl TryFrom<String> for RuleSetKey {
    type Error = ParseRuleSetKeyError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte)
            })
        {
            return Err(ParseRuleSetKeyError);
        }
        Ok(Self(value))
    }
}
impl RuleSetKey {
    /// Borrows the rule-set key used in files and database rows.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for RuleSetKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl From<RuleSetKey> for String {
    fn from(key: RuleSetKey) -> Self {
        key.0
    }
}

/// A persisted revision number within one rule set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleVersion(u32);
impl RuleVersion {
    /// Carries a revision already supplied by the registry or persistence adapter.
    #[must_use]
    pub const fn new(version: u32) -> Self {
        Self(version)
    }
    /// Returns the number at a persistence or protocol boundary.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}
impl fmt::Display for RuleVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Rule identifier local to its defining set, preserving ASCII letter case.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RuleId(String);

/// A local rule identifier is empty or contains unsupported characters.
#[derive(Debug, Snafu)]
#[snafu(display("invalid rule identifier"))]
pub struct ParseRuleIdError;

impl FromStr for RuleId {
    type Err = ParseRuleIdError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl TryFrom<String> for RuleId {
    type Error = ParseRuleIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        {
            return Err(ParseRuleIdError);
        }
        Ok(Self(value))
    }
}
impl RuleId {
    /// Borrows the local rule identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl From<RuleId> for String {
    fn from(id: RuleId) -> Self {
        id.0
    }
}

/// Qualified ancestor rule name, serialized as `<set-key>/<rule-id>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RuleTarget {
    set_key: RuleSetKey,
    rule_id: RuleId,
}
/// A qualified rule name has an invalid separator, set key, or local identifier.
#[derive(Debug, Snafu)]
pub enum ParseRuleTargetError {
    /// The set and local identifier are not separated by a slash.
    #[snafu(display("rule target must contain a set key and rule identifier"))]
    MissingSeparator,
    /// The set component is not a valid rule-set key.
    #[snafu(display("invalid target rule-set key"))]
    SetKey {
        /// Original key validation failure.
        source: ParseRuleSetKeyError,
    },
    /// The rule component is not a valid local identifier.
    #[snafu(display("invalid target rule identifier"))]
    RuleId {
        /// Original identifier validation failure.
        source: ParseRuleIdError,
    },
}
impl RuleTarget {
    /// Constructs a qualified name from validated identity components.
    pub fn new(set_key: RuleSetKey, rule_id: RuleId) -> Self {
        Self { set_key, rule_id }
    }
    /// Returns the defining ancestor's stable key.
    pub fn set_key(&self) -> &RuleSetKey {
        &self.set_key
    }
    /// Returns the identifier local to the ancestor.
    pub fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }
}
impl FromStr for RuleTarget {
    type Err = ParseRuleTargetError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (set, rule) = value
            .split_once('/')
            .ok_or(ParseRuleTargetError::MissingSeparator)?;
        Ok(Self::new(
            set.parse().context(SetKeySnafu)?,
            rule.parse().context(RuleIdSnafu)?,
        ))
    }
}
impl TryFrom<String> for RuleTarget {
    type Error = ParseRuleTargetError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl fmt::Display for RuleTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.set_key, self.rule_id)
    }
}
impl From<RuleTarget> for String {
    fn from(target: RuleTarget) -> Self {
        target.to_string()
    }
}

/// Stable rule-write outcome codes for a future IPC response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleWriteCode {
    /// The submitted base revision differs from the current rule-set revision.
    RuleSetStale,
}
impl RuleWriteCode {
    /// Returns the exact IPC error-code spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuleSetStale => "rule_set_stale",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_validate_and_preserve_string_wire_format() {
        let target: RuleTarget = "builtin.privacy/browser-private"
            .parse()
            .expect("qualified name");
        assert_eq!(target.set_key().as_str(), "builtin.privacy");
        assert_eq!(target.rule_id().as_str(), "browser-private");
        let json = serde_json::to_string(&target).expect("string target");
        assert_eq!(json, "\"builtin.privacy/browser-private\"");
        assert_eq!(
            target,
            serde_json::from_str(&json).expect("target round trip")
        );
        for invalid in ["", "Set/rule", "user.set/", "user.set/a/b", "user.set/中文"] {
            assert!(invalid.parse::<RuleTarget>().is_err(), "{invalid}");
        }
        assert_eq!(
            serde_json::to_string(&RuleWriteCode::RuleSetStale).expect("code"),
            "\"rule_set_stale\""
        );
    }
}
