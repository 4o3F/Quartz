use super::*;
use crate::rules::{
    Action, CaseSensitivity, Condition, Field, FieldSnapshot, MatchMode, Operator, PrivacyAction,
    ReferenceCatalog, RuleBody, WebAction,
};
use crate::text::NfcText;
use std::sync::Arc;

fn source(key: &str, label: &str, parent: Option<&str>) -> String {
    let parent = parent
        .map(|parent| format!("parent='{parent}'\n"))
        .unwrap_or_default();
    format!(
        "format='quartz-rules/1'\n[set]\nkey='{key}'\nkind='privacy'\nname='Test'\n{parent}[[rule]]\nid='one'\nlabel='{label}'\npreset='title_hidden'\nwhen=[{{field='app.exe_name',op='equals',value='quartz-test.exe'}}]\n"
    )
}
fn builtin(label: &str) -> RuleSet {
    RuleSet::parse(
        "builtin.toml",
        &source("builtin.test", label, None),
        ParseContext {
            origin: RuleOrigin::Builtin,
            references: &ReferenceCatalog::default(),
        },
    )
    .expect("valid builtin")
}
fn apply(key: &str, label: &str, base_version: u32) -> RegistryCommand {
    RegistryCommand::Apply {
        key: key.parse().expect("key"),
        file: "test.toml".into(),
        text: source(key, label, None),
        base_version,
    }
}
fn overlay(key: &str, parent: &str) -> RegistryCommand {
    let text = format!(
        "format='quartz-rules/1'\n[set]\nkey='{key}'\nkind='privacy'\nname='Overlay'\nparent='{parent}'\n[[override]]\ntarget='builtin.test/one'\nlabel='User replacement'\npreset='excluded'\nwhen=[{{field='app.exe_name',op='equals',value='quartz-test.exe'}}]\n"
    );
    RegistryCommand::Apply {
        key: key.parse().expect("key"),
        file: "overlay.toml".into(),
        text,
        base_version: 0,
    }
}
struct Fixture {
    core: RuleRegistry,
}
impl Fixture {
    fn new() -> Self {
        Self {
            core: RuleRegistry::from_state(RegistryState::default()).expect("empty state"),
        }
    }
    fn commit(&mut self, command: RegistryCommand) -> RegistryReply {
        let decision = self.core.decide(command);
        let DecisionParts { reply, outcome, .. } = decision.into_parts();
        if let Some(outcome) = outcome {
            self.core
                .committed(outcome)
                .expect("sequential acknowledgement");
        }
        reply
    }
}

#[test]
fn stale_command_returns_code_and_current_revision_without_writes() {
    let mut fixture = Fixture::new();
    fixture
        .commit(apply("user.test", "Original", 0))
        .expect("first version");
    let before = fixture.core.snapshots().load();
    let decision = fixture.core.decide(apply("user.test", "Old client", 0));
    assert!(decision.writes().is_empty());
    let error = decision.reply().as_ref().expect_err("stale");
    assert_eq!(error.code(), Some(RuleWriteCode::RuleSetStale));
    assert!(matches!(
        error,
        DecideRegistryCommandError::Stale {
            current_version: Some(1),
            base_version: 0,
            ..
        }
    ));
    assert!(Arc::ptr_eq(&before, &fixture.core.snapshots().load()));
    assert_eq!(
        fixture.core.state().sets[&"user.test".parse().expect("key")]
            .current
            .version,
        1
    );
}

#[test]
fn compilation_failure_is_a_pure_rejection_with_no_writes_or_epoch_change() {
    let fixture = Fixture::new();
    let mut set = builtin("Invalid");
    set.content.rule[0].body = RuleBody::Privacy {
        action: PrivacyAction {
            app: Action::Keep,
            window_title: Action::Drop,
            web: WebAction::Full,
        },
        mode: MatchMode::All,
        when: vec![Condition {
            field: Field::ExeName,
            op: Operator::Regex,
            value: "(".into(),
            case: CaseSensitivity::Folded,
            negate: false,
        }],
    };
    let decision = fixture
        .core
        .decide(RegistryCommand::InstallBuiltin { set, revision: 1 });
    assert!(decision.writes().is_empty());
    assert!(matches!(
        decision.reply(),
        Err(DecideRegistryCommandError::Snapshot {
            source: BuildRuleSnapshotError::Compile { .. }
        })
    ));
    assert_eq!(fixture.core.snapshots().load().epoch().get(), 0);
    assert!(fixture.core.state().sets.is_empty());
}

