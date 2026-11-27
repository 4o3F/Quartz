use super::*;
use crate::{data_directory::DataDirectory, rules::registry::RegistryShell, store::Store};
use domain::{
    Enablement, ParseContext, RuleOrigin, RuleSet,
    registry::{RegistryCommand, RuleRegistry},
};
use quartz_protocol::{RuleEntryKind, RuleEntryState, RuleProvenance};

/// Distinct commit time used to check persisted rule metadata.
const COMMIT_TIME_MS: i64 = 123_456;
struct Fixture {
    directory: tempfile::TempDir,
    store: Store,
    shell: RegistryShell,
}
impl Fixture {
    fn now() -> quartz_domain::capture::UtcInstant {
        COMMIT_TIME_MS.try_into().expect("UTC")
    }
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("isolated rule read database");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("categories");
        let shell = RegistryShell::open(&mut store, Fixture::now()).expect("registry");
        Self {
            directory,
            store,
            shell,
        }
    }
    fn run(&mut self, command: RegistryCommand) {
        self.shell
            .run(&mut self.store, Fixture::now(), command)
            .expect("registry commit");
    }
    fn apply(&mut self, key: &str, parent: Option<&str>, entries: &str, base_version: u32) {
        self.run(RegistryCommand::Apply {
            key: key.parse().expect("key"),
            file: "read-test.toml".into(),
            text: Self::source(key, parent, entries),
            base_version,
        });
    }
    fn builtin(&mut self, entries: &str, revision: u32) {
        let source = Self::source("builtin.read-test", None, entries);
        let set = RuleSet::parse(
            "builtin-read-test.toml",
            &source,
            ParseContext {
                origin: RuleOrigin::Builtin,
                references: &self.shell.core().state().references,
            },
        )
        .expect("builtin");
        self.run(RegistryCommand::InstallBuiltin { set, revision });
    }
    fn source(key: &str, parent: Option<&str>, entries: &str) -> String {
        let parent = parent
            .map(|parent| format!("parent='{parent}'\n"))
            .unwrap_or_default();
        format!(
            "format='quartz-rules/1'\n[set]\nkey='{key}'\nkind='classification'\nname='Read test'\n{parent}{entries}"
        )
    }
    fn rule(id: &str, priority: u16) -> String {
        format!(
            "[[rule]]\nid='{id}'\nlabel='Rule {id}'\npriority={priority}\ncategory='builtin:learning'\nwhen=[{{field='app.exe_name',op='equals',value='code.exe'}}]\n"
        )
    }
    fn replacement(target: &str) -> String {
        format!(
            "[[override]]\ntarget='{target}'\nlabel='Replacement'\ncategory='builtin:development'\nwhen=[{{field='app.exe_name',op='equals',value='code.exe'}}]\n"
        )
    }
    fn inputs(&mut self) -> ReadInputs {
        ReadInputs {
            snapshot: self.shell.snapshots().load(),
            metadata_version: self.store.rule_read_metadata().expect("metadata"),
        }
    }
    fn get(&mut self, key: &str, view: RuleView) -> RuleSetsGetResponse {
        self.inputs()
            .contents(
                RuleSetsGetRequest {
                    key: key.into(),
                    view,
                },
                self.inputs().versions(),
            )
            .expect("contents")
    }
    fn tree(&mut self) -> RuleSetsTreeResponse {
        let inputs = self.inputs();
        inputs
            .tree(
                RuleSetsTreeRequest {
                    kind: RuleKind::Classification,
                },
                inputs.versions(),
            )
            .expect("tree")
    }
}
#[test]
fn disabled_subtrees_use_resolver_states() {
    let mut fixture = Fixture::new();
    fixture.apply("user.root", None, &Fixture::rule("root", 50), 0);
    fixture.apply(
        "user.child",
        Some("user.root"),
        &Fixture::rule("child", 50),
        0,
    );
    fixture.apply(
        "user.leaf",
        Some("user.child"),
        &Fixture::rule("leaf", 50),
        0,
    );
    fixture.run(RegistryCommand::Toggle {
        key: "user.child".parse().expect("key"),
        base_version: 1,
        enabled: Enablement::Disabled,
    });
    let tree = fixture.tree();
    let nodes: Vec<_> = tree
        .nodes
        .iter()
        .filter(|node| node.key.starts_with("user."))
        .collect();
    assert_eq!(
        nodes
            .iter()
            .map(|node| node.key.as_str())
            .collect::<Vec<_>>(),
        ["user.root", "user.child", "user.leaf"]
    );
    assert!(nodes[0].enabled);
    assert!(!nodes[1].enabled);
    assert!(!nodes[2].enabled);
    assert_eq!(nodes[0].name, "Read test");
    let own = fixture.get("user.leaf", RuleView::Own);
    assert_eq!(own.name, "Read test");
    assert_eq!(own.entries[0].state, RuleEntryState::Disabled);
    let effective = fixture.get("user.leaf", RuleView::Effective);
    assert_eq!(effective.name, "Read test");
    assert_eq!(
        effective
            .entries
            .iter()
            .map(|entry| entry.state)
            .collect::<Vec<_>>(),
        [
            RuleEntryState::Effective,
            RuleEntryState::Disabled,
            RuleEntryState::Disabled
        ]
    );
}

