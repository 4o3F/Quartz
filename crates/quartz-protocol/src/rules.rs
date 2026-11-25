//! Rule-set reads and atomic mutation contracts.

use crate::{QueryCursor, QueryVersions, Uuid};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

mod diff;
pub use diff::{
    DiffMutation, RuleComparison, RuleDiffCategoryDelta, RuleDiffChange, RuleDiffChangeKind,
    RuleDiffEntry, RuleDiffImpact, RuleDiffSet, RuleDiffSides, RulesTextDiffRequest,
    RulesTextDiffResponse,
};
mod preview;
pub use preview::{
    FurtherHidden, PREVIEW_DEFAULT_DEADLINE_MS, PREVIEW_MAX_DAYS, PREVIEW_MAX_DEADLINE_MS,
    PREVIEW_MAX_SAMPLES, ParsePreviewDeadlineError, ParsePreviewSampleLimitError,
    PreviewCategoryRank, PreviewDeadlineMs, PreviewDelete, PreviewReorder, PreviewRevert,
    PreviewSampleLimit, PreviewSetEnabled, RuleConditionMatch, RuleEffectiveContribution,
    RulePreviewMutation, RulePreviewOutput, RulePreviewRequest, RulePreviewSample,
    RulePreviewSideTotals, RulePreviewStatistics, RulePreviewTotals, RulesPreviewRequest,
    RulesPreviewResponse, RulesTextPreviewMutation, RulesTextPreviewRequest,
    RulesTextPreviewResponse, ValidatePreviewDatesError,
};

/// Rule families available to the M4 editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Classification,
    Extraction,
    Privacy,
}
/// Contents authored in a set or the resolved root tree containing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleView {
    Own,
    Effective,
}
/// Reads all current nodes of one rule family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSetsTreeRequest {
    pub kind: RuleKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<QueryCursor>,
}
/// Reads a current set or one retained revision, including deleted history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSetsGetRequest {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<NonZeroU32>,
    pub view: RuleView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<QueryCursor>,
}
/// Lists retained revisions, including revisions preceding deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSetsVersionsRequest {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<QueryCursor>,
}
/// Saves complete quartz-rules/1 text; zero creates or restores a key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSetsEditRequest {
    pub key: String,
    pub base_version: u32,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_receipt: Option<String>,
}
impl std::fmt::Debug for RuleSetsEditRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleSetsEditRequest")
            .field("key", &self.key)
            .field("base_version", &self.base_version)
            .field("text", &"[redacted]")
            .field("preview_receipt", &"[redacted]")
            .finish()
    }
}

/// Validates text against an observed installed revision without saving it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesTextValidateRequest {
    pub key: String,
    pub base_version: u32,
    pub text: String,
}
impl std::fmt::Debug for RulesTextValidateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RulesTextValidateRequest")
            .field("key", &self.key)
            .field("base_version", &self.base_version)
            .field("text", &"[redacted]")
            .finish()
    }
}

/// Content diagnostics from a contextual dry run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesTextValidateResponse {
    pub valid: bool,
    pub diagnostics: Vec<RuleDiagnostic>,
    pub truncated: bool,
}
/// Exports current or retained contents, including deleted history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesTextExportRequest {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<NonZeroU32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<QueryCursor>,
}
/// A UTF-8 fragment of deterministic quartz-rules/1 text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesTextExportResponse {
    pub text: String,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Changes enablement of a user set and its effective subtree.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSetsSetEnabledRequest {
    pub key: String,
    pub base_version: NonZeroU32,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_receipt: Option<String>,
}
impl std::fmt::Debug for RuleSetsSetEnabledRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleSetsSetEnabledRequest")
            .field("key", &self.key)
            .field("base_version", &self.base_version)
            .field("enabled", &self.enabled)
            .field("preview_receipt", &"[redacted]")
            .finish()
    }
}

/// One observed revision in a complete sibling ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSetSibling {
    pub key: String,
    pub base_version: NonZeroU32,
}
/// Replaces the entire same-parent, same-kind sibling order atomically.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSetsReorderRequest {
    pub siblings: Vec<RuleSetSibling>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_receipt: Option<String>,
}
impl std::fmt::Debug for RuleSetsReorderRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleSetsReorderRequest")
            .field("siblings", &self.siblings)
            .field("preview_receipt", &"[redacted]")
            .finish()
    }
}

/// Deletes a childless user set while retaining its history.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSetsDeleteRequest {
    pub key: String,
    pub base_version: NonZeroU32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_receipt: Option<String>,
}
impl std::fmt::Debug for RuleSetsDeleteRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleSetsDeleteRequest")
            .field("key", &self.key)
            .field("base_version", &self.base_version)
            .field("preview_receipt", &"[redacted]")
            .finish()
    }
}

/// Copies historical contents into a new current revision.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSetsRevertRequest {
    pub key: String,
    pub base_version: NonZeroU32,
    pub version: NonZeroU32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_receipt: Option<String>,
}
impl std::fmt::Debug for RuleSetsRevertRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleSetsRevertRequest")
            .field("key", &self.key)
            .field("base_version", &self.base_version)
            .field("version", &self.version)
            .field("preview_receipt", &"[redacted]")
            .finish()
    }
}

