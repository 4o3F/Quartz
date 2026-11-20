//! Persistence and logging around the sans-IO registry core.

use crate::store::{
    Store,
    rules::{ExecuteRuleWritesError, LoadRulesError},
};
use quartz_domain::rules::registry::{
    CommitRuleDecisionError, CreateRuleRegistryError, DecideRegistryCommandError, DecisionParts,
    RegistryCommand, RegistrySuccess, ResetSnapshotEpochError, RuleRegistry, RuleWarning,
    RulesChanged, SnapshotCell,
};
use quartz_domain::rules::{self, ParseContext, ParseRuleSetError, RuleOrigin, RuleSet};
use snafu::{ResultExt, Snafu};

#[derive(Debug, Snafu)]
#[snafu(module(open_error))]
pub(crate) enum OpenRegistryShellError {
    #[snafu(display("could not read persisted registry state"))]
    Load { source: LoadRulesError },
    #[snafu(display("could not initialize registry core"))]
    Create { source: CreateRuleRegistryError },
    #[snafu(display("could not finish registry startup"))]
    FinishStartup { source: ResetSnapshotEpochError },
    #[snafu(display("embedded rules failed validation"))]
    Parse { source: ParseRuleSetError },
    #[snafu(display("could not install embedded rule revision"))]
    Upgrade { source: RunRegistryCommandError },
}

#[derive(Debug, Snafu)]
#[snafu(module(run_error))]
pub(crate) enum RunRegistryCommandError {
    #[snafu(display("rule command was rejected"))]
    Decide { source: DecideRegistryCommandError },
    #[snafu(display("rule writes did not commit"))]
    Persist { source: ExecuteRuleWritesError },
    #[snafu(display("could not publish committed rule state"))]
    Commit { source: CommitRuleDecisionError },
}

#[derive(Debug)]
pub(crate) struct RegistryResult {
    pub reply: RegistrySuccess,
    pub event: Option<RulesChanged>,
}

#[derive(Debug, Clone, Copy)]
struct BuiltinRuleSet<'source> {
    file: &'source str,
    source: &'source str,
    revision: u32,
}

const BUILTIN_RULE_SETS: &[BuiltinRuleSet<'static>] = &[
    BuiltinRuleSet {
        file: "builtin-privacy.toml",
        source: rules::BUILTIN_PRIVACY,
        revision: rules::BUILTIN_PRIVACY_REVISION,
    },
    BuiltinRuleSet {
        file: "builtin-classification.toml",
        source: rules::BUILTIN_CLASSIFICATION,
        revision: rules::BUILTIN_CLASSIFICATION_REVISION,
    },
];