#[test]
fn deleted_sets_are_excluded_from_tree_and_current_reads() {
    let mut fixture = Fixture::new();
    fixture.apply("user.history", None, &Fixture::rule("before", 50), 0);
    fixture.run(RegistryCommand::Delete {
        key: "user.history".parse().expect("key"),
        base_version: 1,
    });
    let inputs = fixture.inputs();
    let error = inputs
        .contents(
            RuleSetsGetRequest {
                key: "user.history".into(),
                view: RuleView::Own,
            },
            inputs.versions(),
        )
        .expect_err("deleted current set");
    assert_eq!(
        error.kind,
        ErrorKind::RuleSetNotFound {
            key: "user.history".into(),
        }
    );
    assert!(
        !fixture
            .tree()
            .nodes
            .iter()
            .any(|node| node.key == "user.history")
    );
    fixture.shell =
        RegistryShell::open(&mut fixture.store, Fixture::now()).expect("reopen registry");
    assert!(
        !fixture
            .tree()
            .nodes
            .iter()
            .any(|node| node.key == "user.history")
    );
    assert!(fixture.directory.path().join("quartz.db").is_file());
}

#[test]
fn own_file_order_and_effective_order_preserve_suppressed_entries_and_uuid_categories() {
    let mut fixture = Fixture::new();
    fixture.apply(
        "user.root",
        None,
        &(Fixture::rule("replace", 50)
            + &Fixture::rule("remove", 50)
            + &Fixture::rule("low", 10)
            + &Fixture::rule("high", 100)),
        0,
    );
    let entries = format!(
        "[[disable]]\ntarget='user.root/remove'\nreason='Suppressed'\n{}{}",
        Fixture::rule("child", 50),
        Fixture::replacement("user.root/replace")
    );
    fixture.apply("user.child", Some("user.root"), &entries, 0);
    let own = fixture.get("user.child", RuleView::Own);
    assert_eq!(
        own.entries
            .iter()
            .map(|entry| entry.entry_kind)
            .collect::<Vec<_>>(),
        [
            RuleEntryKind::Disable,
            RuleEntryKind::Rule,
            RuleEntryKind::Override
        ]
    );
    assert_eq!(
        own.entries[0]
            .provenance
            .last()
            .expect("disable author")
            .key,
        "user.child"
    );
    let page = fixture.get("user.child", RuleView::Effective);
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.state)
            .collect::<Vec<_>>(),
        [
            RuleEntryState::Effective,
            RuleEntryState::Effective,
            RuleEntryState::Effective,
            RuleEntryState::Effective,
            RuleEntryState::Overridden,
            RuleEntryState::Disabled,
            RuleEntryState::Disabled
        ]
    );
    assert_eq!(page.entries[0].content.id.as_deref(), Some("high"));
    assert_eq!(page.entries[1].content.id.as_deref(), Some("child"));
    assert_eq!(
        page.entries[2].content.target.as_deref(),
        Some("user.root/replace")
    );
    assert_eq!(page.entries[3].content.id.as_deref(), Some("low"));
    let replacement = &page.entries[2];
    assert_eq!(
        replacement.provenance,
        [
            RuleProvenance {
                key: "user.root".into(),
                version: 1,
                id: "replace".into()
            },
            RuleProvenance {
                key: "user.child".into(),
                version: 1,
                id: "replace".into()
            }
        ]
    );
    let category = own.entries[1]
        .content
        .category
        .as_ref()
        .expect("stored UUID");
    assert_eq!(
        category.to_string(),
        fixture
            .shell
            .core()
            .state()
            .references
            .resolve_category("builtin:learning")
            .expect("learning")
    );
}

