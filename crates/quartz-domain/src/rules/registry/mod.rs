//! Rule decisions and RCU publication without database, clock, channel, or thread access.

mod epoch;
mod snapshot;
mod state;

pub use epoch::SnapshotEpoch;
pub use snapshot::{
    BuildRuleSnapshotError, ResetSnapshotEpochError, RuleSnapshot, RuleWarning, RulesChanged,
    SiblingRevision, SnapshotCell, SnapshotHashes,
};
pub use state::{
    ExpectedRuleVersion, RegistryState, RuleSetDeletion, RuleSetState, RuleSetUpdate,
    RuleSetVersion, RuleSetWrite, UpstreamChange, UpstreamState,
};

use crate::rules::{
    self, ContentHash, Enablement, InstalledSet, ParseContext, ParseRuleSetError, RuleKind,
    RuleOrigin, RuleSet, RuleSetKey, RuleWriteCode,
};
use snafu::{ResultExt, Snafu};
use snapshot::BuildPolicy;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

/// First persisted revision of a new user rule set.
const INITIAL_REVISION: u32 = 1;

/// A client uses zero to request creation of a missing or deleted rule set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseVersion {
    New,
    Version(u32),
}
impl From<u32> for BaseVersion {
    fn from(version: u32) -> Self {
        match version {
            0 => Self::New,
            version => Self::Version(version),
        }
    }
}

/// A registry operation containing values only; the shell owns any reply channel.
#[derive(Debug)]
pub enum RegistryCommand {
    /// Validates editor text and appends a user revision.
    Apply {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
        /// Source file label used in structured diagnostic locations.
        file: PathBuf,
        /// Complete quartz-rules/1 source to validate.
        text: String,
        /// Current revision observed by the client; zero denotes a new or deleted key.
        base_version: u32,
    },
    /// Imports text after validating its base revision and privacy preview.
    Import {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
        /// Source file label used in structured diagnostic locations.
        file: PathBuf,
        /// Complete quartz-rules/1 source to validate.
        text: String,
        /// Current revision observed by the client; zero denotes a new or deleted key.
        base_version: u32,
        /// Privacy input hash returned by the corresponding preview.
        preview_hash: Option<ContentHash>,
    },
    /// Appends a revision with the requested subtree enablement.
    Toggle {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
        /// Current revision observed by the client; zero denotes a new or deleted key.
        base_version: u32,
        /// Requested subtree enablement.
        enabled: Enablement,
    },
    /// Appends revisions for a complete sibling ordering.
    Reorder {
        /// Complete sibling ordering with each client-observed revision.
        siblings: Vec<SiblingRevision>,
    },
    /// Marks a childless user set deleted while preserving history.
    Delete {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
        /// Current revision observed by the client; zero denotes a new or deleted key.
        base_version: u32,
    },
    /// Appends the selected historical contents as a new revision.
    Revert {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
        /// Current revision observed by the client; zero denotes a new or deleted key.
        base_version: u32,
        /// Rule-set revision identified by this value.
        version: u32,
    },
    /// Installs a newer embedded upstream revision without editing overlays.
    InstallBuiltin {
        /// Parsed embedded contents supplied by the application.
        set: RuleSet,
        /// Embedded upstream revision supplied by the application.
        revision: u32,
    },
}

/// Successful command reply, including its publication epoch and derived alerts.
#[derive(Debug)]
pub struct RegistrySuccess {
    /// Process-local snapshot publication sequence.
    pub epoch: SnapshotEpoch,
    /// New current revisions by key; deleted keys are omitted.
    pub versions: BTreeMap<RuleSetKey, u32>,
    /// Computed alerts comparing saved parent revisions with current ancestors.
    pub upstream_changes: Vec<UpstreamChange>,
}
/// Command success or a structured, source-preserving rejection.
pub type RegistryReply = Result<RegistrySuccess, DecideRegistryCommandError>;

