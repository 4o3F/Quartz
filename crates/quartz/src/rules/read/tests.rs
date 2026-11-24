use super::*;
use crate::{data_directory::DataDirectory, rules::registry::RegistryShell, store::Store};
use domain::{
    Enablement, ParseContext, RuleOrigin, RuleSet,
    registry::{RegistryCommand, RuleRegistry, SiblingRevision},
};
use quartz_protocol::{RuleEntryKind, RuleEntryState, RuleProvenance};
use std::num::NonZeroU32;

/// Distinct commit time used to check in-memory and reopened history metadata.
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
    fn get(
        &mut self,
        key: &str,
        version: Option<NonZeroU32>,
        view: RuleView,
    ) -> RuleSetsGetResponse {
        self.inputs()
            .contents(
                RuleSetsGetRequest {
                    key: key.into(),
                    version,
                    view,
                    cursor: None,
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
                    cursor: None,
                },
                inputs.versions(),
            )
            .expect("tree")
    }
    fn revisions(&mut self, key: &str) -> RuleSetsVersionsResponse {
        let inputs = self.inputs();
        inputs
            .revisions(
                RuleSetsVersionsRequest {
                    key: key.into(),
                    cursor: None,
                },
                inputs.versions(),
            )
            .expect("revisions")
    }
}
#[test]
fn disabled_subtrees_and_saved_chains_use_resolver_states() {
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
    let own = fixture.get("user.leaf", None, RuleView::Own);
    assert_eq!(own.entries[0].state, RuleEntryState::Disabled);
    let effective = fixture.get("user.leaf", None, RuleView::Effective);
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
    let historical = fixture.get("user.leaf", NonZeroU32::new(1), RuleView::Effective);
    assert!(
        historical
            .entries
            .iter()
            .all(|entry| entry.state == RuleEntryState::Effective)
    );
}

#[test]
fn deleted_history_is_readable_and_retains_transaction_timestamps_after_reopen() {
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
                version: None,
                view: RuleView::Own,
                cursor: None,
            },
            inputs.versions(),
        )
        .expect_err("deleted current set");
    assert_eq!(
        error.kind,
        ErrorKind::RuleSetNotFound {
            key: "user.history".into(),
            version: None
        }
    );
    assert!(
        !fixture
            .tree()
            .nodes
            .iter()
            .any(|node| node.key == "user.history")
    );
    assert_eq!(
        fixture
            .get("user.history", NonZeroU32::new(1), RuleView::Own)
            .entries[0]
            .content
            .id
            .as_deref(),
        Some("before")
    );
    let revisions = fixture.revisions("user.history");
    assert_eq!(
        revisions
            .revisions
            .iter()
            .map(|revision| revision.version)
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert!(
        revisions
            .revisions
            .iter()
            .all(|revision| revision.created_utc_ms == COMMIT_TIME_MS)
    );
    fixture.shell =
        RegistryShell::open(&mut fixture.store, Fixture::now()).expect("reopen registry");
    assert_eq!(fixture.revisions("user.history"), revisions);
    assert!(fixture.directory.path().join("quartz.db").is_file());
}