#[test]
fn deleted_targets_report_tree_warnings_and_missing_entry_state() {
    let mut fixture = Fixture::new();
    fixture.builtin(&(Fixture::rule("one", 50) + &Fixture::rule("two", 50)), 1);
    fixture.apply(
        "user.overlay",
        Some("builtin.read-test"),
        &(Fixture::replacement("builtin.read-test/one")
            + "[[disable]]\ntarget='builtin.read-test/two'\n"),
        0,
    );
    fixture.builtin(&Fixture::rule("one", 60), 2);
    let tree = fixture.tree();
    let overlay = tree
        .nodes
        .iter()
        .find(|node| node.key == "user.overlay")
        .expect("overlay node");
    assert_eq!(
        overlay.warnings,
        [RuleSetWarning {
            code: RuleWarningCode::TargetDeleted,
            target: "builtin.read-test/two".into()
        }]
    );
    let page = fixture.get("user.overlay", RuleView::Own);
    assert_eq!(page.entries[1].state, RuleEntryState::TargetMissing);
}

#[test]
fn startup_conflicts_report_both_authors_and_exclude_their_overlays() {
    let mut fixture = Fixture::new();
    fixture.apply("user.root", None, &Fixture::rule("one", 50), 0);
    fixture.apply(
        "user.left",
        Some("user.root"),
        &Fixture::replacement("user.root/one"),
        0,
    );
    let mut state = fixture.shell.core().state().clone();
    let mut sibling = state.sets[&"user.left".parse().expect("key")].clone();
    sibling.current.set.header.key = "user.right".into();
    state
        .sets
        .insert("user.right".parse().expect("key"), sibling);
    let registry = RuleRegistry::from_state(state).expect("startup retains conflict warnings");
    let inputs = ReadInputs {
        snapshot: registry.snapshots().load(),
        metadata_version: 0,
    };
    let tree = inputs
        .tree(
            RuleSetsTreeRequest {
                kind: RuleKind::Classification,
            },
            inputs.versions(),
        )
        .expect("conflict tree");
    for key in ["user.left", "user.right"] {
        let node = tree
            .nodes
            .iter()
            .find(|node| node.key == key)
            .expect("conflicting node");
        assert_eq!(
            node.warnings,
            [RuleSetWarning {
                code: RuleWarningCode::Conflict,
                target: "user.root/one".into()
            }]
        );
        let page = inputs
            .contents(
                RuleSetsGetRequest {
                    key: key.into(),
                    view: RuleView::Own,
                },
                inputs.versions(),
            )
            .expect("conflicting entry");
        assert_eq!(page.entries[0].state, RuleEntryState::Disabled);
    }
}

