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
    let own = fixture.get("user.leaf", RuleView::Own);
    assert_eq!(own.entries[0].state, RuleEntryState::Disabled);
    let effective = fixture.get("user.leaf", RuleView::Effective);
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
fn oversized_effective_results_truncate_to_frame_limit_and_import_updates_versions() {
    /// Each child set remains below the 192 KiB text limit while their root tree exceeds 1 MiB.
    const CHILD_COUNT: usize = 10;
    const RULES_PER_SET: usize = 30;
    const VALUE_BYTES: usize = 4000;
    let mut fixture = Fixture::new();
    fixture.apply("user.root", None, &Fixture::rule("root", 50), 0);
    let small = fixture.get("user.root", RuleView::Effective);
    assert!(!small.truncated);
    assert_eq!(small.entries.len(), 1);
    for child in 0..CHILD_COUNT {
        let entries: String = (0..RULES_PER_SET)
            .map(|index| {
                Fixture::rule(&format!("rule-{index}"), 50)
                    .replace("code.exe", &"x".repeat(VALUE_BYTES))
            })
            .collect();
        fixture.apply(
            &format!("user.child-{child}"),
            Some("user.root"),
            &entries,
            0,
        );
    }
    let before = fixture.inputs();
    let old_versions = before.versions();
    let response = before
        .contents(
            RuleSetsGetRequest {
                key: "user.root".into(),
                view: RuleView::Effective,
            },
            old_versions.clone(),
        )
        .expect("truncated effective view");
    assert!(response.truncated);
    assert!(!response.entries.is_empty());
    assert!(response.entries.len() < 1 + CHILD_COUNT * RULES_PER_SET);
    assert!(
        serde_json::to_vec(&response)
            .expect("encoded response")
            .len()
            <= QUERY_MAX_RESPONSE_BYTES
    );
    fixture.run(RegistryCommand::Import {
        key: "user.unrelated".parse().expect("key"),
        file: "import.toml".into(),
        text: Fixture::source("user.unrelated", None, &Fixture::rule("imported", 50)),
        base_version: 0,
    });
    let inputs = fixture.inputs();
    assert_ne!(inputs.versions(), old_versions);
    // An in-flight work unit retains the publication it captured before the import.
    assert_eq!(before.versions(), old_versions);
}

#[test]
fn metadata_enablement_and_deleted_revisions_are_global_version_inputs() {
    let mut fixture = Fixture::new();
    fixture.apply("user.root", None, &Fixture::rule("root", 50), 0);
    fixture.apply(
        "user.first",
        Some("user.root"),
        &Fixture::rule("first", 50),
        0,
    );
    fixture.apply(
        "user.second",
        Some("user.root"),
        &Fixture::rule("second", 50),
        0,
    );
    let before = fixture.inputs();
    let mut metadata = fixture.inputs();
    metadata.metadata_version += 1;
    assert_ne!(metadata.versions(), before.versions());
    let before = fixture.inputs().versions();
    fixture.run(RegistryCommand::Toggle {
        key: "user.first".parse().expect("key"),
        base_version: 1,
        enabled: Enablement::Disabled,
    });
    assert_ne!(fixture.inputs().versions(), before);
    let before = fixture.inputs().versions();
    fixture.run(RegistryCommand::Delete {
        key: "user.first".parse().expect("key"),
        base_version: 2,
    });
    assert_ne!(fixture.inputs().versions(), before);
    let mut state = fixture.shell.core().state().clone();
    let key = "user.first".parse().expect("key");
    state
        .sets
        .get_mut(&key)
        .expect("deleted history")
        .versions
        .get_mut(&1)
        .expect("old revision")
        .note = Some("Historical annotation".into());
    let registry = RuleRegistry::from_state(state).expect("retained state");
    assert_ne!(
        ReadInputs {
            snapshot: registry.snapshots().load(),
            metadata_version: fixture.inputs().metadata_version
        }
        .versions(),
        fixture.inputs().versions()
    );
}

