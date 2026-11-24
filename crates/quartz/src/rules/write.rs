//! Typed rule mutations executed by the database actor's registry writer.

use super::{
    read::ReadInputs,
    registry::{RegistryShell, RunRegistryCommandError},
};
use crate::{persistence::diagnostics::WriteFailure, query::paging::JsonBytes, store::Store};
use quartz_domain::{
    capture::UtcInstant,
    rules::{
        self as domain,
        registry::{
            BuildRuleSnapshotError, DecideRegistryCommandError, RegistryCommand, RuleSetDeletion,
            SiblingRevision,
        },
    },
};
use quartz_protocol::{self as wire, ErrorKind, ProtocolError};

#[derive(Debug)]
pub(crate) enum RuleMutation {
    Edit(wire::RuleSetsEditRequest),
    SetEnabled(wire::RuleSetsSetEnabledRequest),
    Reorder(wire::RuleSetsReorderRequest),
    Delete(wire::RuleSetsDeleteRequest),
    Revert(wire::RuleSetsRevertRequest),
}
struct PreparedMutation {
    command: RegistryCommand,
    targets: MutationTargets,
}
struct MutationTargets {
    keys: Vec<domain::RuleSetKey>,
    historical_version: Option<u32>,
}
impl RuleMutation {
    pub(crate) fn execute(
        self,
        registry: &mut RegistryShell,
        store: &mut Store,
        now: UtcInstant,
    ) -> Result<wire::RuleSetsWriteResponse, ProtocolError> {
        let prepared = self.prepare()?;
        prepared.targets.check_response_budget()?;
        // Read fallible response dependencies before committing.
        let metadata_version = store
            .rule_read_metadata()
            .map_err(|error| WriteFailure::setting(&error).into_protocol_error())?;
        let result = registry
            .run(store, now, prepared.command)
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
    fn prepare(self) -> Result<PreparedMutation, ProtocolError> {
        let mut keys = Vec::new();
        let mut historical_version = None;
        let command = match self {
            Self::Edit(request) => {
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
            Self::Reorder(request) => {
                let mut siblings = Vec::with_capacity(request.siblings.len());
                for sibling in request.siblings {
                    let key = Self::key(sibling.key)?;
                    keys.push(key.clone());
                    siblings.push(SiblingRevision {
                        key,
                        base_version: sibling.base_version.get(),
                    });
                }
                RegistryCommand::Reorder { siblings }
            }
            Self::Delete(request) => {
                let key = Self::key(request.key)?;
                keys.push(key.clone());
                RegistryCommand::Delete {
                    key,
                    base_version: request.base_version.get(),
                }
            }
            Self::Revert(request) => {
                let key = Self::key(request.key)?;
                keys.push(key.clone());
                historical_version = Some(request.version.get());
                RegistryCommand::Revert {
                    key,
                    base_version: request.base_version.get(),
                    version: request.version.get(),
                }
            }
        };
        keys.sort();
        if keys.is_empty() {
            return Err(ProtocolError::new(ErrorKind::BadRequest));
        }
        if let Some(key) = keys.iter().find(|key| key.as_str().starts_with("builtin.")) {
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
            targets: MutationTargets {
                keys,
                historical_version,
            },
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
            RunRegistryCommandError::PreviewRequired => ErrorKind::PreviewRequired,
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
                | DecideRegistryCommandError::MissingRevision => ErrorKind::RuleSetNotFound {
                    key,
                    version: self.historical_version,
                },
                DecideRegistryCommandError::StalePreview => ErrorKind::PreviewRequired,
                DecideRegistryCommandError::RevisionOverflow
                | DecideRegistryCommandError::EpochOverflow
                | DecideRegistryCommandError::SiblingOrderOverflow { .. } => {
                    ErrorKind::VersionExhausted
                }
                DecideRegistryCommandError::Parse { source } => {
                    return Self::diagnostics(source.diagnostics());
                }
                DecideRegistryCommandError::Snapshot {
                    source: BuildRuleSnapshotError::Tree { diagnostics },
                } => return Self::diagnostics(&diagnostics),
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
                | DecideRegistryCommandError::InvalidOrdering
                | DecideRegistryCommandError::RevisionConflict
                | DecideRegistryCommandError::InvalidBuiltin => ErrorKind::BadRequest,
            },
        };
        ProtocolError::new(kind)
    }
    fn diagnostics(diagnostics: &[domain::Diagnostic]) -> ProtocolError {
        let empty = ProtocolError::new(ErrorKind::RuleValidationFailed {
            diagnostics: Vec::new(),
            truncated: false,
        });
        let mut remaining = wire::QUERY_PAGE_BYTES
            - JsonBytes::measure(&empty).expect("validation envelope is JSON-safe");
        let mut items = Vec::new();
        for diagnostic in diagnostics.iter().take(wire::MAX_RULE_DIAGNOSTICS) {
            let item = wire::RuleDiagnostic {
                code: diagnostic.code.as_str().into(),
                path: diagnostic.path.clone(),
                location: diagnostic.location.has_source_span().then_some(
                    wire::RuleDiagnosticLocation {
                        line: diagnostic.location.line,
                        column: diagnostic.location.column,
                    },
                ),
                params: Default::default(),
            };
            let separator_bytes = usize::from(!items.is_empty());
            let item_bytes =
                JsonBytes::measure(&item).expect("rule diagnostic is JSON-safe") + separator_bytes;
            if item_bytes > remaining {
                break;
            }
            remaining -= item_bytes;
            items.push(item);
        }
        let truncated = items.len() < diagnostics.len();
        ProtocolError::new(ErrorKind::RuleValidationFailed {
            diagnostics: items,
            truncated,
        })
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
        let error = MutationTargets::diagnostics(&[diagnostic]);
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
        let error = MutationTargets::diagnostics(&[diagnostic.clone(), diagnostic]);
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
        use domain::registry::{CommitRuleDecisionError, ComputeUpstreamChangesError};
        let targets = MutationTargets {
            keys: vec!["user.private-marker".parse().expect("key")],
            historical_version: None,
        };
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
                    source: BuildRuleSnapshotError::Upstream {
                        source: ComputeUpstreamChangesError::MissingAncestor,
                    },
                },
            },
            RunRegistryCommandError::Decide {
                source: DecideRegistryCommandError::Snapshot {
                    source: BuildRuleSnapshotError::Upstream {
                        source: ComputeUpstreamChangesError::MissingAncestor,
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
        let request=wire::RuleSetsEditRequest {key:"user.test".into(),base_version:0,text:"format='quartz-rules/1'\n[set]\nkey='user.test'\nkind='classification'\nname='Test'\n".into(),preview_receipt:None};
        RuleMutation::Edit(request)
            .execute(&mut registry, &mut store, now)
            .expect("rule edit ignores unrelated settings");
    }
}
