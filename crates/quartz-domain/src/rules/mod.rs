//! Typed rule files, deterministic tree resolution, and bounded offline evaluation.

mod content_hash;
mod diagnostic;
mod evaluate;
mod identity;
mod model;
mod parse;
pub mod registry;
mod resolve;

pub use content_hash::{ContentHash, ParseContentHashError};
pub use diagnostic::{Diagnostic, DiagnosticCode, SourceLocation};
pub use evaluate::{
    ClassificationMatch, CompileRulesError, CompiledRules, ConditionOutcome, Evaluation,
    ExplanationId, Extraction, ExtractionStatus, FieldSnapshot, ParseExplanationIdError,
    PrivacyLabelSnapshot, PrivacySources, RuleOutcome, RuleStatus, ShadowReason,
};
pub use identity::{
    ParseRuleIdError, ParseRuleSetKeyError, ParseRuleTargetError, RuleId, RuleSetKey, RuleTarget,
    RuleVersion, RuleWriteCode,
};
pub use model::{
    Action, BuiltinMetadata, CaseSensitivity, CategoryReference, Condition, Content, Disable,
    EntryPosition, Field, MatchMode, Operator, Override, PrivacyAction, Rule, RuleBody, RuleKind,
    RuleOrigin, RuleSet, SetHeader, WebAction,
};
pub use parse::{ParseContext, ParseRuleSetError, ReferenceCatalog};
pub use resolve::{
    EffectiveRule, EffectiveRules, Enablement, InstalledSet, OverrideRef, Resolution,
    RuleProvenance, SuppressedRule, SuppressionReason,
};

/// Embedded privacy rules; all language variants are evaluated together.
pub const BUILTIN_PRIVACY: &str = include_str!("builtin-privacy.toml");
/// Revision installed in the database at core startup.
pub const BUILTIN_PRIVACY_REVISION: u32 = 1;

/// Embedded application classification rules, using currently captured fields.
pub const BUILTIN_CLASSIFICATION: &str = include_str!("builtin-classification.toml");
/// Initial published classification revision.
pub const BUILTIN_CLASSIFICATION_REVISION: u32 = 1;

/// Embedded document, project, and browser-profile title extractors.
pub const BUILTIN_EXTRACTION: &str = include_str!("builtin-extraction.toml");
/// Initial published extraction revision.
pub const BUILTIN_EXTRACTION_REVISION: u32 = 1;

#[cfg(test)]
mod tests;
