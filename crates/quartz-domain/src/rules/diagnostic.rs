use serde::{Deserialize, Serialize};
use std::{fmt, ops::Range, path::PathBuf};

/// Stable validation codes defined by quartz-rules/1 section 4.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DiagnosticCode {
    /// `E-RULE-FORMAT` (rule-format.md §4.2): missing or unsupported format version.
    Format,
    /// `E-RULE-KEY` (rule-format.md §4.2): unknown key or missing required key.
    Key,
    /// `E-RULE-SET` (rule-format.md §4.2): invalid or reserved set key, unknown kind, or parent kind mismatch.
    Set,
    /// `E-RULE-PARENT` (rule-format.md §4.2): missing parent rule set or a cycle in the parent chain.
    Parent,
    /// `E-RULE-TARGET` (rule-format.md §4.2): missing override or disable target, or a target outside the ancestor chain.
    Target,
    /// `E-RULE-CONFLICT` (rule-format.md §4.2): unrelated rule sets override or disable the same target.
    Conflict,
    /// `E-RULE-IMMUTABLE` (rule-format.md §4.2): a precedent was changed or deleted, or uses override or disable.
    Immutable,
    /// `E-RULE-FIELD` (rule-format.md §4.2): unknown field or a field unavailable to the rule kind.
    Field,
    /// `E-RULE-OP` (rule-format.md §4.2): unknown operator or an operator unavailable to the field.
    Op,
    /// `E-RULE-VALUE` (rule-format.md §4.2): empty or excessive value, invalid duration or time, or unknown enum value.
    Value,
    /// `E-RULE-REGEX` (rule-format.md §4.2): invalid regex, unsupported syntax, or an exceeded compilation size limit.
    Regex,
    /// `E-RULE-REF` (rule-format.md §4.2): referenced category, chain, or policy does not exist.
    Ref,
    /// `E-RULE-DUP` (rule-format.md §4.2): duplicate rule identifier within one rule set.
    Dup,
}
impl DiagnosticCode {
    /// Returns the stable external code, independent of message wording.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Format => "E-RULE-FORMAT",
            Self::Key => "E-RULE-KEY",
            Self::Set => "E-RULE-SET",
            Self::Parent => "E-RULE-PARENT",
            Self::Target => "E-RULE-TARGET",
            Self::Conflict => "E-RULE-CONFLICT",
            Self::Immutable => "E-RULE-IMMUTABLE",
            Self::Field => "E-RULE-FIELD",
            Self::Op => "E-RULE-OP",
            Self::Value => "E-RULE-VALUE",
            Self::Regex => "E-RULE-REGEX",
            Self::Ref => "E-RULE-REF",
            Self::Dup => "E-RULE-DUP",
        }
    }
}

/// A byte span and one-based line and Unicode scalar column in a source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    /// Source file path, or a database revision identifier for installed rules.
    pub file: PathBuf,
    /// Half-open byte range in the original UTF-8 source.
    pub bytes: Range<usize>,
    /// One-based line containing the start of the byte range.
    pub line: usize,
    /// One-based Unicode scalar column containing the start of the byte range.
    pub column: usize,
}
impl SourceLocation {
    pub(super) fn new(file: &str, source: &str, bytes: Range<usize>) -> Self {
        let prefix = &source[..bytes.start.min(source.len())];
        Self {
            file: file.into(),
            line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
            column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
            bytes,
        }
    }
}

/// A structured diagnostic with an independent code, logical path, and source location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable validation code used to classify the failure independently of its message.
    pub code: DiagnosticCode,
    /// Rule-set key, or the source file name when no key could be parsed.
    pub set_key: String,
    /// Logical field path, such as `rule[0].when[1].field`; empty for a file-level error.
    pub path: String,
    /// Original source span and the starting line and column.
    pub location: SourceLocation,
    /// Human-readable explanation printed after the code and logical path.
    pub message: String,
}
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "error[{}]: {}: {}: {}\n  {}:{}:{}",
            self.code.as_str(),
            self.set_key,
            self.path,
            self.message,
            self.location.file.display(),
            self.location.line,
            self.location.column
        )
    }
}
