//! Persistence and logging around the sans-IO registry core.

use crate::store::{
    Store,
    rules::{ExecuteRuleWritesError, LoadRulesError},
};
use quartz_domain::rules::registry::{
    CommitRuleDecisionError, CreateRuleRegistryError, DecideRegistryCommandError, RegistryCommand,
    RegistrySuccess, RuleRegistry, RuleWarning, RulesChanged, SnapshotCell,
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
    #[snafu(display("embedded privacy rules failed validation"))]
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

/// Owns effects; command interpretation and publication remain in quartz-domain.
#[derive(Debug)]
pub(crate) struct RegistryShell {
    store: Store,
    registry: RuleRegistry,
}
impl RegistryShell {
    pub(crate) fn open(mut store: Store) -> Result<Self, OpenRegistryShellError> {
        let state = store.rule_sets().load().context(open_error::LoadSnafu)?;
        let registry = RuleRegistry::from_state(state).context(open_error::CreateSnafu)?;
        let builtin = RuleSet::parse(
            "builtin-privacy.toml",
            rules::BUILTIN_PRIVACY,
            ParseContext {
                origin: RuleOrigin::Builtin,
                references: &registry.state().references,
            },
        )
        .context(open_error::ParseSnafu)?;
        let mut shell = Self { store, registry };
        shell
            .run(RegistryCommand::InstallBuiltin {
                set: builtin,
                revision: rules::BUILTIN_PRIVACY_REVISION,
            })
            .context(open_error::UpgradeSnafu)?;
        // Startup exposes epoch zero only after embedded upgrades have committed.
        shell.registry = RuleRegistry::from_state(shell.registry.state().clone())
            .context(open_error::CreateSnafu)?;
        shell.log_warnings();
        Ok(shell)
    }
    pub(crate) fn snapshots(&self) -> SnapshotCell {
        self.registry.snapshots()
    }
    pub(crate) fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }
    pub(crate) fn core(&self) -> &RuleRegistry {
        &self.registry
    }

    pub(crate) fn run(
        &mut self,
        command: RegistryCommand,
    ) -> Result<RegistryResult, RunRegistryCommandError> {
        let decision = self.registry.decide(command);
        let (writes, reply, outcome) = decision.into_parts();
        let reply = reply.context(run_error::DecideSnafu)?;
        let event = if let Some(outcome) = outcome {
            let now = jiff::Timestamp::now().as_millisecond();
            self.store
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
