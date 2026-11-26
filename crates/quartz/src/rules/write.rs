//! Typed rule mutations executed by the database actor's registry writer.

use super::{
    read::ReadInputs,
    registry::{RegistryShell, RunRegistryCommandError},
};
use crate::{persistence::diagnostics::WriteFailure, store::Store};
use quartz_domain::{
    capture::UtcInstant,
    rules::{
        self as domain,
        registry::{
            BuildRuleSnapshotError, DecideRegistryCommandError, RegistryCommand, RuleSetDeletion,
        },
    },
};
use quartz_protocol::{self as wire, ErrorKind, ProtocolError};

#[derive(Clone, serde::Serialize)]
#[serde(tag = "op", content = "body")]
pub(crate) enum RuleMutation {
    #[serde(rename = "rule_sets.edit")]
    Edit(wire::RuleSetsEditRequest),
    #[serde(rename = "rules_text.import")]
    Import(wire::RuleSetsEditRequest),
    #[serde(rename = "rule_sets.set_enabled")]
    SetEnabled(wire::RuleSetsSetEnabledRequest),
    #[serde(rename = "rule_sets.delete")]
    Delete(wire::RuleSetsDeleteRequest),
}
impl std::fmt::Debug for RuleMutation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let operation = match self {
            Self::Edit(_) => "rule_sets.edit",
            Self::Import(_) => "rules_text.import",
            Self::SetEnabled(_) => "rule_sets.set_enabled",
            Self::Delete(_) => "rule_sets.delete",
        };
        f.debug_struct("RuleMutation")
            .field("operation", &operation)
            .finish_non_exhaustive()
    }
}
enum TargetAccess<'registry> {
    Write,
    Validate(&'registry RegistryShell),
}
struct PreparedMutation {
    command: RegistryCommand,
    targets: MutationTargets,
}
struct MutationTargets {
    keys: Vec<domain::RuleSetKey>,
}
impl From<wire::RulePreviewMutation> for RuleMutation {
    fn from(mutation: wire::RulePreviewMutation) -> Self {
        match mutation {
            wire::RulePreviewMutation::Edit(request) => Self::Edit(request),
            wire::RulePreviewMutation::Import(request) => Self::Import(request),
            wire::RulePreviewMutation::SetEnabled(request) => Self::SetEnabled(request),
            wire::RulePreviewMutation::Delete(request) => Self::Delete(request),
        }
    }
}
impl RuleMutation {
    pub(crate) fn target_key(&self) -> Result<&str, ProtocolError> {
        match self {
            Self::Edit(request) | Self::Import(request) => Ok(&request.key),
            Self::SetEnabled(request) => Ok(&request.key),
            Self::Delete(request) => Ok(&request.key),
        }
    }

