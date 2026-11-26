//! Persistence and logging around the sans-IO registry core.

use crate::store::{
    Store,
    rules::{ExecuteRuleWritesError, LoadRulesError},
};
use quartz_domain::rules::registry::{
    CommitRuleDecisionError, CreateRuleRegistryError, DecideRegistryCommandError, DecisionParts,
    RegistryCommand, RegistrySuccess, ResetSnapshotEpochError, RuleRegistry, RuleSetWrite,
    RuleWarning, RulesChanged, SnapshotCell,
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
    #[snafu(display("canonical rule text exceeds the IPC text limit"))]
    TextTooLarge,
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
    BuiltinRuleSet {
        file: "builtin-extraction.toml",
        source: rules::BUILTIN_EXTRACTION,
        revision: rules::BUILTIN_EXTRACTION_REVISION,
    },
];

enum TextBudget {
    Ipc,
    Unrestricted,
}

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
        // Embedded revisions retain references to archived builtin categories.
        let builtin_references = store
            .rule_sets()
            .category_references(quartz_domain::categories::IncludeArchivedCategories::Include)
            .context(open_error::LoadSnafu)?;
        for builtin in builtins {
            let set = RuleSet::parse(
                builtin.file,
                builtin.source,
                ParseContext {
                    origin: RuleOrigin::Builtin,
                    references: &builtin_references,
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
    pub(crate) fn refresh_category_references(
        &mut self,
        store: &mut Store,
    ) -> Result<(), LoadRulesError> {
        let references = store
            .rule_sets()
            .category_references(quartz_domain::categories::IncludeArchivedCategories::Exclude)?;
        self.registry.update_references(references);
        Ok(())
    }
    pub(crate) fn snapshots(&self) -> SnapshotCell {
        self.registry.snapshots()
    }
    #[cfg(test)]
    pub(crate) fn core(&self) -> &RuleRegistry {
        &self.registry
    }

    pub(crate) fn rule_origin(&self, key: &rules::RuleSetKey) -> Option<RuleOrigin> {
        self.registry
            .state()
            .sets
            .get(key)
            .map(|entry| entry.origin)
    }
    pub(crate) fn validate(&self, command: RegistryCommand) -> Result<(), RunRegistryCommandError> {
        Self::checked_draft(self.registry.decide_validation(command))?;
        Ok(())
    }
    pub(crate) fn draft(
        &self,
        command: RegistryCommand,
    ) -> Result<std::sync::Arc<quartz_domain::rules::registry::RuleSnapshot>, RunRegistryCommandError>
    {
        Self::checked_draft(self.registry.decide(command))
    }
    fn checked_draft(
        decision: quartz_domain::rules::registry::Decision,
    ) -> Result<std::sync::Arc<quartz_domain::rules::registry::RuleSnapshot>, RunRegistryCommandError>
    {
        let draft = decision.into_draft().context(run_error::DecideSnafu)?;
        Self::check_text_budget(&draft.writes)?;
        Ok(draft.snapshot)
    }
    pub(crate) fn run(
        &mut self,
        store: &mut Store,
        now: quartz_domain::capture::UtcInstant,
        command: RegistryCommand,
    ) -> Result<RegistryResult, RunRegistryCommandError> {
        self.run_with_budget(store, now, command, TextBudget::Unrestricted)
    }
    pub(super) fn run_ipc(
        &mut self,
        store: &mut Store,
        now: quartz_domain::capture::UtcInstant,
        command: RegistryCommand,
    ) -> Result<RegistryResult, RunRegistryCommandError> {
        self.run_with_budget(store, now, command, TextBudget::Ipc)
    }
    fn run_with_budget(
        &mut self,
        store: &mut Store,
        now: quartz_domain::capture::UtcInstant,
        command: RegistryCommand,
        budget: TextBudget,
    ) -> Result<RegistryResult, RunRegistryCommandError> {
        let decision = self.registry.decide(command);
        let DecisionParts {
            writes,
            reply,
            outcome,
        } = decision.into_parts();
        let reply = reply.context(run_error::DecideSnafu)?;
        if matches!(budget, TextBudget::Ipc) {
            Self::check_text_budget(&writes)?;
        }

        let event = if let Some(mut outcome) = outcome {
            let now = now.get().as_millisecond();
            store
                .rule_transaction(|transaction| transaction.execute(&writes, now))
                .context(run_error::PersistSnafu)?;
            outcome.record_commit_time(&writes, now);
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
    fn check_text_budget(writes: &[RuleSetWrite]) -> Result<(), RunRegistryCommandError> {
        for write in writes {
            if let RuleSetWrite::CommitVersion(update) = write
                && update.set.canonical_text().len() > quartz_protocol::MAX_RULE_TEXT_BYTES
            {
                return Err(RunRegistryCommandError::TextTooLarge);
            }
        }
        Ok(())
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

    fn persist_existing_overlay(
        shell: &RegistryShell,
        store: &mut Store,
        now: quartz_domain::capture::UtcInstant,
        command: RegistryCommand,
    ) {
        let decision = shell.core().decide(command).into_parts();
        decision.reply.expect("existing overlay fixture");
        store
            .rule_transaction(|transaction| {
                transaction.execute(&decision.writes, now.get().as_millisecond())
            })
            .expect("persist pre-existing overlay");
    }

    #[test]
    fn classification_revision_two_upgrades_revision_one_and_preserves_overlay() {
        let directory = tempfile::tempdir().expect("isolated classification database");
        let data = DataDirectory::new(directory.path());
        let mut store = Store::open(&data).expect("store");
        store.seed_categories().expect("categories");
        let now = quartz_domain::capture::UtcInstant::try_from(0).expect("UTC");
        let old_source = rules::BUILTIN_CLASSIFICATION
            .split("# Exact project manifest names")
            .next()
            .expect("revision one rules");
        let mut old_builtins = BUILTIN_RULE_SETS.to_vec();
        let classification = old_builtins
            .iter_mut()
            .find(|builtin| builtin.file == "builtin-classification.toml")
            .expect("classification builtin");
        classification.source = old_source;
        classification.revision = 1;
        let mut shell = RegistryShell::open_with_builtins(&mut store, now, &old_builtins)
            .expect("revision one");
        let overlay_key = "user.classification".parse().expect("key");
        shell.run(&mut store, now, RegistryCommand::Apply {
            key: overlay_key,
            file: "overlay.toml".into(),
            text: "format='quartz-rules/1'\n[set]\nkey='user.classification'\nkind='classification'\nname='My classification'\nparent='builtin.classification'\n[[override]]\ntarget='builtin.classification/vscode'\nlabel='Read code'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='code.exe'}]\n[[disable]]\ntarget='builtin.classification/notepad-plus-plus'".into(),
            base_version: 0,
        }).expect("overlay");
        let before = store.rule_sets().load().expect("old state");
        let overlay_key = "user.classification".parse().expect("key");
        let old_hash = shell.snapshots().load().hashes().classification.clone();
        drop(shell);
        let shell = RegistryShell::open(&mut store, now).expect("upgrade");
        let snapshot = shell.snapshots().load();
        assert_ne!(snapshot.hashes().classification, old_hash);
        let after = store.rule_sets().load().expect("new state");
        assert_eq!(
            after.sets[&overlay_key].versions,
            before.sets[&overlay_key].versions
        );
        let key = "builtin.classification".parse().expect("builtin key");
        assert_eq!(
            after.sets[&key].builtin_revision,
            Some(rules::BUILTIN_CLASSIFICATION_REVISION)
        );
        assert_eq!(after.sets[&key].current.version, 2);
        assert_eq!(after.sets[&key].versions.len(), 2);
        let mut fields = FieldSnapshot::default();
        fields.text.insert(Field::ExeName, NfcText::new("code.exe"));
        assert_eq!(
            snapshot
                .classification()
                .classify(&fields)
                .expect("user override")
                .rule
                .as_str(),
            "user.classification/vscode@1"
        );
        fields
            .text
            .insert(Field::ExeName, NfcText::new("notepad++.exe"));
        assert!(snapshot.classification().classify(&fields).is_none());
        fields
            .text
            .insert(Field::ExeName, NfcText::new("notepad.exe"));
        fields
            .text
            .insert(Field::Document, NfcText::new("Cargo.toml"));
        assert_eq!(
            snapshot
                .classification()
                .classify(&fields)
                .expect("new manifest rule")
                .rule
                .as_str(),
            "builtin.classification/notepad-cargo-manifest@2"
        );
        let reopened = RegistryShell::open(&mut store, now).expect("idempotent upgrade");
        assert_eq!(reopened.snapshots().load().hashes(), snapshot.hashes());
        assert_eq!(
            store.rule_sets().load().expect("stable state").sets[&key].versions,
            after.sets[&key].versions
        );
    }

    #[test]
    fn startup_installs_only_the_supplied_builtin_table() {
        let directory = tempfile::tempdir().expect("isolated rule database");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("builtin categories");
        let shell = RegistryShell::open_with_builtins(
            &mut store,
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
            &BUILTIN_RULE_SETS[1..2],
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
    fn startup_adds_extraction_to_an_existing_database_and_is_idempotent() {
        let directory = tempfile::tempdir().expect("isolated rule database");
        let data = DataDirectory::new(directory.path());
        let mut store = Store::open(&data).expect("store");
        store.seed_categories().expect("builtin categories");
        let now = quartz_domain::capture::UtcInstant::try_from(0).expect("UTC");
        let previous = RegistryShell::open_with_builtins(&mut store, now, &BUILTIN_RULE_SETS[..2])
            .expect("existing privacy and classification builtins");
        let previous_hashes = previous.snapshots().load().hashes().clone();
        let shell = RegistryShell::open(&mut store, now).expect("install extraction at startup");
        let snapshot = shell.snapshots().load();
        assert_eq!(snapshot.epoch().get(), 0);
        assert_eq!(
            snapshot.hashes().classification,
            previous_hashes.classification
        );
        assert_eq!(snapshot.hashes().privacy, previous_hashes.privacy);
        assert_ne!(snapshot.hashes().extraction, previous_hashes.extraction);
        let fields = FieldSnapshot {
            text: BTreeMap::from([
                (Field::ExeName, NfcText::new("code.exe")),
                (
                    Field::RawTitle,
                    NfcText::new("main.rs - Quartz - Visual Studio Code"),
                ),
            ]),
            domain: None,
        };
        let result = snapshot.extraction().evaluate(&fields);
        assert_eq!(
            result.selected.as_deref(),
            Some("builtin.extraction/vscode-project@1")
        );
        assert_eq!(
            result.extraction.expect("document").document.as_deref(),
            Some("main.rs")
        );
        let reopened = RegistryShell::open(&mut store, now).expect("idempotent startup");
        assert_eq!(reopened.snapshots().load().hashes(), snapshot.hashes());
        let stored = store.rule_sets().load().expect("persisted builtin table");
        assert_eq!(stored.sets.len(), 3);
        for builtin in stored.sets.values() {
            assert_eq!(
                builtin.current.version,
                builtin.builtin_revision.expect("builtin revision")
            );
            assert_eq!(builtin.versions.len(), 1);
        }
    }

    #[test]
    fn startup_extraction_upgrade_preserves_user_overrides_disables_and_local_rules() {
        let directory = tempfile::tempdir().expect("isolated extraction database");
        let data = DataDirectory::new(directory.path());
        let mut store = Store::open(&data).expect("store");
        store.seed_categories().expect("builtin categories");
        let now = quartz_domain::capture::UtcInstant::try_from(0).expect("UTC");
        let old_source = rules::BUILTIN_EXTRACTION.replace("Visual Studio Code", "Old Code");
        assert_ne!(old_source, rules::BUILTIN_EXTRACTION);
        let mut old_builtins = BUILTIN_RULE_SETS.to_vec();
        old_builtins[2].source = &old_source;
        let mut shell = RegistryShell::open_with_builtins(&mut store, now, &old_builtins)
            .expect("old extraction installation");
        let overlay_key = "user.local.extraction".parse().expect("overlay key");
        shell
            .run(
                &mut store,
                now,
                RegistryCommand::Apply {
                    key: overlay_key,
                    file: "overlay.toml".into(),
                    text: r"format='quartz-rules/1'
[set]
key='user.local.extraction'
kind='extraction'
name='My extraction'
parent='builtin.extraction'
[[override]]
target='builtin.extraction/vscode-project'
label='My code title'
app=[{field='app.exe_name',op='equals',value='code.exe'}]
pattern='\ACustom: (?P<document>[^\r\n]+)\z'
[[disable]]
target='builtin.extraction/office-word'
[[rule]]
id='notes'
label='My notes'
app=[{field='app.exe_name',op='equals',value='notes.exe'}]
pattern='\ANote: (?P<document>[^\r\n]+)\z'
"
                    .into(),
                    base_version: 0,
                },
            )
            .expect("save extraction overlay");
        let saved = store.rule_sets().load().expect("saved overlay");
        let overlay_key = "user.local.extraction".parse().expect("overlay key");
        let before = &saved.sets[&overlay_key];
        let old_snapshot = shell.snapshots().load();
        drop(shell);
        drop(store);
        let revision = rules::BUILTIN_EXTRACTION_REVISION
            .checked_add(1)
            .expect("next revision");
        let mut new_builtins = BUILTIN_RULE_SETS.to_vec();
        new_builtins[2].revision = revision;
        let mut store = Store::open(&data).expect("reopen isolated database");
        let upgraded = RegistryShell::open_with_builtins(&mut store, now, &new_builtins)
            .expect("upgrade extraction before publication");
        let snapshot = upgraded.snapshots().load();
        assert_eq!(snapshot.epoch().get(), 0);
        assert_ne!(
            snapshot.hashes().extraction,
            old_snapshot.hashes().extraction
        );
        assert_eq!(snapshot.hashes().privacy, old_snapshot.hashes().privacy);
        assert_eq!(
            snapshot.hashes().classification,
            old_snapshot.hashes().classification
        );
        let persisted = store.rule_sets().load().expect("upgraded database");
        let after = &persisted.sets[&overlay_key];
        assert_eq!(after.current.set, before.current.set);
        assert_eq!(after.current.version, before.current.version);
        assert_eq!(after.current.enabled, before.current.enabled);
        assert_eq!(after.current.sibling_order, before.current.sibling_order);
        assert_eq!(after.origin, before.origin);
        assert_eq!(after.builtin_revision, before.builtin_revision);
        assert_eq!(after.deletion, before.deletion);
        assert_eq!(after.versions, before.versions);
        let builtin_key = "builtin.extraction".parse().expect("builtin key");
        let builtin = &persisted.sets[&builtin_key];
        assert_eq!(builtin.builtin_revision, Some(revision));
        assert_eq!(builtin.current.version, 2);
        assert_eq!(builtin.versions.len(), 2);
        for (exe, title, document, rule) in [
            (
                "code.exe",
                "Custom: main.rs",
                "main.rs",
                Some("user.local.extraction/vscode-project@1"),
            ),
            (
                "notes.exe",
                "Note: 设计",
                "设计",
                Some("user.local.extraction/notes@1"),
            ),
            ("winword.exe", "Report - Word", "Report - Word", None),
        ] {
            let fields = FieldSnapshot {
                text: BTreeMap::from([
                    (Field::ExeName, NfcText::new(exe)),
                    (Field::RawTitle, NfcText::new(title)),
                ]),
                domain: None,
            };
            let result = snapshot.extraction().evaluate(&fields);
            assert_eq!(result.selected.as_deref(), rule, "{title}");
            assert_eq!(
                result.extraction.expect("extraction").document.as_deref(),
                Some(document),
                "{title}"
            );
        }
        let reopened = RegistryShell::open_with_builtins(&mut store, now, &new_builtins)
            .expect("idempotent upgrade");
        assert_eq!(reopened.snapshots().load().hashes(), snapshot.hashes());
        let final_state = store.rule_sets().load().expect("final state");
        assert_eq!(final_state.sets[&builtin_key].versions, builtin.versions);
        assert_eq!(final_state.sets[&overlay_key].versions, after.versions);
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
        let shell = RegistryShell::open_with_builtins(
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
        persist_existing_overlay(
            &shell,
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
        );
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
