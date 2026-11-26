//! Bounded I/O and main-thread admission to the database writer.

use super::{categories::CategoryRequest, diagnostics::WriteFailure};
use crate::store::{Store, annotations::ExecuteAnnotationWritesError};
use crossbeam_channel::{Receiver, Sender, bounded};
use quartz_domain::capture::UtcInstant;
use quartz_protocol::{ErrorKind, ProtocolError};
use tokio::sync::oneshot;

/// Wire body selected by the write operation.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum WriteResponse {
    Rules(quartz_protocol::RuleSetsWriteResponse),
    RuleValidation(quartz_protocol::RulesTextValidateResponse),
    Annotations(quartz_protocol::AnnotationsResponse),
    Category(quartz_protocol::CategoryResponse),
    Categories(quartz_protocol::CategoriesListResponse),
    Settings(quartz_protocol::SettingsResponse),
    Accepted(quartz_protocol::AcceptedBody),
}
impl WriteRequest {
    pub(crate) fn decode(
        op: quartz_protocol::Op,
        body: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        use crate::rules::write::RuleMutation;
        let request = match op {
            quartz_protocol::Op::RulesTextValidate => {
                serde_json::from_value(body).map(Self::RuleValidation)
            }
            quartz_protocol::Op::RulesTextImport => serde_json::from_value(body)
                .map(RuleMutation::Import)
                .map(Self::Rules),
            quartz_protocol::Op::RuleSetsEdit => serde_json::from_value(body)
                .map(RuleMutation::Edit)
                .map(Self::Rules),
            quartz_protocol::Op::RuleSetsSetEnabled => serde_json::from_value(body)
                .map(RuleMutation::SetEnabled)
                .map(Self::Rules),
            quartz_protocol::Op::RuleSetsDelete => serde_json::from_value(body)
                .map(RuleMutation::Delete)
                .map(Self::Rules),
            quartz_protocol::Op::CategoriesList => serde_json::from_value(body)
                .map(CategoryRequest::List)
                .map(Self::Categories),
            quartz_protocol::Op::CategoriesCreate => serde_json::from_value(body)
                .map(CategoryRequest::Create)
                .map(Self::Categories),
            quartz_protocol::Op::CategoriesUpdate => serde_json::from_value(body)
                .map(CategoryRequest::Update)
                .map(Self::Categories),
            quartz_protocol::Op::CategoriesReorder => serde_json::from_value(body)
                .map(CategoryRequest::Reorder)
                .map(Self::Categories),
            quartz_protocol::Op::CategoriesArchive => serde_json::from_value(body)
                .map(CategoryRequest::Archive)
                .map(Self::Categories),
            quartz_protocol::Op::AnnotationsCreate => serde_json::from_value(body)
                .map(AnnotationWrite::Create)
                .map(Self::Annotations),
            quartz_protocol::Op::AnnotationsUpdate => serde_json::from_value(body)
                .map(AnnotationWrite::Update)
                .map(Self::Annotations),
            quartz_protocol::Op::AnnotationsDelete => serde_json::from_value(body)
                .map(AnnotationWrite::Delete)
                .map(Self::Annotations),
            quartz_protocol::Op::SettingsGet => {
                serde_json::from_value::<quartz_protocol::SettingsGetRequest>(body)
                    .map(|_| Self::SettingsGet)
            }
            quartz_protocol::Op::SettingsSet => serde_json::from_value(body).map(Self::SettingsSet),
            _ => unreachable!("write decoding follows implemented operation dispatch"),
        }?;
        Ok(request)
    }
    pub(crate) fn execute(
        self,
        store: &mut Store,
        now: UtcInstant,
        system: Option<quartz_protocol::EffectiveUiSettings>,
        registry: &mut crate::rules::registry::RegistryShell,
    ) -> Result<WriteResponse, ProtocolError> {
        let result = match self {
            Self::RuleValidation(request) => {
                return crate::rules::write::RuleMutation::validate(request, registry)
                    .map(WriteResponse::RuleValidation);
            }
            Self::Rules(request) => {
                return request
                    .execute(registry, store, now)
                    .map(WriteResponse::Rules);
            }
            Self::Categories(request) => return request.execute(store),
            Self::SettingsGet | Self::SettingsSet(_) => {
                let system = system.ok_or_else(|| ProtocolError::new(ErrorKind::Internal))?;
                let result = match self {
                    Self::SettingsSet(update) => store.update_ui_settings(update),
                    Self::SettingsGet => store.ui_settings(),
                    _ => unreachable!("settings dispatch"),
                };
                return result
                    .map(|ui| {
                        WriteResponse::Settings(quartz_protocol::SettingsResponse {
                            ui,
                            effective: ui.resolve(system),
                        })
                    })
                    .map_err(|error| WriteFailure::setting(&error).into_protocol_error());
            }
            Self::Annotations(AnnotationWrite::Create(request)) => store
                .create_annotations(request, now)
                .map(WriteResponse::Annotations),
            Self::Annotations(AnnotationWrite::Update(request)) => store
                .update_annotations(request, now)
                .map(WriteResponse::Annotations),
            Self::Annotations(AnnotationWrite::Delete(request)) => store
                .delete_annotations(request)
                .map(|()| WriteResponse::Accepted(quartz_protocol::AcceptedBody)),
            #[cfg(test)]
            Self::BumpMetadata { outcome } => {
                return store
                    .test_metadata_write(outcome)
                    .map(|()| WriteResponse::Accepted(quartz_protocol::AcceptedBody))
                    .map_err(|_| ProtocolError::new(ErrorKind::Internal));
            }
        };
        result.map_err(|error| {
            let kind = match error {
                ExecuteAnnotationWritesError::InvalidRange => ErrorKind::InvalidRange,
                ExecuteAnnotationWritesError::InvalidFields
                | ExecuteAnnotationWritesError::InvalidCategory
                | ExecuteAnnotationWritesError::MissingAnnotation
                | ExecuteAnnotationWritesError::ChangeKind
                | ExecuteAnnotationWritesError::DuplicateIdentity => ErrorKind::BadRequest,
                ExecuteAnnotationWritesError::Database { source } => {
                    return WriteFailure::database(&source).into_protocol_error();
                }
                ExecuteAnnotationWritesError::Version { source } => {
                    return WriteFailure::version(&source).into_protocol_error();
                }
                ExecuteAnnotationWritesError::InvalidStoredAnnotation => {
                    return WriteFailure::StoredAnnotation.into_protocol_error();
                }
                ExecuteAnnotationWritesError::Identity { .. } => {
                    return WriteFailure::Identity.into_protocol_error();
                }
            };
            ProtocolError::new(kind)
        })
    }
}