/// A saved parent chain cannot be interpreted from the supplied domain state.
#[derive(Debug, Snafu)]
#[snafu(module(upstream_error))]
pub enum ComputeUpstreamChangesError {
    /// Upstream rule-set key is invalid.
    #[snafu(display("upstream rule-set key is invalid"))]
    SetKey {
        /// Original failure retained as the standard error source.
        source: rules::ParseRuleSetKeyError,
    },
    /// Upstream rule target is invalid.
    #[snafu(display("upstream rule target is invalid"))]
    Target {
        /// Original failure retained as the standard error source.
        source: rules::ParseRuleTargetError,
    },
    /// Saved parent rule set is missing.
    #[snafu(display("saved parent rule set is missing"))]
    MissingAncestor,
    /// Saved parent revision is missing.
    #[snafu(display("saved parent revision is missing"))]
    MissingRevision,
    /// Saved parent chain is cyclic.
    #[snafu(display("saved parent chain is cyclic"))]
    CyclicAncestry,
}

/// A command could not produce a valid write plan.
#[derive(Debug, Snafu)]
#[snafu(module(command_error))]
pub enum DecideRegistryCommandError {
    /// The client base revision differs from the current active revision.
    #[snafu(display("rule set {key} changed since revision {base_version}"))]
    Stale {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
        /// Current revision observed by the client; zero denotes a new or deleted key.
        base_version: u32,
        /// Current active revision, absent for a missing or deleted key.
        current_version: Option<u32>,
    },
    /// Rule source failed validation.
    #[snafu(display("rule source failed validation"))]
    Parse {
        /// Original failure retained as the standard error source.
        source: ParseRuleSetError,
    },
    /// Rule-set key is invalid.
    #[snafu(display("rule-set key is invalid"))]
    Identity {
        /// Original failure retained as the standard error source.
        source: rules::ParseRuleSetKeyError,
    },
    /// Candidate rules failed validation or compilation.
    #[snafu(display("candidate rules failed validation or compilation"))]
    Snapshot {
        /// Original failure retained as the standard error source.
        source: BuildRuleSnapshotError,
    },
    /// Rule set does not exist.
    #[snafu(display("rule set does not exist"))]
    MissingSet,
    /// Retained rule revision does not exist.
    #[snafu(display("retained rule revision does not exist"))]
    MissingRevision,
    /// Source key differs from command target.
    #[snafu(display("source key differs from command target"))]
    IdentityMismatch,
    /// Builtin rules must be edited through a user overlay.
    #[snafu(display("builtin rules must be edited through a user overlay"))]
    ReservedSet,
    /// Ordering must include every sibling exactly once.
    #[snafu(display("ordering must include every sibling exactly once"))]
    InvalidOrdering,
    /// Privacy import requires a current preview.
    #[snafu(display("privacy import requires a current preview"))]
    StalePreview,
    /// Rule revision exceeds the supported range.
    #[snafu(display("rule revision exceeds the supported range"))]
    RevisionOverflow,
    /// Sibling position exceeds the persisted integer range.
    #[snafu(display("rule sibling order exceeds the supported range"))]
    SiblingOrderOverflow {
        /// Checked conversion failure.
        source: std::num::TryFromIntError,
    },
    /// Rule publication epoch exhausted.
    #[snafu(display("rule publication epoch exhausted"))]
    EpochOverflow,
    /// An existing rule set cannot change its parent.
    #[snafu(display("an existing rule set cannot change its parent"))]
    ReparentSet,
    /// A rule set with active children cannot be deleted.
    #[snafu(display("a rule set with active children cannot be deleted"))]
    HasChildren,
    /// Builtin contents changed without a revision bump.
    #[snafu(display("builtin contents changed without a revision bump"))]
    RevisionConflict,
    /// Builtin installation requires a supported root and positive revision.
    #[snafu(display("builtin installation requires a supported root and positive revision"))]
    InvalidBuiltin,
}
impl DecideRegistryCommandError {
    /// Returns a stable IPC code for an optimistic concurrency failure.
    pub fn code(&self) -> Option<RuleWriteCode> {
        match self {
            Self::Stale { .. } => Some(RuleWriteCode::RuleSetStale),
            _ => None,
        }
    }
}