#[test]
fn valid_decision_describes_writes_and_publication_waits_for_commit() {
    let mut fixture = Fixture::new();
    let old = fixture.core.snapshots().load();
    let decision = fixture.core.decide(apply("user.test", "First", 0));
    let [RuleSetWrite::CommitVersion(update)] = decision.writes() else {
        panic!("one user version write")
    };
    assert_eq!(update.key.as_str(), "user.test");
    assert_eq!(update.expected, ExpectedRuleVersion::Absent);
    assert_eq!(update.revision.version, 1);
    assert_eq!(
        update.revision.content_hash(),
        update.set.content.content_hash()
    );
    assert!(fixture.core.state().sets.is_empty());
    assert!(Arc::ptr_eq(&old, &fixture.core.snapshots().load()));
    let DecisionParts { reply, outcome, .. } = decision.into_parts();
    let event = fixture
        .core
        .committed(outcome.expect("pending candidate"))
        .expect("commit");
    assert_eq!(event.epoch.get(), 1);
    assert_eq!(reply.expect("success").epoch, event.epoch);
    let current = fixture.core.snapshots().load();
    let facts = FieldSnapshot {
        text: BTreeMap::from([(Field::ExeName, NfcText::new("quartz-test.exe"))]),
        domain: None,
    };
    assert_eq!(old.epoch().get(), 0);
    assert_eq!(
        old.privacy().evaluate(&facts).privacy.window_title,
        Action::Keep
    );
    assert_eq!(
        current.privacy().evaluate(&facts).privacy.window_title,
        Action::Drop
    );
    assert_ne!(old.hashes(), current.hashes());
    let rebuilt =
        RuleRegistry::from_state(fixture.core.state().clone()).expect("restart from values");
    assert_eq!(rebuilt.snapshots().load().hashes(), current.hashes());
    assert_eq!(rebuilt.snapshots().load().epoch().get(), 0);
}

#[test]
fn toggle_reorder_revert_and_delete_have_explicit_atomic_write_descriptions() {
    let mut fixture = Fixture::new();
    fixture.commit(apply("user.a", "A", 0)).expect("A");
    fixture.commit(apply("user.b", "B", 0)).expect("B");
    let toggle = RegistryCommand::Toggle {
        key: "user.a".parse().expect("key"),
        base_version: 1,
        enabled: Enablement::Disabled,
    };
    assert!(
        matches!(fixture.core.decide(toggle).writes(), [RuleSetWrite::Toggle(update)] if update.enabled == Enablement::Disabled && update.revision.version == 2)
    );
    fixture
        .commit(RegistryCommand::Toggle {
            key: "user.a".parse().expect("key"),
            base_version: 1,
            enabled: Enablement::Disabled,
        })
        .expect("toggle");
    let siblings = vec![
        SiblingRevision {
            key: "user.b".parse().expect("key"),
            base_version: 1,
        },
        SiblingRevision {
            key: "user.a".parse().expect("key"),
            base_version: 2,
        },
    ];
    let decision = fixture.core.decide(RegistryCommand::Reorder {
        siblings: siblings.clone(),
    });
    assert!(
        matches!(decision.writes(), [RuleSetWrite::Reorder(b), RuleSetWrite::Reorder(a)] if b.sibling_order == 0 && a.sibling_order == 1)
    );
    fixture
        .commit(RegistryCommand::Reorder { siblings })
        .expect("reorder");
    fixture.commit(apply("user.b", "Edited", 2)).expect("edit");
    let original_hash = fixture
        .core
        .state()
        .version(&"user.b".parse().expect("key"), 1)
        .expect("history")
        .content_hash();
    fixture
        .commit(RegistryCommand::Revert {
            key: "user.b".parse().expect("key"),
            base_version: 3,
            version: 1,
        })
        .expect("revert");
    assert_eq!(
        fixture
            .core
            .state()
            .version(&"user.b".parse().expect("key"), 4)
            .expect("new revision")
            .content_hash(),
        original_hash
    );
    let key = "user.b".parse().expect("key");
    assert!(
        matches!(fixture.core.decide(RegistryCommand::Delete { key, base_version: 4 }).writes(), [RuleSetWrite::MarkDeleted(update)] if update.deletion == RuleSetDeletion::Deleted && update.revision.version == 5)
    );
    fixture
        .commit(RegistryCommand::Delete {
            key: "user.b".parse().expect("key"),
            base_version: 4,
        })
        .expect("delete");
    assert!(
        !fixture
            .core
            .state()
            .installed_sets()
            .iter()
            .any(|entry| entry.set.header.key == "user.b")
    );
    assert!(
        fixture
            .core
            .state()
            .version(&"user.b".parse().expect("key"), 1)
            .is_some()
    );
    fixture
        .commit(apply("user.b", "Recreated", 0))
        .expect("restore key");
    assert!(matches!(
        fixture
            .core
            .decide(apply("user.b", "Old client", 1))
            .reply(),
        Err(DecideRegistryCommandError::Stale {
            current_version: Some(6),
            ..
        })
    ));
    assert_eq!(fixture.core.snapshots().load().epoch().get(), 8);
}

