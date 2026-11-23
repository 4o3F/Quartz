use super::{ComputeUpstreamChangesError, RegistryState, SnapshotEpoch, UpstreamChange};
use crate::rules::{
    self, CompileRulesError, CompiledRules, ContentHash, Diagnostic, DiagnosticCode, Enablement,
    Resolution, RuleKind, RuleSetKey,
};
use arc_swap::ArcSwap;
use snafu::{ResultExt, Snafu};
use std::sync::Arc;

/// Hashes used by queries and caches independently of a process's publication epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotHashes {
    /// Effective privacy-list hash.
    pub privacy: ContentHash,
    /// Effective classification-list hash.
    pub classification: ContentHash,
    /// Effective extraction-list hash.
    pub extraction: ContentHash,
}

/// A stored set excluded from evaluation, or a tree diagnostic requiring user attention.
#[derive(Debug)]
pub enum RuleWarning {
    /// Excludes a stored set whose expressions no longer compile.
    Compilation {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
        /// Rule-set revision identified by this value.
        version: u32,
        /// Original failure retained as the standard error source.
        source: CompileRulesError,
    },
    /// Retains a tree diagnostic requiring user attention.
    Tree(Diagnostic),
}

/// Immutable compiled inputs; a work unit retains one Arc for its entire evaluation.
#[derive(Debug)]
pub struct RuleSnapshot {
    epoch: SnapshotEpoch,
    privacy: CompiledRules,
    classification: CompiledRules,
    extraction: CompiledRules,
    hashes: SnapshotHashes,
    warnings: Vec<RuleWarning>,
    upstream_changes: Vec<UpstreamChange>,
    state: RegistryState,
    resolutions: Vec<Resolution>,
}
impl RuleSnapshot {
    /// Returns the publication sequence within this registry process.
    pub fn epoch(&self) -> SnapshotEpoch {
        self.epoch
    }
    /// Borrows compiled privacy rules for this snapshot.
    pub fn privacy(&self) -> &CompiledRules {
        &self.privacy
    }
    /// Borrows compiled classification rules for this snapshot.
    pub fn classification(&self) -> &CompiledRules {
        &self.classification
    }
    /// Borrows compiled extraction rules for this snapshot.
    pub fn extraction(&self) -> &CompiledRules {
        &self.extraction
    }
    /// Borrows effective-list hashes used by queries and caches.
    pub fn hashes(&self) -> &SnapshotHashes {
        &self.hashes
    }
    /// Borrows structured startup compilation and tree warnings.
    pub fn warnings(&self) -> &[RuleWarning] {
        &self.warnings
    }

    /// Borrows derived alerts for the snapshot or command result.
    pub fn upstream_changes(&self) -> &[UpstreamChange] {
        &self.upstream_changes
    }

    /// Borrows the exact registry history published with these compiled rules.
    pub fn state(&self) -> &RegistryState {
        &self.state
    }
    /// Borrows shared resolver output for one supported rule family.
    pub fn resolution(&self, kind: RuleKind) -> Option<&Resolution> {
        self.resolutions
            .iter()
            .find(|resolution| resolution.effective.kind == kind)
    }

