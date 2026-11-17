use super::ComputeUpstreamChangesError;
use crate::rules::{
    Content, ContentHash, Enablement, InstalledSet, ReferenceCatalog, Rule, RuleOrigin, RuleSet,
    RuleSetKey, RuleTarget,
};
use serde::{Deserialize, Serialize};
use snafu::ResultExt;
use std::collections::{BTreeMap, BTreeSet};

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
    /// Parent revision captured when these contents were authored.
    pub parent_version: Option<u32>,
    /// Optional revision annotation.
    pub note: Option<String>,
}
impl RuleSetVersion {
    /// Hashes the canonical contents that a persistence adapter writes.
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
    /// Compares saved parent revisions with current ancestor contents without side effects.
    ///
    /// # Errors
    /// Returns invalid identity or incomplete historical ancestry errors.
    pub fn upstream_changes(&self) -> Result<Vec<UpstreamChange>, ComputeUpstreamChangesError> {
        let mut changes = Vec::new();
        for (key, entry) in &self.sets {
            if entry.deletion == RuleSetDeletion::Deleted {
                continue;
            }
            let Some(parent) = &entry.current.set.header.parent else {
                continue;
            };
            let parent: RuleSetKey = parent.parse().context(super::upstream_error::SetKeySnafu)?;
            let parent = self
                .sets
                .get(&parent)
                .ok_or(ComputeUpstreamChangesError::MissingAncestor)?;
            let revision = entry
                .versions
                .get(&entry.current.version)
                .ok_or(ComputeUpstreamChangesError::MissingRevision)?;
            let Some(base_version) = revision.parent_version else {
                continue;
            };
            for target in entry
                .current
                .set
                .content
                .r#override
                .iter()
                .map(|entry| &entry.target)
                .chain(
                    entry
                        .current
                        .set
                        .content
                        .disable
                        .iter()
                        .map(|entry| &entry.target),
                )
            {
                let target: RuleTarget =
                    target.parse().context(super::upstream_error::TargetSnafu)?;
                if !self.sets.contains_key(target.set_key()) {
                    continue;
                }
                let (old, base_target_version) =
                    self.ancestor_rule(parent, base_version, &target, AncestryRevision::Saved)?;
                let (new, current_target_version) = self.ancestor_rule(
                    parent,
                    parent.current.version,
                    &target,
                    AncestryRevision::Current,
                )?;
                let state = if new.is_none() {
                    Some(UpstreamState::TargetDeleted)
                } else if old != new
                    && entry
                        .current
                        .set
                        .content
                        .r#override
                        .iter()
                        .any(|entry| entry.target == target.to_string())
                {
                    Some(UpstreamState::UpstreamChanged)
                } else {
                    None
                };
                if let Some(state) = state {
                    changes.push(UpstreamChange {
                        set_key: key.clone(),
                        target,
                        base_parent_version: Some(i64::from(base_version)),
                        current_parent_version: i64::from(parent.current.version),
                        overlay_version: i64::from(entry.current.version),
                        base_target_version: base_target_version.map(i64::from),
                        current_target_version: i64::from(
                            current_target_version.unwrap_or(parent.current.version),
                        ),
                        state,
                    });
                }
            }
        }
        Ok(changes)
    }
    fn ancestor_rule<'state>(
        &'state self,
        mut entry: &'state RuleSetState,
        mut version: u32,
        target: &RuleTarget,
        ancestry: AncestryRevision,
    ) -> Result<(Option<Rule>, Option<u32>), ComputeUpstreamChangesError> {
        let mut visited = BTreeSet::new();
        let target_name = target.to_string();
        let mut replacement = None;
        loop {
            if !visited.insert(&entry.current.set.header.key) {
                return Err(ComputeUpstreamChangesError::CyclicAncestry);
            }
            let revision = entry
                .versions
                .get(&version)
                .ok_or(ComputeUpstreamChangesError::MissingRevision)?;
            if replacement.is_none() {
                replacement = revision
                    .content
                    .r#override
                    .iter()
                    .find(|entry| entry.target == target_name)
                    .map(|entry| (entry.rule.clone(), version));
            }
            if entry.current.set.header.key == target.set_key().as_str() {
                let definition = revision
                    .content
                    .rule
                    .iter()
                    .find(|rule| rule.id == target.rule_id().as_str())
                    .cloned();
                return Ok(match (definition, replacement) {
                    (Some(_), Some((rule, version))) => (Some(rule), Some(version)),
                    (definition, _) => (definition, Some(version)),
                });
            }
            let (Some(parent), Some(parent_version)) =
                (&entry.current.set.header.parent, revision.parent_version)
            else {
                return Ok((None, None));
            };
            let parent: RuleSetKey = parent.parse().context(super::upstream_error::SetKeySnafu)?;
            entry = self
                .sets
                .get(&parent)
                .ok_or(ComputeUpstreamChangesError::MissingAncestor)?;
            version = match ancestry {
                AncestryRevision::Saved => parent_version,
                AncestryRevision::Current => entry.current.version,
            };
        }
    }
}
#[derive(Debug, Clone, Copy)]
enum AncestryRevision {
    Saved,
    Current,
}

/// Computed comparison of the saved parent revision with the current ancestor contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamChange {
    set_key: RuleSetKey,
    target: RuleTarget,
    base_parent_version: Option<i64>,
    current_parent_version: i64,
    overlay_version: i64,
    base_target_version: Option<i64>,
    current_target_version: i64,
    state: UpstreamState,
}
impl UpstreamChange {
    /// Returns the user overlay key affected by the upstream change.
    pub fn set_key(&self) -> &RuleSetKey {
        &self.set_key
    }
    /// Returns the qualified ancestor target being compared.
    pub fn target(&self) -> &RuleTarget {
        &self.target
    }
    /// Returns the parent revision saved with this overlay version.
    pub fn base_parent_version(&self) -> Option<i64> {
        self.base_parent_version
    }
    /// Returns the direct parent current revision.
    pub fn current_parent_version(&self) -> i64 {
        self.current_parent_version
    }
    /// Returns the user overlay revision carrying this target.
    pub fn overlay_version(&self) -> i64 {
        self.overlay_version
    }
    /// Returns the ancestor revision supplying the saved target contents.
    pub fn base_target_version(&self) -> Option<i64> {
        self.base_target_version
    }
    /// Returns the ancestor revision supplying the current target contents.
    pub fn current_target_version(&self) -> i64 {
        self.current_target_version
    }
    /// Returns whether the target was removed or changed.
    pub fn state(&self) -> UpstreamState {
        self.state
    }
}
/// Difference between a saved parent baseline and the current ancestor contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpstreamState {
    /// The originally defined target is absent from the current ancestor contents.
    TargetDeleted,
    /// The inherited target contents differ from the saved parent baseline.
    UpstreamChanged,
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
    /// Appends imported, edited, or reverted user contents.
    CommitVersion(RuleSetUpdate),
    /// Appends unchanged contents with a new enabled state.
    Toggle(RuleSetUpdate),
    /// Appends unchanged contents with a new sibling position.
    Reorder(RuleSetUpdate),
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
            | Self::Reorder(update)
            | Self::MarkDeleted(update)
            | Self::InstallBuiltin(update) => update,
        }
    }
}