#[test]
fn upstream_changes_follow_parent_versions_and_builtin_revisions_ignore_metadata_edits() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Original"),
            revision: 1,
        })
        .expect("builtin");
    fixture
        .commit(overlay("user.parent", "builtin.test"))
        .expect("parent overlay");
    fixture
        .commit(overlay("user.leaf", "user.parent"))
        .expect("leaf overlay");
    fixture
        .commit(RegistryCommand::Toggle {
            key: "builtin.test".parse().expect("key"),
            base_version: 1,
            enabled: Enablement::Disabled,
        })
        .expect("metadata");
    let reply = fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Changed"),
            revision: 2,
        })
        .expect("new upstream revision");
    assert_eq!(reply.versions[&"builtin.test".parse().expect("key")], 3);
    let parent = reply
        .upstream_changes
        .iter()
        .find(|change| change.set_key().as_str() == "user.parent")
        .expect("root changed under parent");
    assert_eq!(parent.base_parent_version(), Some(1));
    assert_eq!(parent.current_parent_version(), 3);
    assert_eq!(parent.state(), UpstreamState::UpstreamChanged);
    let mut removed = builtin("Removed");
    removed.content.rule.clear();
    removed.content.order.clear();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: removed,
            revision: 3,
        })
        .expect("removed definition");
    assert!(
        fixture
            .core
            .snapshots()
            .load()
            .upstream_changes()
            .iter()
            .any(|change| change.set_key().as_str() == "user.parent"
                && change.state() == UpstreamState::TargetDeleted)
    );
    assert!(
        fixture
            .core
            .snapshots()
            .load()
            .upstream_changes()
            .iter()
            .all(|change| change.state() == UpstreamState::TargetDeleted)
    );
    let rebuilt = RuleRegistry::from_state(fixture.core.state().clone()).expect("recompute alerts");
    assert_eq!(
        fixture.core.snapshots().load().upstream_changes(),
        rebuilt.snapshots().load().upstream_changes()
    );
}