/// A committed revision, including the revision recording a deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutatedRuleSet {
    pub key: String,
    pub version: u32,
    pub deleted: bool,
}
/// Atomic mutation result with the same registry digest used by rule reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetsWriteResponse {
    pub sets: Vec<MutatedRuleSet>,
    pub versions: QueryVersions,
}
/// Immutable portion of an existing rule-set identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSetIdentityField {
    Kind,
    Parent,
}
/// Located content diagnostic; parameters contain only structured values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDiagnostic {
    pub code: String,
    pub path: String,
    pub location: Option<RuleDiagnosticLocation>,
    pub params: std::collections::BTreeMap<String, String>,
}
/// One-based source coordinates without source text or local file paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDiagnosticLocation {
    pub line: usize,
    pub column: usize,
}
/// Maximum UTF-8 bytes in one rule-set edit or import (04 §6.3.3).
pub const MAX_RULE_TEXT_BYTES: usize = 192 * 1024;
/// Maximum diagnostics in one validation response (04 §6.3.3).
pub const MAX_RULE_DIAGNOSTICS: usize = 100;

/// Authority of a rule set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSetSource {
    Builtin,
    User,
}
/// Entry counts authored directly in this set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetCounts {
    pub added: usize,
    pub overridden: usize,
    pub disabled: usize,
}
/// Stable editor warning codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleWarningCode {
    TargetDeleted,
    UpstreamChanged,
    Conflict,
}
/// An overlay warning and its qualified ancestor target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetWarning {
    pub code: RuleWarningCode,
    pub target: String,
}
/// One current tree node, in sibling-order and key preorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetNode {
    pub key: String,
    pub parent: Option<String>,
    pub version: u32,
    pub enabled: bool,
    pub sibling_order: i64,
    pub source: RuleSetSource,
    pub counts: RuleSetCounts,
    pub warnings: Vec<RuleSetWarning>,
}
/// A family tree page; privacy effective_version is the historical deletion scope hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetsTreeResponse {
    pub kind: RuleKind,
    pub effective_version: String,
    pub nodes: Vec<RuleSetNode>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Source section of a canonical entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleEntryKind {
    Rule,
    Override,
    Disable,
}
/// Final participation of an authored entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleEntryState {
    Effective,
    Disabled,
    Overridden,
    TargetMissing,
}
/// A defining or replacing revision, ordered from definition to latest overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleProvenance {
    pub key: String,
    pub version: u32,
    pub id: String,
}
/// Format comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOperator {
    Equals,
    Contains,
    StartsWith,
    EndsWith,
    Regex,
    DomainSuffix,
}
/// Format condition combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMatch {
    All,
    Any,
}
/// Format retention for application and title fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    Keep,
    Drop,
}
/// Format retention for web fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleWebAction {
    Full,
    DomainOnly,
    None,
}
/// Fields recognized by the rule format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleField {
    #[serde(rename = "app.exe_name")]
    ExeName,
    #[serde(rename = "app.identity_key")]
    IdentityKey,
    #[serde(rename = "app.display_name")]
    DisplayName,
    #[serde(rename = "window.class")]
    WindowClass,
    #[serde(rename = "title.raw")]
    RawTitle,
    #[serde(rename = "title.normalized")]
    NormalizedTitle,
    #[serde(rename = "doc.document")]
    Document,
    #[serde(rename = "doc.project")]
    Project,
    #[serde(rename = "doc.profile")]
    Profile,
    #[serde(rename = "web.domain")]
    Domain,
    #[serde(rename = "web.tab_title")]
    TabTitle,
    #[serde(rename = "browser.profile_label")]
    BrowserProfile,
    #[serde(rename = "category")]
    Category,
    #[serde(rename = "category.tendency")]
    Tendency,
    #[serde(rename = "activity")]
    Activity,
}
/// Canonical format condition; category values remain stored UUID text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleCondition {
    pub field: RuleField,
    pub op: RuleOperator,
    pub value: String,
    pub case_sensitive: bool,
    pub negate: bool,
}
/// Rule-format fields shared by definitions, overrides, and disable directives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleEntryContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#match: Option<RuleMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Vec<RuleCondition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<RuleApp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<RuleAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<RuleWebAction>,
}
/// The format uses app for extraction predicates or privacy retention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuleApp {
    Conditions(Vec<RuleCondition>),
    Action(RuleAction),
}
/// Canonical entry with final state and definition/replacement chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetEntry {
    pub entry_kind: RuleEntryKind,
    pub content: RuleEntryContent,
    pub provenance: Vec<RuleProvenance>,
    pub state: RuleEntryState,
}
/// Contents page bound to every registry and metadata input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetsGetResponse {
    pub key: String,
    pub version: u32,
    pub parent_version: Option<u32>,
    pub entries: Vec<RuleSetEntry>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Immutable saved revision metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetRevision {
    pub version: u32,
    pub parent_version: Option<u32>,
    pub created_utc_ms: i64,
    pub note: Option<String>,
}
/// Revision page in descending version order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetsVersionsResponse {
    pub revisions: Vec<RuleSetRevision>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}

#[cfg(test)]
mod tests;