/// Supplied persisted state could not initialize the registry.
#[derive(Debug, Snafu)]
#[snafu(module(create_error))]
pub enum CreateRuleRegistryError {
    /// Current revision is missing for rule set.
    #[snafu(display("current revision is missing for rule set {key}"))]
    MissingRevision {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
    },
    /// Current contents differ from their retained revision for.
    #[snafu(display("current contents differ from their retained revision for {key}"))]
    ContentMismatch {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
    },
    /// Registry key differs from the current header for.
    #[snafu(display("registry key differs from the current header for {key}"))]
    IdentityMismatch {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
    },
    /// Builtin revision does not match import authority for.
    #[snafu(display("builtin revision does not match import authority for {key}"))]
    InvalidOrigin {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
    },
    /// Could not build initial rule snapshot.
    #[snafu(display("could not build initial rule snapshot"))]
    Snapshot {
        /// Original failure retained as the standard error source.
        source: BuildRuleSnapshotError,
    },
}

/// A validated, immutable write plan and its reply, pending persistence.
#[derive(Debug)]
#[must_use]
pub struct Decision {
    writes: Vec<RuleSetWrite>,
    reply: RegistryReply,
    outcome: Option<CommitOutcome>,
}
/// Named outputs of a registry decision, consumed by the persistence shell.
#[derive(Debug)]
pub struct DecisionParts {
    /// Atomic writes to execute before acknowledging the candidate.
    pub writes: Vec<RuleSetWrite>,
    /// Command success or its structured rejection.
    pub reply: RegistryReply,
    /// Opaque candidate state to acknowledge after a successful commit.
    pub outcome: Option<CommitOutcome>,
}
impl Decision {
    /// Borrows the exact writes that must succeed in one transaction.
    pub fn writes(&self) -> &[RuleSetWrite] {
        &self.writes
    }
    /// Borrows the success or rejection without consuming the plan.
    pub fn reply(&self) -> &RegistryReply {
        &self.reply
    }
    /// Consumes the plan into write descriptions, its reply, and an opaque commit token.
    pub fn into_parts(self) -> DecisionParts {
        DecisionParts {
            writes: self.writes,
            reply: self.reply,
            outcome: self.outcome,
        }
    }
}
/// Opaque candidate state returned to the core after its writes commit.
#[derive(Debug)]
#[must_use]
pub struct CommitOutcome {
    state: RegistryState,
    base_epoch: SnapshotEpoch,
}

impl CommitOutcome {
    /// Records the shell-supplied transaction timestamp in newly committed revisions.
    pub fn record_commit_time(&mut self, writes: &[RuleSetWrite], created_utc_ms: i64) {
        for write in writes {
            let update = write.update();
            self.state
                .sets
                .get_mut(&update.key)
                .expect("decision contains written set")
                .versions
                .get_mut(&update.revision.version)
                .expect("decision contains written revision")
                .created_utc_ms = Some(created_utc_ms);
        }
    }
}

/// A commit acknowledgement was stale or its snapshot could not be rebuilt.
#[derive(Debug, Snafu)]
#[snafu(module(commit_error))]
pub enum CommitRuleDecisionError {
    /// A newer publication makes this acknowledgement stale.
    #[snafu(display(
        "commit acknowledgement was based on epoch {expected}; current epoch is {current}"
    ))]
    StaleEpoch {
        /// Publication epoch on which the acknowledgement was based.
        expected: SnapshotEpoch,
        /// Current publication epoch that rejects the delayed acknowledgement.
        current: SnapshotEpoch,
    },
    /// Could not rebuild committed rule snapshot.
    #[snafu(display("could not rebuild committed rule snapshot"))]
    Snapshot {
        /// Original failure retained as the standard error source.
        source: BuildRuleSnapshotError,
    },
}

