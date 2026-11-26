use super::*;
use crate::rules::{
    Action, CaseSensitivity, Condition, Field, FieldSnapshot, MatchMode, Operator, PrivacyAction,
    ReferenceCatalog, RuleBody, RuleKind, WebAction,
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
fn toggle_and_delete_have_explicit_atomic_write_descriptions() {
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
    fixture.commit(apply("user.b", "Edited", 1)).expect("edit");
    let key = "user.b".parse().expect("key");
    assert!(
        matches!(fixture.core.decide(RegistryCommand::Delete { key, base_version: 2 }).writes(), [RuleSetWrite::MarkDeleted(update)] if update.deletion == RuleSetDeletion::Deleted && update.revision.version == 3)
    );
    fixture
        .commit(RegistryCommand::Delete {
            key: "user.b".parse().expect("key"),
            base_version: 2,
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
            current_version: Some(4),
            ..
        })
    ));
    assert_eq!(fixture.core.snapshots().load().epoch().get(), 6);
}

#[test]
fn reparenting_is_rejected_without_writes() {
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
fn imports_prepare_user_revisions_and_builtin_revision_reuse_is_checked() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Original"),
            revision: 1,
        })
        .expect("builtin");
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
    fixture
        .commit(RegistryCommand::Import {
            key: "user.import".parse().expect("key"),
            file: "import.toml".into(),
            text: source("user.import", "Import", None),
            base_version: 0,
        })
        .expect("prepared import");
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

#[test]
fn builtin_metadata_mutations_are_rejected_without_writes_or_publication() {
    let mut fixture = Fixture::new();
    fixture
        .commit(RegistryCommand::InstallBuiltin {
            set: builtin("Builtin"),
            revision: 1,
        })
        .expect("builtin");
    let snapshot = fixture.core.snapshots().load();
    let decision = fixture.core.decide(RegistryCommand::Toggle {
        key: "builtin.test".parse().expect("key"),
        base_version: 1,
        enabled: Enablement::Disabled,
    });
    assert!(
        matches!(decision.reply(),Err(DecideRegistryCommandError::ReservedSet {key}) if key.as_str() == "builtin.test")
    );
    assert!(decision.writes().is_empty());
    assert!(Arc::ptr_eq(&snapshot, &fixture.core.snapshots().load()));
}

#[test]
fn new_and_restored_sets_reject_invalid_ancestry_with_located_parent_diagnostics() {
    enum TargetState {
        New,
        Deleted,
    }
    for target_state in [TargetState::New, TargetState::Deleted] {
        for parent in ["user.missing", "INVALID!", "user.child", "user.ancestor"] {
            let mut fixture = Fixture::new();
            fixture
                .commit(apply("user.child", "Child", 0))
                .expect("seed child");
            fixture
                .commit(apply("user.ancestor", "Ancestor", 0))
                .expect("seed ancestor");
            let mut state = fixture.core.state().clone();
            let child = state
                .sets
                .get_mut(&"user.child".parse().expect("key"))
                .expect("child");
            child.deletion = RuleSetDeletion::Deleted;
            child.current.set.header.parent = Some(parent.into());
            if parent == "user.ancestor" {
                state
                    .sets
                    .get_mut(&"user.ancestor".parse().expect("key"))
                    .expect("ancestor")
                    .current
                    .set
                    .header
                    .parent = Some("user.child".into());
            }
            if matches!(target_state, TargetState::New) {
                state.sets.remove(&"user.child".parse().expect("key"));
            }
            // A missing or cyclic retained chain must be rejected before a candidate is published.
            let mut registry = fixture.core;
            registry.state = state;
            let decision = registry.decide(RegistryCommand::Apply {
                key: "user.child".parse().expect("key"),
                file: "restore.toml".into(),
                text: source("user.child", "Restored", Some(parent)),
                base_version: 0,
            });
            let Err(DecideRegistryCommandError::Snapshot {
                source: BuildRuleSnapshotError::Tree { diagnostics },
            }) = decision.reply()
            else {
                panic!("parent diagnostic required: {:?}", decision.reply())
            };
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].code, rules::DiagnosticCode::Parent);
            assert_eq!(diagnostics[0].path, "set.parent");
            assert_eq!(
                diagnostics[0].location.file,
                std::path::PathBuf::from("restore.toml")
            );
            assert!(decision.writes().is_empty());
        }
    }
}

#[test]
fn disposable_candidate_matches_a_committed_snapshot_without_publication() {
    let mut fixture = Fixture::new();
    let before = fixture.core.snapshots().load();
    let draft = fixture
        .core
        .decide(apply("user.test", "Draft", 0))
        .into_draft()
        .expect("privacy draft");
    assert!(Arc::ptr_eq(&before, &fixture.core.snapshots().load()));
    assert!(fixture.core.state().sets.is_empty());
    assert_eq!(draft.writes.len(), 1);
    fixture
        .commit(apply("user.test", "Draft", 0))
        .expect("same command");
    let committed = fixture.core.snapshots().load();
    assert_eq!(draft.snapshot.hashes(), committed.hashes());
    assert_eq!(draft.snapshot.epoch(), committed.epoch());
    let key = "user.test".parse().expect("key");
    assert_eq!(
        draft.snapshot.state().sets[&key].versions,
        committed.state().sets[&key].versions
    );
}

#[test]
fn disposable_candidate_keeps_write_rejections_and_does_not_publish() {
    let mut fixture = Fixture::new();
    fixture
        .commit(apply("user.test", "Installed", 0))
        .expect("installed");
    let before = fixture.core.snapshots().load();
    let error = fixture
        .core
        .decide(apply("user.test", "Stale", 0))
        .into_draft()
        .expect_err("stale draft");
    assert!(matches!(
        error,
        DecideRegistryCommandError::Stale {
            current_version: Some(1),
            ..
        }
    ));
    assert!(Arc::ptr_eq(&before, &fixture.core.snapshots().load()));
    let no_change = fixture.core.decide(RegistryCommand::InstallBuiltin {
        set: builtin("Builtin"),
        revision: 1,
    });
    let parts = no_change.into_parts();
    fixture
        .core
        .committed(parts.outcome.expect("builtin install"))
        .expect("commit builtin");
    let before = fixture.core.snapshots().load();
    let draft = fixture
        .core
        .decide(RegistryCommand::InstallBuiltin {
            set: builtin("Builtin"),
            revision: 1,
        })
        .into_draft()
        .expect("unchanged builtin");
    assert!(draft.writes.is_empty());
    assert!(Arc::ptr_eq(&before, &draft.snapshot));
    assert!(Arc::ptr_eq(&before, &fixture.core.snapshots().load()));
}