#[test]
fn old_parent_revisions_survive_rejected_reparenting_and_parent_override_edits() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Original"),
            revision: 1,
        })
        .expect("builtin");
    fixture
        .commit(overlay("user.parent", "builtin.test"))
        .expect("parent");
    fixture
        .commit(overlay("user.leaf", "user.parent"))
        .expect("leaf");
    let RegistryCommand::Apply { mut text, .. } = overlay("user.parent", "builtin.test") else {
        panic!("source")
    };
    text = text.replace("User replacement", "Parent edited");
    fixture
        .commit(RegistryCommand::Apply {
            key: "user.parent".parse().expect("key"),
            file: "edit.toml".into(),
            text,
            base_version: 1,
        })
        .expect("parent edit");
    let snapshot = fixture.core.snapshots().load();
    let leaf = snapshot
        .upstream_changes()
        .iter()
        .find(|change| change.set_key().as_str() == "user.leaf")
        .expect("changed parent override");
    assert_eq!(leaf.base_parent_version(), Some(1));
    assert_eq!(leaf.current_parent_version(), 2);
    let mut text = source("user.leaf", "Reparent", Some("builtin.test"));
    text = text.replace("id='one'", "id='new-rule'");
    let rejected = fixture.core.decide(RegistryCommand::Apply {
        key: "user.leaf".parse().expect("key"),
        file: "reparent.toml".into(),
        text,
        base_version: 1,
    });
    assert!(rejected.writes().is_empty());
    assert!(matches!(
        rejected.reply(),
        Err(DecideRegistryCommandError::ReparentSet)
    ));
}

#[test]
fn imports_bind_to_current_privacy_preview_and_builtin_revision_reuse_is_checked() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Original"),
            revision: 1,
        })
        .expect("builtin");
    let before = fixture.core.snapshots().load();
    let same = fixture.core.decide(RegistryCommand::InstallBuiltin {
        set: builtin("Original"),
        revision: 1,
    });
    assert!(same.writes().is_empty());
    assert!(same.reply().is_ok());
    let conflict = fixture.core.decide(RegistryCommand::InstallBuiltin {
        set: builtin("Different"),
        revision: 1,
    });
    assert!(matches!(
        conflict.reply(),
        Err(DecideRegistryCommandError::RevisionConflict)
    ));
    let invalid = fixture.core.decide(RegistryCommand::Import {
        key: "user.import".parse().expect("key"),
        file: "import.toml".into(),
        text: source("user.import", "Import", None),
        base_version: 0,
        preview_hash: None,
    });
    assert!(invalid.writes().is_empty());
    assert!(matches!(
        invalid.reply(),
        Err(DecideRegistryCommandError::StalePreview)
    ));
    fixture
        .commit(RegistryCommand::Import {
            key: "user.import".parse().expect("key"),
            file: "import.toml".into(),
            text: source("user.import", "Import", None),
            base_version: 0,
            preview_hash: Some(before.hashes().privacy.clone()),
        })
        .expect("preview bound import");
}

#[test]
fn startup_degrades_only_bad_sets_and_allows_healthy_commands() {
    let mut fixture = Fixture::new();
    fixture
        .commit(apply("user.bad", "Bad", 0))
        .expect("bad fixture");
    fixture
        .commit(apply("user.good", "Good", 0))
        .expect("good fixture");
    let mut state = fixture.core.state().clone();
    let entry = state
        .sets
        .get_mut(&"user.bad".parse().expect("key"))
        .expect("bad");
    if let RuleBody::Privacy { when, .. } = &mut entry.current.set.content.rule[0].body {
        when[0].op = Operator::Regex;
        when[0].value = "(".into();
    }
    entry.versions.get_mut(&1).expect("version").content = entry.current.set.content.clone();
    let core = RuleRegistry::from_state(state).expect("bad set does not abort startup");
    let snapshot = core.snapshots().load();
    assert!(
        matches!(snapshot.warnings(), [RuleWarning::Compilation { key, version: 1, .. }] if key.as_str() == "user.bad")
    );
    let facts = FieldSnapshot {
        text: BTreeMap::from([(Field::ExeName, NfcText::new("quartz-test.exe"))]),
        domain: None,
    };
    let result = snapshot.privacy().evaluate(&facts);
    assert!(
        result
            .rules
            .iter()
            .any(|rule| rule.rule_id.starts_with("user.good/"))
    );
    assert!(
        !result
            .rules
            .iter()
            .any(|rule| rule.rule_id.starts_with("user.bad/"))
    );
    let mut fixture = Fixture { core };
    fixture
        .commit(apply("user.good", "Edited", 1))
        .expect("healthy edits remain possible");
    assert_eq!(fixture.core.snapshots().load().warnings().len(), 1);
}