#[test]
fn historical_effective_view_follows_only_saved_ancestor_revisions() {
    let mut fixture = Fixture::new();
    fixture.apply("user.root", None, &Fixture::rule("old-root", 50), 0);
    fixture.apply(
        "user.child",
        Some("user.root"),
        &Fixture::rule("old-child", 50),
        0,
    );
    fixture.apply(
        "user.leaf",
        Some("user.child"),
        &Fixture::rule("leaf", 50),
        0,
    );
    fixture.apply(
        "user.sibling",
        Some("user.root"),
        &Fixture::rule("sibling", 50),
        0,
    );
    fixture.apply("user.root", None, &Fixture::rule("new-root", 50), 1);
    fixture.apply(
        "user.child",
        Some("user.root"),
        &Fixture::rule("new-child", 50),
        1,
    );
    let old = fixture.get("user.leaf", NonZeroU32::new(1), RuleView::Effective);
    assert_eq!(old.parent_version, Some(1));
    assert_eq!(
        old.entries
            .iter()
            .filter_map(|entry| entry.content.id.as_deref())
            .collect::<Vec<_>>(),
        ["leaf", "old-child", "old-root"]
    );
    assert!(
        old.entries
            .iter()
            .flat_map(|entry| &entry.provenance)
            .all(|source| source.version == 1)
    );
    let current = fixture.get("user.leaf", None, RuleView::Effective);
    assert_eq!(
        current
            .entries
            .iter()
            .filter_map(|entry| entry.content.id.as_deref())
            .collect::<Vec<_>>(),
        ["leaf", "new-child", "sibling", "new-root"]
    );
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
    let own = fixture.get("user.child", None, RuleView::Own);
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
    let page = fixture.get("user.child", None, RuleView::Effective);
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
fn upstream_changes_and_deleted_targets_reuse_snapshot_warnings() {
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
    assert!(overlay.warnings.contains(&RuleSetWarning {
        code: RuleWarningCode::UpstreamChanged,
        target: "builtin.read-test/one".into()
    }));
    assert!(overlay.warnings.contains(&RuleSetWarning {
        code: RuleWarningCode::TargetDeleted,
        target: "builtin.read-test/two".into()
    }));
    let page = fixture.get("user.overlay", None, RuleView::Own);
    assert_eq!(page.entries[1].state, RuleEntryState::TargetMissing);
    assert_eq!(
        fixture
            .get("user.overlay", NonZeroU32::new(1), RuleView::Own)
            .entries[1]
            .state,
        RuleEntryState::Disabled
    );
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
                cursor: None,
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
                    version: None,
                    view: RuleView::Own,
                    cursor: None,
                },
                inputs.versions(),
            )
            .expect("conflicting entry");
        assert_eq!(page.entries[0].state, RuleEntryState::Disabled);
    }
}

#[test]
fn oversized_effective_results_split_and_import_invalidates_all_read_cursors() {
    /// Each set remains below the 192 KiB text limit while their root tree exceeds one page.
    const RULES_PER_SET: usize = 24;
    const VALUE_BYTES: usize = 3000;
    let mut fixture = Fixture::new();
    fixture.apply("user.root", None, &Fixture::rule("root", 50), 0);
    for key in ["user.first", "user.second", "user.third"] {
        let entries: String = (0..RULES_PER_SET)
            .map(|index| {
                Fixture::rule(&format!("rule-{index}"), 50)
                    .replace("code.exe", &"x".repeat(VALUE_BYTES))
            })
            .collect();
        fixture.apply(key, Some("user.root"), &entries, 0);
    }
    let mut request = RuleSetsGetRequest {
        key: "user.root".into(),
        version: None,
        view: RuleView::Effective,
        cursor: None,
    };
    let mut count = 0;
    let mut pages = 0;
    let mut first_cursor = None;
    loop {
        let inputs = fixture.inputs();
        let page = inputs
            .contents(request.clone(), inputs.versions())
            .expect("page");
        assert!(serde_json::to_vec(&page).expect("encoded page").len() < wire::QUERY_PAGE_BYTES);
        count += page.entries.len();
        pages += 1;
        if first_cursor.is_none() {
            first_cursor = page.next_cursor.clone();
        }
        request.cursor = page.next_cursor;
        if request.cursor.is_none() {
            break;
        }
    }
    assert!(pages > 1);
    assert_eq!(count, 1 + 3 * RULES_PER_SET);
    let before = fixture.inputs();
    let old_versions = before.versions();
    let versions_cursor = wire::QueryCursor::new(
        old_versions.clone(),
        wire::CursorQuery::RuleSetsVersions {
            key: "user.root".into(),
        },
        1,
    );
    let tree_cursor = wire::QueryCursor::new(
        old_versions.clone(),
        wire::CursorQuery::RuleSetsTree {
            rule_kind: RuleKind::Classification,
        },
        1,
    );
    fixture.run(RegistryCommand::Import {
        key: "user.unrelated".parse().expect("key"),
        file: "import.toml".into(),
        text: Fixture::source("user.unrelated", None, &Fixture::rule("imported", 50)),
        base_version: 0,
    });
    let inputs = fixture.inputs();
    assert_ne!(inputs.versions(), old_versions);
    request.cursor = first_cursor;
    assert_eq!(
        inputs
            .contents(request, inputs.versions())
            .expect_err("stale contents")
            .kind,
        ErrorKind::StaleCursor
    );
    assert_eq!(
        inputs
            .tree(
                RuleSetsTreeRequest {
                    kind: RuleKind::Classification,
                    cursor: Some(tree_cursor)
                },
                inputs.versions()
            )
            .expect_err("stale tree")
            .kind,
        ErrorKind::StaleCursor
    );
    assert_eq!(
        inputs
            .revisions(
                RuleSetsVersionsRequest {
                    key: "user.root".into(),
                    cursor: Some(versions_cursor)
                },
                inputs.versions()
            )
            .expect_err("stale history")
            .kind,
        ErrorKind::StaleCursor
    );
    // An in-flight work unit retains the publication it captured before the import.
    assert_eq!(before.versions(), old_versions);
}