/// Limits pending user writes independently of capture persistence and queries.
pub(crate) const WRITE_CHANNEL_CAPACITY: usize = 8;

/// User writes executed serially by the database loop after capture commits.
#[derive(Debug)]
pub(crate) enum WriteRequest {
    Rules(crate::rules::write::RuleMutation),
    RuleValidation(quartz_protocol::RulesTextValidateRequest),
    SettingsGet,
    SettingsSet(quartz_protocol::SettingsSetRequest),
    Annotations(AnnotationWrite),
    Categories(CategoryRequest),
    #[cfg(test)]
    BumpMetadata {
        outcome: TestCommit,
    },
}
/// Annotation transaction selected by the three typed IPC operations.
#[derive(Debug)]
pub(crate) enum AnnotationWrite {
    Create(quartz_protocol::AnnotationsCreateRequest),
    Update(quartz_protocol::AnnotationsUpdateRequest),
    Delete(quartz_protocol::AnnotationsDeleteRequest),
}
#[cfg(test)]
pub(crate) use crate::store::versions::TestCommit;
#[derive(Clone)]
pub(crate) struct CompletionWake(std::sync::Arc<dyn Fn() + Send + Sync>);
impl std::fmt::Debug for CompletionWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletionWake").finish_non_exhaustive()
    }
}
impl CompletionWake {
    pub(crate) fn wake(&self) {
        (self.0)();
    }
}
#[derive(Debug)]
pub(crate) struct WriteCommand {
    pub completion: Option<CompletionWake>,
    pub request: WriteRequest,
    pub reply: oneshot::Sender<Result<WriteResponse, ProtocolError>>,
    pub settings: Option<SettingsPorts>,
}
#[derive(Clone)]
pub(crate) struct SettingsPorts {
    pub system: quartz_protocol::EffectiveUiSettings,
    pub notify: crate::ipc::UiPort,
}
impl std::fmt::Debug for SettingsPorts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsPorts")
            .field("system", &self.system)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone)]