#[test]
fn delayed_commit_acknowledgements_cannot_replace_a_newer_snapshot() {
    let mut fixture = Fixture::new();
    let DecisionParts { outcome: first, .. } = fixture
        .core
        .decide(apply("user.first", "First", 0))
        .into_parts();
    let DecisionParts {
        outcome: second, ..
    } = fixture
        .core
        .decide(apply("user.second", "Second", 0))
        .into_parts();
    fixture
        .core
        .committed(first.expect("first candidate"))
        .expect("first acknowledgement");
    let before = fixture.core.snapshots().load();
    assert!(matches!(
        fixture.core.committed(second.expect("second candidate")),
        Err(CommitRuleDecisionError::StaleEpoch {
            expected,
            current,
        }) if expected.get() == 0 && current.get() == 1
    ));
    assert!(Arc::ptr_eq(&before, &fixture.core.snapshots().load()));
}

#[test]
fn nested_overlay_uses_its_saved_parent_revision_after_the_parent_is_edited() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Original"),
            revision: 1,
        })
        .expect("builtin");
    let text = "format='quartz-rules/1'\n[set]\nkey='user.parent'\nkind='privacy'\nname='Parent'\nparent='builtin.test'\n";
    fixture
        .commit(RegistryCommand::Apply {
            key: "user.parent".parse().expect("key"),
            file: "parent.toml".into(),
            text: text.into(),
            base_version: 0,
        })
        .expect("empty intermediate overlay");
    fixture
        .commit(overlay("user.leaf", "user.parent"))
        .expect("leaf");
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Changed"),
            revision: 2,
        })
        .expect("upstream edit");
    fixture
        .commit(RegistryCommand::Apply {
            key: "user.parent".parse().expect("key"),
            file: "parent.toml".into(),
            text: text.replace("name='Parent'", "name='Renamed'"),
            base_version: 1,
        })
        .expect("parent now based on the new upstream");
    let snapshot = fixture.core.snapshots().load();
    let leaf = snapshot
        .upstream_changes()
        .iter()
        .find(|change| change.set_key().as_str() == "user.leaf")
        .expect("original leaf baseline remains changed");
    assert_eq!(leaf.base_parent_version(), Some(1));
    assert_eq!(leaf.current_parent_version(), 2);
    assert_eq!(leaf.base_target_version(), Some(1));
    assert_eq!(leaf.current_target_version(), 2);
    assert_eq!(leaf.target().set_key().as_str(), "builtin.test");
    assert_eq!(leaf.target().rule_id().as_str(), "one");
}

#[test]
fn editing_an_overlay_still_requires_valid_targets_after_an_upstream_deletion() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Original"),
            revision: 1,
        })
        .expect("builtin");
    fixture
        .commit(overlay("user.overlay", "builtin.test"))
        .expect("overlay");
    let mut removed = builtin("Removed");
    removed.content.rule.clear();
    removed.content.order.clear();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: removed,
            revision: 2,
        })
        .expect("builtin removes target");
    let RegistryCommand::Apply {
        key, file, text, ..
    } = overlay("user.overlay", "builtin.test")
    else {
        panic!("source")
    };
    let decision = fixture.core.decide(RegistryCommand::Apply {
        key,
        file,
        text: text.replace("User replacement", "Edited overlay"),
        base_version: 1,
    });
    assert!(decision.writes().is_empty());
    assert!(
        matches!(decision.reply(), Err(DecideRegistryCommandError::Snapshot { source: BuildRuleSnapshotError::Tree { diagnostics } }) if diagnostics.len() == 1 && diagnostics[0].code == rules::DiagnosticCode::Target && diagnostics[0].set_key == "user.overlay" && diagnostics[0].path == "override[0].target")
    );
}