#[test]
fn metadata_enablement_ordering_and_deleted_revisions_are_global_version_inputs() {
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
    fixture.run(RegistryCommand::Reorder {
        siblings: ["user.second", "user.first"]
            .into_iter()
            .map(|key| SiblingRevision {
                key: key.parse().expect("key"),
                base_version: 1,
            })
            .collect(),
    });
    assert_ne!(fixture.inputs().versions(), before.versions());
    assert_eq!(
        fixture
            .tree()
            .nodes
            .iter()
            .filter(|node| node.key.starts_with("user."))
            .map(|node| node.key.as_str())
            .collect::<Vec<_>>(),
        ["user.root", "user.second", "user.first"]
    );
    let before = fixture.inputs().versions();
    fixture.run(RegistryCommand::Toggle {
        key: "user.first".parse().expect("key"),
        base_version: 2,
        enabled: Enablement::Disabled,
    });
    assert_ne!(fixture.inputs().versions(), before);
    let before = fixture.inputs().versions();
    fixture.run(RegistryCommand::Delete {
        key: "user.first".parse().expect("key"),
        base_version: 3,
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
fn tree_and_revision_lists_split_without_losing_order_or_revisions() {
    /// Long but valid keys make the tree exceed a page with bounded fixture size.
    const SET_COUNT: usize = 900;
    const KEY_PADDING: usize = 120;
    /// Revision notes exercise byte paging independently of revision count.
    const REVISION_COUNT: u32 = 3000;
    const NOTE_BYTES: usize = 120;
    let mut fixture = Fixture::new();
    fixture.apply("user.history", None, "", 0);
    let mut state = fixture.shell.core().state().clone();
    let template_key = "user.history".parse().expect("key");
    let template = state.sets[&template_key].clone();
    for index in 0..SET_COUNT {
        let key = format!("user.paged-{index:04}-{}", "x".repeat(KEY_PADDING));
        let mut entry = template.clone();
        entry.current.set.header.key = key.clone();
        entry.current.sibling_order = 0;
        state.sets.insert(key.parse().expect("key"), entry);
    }
    let history = state.sets.get_mut(&template_key).expect("history");
    let revision = history.versions[&1].clone();
    for version in 2..=REVISION_COUNT {
        let mut revision = revision.clone();
        revision.version = version;
        revision.note = Some("n".repeat(NOTE_BYTES));
        history.versions.insert(version, revision);
    }
    history.current.version = REVISION_COUNT;
    let registry = RuleRegistry::from_state(state).expect("large retained registry");
    let inputs = ReadInputs {
        snapshot: registry.snapshots().load(),
        metadata_version: 0,
    };
    let versions = inputs.versions();
    let mut request = RuleSetsTreeRequest {
        kind: RuleKind::Classification,
        cursor: None,
    };
    let mut keys = Vec::new();
    let mut pages = 0;
    loop {
        let page = inputs
            .tree(request.clone(), versions.clone())
            .expect("tree page");
        assert!(serde_json::to_vec(&page).expect("tree JSON").len() < wire::QUERY_PAGE_BYTES);
        keys.extend(page.nodes.into_iter().map(|node| node.key));
        pages += 1;
        request.cursor = page.next_cursor;
        if request.cursor.is_none() {
            break;
        }
    }
    assert!(pages > 1);
    assert_eq!(
        keys.iter()
            .filter(|key| key.starts_with("user.paged-"))
            .count(),
        SET_COUNT
    );
    let paged_keys: Vec<_> = keys
        .iter()
        .filter(|key| key.starts_with("user.paged-"))
        .collect();
    assert!(paged_keys.windows(2).all(|window| window[0] < window[1]));
    let mut request = RuleSetsVersionsRequest {
        key: "user.history".into(),
        cursor: None,
    };
    let mut revisions = Vec::new();
    let mut pages = 0;
    loop {
        let page = inputs
            .revisions(request.clone(), versions.clone())
            .expect("revision page");
        assert!(serde_json::to_vec(&page).expect("revision JSON").len() < wire::QUERY_PAGE_BYTES);
        revisions.extend(page.revisions.into_iter().map(|revision| revision.version));
        pages += 1;
        request.cursor = page.next_cursor;
        if request.cursor.is_none() {
            break;
        }
    }
    assert!(pages > 1);
    assert_eq!(revisions, (1..=REVISION_COUNT).rev().collect::<Vec<_>>());
}

#[test]
fn oversized_stored_entries_return_internal_instead_of_an_unpageable_success() {
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
                    version: None,
                    view: RuleView::Own,
                    cursor: None
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
            .tree(
                RuleSetsTreeRequest { kind, cursor: None },
                inputs.versions(),
            )
            .expect("family tree");
        let hash = match kind {
            RuleKind::Classification => &inputs.snapshot.hashes().classification,
            RuleKind::Extraction => &inputs.snapshot.hashes().extraction,
            RuleKind::Privacy => &inputs.snapshot.hashes().privacy,
        };
        assert_eq!(tree.effective_version, hash.as_str());
    }
    let privacy = fixture.get("builtin.privacy", None, RuleView::Own);
    assert!(privacy.entries.iter().all(|entry| matches!(
        entry.content.app,
        Some(wire::RuleApp::Action(_))
    ) && entry.content.window_title.is_some()
        && entry.content.web.is_some()));
    let extraction = fixture.get("builtin.extraction", None, RuleView::Own);
    assert!(extraction.entries.iter().all(|entry| matches!(
        entry.content.app,
        Some(wire::RuleApp::Conditions(_))
    ) && entry.content.pattern.is_some()));
}

#[test]
fn root_reorders_keep_builtin_first_and_reorder_only_active_user_roots() {
    let mut fixture = Fixture::new();
    fixture.apply("a.custom", None, &Fixture::rule("a", 50), 0);
    fixture.apply("z.custom", None, &Fixture::rule("z", 50), 0);
    fixture.apply("user.deleted", None, &Fixture::rule("deleted", 50), 0);
    fixture.run(RegistryCommand::Delete {
        key: "user.deleted".parse().expect("key"),
        base_version: 1,
    });
    fixture.run(RegistryCommand::Reorder {
        siblings: vec![
            SiblingRevision {
                key: "z.custom".parse().expect("key"),
                base_version: 1,
            },
            SiblingRevision {
                key: "a.custom".parse().expect("key"),
                base_version: 1,
            },
        ],
    });
    let tree = fixture.tree();
    let roots: Vec<_> = tree
        .nodes
        .iter()
        .filter(|node| node.parent.is_none())
        .collect();
    assert!(roots.iter().map(|node| node.key.as_str()).eq([
        "builtin.classification",
        "z.custom",
        "a.custom"
    ]));
    assert_eq!(roots[0].sibling_order, 0);
    assert_eq!(
        roots[0].version,
        quartz_domain::rules::BUILTIN_CLASSIFICATION_REVISION
    );
    assert_eq!(roots[1].sibling_order, 1);
    assert_eq!(roots[2].sibling_order, 2);
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
    fn export_request(key: &str, version: Option<NonZeroU32>) -> wire::RulesTextExportRequest {
        wire::RulesTextExportRequest {
            key: key.into(),
            version,
            cursor: None,
        }
    }
}
#[test]
fn text_exports_preserve_saved_headers_overlays_and_deleted_history_after_reopening() {
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
        .export_text(Fixture::export_request(key, None))
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
    fixture.run(RegistryCommand::Delete {
        key: key.parse().expect("key"),
        base_version: 2,
    });
    fixture.shell =
        RegistryShell::open(&mut fixture.store, Fixture::now()).expect("reopened registry");
    assert!(matches!(
        fixture
            .export_text(Fixture::export_request(key, None))
            .expect_err("deleted current")
            .kind,
        ErrorKind::RuleSetNotFound { version: None, .. }
    ));
    let historical = fixture
        .export_text(Fixture::export_request(key, NonZeroU32::new(1)))
        .expect("retained history");
    assert_eq!(historical.text, first.text);
    let renamed = fixture
        .export_text(Fixture::export_request(key, NonZeroU32::new(2)))
        .expect("renamed history");
    assert!(renamed.text.contains("Renamed"));
    assert!(renamed.text.contains("New description"));
    assert!(matches!(
        fixture
            .export_text(Fixture::export_request(key, NonZeroU32::new(99)))
            .expect_err("missing revision")
            .kind,
        ErrorKind::RuleSetNotFound {
            version: Some(99),
            ..
        }
    ));
    fixture.run(RegistryCommand::Apply {
        key: key.parse().expect("key"),
        file: "restore.toml".into(),
        text: source,
        base_version: 0,
    });
    fixture.run(RegistryCommand::Revert {
        key: key.parse().expect("key"),
        base_version: 4,
        version: 2,
    });
    let reverted = fixture
        .export_text(Fixture::export_request(key, None))
        .expect("reverted header");
    assert_eq!(reverted.text, renamed.text);
}
#[test]
fn text_export_pages_fit_encoded_budget_preserve_utf8_and_reject_stale_or_rebound_cursors() {
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
    fixture.apply("user.pages", None, &entries, 0);
    let request = Fixture::export_request("user.pages", None);
    let first = fixture.export_text(request.clone()).expect("first page");
    assert!(first.next_cursor.is_some());
    let mut text = first.text.clone();
    let mut cursor = first.next_cursor.clone();
    while let Some(next) = cursor {
        let page = fixture
            .export_text(wire::RulesTextExportRequest {
                cursor: Some(next),
                ..request.clone()
            })
            .expect("next UTF-8 page");
        assert!(serde_json::to_vec(&page).expect("page bytes").len() <= wire::QUERY_PAGE_BYTES);
        text.push_str(&page.text);
        cursor = page.next_cursor;
    }
    let current = fixture
        .shell
        .core()
        .state()
        .sets
        .get(&"user.pages".parse().expect("key"))
        .expect("saved set");
    assert_eq!(text, current.current.set.canonical_text());
    assert!(text.len() <= wire::MAX_RULE_TEXT_BYTES);
    assert!(
        crate::rules::write::RuleMutation::validate(
            wire::RulesTextValidateRequest {
                key: "user.pages".into(),
                base_version: 1,
                text
            },
            &fixture.shell
        )
        .expect("export validates")
        .valid
    );
    let split_character = first.text.find('中').expect("multibyte text") + 1;
    let invalid = wire::QueryCursor::new(
        first.versions.clone(),
        wire::CursorQuery::RulesTextExport {
            key: request.key.clone(),
            version: None,
        },
        split_character,
    );
    assert_eq!(
        fixture
            .export_text(wire::RulesTextExportRequest {
                cursor: Some(invalid),
                ..request.clone()
            })
            .expect_err("offset splits UTF-8 character")
            .kind,
        ErrorKind::BadRequest
    );
    let cursor = first.next_cursor.expect("cursor");
    let mut rebound = request.clone();
    rebound.version = NonZeroU32::new(1);
    rebound.cursor = Some(cursor.clone());
    assert!(
        ReadRequest::decode(
            wire::Op::RulesTextExport,
            serde_json::to_value(rebound).expect("body")
        )
        .is_err()
    );
    let invalid = wire::QueryCursor::new(
        first.versions.clone(),
        wire::CursorQuery::RulesTextExport {
            key: request.key.clone(),
            version: None,
        },
        usize::MAX,
    );
    assert_eq!(
        fixture
            .export_text(wire::RulesTextExportRequest {
                cursor: Some(invalid),
                ..request.clone()
            })
            .expect_err("offset out of range")
            .kind,
        ErrorKind::BadRequest
    );
    fixture.apply("user.pages", None, &entries, 1);
    assert_eq!(
        fixture
            .export_text(wire::RulesTextExportRequest {
                cursor: Some(cursor),
                ..request
            })
            .expect_err("new revision invalidates cursor")
            .kind,
        ErrorKind::StaleCursor
    );
}

impl Fixture {
    fn legacy_header_history() -> Self {
        let mut fixture = Self::new();
        fixture.apply("user.legacy", None, &Self::rule("one", 50), 0);
        fixture.store.rewrite_rule_revisions_as_legacy();
        fixture.shell =
            RegistryShell::open(&mut fixture.store, Self::now()).expect("legacy registry");
        let old = fixture
            .export_text(Self::export_request("user.legacy", NonZeroU32::new(1)))
            .expect("legacy historical export");
        assert!(!old.text.contains("description"));
        assert_eq!(
            old.text,
            fixture.shell.core().state().sets[&"user.legacy".parse().expect("key")]
                .current
                .set
                .canonical_text()
        );
        let source = Self::source("user.legacy", None, &Self::rule("two", 50)).replace(
            "name='Read test'",
            "name='New name'\ndescription='Later description'",
        );
        fixture.run(RegistryCommand::Apply {
            key: "user.legacy".parse().expect("key"),
            file: "new.toml".into(),
            text: source,
            base_version: 1,
        });
        fixture.shell =
            RegistryShell::open(&mut fixture.store, Self::now()).expect("reopen mixed history");
        fixture
    }
}
#[test]
fn legacy_exports_use_row_header_without_later_revision_description() {
    let mut fixture = Fixture::legacy_header_history();
    let text = fixture
        .export_text(Fixture::export_request("user.legacy", NonZeroU32::new(1)))
        .expect("legacy export")
        .text;
    let parsed = RuleSet::parse(
        "legacy.toml",
        &text,
        ParseContext {
            origin: RuleOrigin::User,
            references: &fixture.shell.core().state().references,
        },
    )
    .expect("legacy text");
    assert_eq!(parsed.header.name, "New name");
    assert_eq!(parsed.header.description, None);
    assert_eq!(parsed.header.chain, None);
    assert_eq!(parsed.content.rule[0].id, "one");
}
#[test]
fn reverting_legacy_revisions_uses_row_header_without_later_revision_description() {
    let mut fixture = Fixture::legacy_header_history();
    fixture.run(RegistryCommand::Revert {
        key: "user.legacy".parse().expect("key"),
        base_version: 2,
        version: 1,
    });
    fixture.shell =
        RegistryShell::open(&mut fixture.store, Fixture::now()).expect("reopen reverted history");
    let entry = &fixture.shell.core().state().sets[&"user.legacy".parse().expect("key")];
    assert_eq!(entry.current.version, 3);
    assert_eq!(entry.current.set.content.rule[0].id, "one");
    let header = entry.versions[&3]
        .header
        .as_ref()
        .expect("new saved header");
    assert_eq!(header.name, "New name");
    assert_eq!(header.description, None);
    assert_eq!(header.chain, None);
    let exported = fixture
        .export_text(Fixture::export_request("user.legacy", None))
        .expect("reverted export");
    assert!(!exported.text.contains("description"));
}