#[test]
fn family_trees_reuse_evaluator_hashes_and_format_specific_fields() {
    let mut fixture = Fixture::new();
    let inputs = fixture.inputs();
    for kind in [
        RuleKind::Classification,
        RuleKind::Extraction,
        RuleKind::Privacy,
    ] {
        let tree = inputs
            .tree(RuleSetsTreeRequest { kind }, inputs.versions())
            .expect("family tree");
        let hash = match kind {
            RuleKind::Classification => &inputs.snapshot.hashes().classification,
            RuleKind::Extraction => &inputs.snapshot.hashes().extraction,
            RuleKind::Privacy => &inputs.snapshot.hashes().privacy,
        };
        assert_eq!(tree.effective_version, hash.as_str());
    }
    let privacy = fixture.get("builtin.privacy", RuleView::Own);
    assert!(privacy.entries.iter().all(|entry| matches!(
        entry.content.app,
        Some(wire::RuleApp::Action(_))
    ) && entry.content.window_title.is_some()
        && entry.content.web.is_some()));
    let extraction = fixture.get("builtin.extraction", RuleView::Own);
    assert!(extraction.entries.iter().all(|entry| matches!(
        entry.content.app,
        Some(wire::RuleApp::Conditions(_))
    ) && entry.content.pattern.is_some()));
}

impl Fixture {
    fn export_text(
        &mut self,
        request: wire::RulesTextExportRequest,
    ) -> Result<wire::RulesTextExportResponse, ProtocolError> {
        let inputs = self.inputs();
        let versions = inputs.versions();
        inputs.export(request, versions)
    }
    fn export_request(key: &str) -> wire::RulesTextExportRequest {
        wire::RulesTextExportRequest { key: key.into() }
    }
}
#[test]
fn text_exports_preserve_saved_headers_and_overlays_after_reopening() {
    let mut fixture = Fixture::new();
    let key = "user.export";
    let entries = format!(
        "{}[[disable]]\ntarget='builtin.classification/vscode'\nreason='Unused'\n{}",
        Fixture::rule("local", 100),
        Fixture::replacement("builtin.classification/notepad-plus-plus")
    );
    let source = format!(
        "{}\n",
        Fixture::source(key, Some("builtin.classification"), &entries).replace(
            "name='Read test'",
            "name='Original'\ndescription='Saved description'"
        )
    );
    fixture.run(RegistryCommand::Apply {
        key: key.parse().expect("key"),
        file: "export.toml".into(),
        text: source.clone(),
        base_version: 0,
    });
    let first = fixture
        .export_text(Fixture::export_request(key))
        .expect("export");
    let parsed = RuleSet::parse(
        "export.toml",
        &first.text,
        ParseContext {
            origin: RuleOrigin::User,
            references: &fixture.shell.core().state().references,
        },
    )
    .expect("parse exported text");
    assert_eq!(parsed.header.name, "Original");
    assert_eq!(
        parsed.header.description.as_deref(),
        Some("Saved description")
    );
    assert_eq!(parsed.canonical_text(), first.text);
    let response = crate::rules::write::RuleMutation::validate(
        wire::RulesTextValidateRequest {
            key: key.into(),
            base_version: 1,
            text: first.text.clone(),
        },
        &fixture.shell,
    )
    .expect("contextual export validation");
    assert!(response.valid);
    fixture.run(RegistryCommand::Apply {
        key: key.parse().expect("key"),
        file: "export.toml".into(),
        text: source
            .replace("Original", "Renamed")
            .replace("Saved description", "New description"),
        base_version: 1,
    });
    fixture.shell =
        RegistryShell::open(&mut fixture.store, Fixture::now()).expect("reopened registry");
    let renamed = fixture
        .export_text(Fixture::export_request(key))
        .expect("renamed current");
    assert!(renamed.text.contains("Renamed"));
    assert!(renamed.text.contains("New description"));
    fixture.run(RegistryCommand::Delete {
        key: key.parse().expect("key"),
        base_version: 2,
    });
    fixture.shell =
        RegistryShell::open(&mut fixture.store, Fixture::now()).expect("reopened registry");
    assert_eq!(
        fixture
            .export_text(Fixture::export_request(key))
            .expect_err("deleted current")
            .kind,
        ErrorKind::RuleSetNotFound { key: key.into() }
    );
    fixture.run(RegistryCommand::Apply {
        key: key.parse().expect("key"),
        file: "restore.toml".into(),
        text: source,
        base_version: 0,
    });
    let restored = fixture
        .export_text(Fixture::export_request(key))
        .expect("restored header");
    assert_eq!(restored.text, first.text);
}
