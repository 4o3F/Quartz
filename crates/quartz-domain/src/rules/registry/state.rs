use crate::rules::{
    Content, ContentHash, Enablement, InstalledSet, ReferenceCatalog, RuleOrigin, RuleSet,
    RuleSetKey,
};
use std::collections::BTreeMap;

/// Whether a retained rule set participates in the current tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSetDeletion {
    /// Includes the set according to its enabled flag and ancestors.
    Active,
    /// Hides the set while retaining every historical revision.
    Deleted,
}

/// One immutable canonical revision and the parent revision it was based on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSetVersion {
    /// Revision number within this rule set.
    pub version: u32,
    /// Canonical rule definitions and overlays.
    pub content: Content,
    /// Saved header; absent only in revisions written before header snapshots.
    pub header: Option<crate::rules::SetHeader>,
    /// Parent revision captured when these contents were authored.
    pub parent_version: Option<u32>,
    /// Optional revision annotation.
    pub note: Option<String>,
    /// UTC milliseconds assigned by the shell; absent while a decision awaits persistence.
    pub created_utc_ms: Option<i64>,
}
impl RuleSetVersion {
    /// Hashes canonical entries for rule-content comparisons, excluding the saved header.
    pub fn content_hash(&self) -> ContentHash {
        self.content.content_hash()
    }
}

/// Current tree metadata and all retained revisions of one set.
#[derive(Debug, Clone)]
pub struct RuleSetState {
    /// Current parsed contents, revision, enablement, and sibling position.
    pub current: InstalledSet,
    /// Import authority recorded independently of the key prefix.
    pub origin: RuleOrigin,
    /// Embedded upstream revision, present only for builtin sets.
    pub builtin_revision: Option<u32>,
    /// Whether the set is visible in the active tree.
    pub deletion: RuleSetDeletion,
    /// Immutable revisions, including the current revision and deleted contents.
    pub versions: BTreeMap<u32, RuleSetVersion>,
}

/// Complete bounded registry state loaded by the shell, including deleted history.
#[derive(Debug, Clone, Default)]
pub struct RegistryState {
    /// Sets indexed by their validated durable keys.
    pub sets: BTreeMap<RuleSetKey, RuleSetState>,
    /// Category identities available when validating imported text.
    pub references: ReferenceCatalog,
}
impl RegistryState {
    /// Returns current, non-deleted tree inputs without compiling them.
    pub fn installed_sets(&self) -> Vec<InstalledSet> {
        self.sets
            .values()
            .filter(|entry| entry.deletion == RuleSetDeletion::Active)
            .map(|entry| entry.current.clone())
            .collect()
    }
    /// Reads any retained revision, including a deleted set's history.
    pub fn version(&self, key: &RuleSetKey, version: u32) -> Option<&RuleSetVersion> {
        self.sets.get(key)?.versions.get(&version)
    }
}

/// Exact after-image of one revision, with no database identifiers or timestamps.
#[derive(Debug, Clone)]
pub struct RuleSetUpdate {
    /// Stable key identifying the persisted row.
    pub key: RuleSetKey,
    /// Current header and source locations for in-memory validation.
    pub set: RuleSet,
    /// Immutable revision to append.
    pub revision: RuleSetVersion,
    /// Stored import authority.
    pub origin: RuleOrigin,
    /// Expected persisted state before this write.
    pub expected: ExpectedRuleVersion,
    /// Current enablement after this write.
    pub enabled: Enablement,
    /// Current sibling position after this write.
    pub sibling_order: i64,
    /// Embedded revision after this write, present only for builtin sets.
    pub builtin_revision: Option<u32>,
    /// Current deletion state after this write.
    pub deletion: RuleSetDeletion,
}
/// Persistence precondition, including retained revisions of deleted sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedRuleVersion {
    /// Creates a key that has never been persisted.
    Absent,
    /// Updates a row still at this revision.
    Version(u32),
}
/// Domain descriptions executed together by a persistence adapter.
#[derive(Debug, Clone)]
pub enum RuleSetWrite {
    /// Appends imported or edited user contents.
    CommitVersion(RuleSetUpdate),
    /// Appends unchanged contents with a new enabled state.
    Toggle(RuleSetUpdate),
    /// Retains the row and history while marking it deleted.
    MarkDeleted(RuleSetUpdate),
    /// Appends a builtin version and records its upstream revision.
    InstallBuiltin(RuleSetUpdate),
}
impl RuleSetWrite {
    /// Borrows the exact after-image to persist without interpreting the command.
    pub fn update(&self) -> &RuleSetUpdate {
        match self {
            Self::CommitVersion(update)
            | Self::Toggle(update)
            | Self::MarkDeleted(update)
            | Self::InstallBuiltin(update) => update,
        }
    }
}