pub(crate) struct WritePort {
    completion: Option<CompletionWake>,
    sender: Sender<WriteCommand>,
    settings: Option<SettingsPorts>,
}
pub(crate) struct WriteChannel {
    pub port: WritePort,
    pub receiver: Receiver<WriteCommand>,
}
impl WriteChannel {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = bounded(WRITE_CHANNEL_CAPACITY);
        Self {
            port: WritePort {
                completion: None,
                sender,
                settings: None,
            },
            receiver,
        }
    }
}
impl WritePort {
    pub(crate) fn with_completion_wake(
        mut self,
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.completion = Some(CompletionWake(wake));
        self
    }
    pub(crate) fn with_settings(
        mut self,
        system: quartz_protocol::EffectiveUiSettings,
        notify: crate::ipc::UiPort,
    ) -> Self {
        self.settings = Some(SettingsPorts { system, notify });
        self
    }
    #[cfg(test)]
    pub(crate) async fn call(&self, request: WriteRequest) -> Result<WriteResponse, ProtocolError> {
        self.submit(request)?
            .await
            .map_err(|_| ProtocolError::new(ErrorKind::Internal))?
    }
    /// Admission never blocks. The main thread must not wait for the reply (01 §6): it keeps the
    /// returned receiver and polls it with `try_recv` after a wakeup.
    pub(crate) fn submit(
        &self,
        request: WriteRequest,
    ) -> Result<oneshot::Receiver<Result<WriteResponse, ProtocolError>>, ProtocolError> {
        let (reply, received) = oneshot::channel();
        self.sender
            .try_send(WriteCommand {
                completion: self.completion.clone(),
                request,
                reply,
                settings: self.settings.clone(),
            })
            .map_err(|error| {
                ProtocolError::new(match error {
                    crossbeam_channel::TrySendError::Full(_) => ErrorKind::Busy,
                    crossbeam_channel::TrySendError::Disconnected(_) => ErrorKind::Internal,
                })
            })?;
        Ok(received)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rule_requests_execute_through_registry_without_actor_special_cases() {
        let directory = tempfile::tempdir().expect("isolated rules");
        let mut store = Store::open(&crate::data_directory::DataDirectory::new(directory.path()))
            .expect("store");
        store.seed_categories().expect("categories");
        let mut registry = crate::rules::registry::RegistryShell::open(
            &mut store,
            UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("registry");
        let request=WriteRequest::decode(quartz_protocol::Op::RuleSetsEdit,serde_json::json!({"key":"user.dispatch","base_version":0,"text":"format='quartz-rules/1'\n[set]\nkey='user.dispatch'\nkind='classification'\nname='Test'\n"})).expect("request");
        request
            .execute(
                &mut store,
                UtcInstant::try_from(0).expect("UTC"),
                None,
                &mut registry,
            )
            .expect("registry dispatch");
    }
    #[test]
    fn full_write_channel_reports_busy_without_waiting() {
        let channel = WriteChannel::new();
        let mut replies = Vec::new();
        for _ in 0..WRITE_CHANNEL_CAPACITY {
            replies.push(
                channel
                    .port
                    .submit(WriteRequest::BumpMetadata {
                        outcome: TestCommit::Commit,
                    })
                    .expect("admitted"),
            );
        }
        assert_eq!(
            channel
                .port
                .submit(WriteRequest::BumpMetadata {
                    outcome: TestCommit::Commit
                })
                .expect_err("full channel")
                .kind,
            ErrorKind::Busy
        );
        assert_eq!(channel.receiver.len(), WRITE_CHANNEL_CAPACITY);
    }
    #[test]
    fn disconnected_writer_reports_internal() {
        let channel = WriteChannel::new();
        drop(channel.receiver);
        assert_eq!(
            channel
                .port
                .submit(WriteRequest::BumpMetadata {
                    outcome: TestCommit::Commit
                })
                .expect_err("closed writer")
                .kind,
            ErrorKind::Internal
        );
    }
}