    pub(super) fn build(
        installed: &RegistryState,
        epoch: SnapshotEpoch,
        policy: BuildPolicy,
        baseline: Option<&RuleSnapshot>,
    ) -> Result<Self, BuildRuleSnapshotError> {
        let mut sets = installed.installed_sets();
        let mut warnings = Vec::new();
        for entry in &mut sets {
            if let Err(source) = entry.set.validate_compilation() {
                let key = entry
                    .set
                    .header
                    .key
                    .parse()
                    .context(snapshot_error::IdentitySnafu)?;
                let already_disabled = baseline.is_some_and(|snapshot| snapshot.warnings.iter().any(|warning| matches!(warning,
                    RuleWarning::Compilation { key: old_key, version, .. } if old_key == &key && *version == entry.version)));
                match policy {
                    _ if policy == BuildPolicy::Startup || already_disabled => {
                        warnings.push(RuleWarning::Compilation {
                            key,
                            version: entry.version,
                            source,
                        });
                        entry.enabled = Enablement::Disabled;
                    }
                    _ => {
                        return Err(BuildRuleSnapshotError::Compile { key, source });
                    }
                }
            }
        }
        let mut families = Vec::new();
        let mut resolutions = Vec::new();
        for kind in [
            RuleKind::Privacy,
            RuleKind::Classification,
            RuleKind::Extraction,
        ] {
            let resolution = Resolution::resolve(&sets, kind);
            let blocking: Vec<_> = resolution
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    let existing = baseline.is_some_and(|snapshot| snapshot.warnings.iter().any(|warning| matches!(warning,
                        RuleWarning::Tree(old) if old.code == diagnostic.code && old.set_key == diagnostic.set_key && old.path == diagnostic.path && old.location == diagnostic.location)));
                    !existing && (policy == BuildPolicy::Strict || diagnostic.code != DiagnosticCode::Target)
                })
                .cloned()
                .collect();
            if !blocking.is_empty() && policy != BuildPolicy::Startup {
                return Err(BuildRuleSnapshotError::Tree {
                    diagnostics: blocking,
                });
            }
            warnings.extend(
                resolution
                    .diagnostics
                    .iter()
                    .cloned()
                    .map(RuleWarning::Tree),
            );
            let hash = resolution.effective.version_hash.clone();
            let compiled = resolution
                .effective
                .compile()
                .context(snapshot_error::EffectiveSnafu)?;
            families.push((compiled, hash));
            resolutions.push(resolution);
        }
        let (extraction, extraction_hash) = families.pop().expect("three rule families");
        let (classification, classification_hash) =
            families.pop().expect("two rule families remain");
        let (privacy, privacy_hash) = families.pop().expect("privacy family remains");
        Ok(Self {
            epoch,
            state: installed.clone(),
            resolutions,
            privacy,
            classification,
            extraction,
            hashes: SnapshotHashes {
                privacy: privacy_hash,
                classification: classification_hash,
                extraction: extraction_hash,
            },
            warnings,
            upstream_changes: installed
                .upstream_changes()
                .context(snapshot_error::UpstreamSnafu)?,
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BuildPolicy {
    Strict,
    BuiltinUpgrade,
    Startup,
}

/// Tree or expression failures encountered while constructing a rule snapshot.
#[derive(Debug, Snafu)]
#[snafu(module(snapshot_error))]
pub enum BuildRuleSnapshotError {
    /// Could not compile rule set.
    #[snafu(display("could not compile rule set {key}"))]
    Compile {
        /// Stable key of the rule set affected by this value.
        key: RuleSetKey,
        /// Original failure retained as the standard error source.
        source: CompileRulesError,
    },
    /// Could not compile effective rules.
    #[snafu(display("could not compile effective rules"))]
    Effective {
        /// Original failure retained as the standard error source.
        source: CompileRulesError,
    },
    /// Rule tree failed validation.
    #[snafu(display("rule tree failed validation"))]
    Tree {
        /// Structured tree diagnostics with their original source locations.
        diagnostics: Vec<Diagnostic>,
    },
    /// Stored rule-set identity is invalid.
    #[snafu(display("stored rule-set identity is invalid"))]
    Identity {
        /// Original failure retained as the standard error source.
        source: rules::ParseRuleSetKeyError,
    },
    /// Could not compute upstream comparisons.
    #[snafu(display("could not compute upstream comparisons"))]
    Upstream {
        /// Original failure retained as the standard error source.
        source: ComputeUpstreamChangesError,
    },
}

/// Startup retained a snapshot or a read handle before publication began.
#[derive(Debug, Snafu)]
#[snafu(display("could not reset snapshot epoch while readers are retained"))]
pub struct ResetSnapshotEpochError;

/// Cloneable read handle. Only this module can publish a replacement.
#[derive(Debug, Clone)]
pub struct SnapshotCell(Arc<ArcSwap<RuleSnapshot>>);
impl SnapshotCell {
    pub(super) fn new(snapshot: RuleSnapshot) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(snapshot)))
    }
    /// Atomically retains one immutable snapshot for a complete work unit.
    pub fn load(&self) -> Arc<RuleSnapshot> {
        self.0.load_full()
    }
    pub(super) fn into_initial(self) -> Result<Self, ResetSnapshotEpochError> {
        let snapshots = Arc::try_unwrap(self.0).map_err(|_| ResetSnapshotEpochError)?;
        let mut snapshot =
            Arc::try_unwrap(snapshots.into_inner()).map_err(|_| ResetSnapshotEpochError)?;
        snapshot.epoch = SnapshotEpoch::initial();
        Ok(Self::new(snapshot))
    }
    pub(super) fn publish(&self, snapshot: RuleSnapshot) {
        self.0.store(Arc::new(snapshot));
    }
}

/// Signals a published snapshot; readers also compare epochs directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RulesChanged {
    /// Process-local snapshot publication sequence.
    pub epoch: SnapshotEpoch,
}

/// A checked sibling revision participating in a complete sibling ordering.
#[derive(Debug, Clone)]
pub struct SiblingRevision {
    /// Stable key of the rule set affected by this value.
    pub key: RuleSetKey,
    /// Current revision observed by the client; zero denotes a new or deleted key.
    pub base_version: u32,
}