#[test]
fn tree_lists_preserve_order_and_cap_with_truncated() {
    const SET_COUNT: usize = wire::RULE_SETS_TREE_MAX_NODES + 20;
    const KEY_PADDING: usize = 64;
    let mut fixture = Fixture::new();
    fixture.apply("user.history", None, "", 0);
    let small = fixture.tree();
    assert!(!small.truncated);
    let mut state = fixture.shell.core().state().clone();
    let template_key = "user.history".parse().expect("key");
    let template = state.sets[&template_key].clone();
    for index in 0..SET_COUNT {
        let key = format!("user.capped-{index:04}-{}", "x".repeat(KEY_PADDING));
        let mut entry = template.clone();
        entry.current.set.header.key = key.clone();
        entry.current.sibling_order = 0;
        state.sets.insert(key.parse().expect("key"), entry);
    }
    let registry = RuleRegistry::from_state(state).expect("large retained registry");
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
        .expect("capped tree");
    assert!(tree.truncated);
    assert_eq!(tree.nodes.len(), wire::RULE_SETS_TREE_MAX_NODES);
    assert!(serde_json::to_vec(&tree).expect("tree JSON").len() <= QUERY_MAX_RESPONSE_BYTES);
    let capped_keys: Vec<_> = tree
        .nodes
        .iter()
        .map(|node| node.key.as_str())
        .filter(|key| key.starts_with("user.capped-"))
        .collect();
    assert!(!capped_keys.is_empty());
    assert!(capped_keys.windows(2).all(|window| window[0] < window[1]));
}

#[test]
fn oversized_stored_entries_return_internal_instead_of_an_unbounded_success() {
    let mut fixture = Fixture::new();
    fixture.apply("user.invalid", None, &Fixture::rule("one", 50), 0);
    let mut state = fixture.shell.core().state().clone();
    let set = state
        .sets
        .get_mut(&"user.invalid".parse().expect("key"))
        .expect("set");
    set.current.set.content.rule[0].label = Some("x".repeat(domain::MAX_ENTRY_BYTES));
    set.versions.get_mut(&1).expect("revision").content = set.current.set.content.clone();
    let registry = RuleRegistry::from_state(state).expect("startup preserves stored warnings");
    let inputs = ReadInputs {
        snapshot: registry.snapshots().load(),
        metadata_version: 0,
    };
    assert_eq!(
        inputs
            .contents(
                RuleSetsGetRequest {
                    key: "user.invalid".into(),
                    view: RuleView::Own,
                },
                inputs.versions()
            )
            .expect_err("oversized stored entry")
            .kind,
        ErrorKind::Internal
    );
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
#[test]
fn text_export_returns_complete_utf8_within_frame_and_rejects_cursor() {
    const RULE_COUNT: usize = 20;
    const LABEL_QUOTES: usize = 4000;
    let mut fixture = Fixture::new();
    let label = serde_json::to_string(&format!("{}中文😀", "\"".repeat(LABEL_QUOTES)))
        .expect("TOML string");
    let entries = (0..RULE_COUNT)
        .map(|index| {
            Fixture::rule(&format!("entry-{index}"), 50)
                .replace(&format!("'Rule entry-{index}'"), &label)
        })
        .collect::<String>();
    fixture.apply("user.export-large", None, &entries, 0);
    let exported = fixture
        .export_text(Fixture::export_request("user.export-large"))
        .expect("complete export");
    assert!(serde_json::to_vec(&exported).expect("export bytes").len() <= QUERY_MAX_RESPONSE_BYTES);
    let current = fixture
        .shell
        .core()
        .state()
        .sets
        .get(&"user.export-large".parse().expect("key"))
        .expect("saved set");
    assert_eq!(exported.text, current.current.set.canonical_text());
    assert!(exported.text.len() <= wire::MAX_RULE_TEXT_BYTES);
    assert!(
        crate::rules::write::RuleMutation::validate(
            wire::RulesTextValidateRequest {
                key: "user.export-large".into(),
                base_version: 1,
                text: exported.text,
            },
            &fixture.shell
        )
        .expect("export validates")
        .valid
    );
    assert!(
        ReadRequest::decode(
            wire::Op::RulesTextExport,
            serde_json::json!({"key":"user.export-large","cursor":null}),
        )
        .is_err()
    );
}