/// In-memory single-writer core; persistence and notification belong to its shell.
#[derive(Debug)]
pub struct RuleRegistry {
    state: RegistryState,
    snapshots: SnapshotCell,
}
impl RuleRegistry {
    /// Constructs epoch zero from persisted values, retaining warnings for uncompilable sets.
    ///
    /// # Errors
    /// Returns inconsistent state or snapshot reconstruction failures.
    pub fn from_state(state: RegistryState) -> Result<Self, CreateRuleRegistryError> {
        for (key, entry) in &state.sets {
            let version = entry
                .versions
                .get(&entry.current.version)
                .ok_or_else(|| CreateRuleRegistryError::MissingRevision { key: key.clone() })?;
            if version.version != entry.current.version
                || version.content != entry.current.set.content
            {
                return Err(CreateRuleRegistryError::ContentMismatch { key: key.clone() });
            }
            if entry.current.set.header.key != key.as_str() {
                return Err(CreateRuleRegistryError::IdentityMismatch { key: key.clone() });
            }
            if !matches!(
                (entry.origin, entry.builtin_revision),
                (RuleOrigin::Builtin, Some(1..)) | (RuleOrigin::User, None)
            ) {
                return Err(CreateRuleRegistryError::InvalidOrigin { key: key.clone() });
            }
        }
        let snapshot =
            RuleSnapshot::build(&state, SnapshotEpoch::initial(), BuildPolicy::Startup, None)
                .context(create_error::SnapshotSnafu)?;
        Ok(Self {
            state,
            snapshots: SnapshotCell::new(snapshot),
        })
    }
    /// Starts publication at epoch zero using the snapshot compiled during builtin installation.
    ///
    /// # Errors
    /// Rejects startup completion if any snapshot or read handle is still retained.
    pub fn finish_startup(self) -> Result<Self, ResetSnapshotEpochError> {
        Ok(Self {
            state: self.state,
            snapshots: self.snapshots.into_initial()?,
        })
    }
    /// Borrows all current and retained state without loading any external data.
    pub fn state(&self) -> &RegistryState {
        &self.state
    }
    /// Replaces lookup aliases for future commands after category metadata commits.
    /// Existing rule contents and snapshots retain their resolved category UUIDs.
    pub fn update_references(&mut self, references: rules::ReferenceCatalog) {
        self.state.references = references;
    }
    /// Clones a read handle whose loads retain one immutable snapshot per work unit.
    pub fn snapshots(&self) -> SnapshotCell {
        self.snapshots.clone()
    }
    /// Computes a write plan without mutating state, epoch, or the published snapshot.
    pub fn decide(&self, command: RegistryCommand) -> Decision {
        match self.try_decide(command) {
            Ok(decision) => decision,
            Err(error) => Decision {
                writes: Vec::new(),
                reply: Err(error),
                outcome: None,
            },
        }
    }
    /// Publishes a previously validated candidate after the shell commits its writes.
    ///
    /// # Errors
    /// Rejects acknowledgements based on an old epoch or failed snapshot reconstruction.
    pub fn committed(
        &mut self,
        outcome: CommitOutcome,
    ) -> Result<RulesChanged, CommitRuleDecisionError> {
        let current = self.snapshots.load();
        if current.epoch() != outcome.base_epoch {
            return Err(CommitRuleDecisionError::StaleEpoch {
                expected: outcome.base_epoch,
                current: current.epoch(),
            });
        }
        // decide checked the epoch increment and compiled this exact candidate before persistence.
        let epoch = outcome
            .base_epoch
            .next()
            .expect("decide checked the snapshot epoch increment");
        let snapshot =
            RuleSnapshot::build(&outcome.state, epoch, BuildPolicy::Startup, Some(&current))
                .context(commit_error::SnapshotSnafu)?;
        self.state = outcome.state;
        self.snapshots.publish(snapshot);
        Ok(RulesChanged { epoch })
    }
    fn try_decide(&self, command: RegistryCommand) -> Result<Decision, DecideRegistryCommandError> {
        let current = self.snapshots.load();
        let epoch = current
            .epoch()
            .next()
            .ok_or(DecideRegistryCommandError::EpochOverflow)?;
        let mut state = self.state.clone();
        let mut writes = Vec::new();
        let policy = if matches!(command, RegistryCommand::InstallBuiltin { .. }) {
            BuildPolicy::BuiltinUpgrade
        } else {
            BuildPolicy::Strict
        };
        match command {
            RegistryCommand::Apply {
                key,
                file,
                text,
                base_version,
            } => {
                state.check_base(&key, base_version)?;
                let set = RuleSet::parse(
                    &file.to_string_lossy(),
                    &text,
                    ParseContext {
                        origin: RuleOrigin::User,
                        references: &state.references,
                    },
                )
                .context(command_error::ParseSnafu)?;
                if set.header.key != key.as_str() {
                    return Err(DecideRegistryCommandError::IdentityMismatch);
                }
                writes.push(state.save_user(set)?);
            }
            RegistryCommand::Import {
                key,
                file,
                text,
                base_version,
                preview_hash,
            } => {
                state.check_base(&key, base_version)?;
                let set = RuleSet::parse(
                    &file.to_string_lossy(),
                    &text,
                    ParseContext {
                        origin: RuleOrigin::User,
                        references: &state.references,
                    },
                )
                .context(command_error::ParseSnafu)?;
                if set.header.key != key.as_str() {
                    return Err(DecideRegistryCommandError::IdentityMismatch);
                }
                if set.header.kind == RuleKind::Privacy
                    && preview_hash.as_ref() != Some(&current.hashes().privacy)
                {
                    return Err(DecideRegistryCommandError::StalePreview);
                }
                writes.push(state.save_user(set)?);
            }
            RegistryCommand::Revert {
                key,
                base_version,
                version,
            } => {
                state.check_base(&key, base_version)?;
                let mut set = state.active(&key)?.current.set.clone();
                set.content = state
                    .version(&key, version)
                    .ok_or(DecideRegistryCommandError::MissingRevision)?
                    .content
                    .clone();
                writes.push(state.save_user(set)?);
            }
            RegistryCommand::Toggle {
                key,
                base_version,
                enabled,
            } => {
                state.check_base(&key, base_version)?;
                state.active(&key)?;
                let update = state.metadata(&key, Some(enabled), None, RuleSetDeletion::Active)?;
                writes.push(RuleSetWrite::Toggle(update));
            }
            RegistryCommand::Reorder { siblings } => {
                let mut seen = BTreeSet::new();
                for sibling in &siblings {
                    state.check_base(&sibling.key, sibling.base_version)?;
                    if !seen.insert(sibling.key.clone()) {
                        return Err(DecideRegistryCommandError::InvalidOrdering);
                    }
                }
                let first = siblings
                    .first()
                    .ok_or(DecideRegistryCommandError::InvalidOrdering)?;
                let first = state.active(&first.key)?;
                let expected: BTreeSet<_> = state
                    .sets
                    .iter()
                    .filter(|(_, entry)| {
                        entry.deletion == RuleSetDeletion::Active
                            && entry.current.set.header.parent == first.current.set.header.parent
                            && entry.current.set.header.kind == first.current.set.header.kind
                    })
                    .map(|(key, _)| key.clone())
                    .collect();
                if seen != expected {
                    return Err(DecideRegistryCommandError::InvalidOrdering);
                }
                for (order, sibling) in siblings.into_iter().enumerate() {
                    let update = state.metadata(
                        &sibling.key,
                        None,
                        Some(
                            i64::try_from(order)
                                .context(command_error::SiblingOrderOverflowSnafu)?,
                        ),
                        RuleSetDeletion::Active,
                    )?;
                    writes.push(RuleSetWrite::Reorder(update));
                }
            }
            RegistryCommand::Delete { key, base_version } => {
                state.check_base(&key, base_version)?;
                if state.active(&key)?.origin == RuleOrigin::Builtin {
                    return Err(DecideRegistryCommandError::ReservedSet);
                }
                if state.sets.values().any(|entry| {
                    entry.deletion == RuleSetDeletion::Active
                        && entry.current.set.header.parent.as_deref() == Some(key.as_str())
                }) {
                    return Err(DecideRegistryCommandError::HasChildren);
                }
                let update = state.metadata(
                    &key,
                    Some(Enablement::Disabled),
                    None,
                    RuleSetDeletion::Deleted,
                )?;
                writes.push(RuleSetWrite::MarkDeleted(update));
            }
            RegistryCommand::InstallBuiltin { set, revision } => {
                let key: RuleSetKey = set
                    .header
                    .key
                    .parse()
                    .context(command_error::IdentitySnafu)?;
                if !key.as_str().starts_with("builtin.")
                    || set.header.parent.is_some()
                    || revision == 0
                    || matches!(set.header.kind, RuleKind::Precedent | RuleKind::PolicyCheck)
                {
                    return Err(DecideRegistryCommandError::InvalidBuiltin);
                }
                if let Some(old) = state.sets.get(&key) {
                    if old.origin != RuleOrigin::Builtin {
                        return Err(DecideRegistryCommandError::ReservedSet);
                    }
                    let upstream = old
                        .builtin_revision
                        .ok_or(DecideRegistryCommandError::InvalidBuiltin)?;
                    if revision <= upstream {
                        if revision == upstream
                            && old.current.set.validate_compilation().is_ok()
                            && old.current.set.content != set.content
                        {
                            return Err(DecideRegistryCommandError::RevisionConflict);
                        }
                        return Ok(Decision {
                            writes,
                            reply: Ok(RegistrySuccess {
                                epoch: current.epoch(),
                                versions: BTreeMap::new(),
                                upstream_changes: current.upstream_changes().to_vec(),
                            }),
                            outcome: None,
                        });
                    }
                }
                let old = state.sets.get(&key);
                let version = match old {
                    Some(entry) => entry
                        .current
                        .version
                        .checked_add(1)
                        .ok_or(DecideRegistryCommandError::RevisionOverflow)?
                        .max(revision),
                    None => revision,
                };
                let update =
                    state.content_update(set, version, RuleOrigin::Builtin, Some(revision))?;
                state.apply_update(&update);
                writes.push(RuleSetWrite::InstallBuiltin(update));
            }
        }
        let edited: BTreeSet<_> = writes
            .iter()
            .filter_map(|write| match write {
                RuleSetWrite::CommitVersion(update) => Some(update.key.as_str()),
                _ => None,
            })
            .collect();
        let installed = state.installed_sets();
        let diagnostics: Vec<_> = [
            RuleKind::Privacy,
            RuleKind::Classification,
            RuleKind::Extraction,
        ]
        .into_iter()
        .flat_map(|kind| rules::Resolution::resolve(&installed, kind).diagnostics)
        .filter(|diagnostic| {
            diagnostic.code == rules::DiagnosticCode::Target
                && edited.contains(diagnostic.set_key.as_str())
        })
        .collect();
        if !diagnostics.is_empty() {
            return Err(DecideRegistryCommandError::Snapshot {
                source: BuildRuleSnapshotError::Tree { diagnostics },
            });
        }
        let candidate = RuleSnapshot::build(&state, epoch, policy, Some(&current))
            .context(command_error::SnapshotSnafu)?;
        let versions = writes
            .iter()
            .filter(|write| !matches!(write, RuleSetWrite::MarkDeleted(_)))
            .map(|write| (write.update().key.clone(), write.update().revision.version))
            .collect();
        Ok(Decision {
            writes,
            reply: Ok(RegistrySuccess {
                epoch,
                versions,
                upstream_changes: candidate.upstream_changes().to_vec(),
            }),
            outcome: Some(CommitOutcome {
                state,
                base_epoch: current.epoch(),
            }),
        })
    }
}