/// Owns effects; command interpretation and publication remain in quartz-domain.
#[derive(Debug)]
pub(crate) struct RegistryShell {
    registry: RuleRegistry,
}
impl RegistryShell {
    pub(crate) fn open(
        store: &mut Store,
        now: quartz_domain::capture::UtcInstant,
    ) -> Result<Self, OpenRegistryShellError> {
        Self::open_with_builtins(store, now, BUILTIN_RULE_SETS)
    }
    fn open_with_builtins(
        store: &mut Store,
        now: quartz_domain::capture::UtcInstant,
        builtins: &[BuiltinRuleSet<'_>],
    ) -> Result<Self, OpenRegistryShellError> {
        let state = store.rule_sets().load().context(open_error::LoadSnafu)?;
        let registry = RuleRegistry::from_state(state).context(open_error::CreateSnafu)?;
        let mut shell = Self { registry };
        for builtin in builtins {
            let set = RuleSet::parse(
                builtin.file,
                builtin.source,
                ParseContext {
                    origin: RuleOrigin::Builtin,
                    references: &shell.registry.state().references,
                },
            )
            .context(open_error::ParseSnafu)?;
            shell
                .run(
                    store,
                    now,
                    RegistryCommand::InstallBuiltin {
                        set,
                        revision: builtin.revision,
                    },
                )
                .context(open_error::UpgradeSnafu)?;
        }
        shell.registry = shell
            .registry
            .finish_startup()
            .context(open_error::FinishStartupSnafu)?;
        shell.log_warnings();
        Ok(shell)
    }
    pub(crate) fn snapshots(&self) -> SnapshotCell {
        self.registry.snapshots()
    }
    pub(crate) fn core(&self) -> &RuleRegistry {
        &self.registry
    }

    pub(crate) fn run(
        &mut self,
        store: &mut Store,
        now: quartz_domain::capture::UtcInstant,
        command: RegistryCommand,
    ) -> Result<RegistryResult, RunRegistryCommandError> {
        let decision = self.registry.decide(command);
        let DecisionParts {
            writes,
            reply,
            outcome,
        } = decision.into_parts();
        let reply = reply.context(run_error::DecideSnafu)?;
        let event = if let Some(outcome) = outcome {
            let now = now.get().as_millisecond();
            store
                .transaction(|transaction| transaction.execute(&writes, now))
                .context(run_error::PersistSnafu)?;
            Some(
                self.registry
                    .committed(outcome)
                    .context(run_error::CommitSnafu)?,
            )
        } else {
            None
        };
        Ok(RegistryResult { reply, event })
    }
    fn log_warnings(&self) {
        for warning in self.snapshots().load().warnings() {
            match warning {
                RuleWarning::Compilation {
                    key,
                    version,
                    source,
                } => {
                    tracing::warn!(set = %key, version, %source, "disabling uncompilable stored rule set")
                }
                RuleWarning::Tree(diagnostic) => {
                    tracing::warn!(set = %diagnostic.set_key, code = diagnostic.code.as_str(), "installed rule tree needs user attention")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_directory::DataDirectory;
    use quartz_domain::{
        rules::{Action, Field, FieldSnapshot},
        text::NfcText,
    };
    use std::collections::BTreeMap;

    #[test]
    fn startup_installs_only_the_supplied_builtin_table() {
        let directory = tempfile::tempdir().expect("isolated rule database");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("builtin categories");
        let shell = RegistryShell::open_with_builtins(
            &mut store,
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
            &BUILTIN_RULE_SETS[1..],
        )
        .expect("classification-only table");
        let classification_key = "builtin.classification"
            .parse()
            .expect("classification key");
        assert_eq!(shell.core().state().sets.len(), 1);
        assert!(shell.core().state().sets.contains_key(&classification_key));
        assert_eq!(shell.snapshots().load().epoch().get(), 0);
        let stored = store.rule_sets().load().expect("persisted builtin table");
        assert_eq!(stored.sets.len(), 1);
        assert!(stored.sets.contains_key(&classification_key));
    }

    #[test]
    fn startup_installs_and_upgrades_builtin_before_publishing_and_keeps_user_overlay() {
        let directory = tempfile::tempdir().expect("isolated rule database");
        let data = DataDirectory::new(directory.path());
        let old_source =
            rules::BUILTIN_PRIVACY.replace("value = 'InPrivate'", "value = 'OldPrivate'");
        assert_ne!(
            old_source,
            rules::BUILTIN_PRIVACY,
            "fixture changes builtin contents"
        );
        let old_revision = rules::BUILTIN_PRIVACY_REVISION;
        let next_revision = old_revision.checked_add(1).expect("next builtin revision");
        let mut store = Store::open(&data).expect("store");
        store.seed_categories().expect("builtin categories");
        let now = quartz_domain::capture::UtcInstant::try_from(0).expect("UTC");
        let mut shell = RegistryShell::open_with_builtins(
            &mut store,
            now,
            &[
                BuiltinRuleSet {
                    source: &old_source,
                    revision: old_revision,
                    ..BUILTIN_RULE_SETS[0]
                },
                BUILTIN_RULE_SETS[1],
            ],
        )
        .expect("initial builtin installation");
        assert_eq!(shell.snapshots().load().epoch().get(), 0);
        let builtin_key = "builtin.privacy".parse().expect("builtin key");
        assert_eq!(shell.core().state().sets[&builtin_key].current.version, 1);
        shell
            .run(
                &mut store,
                now,
                RegistryCommand::Apply {
                    key: "user.overlay".parse().expect("overlay key"),
                    file: "overlay.toml".into(),
                    text: r"format='quartz-rules/1'
[set]
key='user.overlay'
kind='privacy'
name='Overlay'
parent='builtin.privacy'
[[override]]
target='builtin.privacy/browser-private-edge'
label='User replacement'
preset='excluded'
when=[{field='app.exe_name',op='equals',value='quartz-test.exe'}]
"
                    .into(),
                    base_version: 0,
                },
            )
            .expect("persist user overlay");
        drop(shell);
        let upgraded = RegistryShell::open_with_builtins(
            &mut Store::open(&data).expect("reopen store"),
            now,
            &[
                BuiltinRuleSet {
                    revision: next_revision,
                    ..BUILTIN_RULE_SETS[0]
                },
                BUILTIN_RULE_SETS[1],
            ],
        )
        .expect("startup builtin upgrade");
        let snapshot = upgraded.snapshots().load();
        assert_eq!(
            snapshot.epoch().get(),
            0,
            "first published snapshot includes the upgrade"
        );
        let state = upgraded.core().state();
        let builtin = &state.sets[&builtin_key];
        assert_eq!(builtin.builtin_revision, Some(next_revision));
        assert_eq!(builtin.current.version, 2);
        assert_ne!(
            builtin.versions[&1].content_hash(),
            builtin.versions[&2].content_hash()
        );
        let overlay_key = "user.overlay".parse().expect("overlay key");
        let overlay = &state.sets[&overlay_key];
        assert_eq!(overlay.current.version, 1);
        assert_eq!(overlay.versions[&1].parent_version, Some(1));
        assert_eq!(snapshot.upstream_changes().len(), 1);
        let fields = FieldSnapshot {
            text: BTreeMap::from([(Field::ExeName, NfcText::new("quartz-test.exe"))]),
            domain: None,
        };
        assert_eq!(
            snapshot.privacy().evaluate(&fields).privacy.app,
            Action::Drop
        );
        drop(upgraded);
        let reopened = RegistryShell::open_with_builtins(
            &mut Store::open(&data).expect("second reopen"),
            now,
            &[
                BuiltinRuleSet {
                    revision: next_revision,
                    ..BUILTIN_RULE_SETS[0]
                },
                BUILTIN_RULE_SETS[1],
            ],
        )
        .expect("idempotent startup");
        assert_eq!(reopened.snapshots().load().hashes(), snapshot.hashes());
        let stored = crate::store::ReadOnlyStore::open(&data.database())
            .expect("read-only rule database")
            .rule_sets()
            .load()
            .expect("persisted rules");
        assert_eq!(stored.sets[&builtin_key].versions.len(), 2);
        assert_eq!(stored.sets[&overlay_key].versions.len(), 1);
    }
}