    pub(crate) fn execute(
        self,
        registry: &mut RegistryShell,
        store: &mut Store,
        now: UtcInstant,
    ) -> Result<wire::RuleSetsWriteResponse, ProtocolError> {
        let prepared = self.prepare(TargetAccess::Write)?;
        prepared.targets.check_response_budget()?;
        // Read fallible response dependencies before committing.
        let metadata_version = store
            .rule_read_metadata()
            .map_err(|error| WriteFailure::setting(&error).into_protocol_error())?;
        let result = registry
            .run_ipc(store, now, prepared.command)
            .map_err(|error| prepared.targets.targets_error(error))?;
        let snapshot = registry.snapshots().load();
        debug_assert_eq!(result.reply.epoch, snapshot.epoch());
        let sets = prepared
            .targets
            .keys
            .iter()
            .map(|key| {
                let entry = &snapshot.state().sets[key];
                wire::MutatedRuleSet {
                    key: key.to_string(),
                    version: entry.current.version,
                    deleted: entry.deletion == RuleSetDeletion::Deleted,
                }
            })
            .collect();
        let versions = ReadInputs {
            snapshot,
            metadata_version,
        }
        .versions();
        Ok(wire::RuleSetsWriteResponse { sets, versions })
    }
    pub(crate) fn validate(
        request: wire::RulesTextValidateRequest,
        registry: &RegistryShell,
    ) -> Result<wire::RulesTextValidateResponse, ProtocolError> {
        let builtin = request.key.starts_with("builtin.");
        let mutation = Self::Edit(wire::RuleSetsEditRequest {
            key: request.key,
            base_version: request.base_version,
            text: request.text,
        });
        let result = if builtin {
            let prepared = mutation.prepare(TargetAccess::Validate(registry))?;
            registry
                .validate(prepared.command)
                .map_err(|error| prepared.targets.targets_error(error))
        } else {
            mutation.draft(registry).map(|_| ())
        };
        match result {
            Ok(()) => Ok(wire::RulesTextValidateResponse {
                valid: true,
                diagnostics: Vec::new(),
                truncated: false,
            }),
            Err(ProtocolError {
                kind:
                    ErrorKind::RuleValidationFailed {
                        diagnostics,
                        truncated,
                    },
                ..
            }) => Ok(wire::RulesTextValidateResponse {
                valid: false,
                diagnostics,
                truncated,
            }),
            Err(error) => Err(error),
        }
    }
    pub(crate) fn draft(
        self,
        registry: &RegistryShell,
    ) -> Result<std::sync::Arc<domain::registry::RuleSnapshot>, ProtocolError> {
        let prepared = self.prepare(TargetAccess::Write)?;
        prepared.targets.check_response_budget()?;
        registry
            .draft(prepared.command)
            .map_err(|error| prepared.targets.targets_error(error))
    }
    fn prepare(self, access: TargetAccess<'_>) -> Result<PreparedMutation, ProtocolError> {
        let mut keys = Vec::new();
        let command = match self {
            Self::Edit(request) | Self::Import(request) => {
                let key = Self::key(request.key)?;
                keys.push(key.clone());
                RegistryCommand::Apply {
                    key,
                    file: "ipc-rule-set.toml".into(),
                    text: request.text,
                    base_version: request.base_version,
                }
            }
            Self::SetEnabled(request) => {
                let key = Self::key(request.key)?;
                keys.push(key.clone());
                RegistryCommand::Toggle {
                    key,
                    base_version: request.base_version.get(),
                    enabled: if request.enabled {
                        domain::Enablement::Enabled
                    } else {
                        domain::Enablement::Disabled
                    },
                }
            }
            Self::Delete(request) => {
                let key = Self::key(request.key)?;
                keys.push(key.clone());
                RegistryCommand::Delete {
                    key,
                    base_version: request.base_version.get(),
                }
            }
        };
        keys.sort();
        if keys.is_empty() {
            return Err(ProtocolError::new(ErrorKind::BadRequest));
        }
        if let Some(key) = keys.iter().find(|key| key.as_str().starts_with("builtin."))
            && !matches!(access, TargetAccess::Validate(registry) if registry.rule_origin(key) == Some(domain::RuleOrigin::Builtin))
        {
            return Err(ProtocolError::new(ErrorKind::RuleSetReadOnly {
                key: key.to_string(),
            }));
        }
        if let RegistryCommand::Apply { text, .. } = &command
            && text.len() > wire::MAX_RULE_TEXT_BYTES
        {
            return Err(ProtocolError::new(ErrorKind::BadRequest));
        }
        Ok(PreparedMutation {
            command,
            targets: MutationTargets { keys },
        })
    }
    fn key(key: String) -> Result<domain::RuleSetKey, ProtocolError> {
        let key: domain::RuleSetKey = key
            .parse()
            .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        Ok(key)
    }
}
impl MutationTargets {
    fn check_response_budget(&self) -> Result<(), ProtocolError> {
        // Reserve the largest u32 revisions and the longer deletion flag before any write.
        let response = wire::RuleSetsWriteResponse {
            sets: self
                .keys
                .iter()
                .map(|key| wire::MutatedRuleSet {
                    key: key.to_string(),
                    version: u32::MAX,
                    deleted: false,
                })
                .collect(),
            versions: wire::QueryVersions(domain::ContentHash::of("").to_string()),
        };
        if serde_json::to_vec(&response)
            .expect("mutation response contains JSON values")
            .len()
            > wire::QUERY_PAGE_BYTES
        {
            return Err(ProtocolError::new(ErrorKind::BadRequest));
        }
        Ok(())
    }
    fn targets_error(&self, error: RunRegistryCommandError) -> ProtocolError {
        let key = self
            .keys
            .first()
            .expect("prepared mutation has targets")
            .to_string();
        let kind = match error {
            RunRegistryCommandError::TextTooLarge => ErrorKind::BadRequest,
            RunRegistryCommandError::Persist { source } => {
                return WriteFailure::rule_persist(&source).into_protocol_error();
            }
            RunRegistryCommandError::Commit { source } => {
                return WriteFailure::rule_commit(&source).into_protocol_error();
            }
            RunRegistryCommandError::Decide { source } => match source {
                DecideRegistryCommandError::Stale {
                    key,
                    current_version,
                    ..
                } => ErrorKind::RuleSetStale {
                    key: key.to_string(),
                    current_version: wire::RuleSetVersion(current_version.unwrap_or(0)),
                },
                DecideRegistryCommandError::ReservedSet { key } => ErrorKind::RuleSetReadOnly {
                    key: key.to_string(),
                },
                DecideRegistryCommandError::HasChildren => ErrorKind::RuleSetHasChildren { key },
                DecideRegistryCommandError::ChangeRuleKind => ErrorKind::RuleSetIdentity {
                    key,
                    field: wire::RuleSetIdentityField::Kind,
                },
                DecideRegistryCommandError::ReparentSet => ErrorKind::RuleSetIdentity {
                    key,
                    field: wire::RuleSetIdentityField::Parent,
                },
                DecideRegistryCommandError::MissingSet
                | DecideRegistryCommandError::MissingRevision => ErrorKind::RuleSetNotFound { key },
                DecideRegistryCommandError::RevisionOverflow
                | DecideRegistryCommandError::EpochOverflow
                | DecideRegistryCommandError::SiblingOrderOverflow { .. } => {
                    ErrorKind::VersionExhausted
                }
                DecideRegistryCommandError::Parse { source } => {
                    return super::diagnostic::Diagnostics::error(source.diagnostics());
                }
                DecideRegistryCommandError::Snapshot {
                    source: BuildRuleSnapshotError::Tree { diagnostics },
                } => return super::diagnostic::Diagnostics::tree(&diagnostics, &self.keys),
                DecideRegistryCommandError::Snapshot {
                    source:
                        BuildRuleSnapshotError::Compile { source, .. }
                        | BuildRuleSnapshotError::Effective { source },
                } => {
                    let code = match source {
                        domain::CompileRulesError::Regex { .. } => domain::DiagnosticCode::Regex,
                        domain::CompileRulesError::Domain { .. } => domain::DiagnosticCode::Value,
                        domain::CompileRulesError::KindMismatch => domain::DiagnosticCode::Set,
                    };
                    ErrorKind::RuleValidationFailed {
                        diagnostics: vec![wire::RuleDiagnostic {
                            code: code.as_str().into(),
                            path: String::new(),
                            location: None,
                            params: Default::default(),
                        }],
                        truncated: false,
                    }
                }
                DecideRegistryCommandError::Snapshot { source } => {
                    return WriteFailure::rule_snapshot(&source).into_protocol_error();
                }
                DecideRegistryCommandError::Identity { .. }
                | DecideRegistryCommandError::IdentityMismatch
                | DecideRegistryCommandError::RevisionConflict
                | DecideRegistryCommandError::InvalidBuiltin => ErrorKind::BadRequest,
            },
        };
        ProtocolError::new(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek};
    #[test]
    fn database_diagnostics_have_no_source_coordinates() {
        let diagnostic = domain::Diagnostic {
            code: domain::DiagnosticCode::Target,
            set_key: "user.test".into(),
            path: "override[0].target".into(),
            location: domain::SourceLocation::database(&"user.test".parse().expect("key"), 1),
            message: "private rule text".into(),
        };
        let error = super::super::diagnostic::Diagnostics::error(&[diagnostic]);
        let encoded = serde_json::to_value(error).expect("error");
        assert!(encoded["diagnostics"][0]["location"].is_null());
    }
    #[test]
    fn diagnostic_byte_limits_include_json_escaping_and_envelope_fields() {
        const CONTROL_ESCAPE_BYTES: usize = r"\u0000".len();
        const DIAGNOSTIC_COUNT: usize = 2;
        let diagnostic = domain::Diagnostic {
            code: domain::DiagnosticCode::Parent,
            set_key: "user.test".into(),
            path: "\0".repeat(wire::QUERY_PAGE_BYTES / (CONTROL_ESCAPE_BYTES * DIAGNOSTIC_COUNT)),
            location: domain::SourceLocation::database(&"user.test".parse().expect("key"), 1),
            message: "private text".into(),
        };
        let error = super::super::diagnostic::Diagnostics::error(&[diagnostic.clone(), diagnostic]);
        assert!(serde_json::to_vec(&error).expect("bounded JSON").len() <= wire::QUERY_PAGE_BYTES);
        let ErrorKind::RuleValidationFailed {
            diagnostics,
            truncated,
        } = error.kind
        else {
            panic!("validation error")
        };
        assert_eq!(diagnostics.len(), 1);
        assert!(truncated);
    }
    #[test]
    fn internal_rule_failures_log_only_typed_causes() {
        use crate::store::rules::ExecuteRuleWritesError;
        use domain::registry::CommitRuleDecisionError;
        let targets = MutationTargets {
            keys: vec!["user.private-marker".parse().expect("key")],
        };
        let invalid_key = || "INVALID!".parse::<domain::RuleSetKey>().expect_err("key");
        let failures = [
            RunRegistryCommandError::Persist {
                source: ExecuteRuleWritesError::Database {
                    source: diesel::result::Error::DatabaseError(
                        diesel::result::DatabaseErrorKind::Unknown,
                        Box::new(String::from("private-marker raw database message")),
                    ),
                },
            },
            RunRegistryCommandError::Persist {
                source: ExecuteRuleWritesError::PreconditionMismatch {
                    key: "user.private-marker".parse().expect("key"),
                },
            },
            RunRegistryCommandError::Commit {
                source: CommitRuleDecisionError::Snapshot {
                    source: BuildRuleSnapshotError::Identity {
                        source: invalid_key(),
                    },
                },
            },
            RunRegistryCommandError::Decide {
                source: DecideRegistryCommandError::Snapshot {
                    source: BuildRuleSnapshotError::Identity {
                        source: invalid_key(),
                    },
                },
            },
        ];
        let mut log = tempfile::tempfile().expect("safe log");
        let subscriber = tracing::Dispatch::new(
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(log.try_clone().expect("log writer")))
                .finish(),
        );
        for failure in failures {
            let error =
                tracing::dispatcher::with_default(&subscriber, || targets.targets_error(failure));
            assert_eq!(error.kind, ErrorKind::Internal);
        }
        log.rewind().expect("rewind");
        let mut text = String::new();
        log.read_to_string(&mut text).expect("log");
        assert_eq!(
            text.lines().count(),
            4,
            "one typed diagnostic per failure: {text}"
        );
        assert!(text.contains("Database"));
        assert!(!text.contains("private-marker"));
    }
    #[test]
    fn rule_edits_read_only_rule_metadata() {
        use crate::data_directory::DataDirectory;
        use diesel::{Connection, connection::SimpleConnection};
        let directory = tempfile::tempdir().expect("isolated rules");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("categories");
        let now = UtcInstant::try_from(0).expect("UTC");
        let mut registry = RegistryShell::open(&mut store, now).expect("registry");
        let mut connection = diesel::sqlite::SqliteConnection::establish(
            directory.path().join("quartz.db").to_str().expect("path"),
        )
        .expect("fixture connection");
        connection.batch_execute("INSERT OR REPLACE INTO settings(key,value) VALUES ('day.start_offset_min','invalid'),('stats_policy_version','invalid');").expect("unrelated invalid settings");
        let request = wire::RuleSetsEditRequest {
            key: "user.test".into(),
            base_version: 0,
            text: "format='quartz-rules/1'\n[set]\nkey='user.test'\nkind='classification'\nname='Test'\n".into(),
        };
        RuleMutation::Edit(request)
            .execute(&mut registry, &mut store, now)
            .expect("rule edit ignores unrelated settings");
    }
    struct ValidationFixture {
        directory: tempfile::TempDir,
        store: Store,
        registry: RegistryShell,
    }
    impl ValidationFixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("isolated validation database");
            let mut store =
                Store::open(&crate::data_directory::DataDirectory::new(directory.path()))
                    .expect("store");
            store.seed_categories().expect("categories");
            let registry = RegistryShell::open(&mut store, UtcInstant::try_from(0).expect("UTC"))
                .expect("registry");
            Self {
                directory,
                store,
                registry,
            }
        }
        fn validate(
            &self,
            key: &str,
            base_version: u32,
            text: &str,
        ) -> Result<wire::RulesTextValidateResponse, ProtocolError> {
            RuleMutation::validate(
                wire::RulesTextValidateRequest {
                    key: key.into(),
                    base_version,
                    text: text.into(),
                },
                &self.registry,
            )
        }
        fn edit(&mut self, key: &str, base_version: u32, text: &str) {
            RuleMutation::Edit(wire::RuleSetsEditRequest {
                key: key.into(),
                base_version,
                text: text.into(),
            })
            .execute(
                &mut self.registry,
                &mut self.store,
                UtcInstant::try_from(0).expect("UTC"),
            )
            .expect("edit");
        }
    }
    #[test]
    fn validation_reports_every_documented_content_code_with_locations() {
        struct Case {
            name: &'static str,
            code: &'static str,
            path: &'static str,
            line: usize,
            column: usize,
        }
        let cases = [
            Case {
                name: "format",
                code: "E-RULE-FORMAT",
                path: "format",
                line: 1,
                column: 10,
            },
            Case {
                name: "key",
                code: "E-RULE-KEY",
                path: "rule[0].unknown",
                line: 11,
                column: 1,
            },
            Case {
                name: "set",
                code: "E-RULE-SET",
                path: "set.key",
                line: 3,
                column: 7,
            },
            Case {
                name: "parent",
                code: "E-RULE-PARENT",
                path: "set.parent",
                line: 6,
                column: 10,
            },
            Case {
                name: "target",
                code: "E-RULE-TARGET",
                path: "disable[0].target",
                line: 13,
                column: 10,
            },
            Case {
                name: "conflict",
                code: "E-RULE-CONFLICT",
                path: "disable[1].target",
                line: 17,
                column: 8,
            },
            Case {
                name: "field",
                code: "E-RULE-FIELD",
                path: "rule[0].when[0].field",
                line: 9,
                column: 19,
            },
            Case {
                name: "op",
                code: "E-RULE-OP",
                path: "rule[0].when[0].op",
                line: 9,
                column: 37,
            },
            Case {
                name: "value",
                code: "E-RULE-VALUE",
                path: "rule[0].when[0].value",
                line: 9,
                column: 57,
            },
            Case {
                name: "regex",
                code: "E-RULE-REGEX",
                path: "rule[0].when[0].value",
                line: 9,
                column: 54,
            },
            Case {
                name: "ref",
                code: "E-RULE-REF",
                path: "rule[0].category",
                line: 10,
                column: 12,
            },
            Case {
                name: "dup",
                code: "E-RULE-DUP",
                path: "rule[1].id",
                line: 12,
                column: 6,
            },
        ];
        let fixture = ValidationFixture::new();
        for case in cases {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../quartz-domain/tests/fixtures/rules/{}.toml",
                case.name
            ));
            let mut text = std::fs::read_to_string(path).expect("negative fixture");
            if case.name == "conflict" {
                text.push_str("\n[[disable]]\ntarget='user.root/one'\n");
            }
            let response = fixture
                .validate("user.fixture", 0, &text)
                .expect("content response");
            assert!(!response.valid, "{}", case.name);
            let diagnostic = response
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == case.code)
                .expect(case.code);
            assert_eq!(diagnostic.path, case.path);
            let location = diagnostic.location.as_ref().expect("submitted text span");
            assert_eq!(location.line, case.line, "{}", case.name);
            assert_eq!(location.column, case.column, "{}", case.name);
            assert!(diagnostic.params.is_empty());
            let encoded = serde_json::to_value(diagnostic).expect("wire diagnostic");
            assert_eq!(encoded.as_object().expect("object").len(), 4);
        }
    }
    #[test]
    fn validation_keeps_edit_preconditions_and_never_changes_persisted_or_published_state() {
        let mut fixture = ValidationFixture::new();
        let text = "format='quartz-rules/1'\n[set]\nkey='user.draft'\nkind='classification'\nname='Draft'\n";
        let before = fixture.store.rule_sets().load().expect("stored state");
        let snapshot = fixture.registry.snapshots().load();
        assert!(
            fixture
                .validate("user.draft", 0, text)
                .expect("valid draft")
                .valid
        );
        assert!(std::sync::Arc::ptr_eq(
            &snapshot,
            &fixture.registry.snapshots().load()
        ));
        let after = fixture
            .store
            .rule_sets()
            .load()
            .expect("unchanged database");
        assert_eq!(before.sets.len(), after.sets.len());
        for (key, entry) in &before.sets {
            assert_eq!(entry.versions, after.sets[key].versions);
        }
        assert!(
            fixture
                .validate(
                    "user.private",
                    0,
                    &text
                        .replace("user.draft", "user.private")
                        .replace("classification", "privacy")
                )
                .expect("privacy dry run")
                .valid
        );
        fixture.edit("user.draft", 0, text);
        assert!(
            fixture
                .validate("user.draft", 1, text)
                .expect("same edit")
                .valid
        );
        assert!(matches!(
            fixture
                .validate("user.draft", 0, "invalid text")
                .expect_err("stale precedes parsing")
                .kind,
            ErrorKind::RuleSetStale { .. }
        ));
        assert!(matches!(
            fixture
                .validate("user.draft", 1, &text.replace("classification", "privacy"))
                .expect_err("kind identity")
                .kind,
            ErrorKind::RuleSetIdentity {
                field: wire::RuleSetIdentityField::Kind,
                ..
            }
        ));
        assert!(matches!(
            fixture
                .validate(
                    "user.draft",
                    1,
                    &format!("{text}parent='builtin.classification'\n")
                )
                .expect_err("parent identity")
                .kind,
            ErrorKind::RuleSetIdentity {
                field: wire::RuleSetIdentityField::Parent,
                ..
            }
        ));
        assert_eq!(
            fixture
                .validate("user.other", 0, text)
                .expect_err("key mismatch")
                .kind,
            ErrorKind::BadRequest
        );
        assert_eq!(
            fixture
                .validate("user.large", 0, &"x".repeat(wire::MAX_RULE_TEXT_BYTES + 1))
                .expect_err("oversize")
                .kind,
            ErrorKind::BadRequest
        );
        let reopened =
            crate::store::ReadOnlyStore::open(&fixture.directory.path().join("quartz.db"))
                .expect("read-only state")
                .rule_sets()
                .load()
                .expect("state");
        assert!(
            !reopened
                .sets
                .contains_key(&"user.private".parse().expect("key"))
        );
    }
    #[test]
    fn builtin_exports_validate_against_their_installed_revisions_without_authorizing_edits() {
        let mut fixture = ValidationFixture::new();
        let snapshot = fixture.registry.snapshots().load();
        for entry in snapshot.state().sets.values() {
            let key = &entry.current.set.header.key;
            let text = entry.current.set.canonical_text();
            assert!(
                fixture
                    .validate(key, entry.current.version, &text)
                    .expect("builtin validation")
                    .valid
            );
            assert!(matches!(
                fixture
                    .validate(key, 0, &text)
                    .expect_err("stale builtin")
                    .kind,
                ErrorKind::RuleSetStale { .. }
            ));
            assert!(matches!(
                RuleMutation::Edit(wire::RuleSetsEditRequest {
                    key: key.clone(),
                    base_version: entry.current.version,
                    text,
                })
                .execute(
                    &mut fixture.registry,
                    &mut fixture.store,
                    UtcInstant::try_from(0).expect("UTC")
                )
                .expect_err("builtin remains read-only")
                .kind,
                ErrorKind::RuleSetReadOnly { .. }
            ));
        }
        assert!(std::sync::Arc::ptr_eq(
            &snapshot,
            &fixture.registry.snapshots().load()
        ));
    }
    #[test]
    fn validation_uses_live_category_references_and_marks_installed_conflict_locations_as_unavailable()
     {
        use crate::persistence::dispatch::WriteRequest;
        let mut fixture = ValidationFixture::new();
        let source = "format='quartz-rules/1'\n[set]\nkey='user.category'\nkind='classification'\nname='Category'\n[[rule]]\nid='one'\nlabel='Label'\ncategory='builtin:development'\nwhen=[{field='app.exe_name',op='equals',value='code.exe'}]\n";
        assert!(
            fixture
                .validate("user.category", 0, source)
                .expect("known category")
                .valid
        );
        let uuid = quartz_domain::categories::BuiltinCategory::Development
            .uuid()
            .0;
        let archive = WriteRequest::decode(
            wire::Op::CategoriesArchive,
            serde_json::json!({"uuid":uuid,"archived":true}),
        )
        .expect("archive request");
        archive
            .execute(
                &mut fixture.store,
                UtcInstant::try_from(0).expect("UTC"),
                None,
                &mut fixture.registry,
            )
            .expect("archive");
        fixture
            .registry
            .refresh_category_references(&mut fixture.store)
            .expect("current references");
        let response = fixture
            .validate("user.category", 0, source)
            .expect("archived reference diagnostic");
        assert!(!response.valid);
        assert!(
            response
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E-RULE-REF"
                    && diagnostic.path == "rule[0].category")
        );
        let overlay = "format='quartz-rules/1'\n[set]\nkey='user.first'\nkind='classification'\nname='Overlay'\nparent='builtin.classification'\n[[override]]\ntarget='builtin.classification/vscode'\nlabel='Override'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='code.exe'}]\n";
        fixture.edit("user.first", 0, overlay);
        let conflict = fixture
            .validate(
                "user.second",
                0,
                &overlay.replace("user.first", "user.second"),
            )
            .expect("tree conflict");
        assert!(!conflict.valid);
        assert!(conflict.diagnostics.iter().any(
            |diagnostic| diagnostic.code == "E-RULE-CONFLICT" && diagnostic.location.is_some()
        ));
        let earlier = fixture
            .validate("user.aaa", 0, &overlay.replace("user.first", "user.aaa"))
            .expect("conflict reported at installed set");
        assert!(earlier.diagnostics.iter().any(
            |diagnostic| diagnostic.code == "E-RULE-CONFLICT" && diagnostic.location.is_none()
        ));
    }
    #[test]
    fn validation_bounds_diagnostic_count_and_rejects_oversized_canonical_text_before_saving() {
        let mut fixture = ValidationFixture::new();
        let mut text = String::from(
            "format='quartz-rules/1'\n[set]\nkey='user.budget'\nkind='privacy'\nname='Budget'\n",
        );
        for index in 0..wire::MAX_RULE_DIAGNOSTICS + 1 {
            use std::fmt::Write;
            writeln!(text,"[[rule]]\nid='item-{index}'\nlabel='Item'\npreset='title_hidden'\nwhen=[{{field='unknown',op='equals',value='x'}}]").expect("string");
        }
        let response = fixture
            .validate("user.budget", 0, &text)
            .expect("diagnostics");
        assert!(!response.valid);
        assert_eq!(response.diagnostics.len(), wire::MAX_RULE_DIAGNOSTICS);
        assert!(response.truncated);
        assert!(serde_json::to_vec(&response).expect("response").len() <= wire::QUERY_PAGE_BYTES);
        // Literal TOML strings can be much smaller than their canonical escaped rendering.
        let text = format!(
            "format='quartz-rules/1'\n[set]\nkey='user.budget'\nkind='classification'\nname='{}'\n",
            "\"".repeat(wire::MAX_RULE_TEXT_BYTES / 2)
        );
        assert!(text.len() < wire::MAX_RULE_TEXT_BYTES);
        assert_eq!(
            fixture
                .validate("user.budget", 0, &text)
                .expect_err("canonical text limit")
                .kind,
            ErrorKind::BadRequest
        );
        let error = RuleMutation::Edit(wire::RuleSetsEditRequest {
            key: "user.budget".into(),
            base_version: 0,
            text,
        })
        .execute(
            &mut fixture.registry,
            &mut fixture.store,
            UtcInstant::try_from(0).expect("UTC"),
        )
        .expect_err("edit checks same canonical limit");
        assert_eq!(error.kind, ErrorKind::BadRequest);
        assert!(
            !fixture
                .store
                .rule_sets()
                .load()
                .expect("unchanged database")
                .sets
                .contains_key(&"user.budget".parse().expect("key"))
        );
    }
    impl ValidationFixture {
        fn oversized_legacy_text() -> String {
            format!(
                "format='quartz-rules/1'\n[set]\nkey='user.oversized'\nkind='classification'\nname='{}'\n",
                "\"".repeat(wire::MAX_RULE_TEXT_BYTES / 2)
            )
        }
        fn oversized_legacy() -> Self {
            let mut fixture = ValidationFixture::new();
            let key = "user.oversized";
            let text = Self::oversized_legacy_text();
            fixture
                .registry
                .run(
                    &mut fixture.store,
                    UtcInstant::try_from(0).expect("UTC"),
                    RegistryCommand::Apply {
                        key: key.parse().expect("key"),
                        file: "legacy.toml".into(),
                        text,
                        base_version: 0,
                    },
                )
                .expect("legacy set");
            fixture.store.rewrite_rule_revisions_as_legacy();
            fixture.registry =
                RegistryShell::open(&mut fixture.store, UtcInstant::try_from(0).expect("UTC"))
                    .expect("legacy registry");
            fixture
        }
    }
    #[test]
    fn oversized_legacy_sets_can_be_disabled_but_not_edited() {
        let mut fixture = ValidationFixture::oversized_legacy();
        let key = "user.oversized";
        let text = ValidationFixture::oversized_legacy_text();
        let now = UtcInstant::try_from(0).expect("UTC");
        let error = RuleMutation::Edit(wire::RuleSetsEditRequest {
            key: key.into(),
            base_version: 1,
            text,
        })
        .execute(&mut fixture.registry, &mut fixture.store, now)
        .expect_err("edited canonical text exceeds budget");
        assert_eq!(error.kind, ErrorKind::BadRequest);
        let response = RuleMutation::SetEnabled(wire::RuleSetsSetEnabledRequest {
            key: key.into(),
            base_version: std::num::NonZeroU32::new(1).expect("version"),
            enabled: false,
        })
        .execute(&mut fixture.registry, &mut fixture.store, now)
        .expect("disable oversized legacy set");
        assert_eq!(response.sets[0].version, 2);
    }
    #[test]
    fn oversized_legacy_sets_can_be_deleted() {
        let mut fixture = ValidationFixture::oversized_legacy();
        let response = RuleMutation::Delete(wire::RuleSetsDeleteRequest {
            key: "user.oversized".into(),
            base_version: std::num::NonZeroU32::new(1).expect("version"),
        })
        .execute(
            &mut fixture.registry,
            &mut fixture.store,
            UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("delete oversized legacy set");
        assert!(response.sets[0].deleted);
        assert_eq!(response.sets[0].version, 2);
    }
    #[test]
    fn validation_of_uninstalled_builtin_keys_preserves_edit_error_order_for_every_text_size() {
        let mut fixture = ValidationFixture::new();
        for text in [
            String::from("invalid text"),
            "x".repeat(wire::MAX_RULE_TEXT_BYTES + 1),
        ] {
            let expected = RuleMutation::Edit(wire::RuleSetsEditRequest {
                key: "builtin.unknown".into(),
                base_version: 0,
                text: text.clone(),
            })
            .execute(
                &mut fixture.registry,
                &mut fixture.store,
                UtcInstant::try_from(0).expect("UTC"),
            )
            .expect_err("reserved edit");
            let actual = fixture
                .validate("builtin.unknown", 0, &text)
                .expect_err("reserved validation");
            assert_eq!(
                expected.kind,
                ErrorKind::RuleSetReadOnly {
                    key: "builtin.unknown".into()
                }
            );
            assert_eq!(actual.kind, expected.kind);
        }
    }
    impl ValidationFixture {
        fn draft_mutation(body: serde_json::Value) -> RuleMutation {
            let mutation: wire::RulePreviewMutation =
                serde_json::from_value(body).expect("typed draft mutation");
            mutation.into()
        }
        fn assert_unchanged(&mut self, before: &std::sync::Arc<domain::registry::RuleSnapshot>) {
            assert!(std::sync::Arc::ptr_eq(
                before,
                &self.registry.snapshots().load()
            ));
            let stored = self.store.rule_sets().load().expect("persisted state");
            assert_eq!(stored.sets.len(), before.state().sets.len());
            for (key, entry) in &before.state().sets {
                assert_eq!(stored.sets[key].versions, entry.versions);
                assert_eq!(stored.sets[key].deletion, entry.deletion);
            }
        }
    }
    #[test]
    fn draft_compiles_every_rule_family_without_persistence_or_publication() {
        let mut fixture = ValidationFixture::new();
        let before = fixture.registry.snapshots().load();
        for kind in ["classification", "extraction", "privacy"] {
            let body = match kind {
                "classification" => {
                    "category='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='draft.exe'}]\n"
                }
                "extraction" => {
                    "app=[{field='app.exe_name',op='equals',value='draft.exe'}]\npattern='(?P<document>.+)'\n"
                }
                "privacy" => {
                    "preset='title_hidden'\nwhen=[{field='app.exe_name',op='equals',value='draft.exe'}]\n"
                }
                _ => unreachable!("fixture families"),
            };
            let text = format!(
                "format='quartz-rules/1'\n[set]\nkey='user.draft'\nkind='{kind}'\nname='Draft'\n[[rule]]\nid='one'\nlabel='Draft'\n{body}"
            );
            let mutation = serde_json::json!({"op":"rule_sets.edit","body":{"key":"user.draft","base_version":0,"text":text}});
            let draft = ValidationFixture::draft_mutation(mutation)
                .draft(&fixture.registry)
                .expect("compiled draft");
            let mut fields = domain::FieldSnapshot::default();
            fields.text.insert(
                domain::Field::ExeName,
                quartz_domain::text::NfcText::new("draft.exe"),
            );
            fields.text.insert(
                domain::Field::RawTitle,
                quartz_domain::text::NfcText::new("draft document"),
            );
            match kind {
                "classification" => assert_eq!(
                    draft
                        .classification()
                        .classify(&fields)
                        .expect("draft classification")
                        .rule
                        .as_str(),
                    "user.draft/one@1"
                ),
                "extraction" => assert_eq!(
                    draft
                        .extraction()
                        .evaluate(&fields)
                        .extraction
                        .expect("draft extraction")
                        .document
                        .as_deref(),
                    Some("draft document")
                ),
                "privacy" => assert_eq!(
                    draft.privacy().evaluate(&fields).privacy.window_title,
                    domain::Action::Drop
                ),
                _ => unreachable!("fixture families"),
            }
            fixture.assert_unchanged(&before);
        }
    }
    #[test]
    fn every_rule_set_draft_matches_the_corresponding_committed_snapshot() {
        let text = "format='quartz-rules/1'\n[set]\nkey='user.draft'\nkind='classification'\nname='Draft'\n";
        for op in [
            "rule_sets.edit",
            "rule_sets.set_enabled",
            "rule_sets.delete",
        ] {
            let mut fixture = ValidationFixture::new();
            fixture.edit("user.draft", 0, text);
            let body = match op {
                "rule_sets.edit" => {
                    serde_json::json!({"key":"user.draft","base_version":1,"text":text.replace("name='Draft'", "name='Changed'")})
                }
                "rule_sets.set_enabled" => {
                    serde_json::json!({"key":"user.draft","base_version":1,"enabled":false})
                }
                "rule_sets.delete" => serde_json::json!({"key":"user.draft","base_version":1}),
                _ => unreachable!("fixture operations"),
            };
            let mutation = serde_json::json!({"op":op,"body":body});
            let before = fixture.registry.snapshots().load();
            let draft = ValidationFixture::draft_mutation(mutation.clone())
                .draft(&fixture.registry)
                .expect("draft");
            fixture.assert_unchanged(&before);
            ValidationFixture::draft_mutation(mutation)
                .execute(
                    &mut fixture.registry,
                    &mut fixture.store,
                    UtcInstant::try_from(0).expect("UTC"),
                )
                .expect("same write");
            let committed = fixture.registry.snapshots().load();
            assert_eq!(draft.hashes(), committed.hashes(), "{op}");
            for (key, entry) in &draft.state().sets {
                assert_eq!(
                    entry.current.set.canonical_text(),
                    committed.state().sets[key].current.set.canonical_text(),
                    "{op}"
                );
                assert_eq!(entry.deletion, committed.state().sets[key].deletion, "{op}");
            }
        }
    }
    #[test]
    fn draft_rejections_preserve_write_preconditions_and_error_details() {
        let mut fixture = ValidationFixture::new();
        let text = "format='quartz-rules/1'\n[set]\nkey='user.draft'\nkind='classification'\nname='Draft'\n";
        fixture.edit("user.draft", 0, text);
        let before = fixture.registry.snapshots().load();
        for body in [
            serde_json::json!({"op":"rule_sets.edit","body":{"key":"builtin.classification","base_version":0,"text":"invalid"}}),
            serde_json::json!({"op":"rule_sets.edit","body":{"key":"user.draft","base_version":0,"text":"invalid"}}),
            serde_json::json!({"op":"rule_sets.edit","body":{"key":"user.draft","base_version":1,"text":text.replace("classification", "privacy")}}),
            serde_json::json!({"op":"rule_sets.edit","body":{"key":"user.draft","base_version":1,"text":format!("{text}parent='builtin.classification'\n")}}),
            serde_json::json!({"op":"rule_sets.edit","body":{"key":"user.draft","base_version":1,"text":"invalid"}}),
            serde_json::json!({"op":"rule_sets.edit","body":{"key":"user.large","base_version":0,"text":"x".repeat(wire::MAX_RULE_TEXT_BYTES+1)}}),
        ] {
            let actual = ValidationFixture::draft_mutation(body.clone())
                .draft(&fixture.registry)
                .expect_err("draft rejection");
            let expected = ValidationFixture::draft_mutation(body)
                .execute(
                    &mut fixture.registry,
                    &mut fixture.store,
                    UtcInstant::try_from(0).expect("UTC"),
                )
                .expect_err("write rejection");
            assert_eq!(actual, expected);
            fixture.assert_unchanged(&before);
        }
    }
    #[test]
    fn privacy_import_draft_and_save_succeed_and_take_effect() {
        let mut fixture = ValidationFixture::new();
        let text = "format='quartz-rules/1'\n[set]\nkey='user.private'\nkind='privacy'\nname='Private'\n[[rule]]\nid='hide'\nlabel='Hide'\npreset='title_hidden'\nwhen=[{field='app.exe_name',op='equals',value='secret.exe'}]\n";
        let mutation: wire::RulePreviewMutation = serde_json::from_value(serde_json::json!({"op":"rules_text.import","body":{"key":"user.private","base_version":0,"text":text}})).expect("import preview");
        let draft = RuleMutation::from(mutation.clone())
            .draft(&fixture.registry)
            .expect("privacy draft");
        assert!(
            draft
                .state()
                .sets
                .contains_key(&"user.private".parse().expect("key"))
        );
        let saved = RuleMutation::from(mutation)
            .execute(
                &mut fixture.registry,
                &mut fixture.store,
                UtcInstant::try_from(0).expect("UTC"),
            )
            .expect("privacy save succeeds without receipt");
        assert_eq!(saved.sets[0].version, 1);
        let mut fields = domain::FieldSnapshot::default();
        fields.text.insert(
            domain::Field::ExeName,
            quartz_domain::text::NfcText::new("secret.exe"),
        );
        let snapshot = fixture.registry.snapshots().load();
        assert_eq!(
            snapshot.privacy().evaluate(&fields).privacy.window_title,
            domain::Action::Drop
        );
    }
    #[test]
    fn identity_draft_matches_query_day_totals_with_inactive_facts() {
        use crate::query::{QueryJob, QueryResponse};
        let mut fixture = crate::store::query::test_support::QueryFixture::new(4);
        fixture.mask_idle(1000, 2000);
        let request = fixture.request_day();
        let before = fixture.run(request.clone()).expect("production day");
        let registry =
            RegistryShell::open(&mut fixture.store, UtcInstant::try_from(0).expect("UTC"))
                .expect("registry");
        let mutation = serde_json::json!({"op":"rule_sets.edit","body":{"key":"user.identity","base_version":0,"text":"format='quartz-rules/1'\n[set]\nkey='user.identity'\nkind='classification'\nname='Identity'\n"}});
        let draft = ValidationFixture::draft_mutation(mutation)
            .draft(&registry)
            .expect("identity draft");
        let mut reader = fixture.store.take_query_reader();
        let completed = QueryJob::start(&mut reader, request, draft)
            .expect("draft day")
            .run_measured()
            .expect("draft query pipeline");
        fixture.store.return_query_reader(completed.reader);
        let QueryResponse::Day(before) = before else {
            panic!("production day response")
        };
        let QueryResponse::Day(after) = completed.response else {
            panic!("draft day response")
        };
        assert_eq!(before.metrics, after.metrics);
        assert_eq!(before.hours, after.hours);
        assert_eq!(before.applications, after.applications);
        // Document references bind the rule versions, so only the visible ranking must match.
        let visible = |documents: &[quartz_protocol::DocumentRank]| {
            documents
                .iter()
                .map(|rank| (rank.app, rank.document.clone(), rank.active_ms))
                .collect::<Vec<_>>()
        };
        assert!(!before.documents.is_empty());
        assert_eq!(visible(&before.documents), visible(&after.documents));
        assert_eq!(after.metrics.active_ms, 3000);
        assert!(
            !registry
                .snapshots()
                .load()
                .state()
                .sets
                .contains_key(&"user.identity".parse().expect("key"))
        );
    }
}