impl RegistryState {
    fn active(&self, key: &RuleSetKey) -> Result<&RuleSetState, DecideRegistryCommandError> {
        self.sets
            .get(key)
            .filter(|entry| entry.deletion == RuleSetDeletion::Active)
            .ok_or(DecideRegistryCommandError::MissingSet)
    }
    fn check_base(
        &self,
        key: &RuleSetKey,
        base_version: u32,
    ) -> Result<(), DecideRegistryCommandError> {
        let current_version = self
            .sets
            .get(key)
            .filter(|entry| entry.deletion == RuleSetDeletion::Active)
            .map(|entry| entry.current.version);
        if current_version.map_or(BaseVersion::New, BaseVersion::Version)
            != BaseVersion::from(base_version)
        {
            return Err(DecideRegistryCommandError::Stale {
                key: key.clone(),
                base_version,
                current_version,
            });
        }
        Ok(())
    }
    fn save_user(&mut self, set: RuleSet) -> Result<RuleSetWrite, DecideRegistryCommandError> {
        let key: RuleSetKey = set
            .header
            .key
            .parse()
            .context(command_error::IdentitySnafu)?;
        if key.as_str().starts_with("builtin.") {
            return Err(DecideRegistryCommandError::ReservedSet);
        }
        let old = self.sets.get(&key);
        if old.is_some_and(|entry| entry.origin != RuleOrigin::User) {
            return Err(DecideRegistryCommandError::ReservedSet);
        }
        if old.is_some_and(|entry| entry.current.set.header.parent != set.header.parent) {
            return Err(DecideRegistryCommandError::ReparentSet);
        }
        // New keys start at the initial revision; retained histories advance from their last revision.
        let version = old
            .map(|entry| {
                entry
                    .current
                    .version
                    .checked_add(1)
                    .ok_or(DecideRegistryCommandError::RevisionOverflow)
            })
            .transpose()?
            .unwrap_or(INITIAL_REVISION);
        let update = self.content_update(set, version, RuleOrigin::User, None)?;
        self.apply_update(&update);
        Ok(RuleSetWrite::CommitVersion(update))
    }
    fn content_update(
        &self,
        set: RuleSet,
        version: u32,
        origin: RuleOrigin,
        builtin_revision: Option<u32>,
    ) -> Result<RuleSetUpdate, DecideRegistryCommandError> {
        let key: RuleSetKey = set
            .header
            .key
            .parse()
            .context(command_error::IdentitySnafu)?;
        let old = self.sets.get(&key);
        let parent_version = set
            .header
            .parent
            .as_ref()
            .map(|parent| -> Result<u32, DecideRegistryCommandError> {
                let parent = parent.parse().context(command_error::IdentitySnafu)?;
                Ok(self.active(&parent)?.current.version)
            })
            .transpose()?;
        let enabled = old
            .filter(|entry| entry.deletion == RuleSetDeletion::Active)
            .map_or(Enablement::Enabled, |entry| entry.current.enabled);
        let sibling_order = match old {
            Some(entry) => entry.current.sibling_order,
            None if origin == RuleOrigin::Builtin => 0,
            None => i64::try_from(self.installed_sets().len())
                .context(command_error::SiblingOrderOverflowSnafu)?,
        };
        Ok(RuleSetUpdate {
            key,
            revision: RuleSetVersion {
                version,
                content: set.content.clone(),
                parent_version,
                note: None,
                created_utc_ms: None,
            },
            set,
            origin,
            expected: old.map_or(ExpectedRuleVersion::Absent, |entry| {
                ExpectedRuleVersion::Version(entry.current.version)
            }),
            enabled,
            sibling_order,
            builtin_revision,
            deletion: RuleSetDeletion::Active,
        })
    }
    fn metadata(
        &mut self,
        key: &RuleSetKey,
        enabled: Option<Enablement>,
        order: Option<i64>,
        deletion: RuleSetDeletion,
    ) -> Result<RuleSetUpdate, DecideRegistryCommandError> {
        let old = self.active(key)?;
        let version = old
            .current
            .version
            .checked_add(1)
            .ok_or(DecideRegistryCommandError::RevisionOverflow)?;
        let mut revision = old
            .versions
            .get(&old.current.version)
            .ok_or(DecideRegistryCommandError::MissingRevision)?
            .clone();
        revision.version = version;
        revision.created_utc_ms = None;
        revision.note = Some("rule-set state changed".into());
        let update = RuleSetUpdate {
            key: key.clone(),
            set: old.current.set.clone(),
            revision,
            origin: old.origin,
            expected: ExpectedRuleVersion::Version(old.current.version),
            // Absent metadata edits retain the persisted values.
            enabled: enabled.unwrap_or(old.current.enabled),
            sibling_order: order.unwrap_or(old.current.sibling_order),
            builtin_revision: old.builtin_revision,
            deletion,
        };
        self.apply_update(&update);
        Ok(update)
    }
    fn apply_update(&mut self, update: &RuleSetUpdate) {
        let mut versions = self
            .sets
            .remove(&update.key)
            .map_or_else(BTreeMap::new, |entry| entry.versions);
        versions.insert(update.revision.version, update.revision.clone());
        self.sets.insert(
            update.key.clone(),
            RuleSetState {
                current: InstalledSet {
                    set: update.set.clone(),
                    version: update.revision.version,
                    enabled: update.enabled,
                    sibling_order: update.sibling_order,
                },
                origin: update.origin,
                builtin_revision: update.builtin_revision,
                deletion: update.deletion,
                versions,
            },
        );
    }
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RuleSnapshot>();
    assert_send_sync::<crate::rules::CompiledRules>();
    assert_send_sync::<SnapshotCell>();
};

#[cfg(test)]
mod tests;