#[test]
fn finishing_startup_preserves_compiled_rules_and_restarts_publication_at_zero() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Startup"),
            revision: 1,
        })
        .expect("install builtin");
    let snapshot = fixture.core.snapshots().load();
    let hashes = snapshot.hashes().clone();
    assert_eq!(snapshot.epoch().get(), 1);
    let mut fields = FieldSnapshot::default();
    fields
        .text
        .insert(Field::ExeName, NfcText::new("quartz-test.exe"));
    let privacy = snapshot.privacy().evaluate(&fields).privacy;
    drop(snapshot);
    fixture.core = fixture.core.finish_startup().expect("unpublished snapshot");
    let snapshot = fixture.core.snapshots().load();
    assert_eq!(snapshot.epoch().get(), 0);
    assert_eq!(snapshot.hashes(), &hashes);
    assert_eq!(snapshot.privacy().evaluate(&fields).privacy, privacy);
    assert!(snapshot.warnings().is_empty());
    fixture
        .commit(apply("user.test", "First publication", 0))
        .expect("runtime edit");
    assert_eq!(fixture.core.snapshots().load().epoch().get(), 1);
}

#[test]
fn finishing_startup_rejects_retained_read_handles_and_snapshots() {
    let core = RuleRegistry::from_state(RegistryState::default()).expect("empty registry");
    let handle = core.snapshots();
    assert!(
        core.finish_startup().is_err(),
        "read handle escaped startup"
    );
    assert_eq!(handle.load().epoch().get(), 0);

    let core = RuleRegistry::from_state(RegistryState::default()).expect("empty registry");
    let snapshot = core.snapshots().load();
    assert!(core.finish_startup().is_err(), "snapshot escaped startup");
    assert_eq!(snapshot.epoch().get(), 0);
}

#[test]
fn retained_keys_reject_family_changes_on_apply_and_import() {
    for kind in [
        RuleKind::Privacy,
        RuleKind::Extraction,
        RuleKind::Classification,
    ] {
        for deletion in [RuleSetDeletion::Active, RuleSetDeletion::Deleted] {
            let mut fixture = Fixture::new();
            let text = format!(
                "format='quartz-rules/1'\n[set]\nkey='user.family'\nkind='{}'\nname='Original'\n",
                kind.as_str()
            );
            fixture
                .commit(RegistryCommand::Apply {
                    key: "user.family".parse().expect("key"),
                    file: "original.toml".into(),
                    text,
                    base_version: 0,
                })
                .expect("original");
            let base_version = if deletion == RuleSetDeletion::Deleted {
                fixture
                    .commit(RegistryCommand::Delete {
                        key: "user.family".parse().expect("key"),
                        base_version: 1,
                    })
                    .expect("delete");
                0
            } else {
                1
            };
            let replacement_kind = if kind == RuleKind::Privacy {
                RuleKind::Extraction
            } else {
                RuleKind::Privacy
            };
            let text = format!(
                "format='quartz-rules/1'\n[set]\nkey='user.family'\nkind='{}'\nname='Changed'\n",
                replacement_kind.as_str()
            );
            for command in [
                RegistryCommand::Apply {
                    key: "user.family".parse().expect("key"),
                    file: "changed.toml".into(),
                    text: text.clone(),
                    base_version,
                },
                RegistryCommand::Import {
                    key: "user.family".parse().expect("key"),
                    file: "changed.toml".into(),
                    text: text.clone(),
                    base_version,
                    preview_hash: None,
                },
                RegistryCommand::Import {
                    key: "user.family".parse().expect("key"),
                    file: "changed.toml".into(),
                    text: text.clone(),
                    base_version,
                    preview_hash: Some(ContentHash::of("stale")),
                },
            ] {
                let before = fixture.core.snapshots().load();
                let decision = fixture.core.decide(command);
                assert!(
                    matches!(
                        decision.reply(),
                        Err(DecideRegistryCommandError::ChangeRuleKind)
                    ),
                    "{kind:?}, {deletion:?}: {:?}",
                    decision.reply()
                );
                assert!(decision.writes().is_empty());
                assert!(Arc::ptr_eq(&before, &fixture.core.snapshots().load()));
            }
        }
    }
}

#[test]
fn revert_uses_historical_database_location() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Original"),
            revision: 1,
        })
        .expect("builtin");
    fixture
        .commit(overlay("user.overlay", "builtin.test"))
        .expect("overlay");
    let RegistryCommand::Apply {
        key, file, text, ..
    } = overlay("user.overlay", "builtin.test")
    else {
        panic!("overlay")
    };
    let header = text
        .split("[[override]]")
        .next()
        .expect("header")
        .to_owned();
    fixture
        .commit(RegistryCommand::Apply {
            key,
            file,
            text: header,
            base_version: 1,
        })
        .expect("remove overlay");
    let mut removed = builtin("Removed");
    removed.content = rules::Content::default();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: removed,
            revision: 2,
        })
        .expect("remove target");
    let decision = fixture.core.decide(RegistryCommand::Revert {
        key: "user.overlay".parse().expect("key"),
        base_version: 2,
        version: 1,
    });
    let Err(DecideRegistryCommandError::Snapshot {
        source: BuildRuleSnapshotError::Tree { diagnostics },
    }) = decision.reply()
    else {
        panic!("invalid historical target")
    };
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].path, "override[0].target");
    assert_eq!(
        diagnostics[0].location.file,
        PathBuf::from("database:user.overlay@1")
    );
    assert_eq!(diagnostics[0].location.bytes, 0..0);
    assert_eq!(diagnostics[0].location.line, 1);
    assert_eq!(diagnostics[0].location.column, 1);
}

#[test]
fn startup_reports_each_tree_diagnostic_once() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Original"),
            revision: 1,
        })
        .expect("builtin");
    fixture
        .commit(overlay("user.overlay", "builtin.test"))
        .expect("overlay");
    let mut state = fixture.core.state().clone();
    let parent = state
        .sets
        .get_mut(&"builtin.test".parse().expect("key"))
        .expect("builtin");
    parent.current.set.content = rules::Content::default();
    parent.versions.get_mut(&1).expect("revision").content = rules::Content::default();
    let core = RuleRegistry::from_state(state).expect("startup retains warning");
    let snapshot = core.snapshots().load();
    let [RuleWarning::Tree(diagnostic)] = snapshot.warnings() else {
        panic!("exactly one tree warning: {:?}", snapshot.warnings())
    };
    assert_eq!(diagnostic.code, rules::DiagnosticCode::Target);
    assert_eq!(diagnostic.set_key, "user.overlay");
    assert_eq!(diagnostic.path, "override[0].target");
    assert_eq!(diagnostic.location.file, PathBuf::from("overlay.toml"));
}

#[test]
fn revert_rejects_historical_body_from_another_family() {
    let mut fixture = Fixture::new();
    fixture
        .commit(apply("user.history", "First", 0))
        .expect("first");
    fixture
        .commit(apply("user.history", "Second", 1))
        .expect("second");
    let mut state = fixture.core.state().clone();
    let entry = state
        .sets
        .get_mut(&"user.history".parse().expect("key"))
        .expect("set");
    entry.versions.get_mut(&1).expect("revision").content.rule[0].body = RuleBody::Extraction {
        app: vec![],
        pattern: "(?P<document>.+)".into(),
    };
    let core = RuleRegistry::from_state(state).expect("current revision remains valid");
    let decision = core.decide(RegistryCommand::Revert {
        key: "user.history".parse().expect("key"),
        base_version: 2,
        version: 1,
    });
    assert!(matches!(
        decision.reply(),
        Err(DecideRegistryCommandError::Snapshot {
            source: BuildRuleSnapshotError::Compile {
                source: rules::CompileRulesError::KindMismatch,
                ..
            },
        })
    ));
    assert!(decision.writes().is_empty());
}

#[test]
fn builtin_revision_cannot_change_family() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("First"),
            revision: 1,
        })
        .expect("first");
    let mut set = builtin("Changed");
    set.header.kind = RuleKind::Extraction;
    set.content = rules::Content::default();
    let decision = fixture
        .core
        .decide(RegistryCommand::InstallBuiltin { set, revision: 2 });
    assert!(matches!(
        decision.reply(),
        Err(DecideRegistryCommandError::ChangeRuleKind)
    ));
    assert!(decision.writes().is_empty());
}
