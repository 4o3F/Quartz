//! Resumable query ownership and reply delivery on the database thread.
use super::{QueryJob, QueryResponse, RunQueryError, dispatch::QueryCommand};
use crate::store::Store;
use quartz_protocol::{ErrorKind, ProtocolError};

#[derive(Default)]
pub(crate) struct QueryRunner {
    running: Option<RunningQuery>,
}
struct RunningQuery {
    job: RunningJob,
    control: super::control::QueryControl,
    reply: tokio::sync::oneshot::Sender<Result<QueryResponse, ProtocolError>>,
}
struct StartedQuery {
    result: Result<RunningJob, RunQueryError>,
    reply: tokio::sync::oneshot::Sender<Result<QueryResponse, ProtocolError>>,
    control: super::control::QueryControl,
}
enum RunningJob {
    Explain(Box<super::explain::ExplainJob>),
    Query(Box<QueryJob>),
    Preview(Box<super::preview::PreviewJob>),
}
impl RunningJob {
    fn take_receipt_grant(&mut self) -> Option<crate::rules::receipts::ReceiptGrant> {
        match self {
            Self::Query(_) | Self::Explain(_) => None,
            Self::Preview(job) => job.take_receipt_grant(),
        }
    }

    fn advance(&mut self) -> Result<Option<QueryResponse>, RunQueryError> {
        match self {
            Self::Explain(job) => job.advance(),
            Self::Query(job) => job.advance(),
            Self::Preview(job) => job.advance(),
        }
    }
    fn finish(self) -> Result<crate::store::ReadOnlyStore, RunQueryError> {
        match self {
            Self::Explain(job) => job.finish(),
            Self::Query(job) => job.finish(),
            Self::Preview(job) => job.finish(),
        }
    }
}
impl QueryRunner {
    pub(crate) fn is_running(&self) -> bool {
        self.running.is_some()
    }
    pub(crate) fn start(
        &mut self,
        store: &mut Store,
        command: QueryCommand,
        registry: &crate::rules::registry::RegistryShell,
    ) -> Result<(), RunQueryError> {
        if command.reply_is_closed() {
            return Ok(());
        }
        let rules = registry.snapshots().load();
        let mut reader = store.take_query_reader();
        let StartedQuery {
            result,
            reply,
            control,
        } = match command {
            QueryCommand::RuleInputs { reply } => {
                if let Some(reader) = reader {
                    store.return_query_reader(reader);
                }
                let result = store
                    .rule_read_metadata()
                    .map(|metadata_version| crate::rules::read::ReadInputs {
                        snapshot: rules,
                        metadata_version,
                    })
                    .map_err(|_| ProtocolError::new(ErrorKind::Internal));
                let _ = reply.send(result);
                return Ok(());
            }
            QueryCommand::Query { request, reply } => {
                let result = QueryJob::start(&mut reader, request, rules)
                    .map(|job| RunningJob::Query(Box::new(job)));
                StartedQuery {
                    result,
                    reply,
                    control: Default::default(),
                }
            }
            QueryCommand::Controlled {
                request,
                reply,
                control,
            } => {
                let result = control
                    .check()
                    .and_then(|()| QueryJob::start(&mut reader, request, rules))
                    .map(|job| RunningJob::Query(Box::new(job)));
                StartedQuery {
                    result,
                    reply,
                    control,
                }
            }
            QueryCommand::Explain { request, reply } => {
                let control = request.control.clone();
                let result = super::explain::ExplainJob::start(&mut reader, request, rules)
                    .map(|job| RunningJob::Explain(Box::new(job)));
                StartedQuery {
                    result,
                    reply,
                    control,
                }
            }
            QueryCommand::Preview { request, reply } => {
                let control = request.control.clone();
                let result = super::preview::PreviewJob::start(&mut reader, request, registry)
                    .map(|job| RunningJob::Preview(Box::new(job)));
                StartedQuery {
                    result,
                    reply,
                    control,
                }
            }
        };
        match result {
            Ok(job) => {
                self.running = Some(RunningQuery {
                    job,
                    reply,
                    control,
                })
            }
            Err(error) => {
                if let Some(reader) = reader {
                    store.return_query_reader(reader);
                }
                let _ = reply.send(Err(ProtocolError::new(error.kind())));
                match error {
                    RunQueryError::Cleanup { .. } => return Err(error.recover_reader(store)),
                    RunQueryError::ReaderUnavailable => return Err(error),
                    _ => {}
                }
            }
        }
        Ok(())
    }
    pub(crate) fn step(&mut self, store: &mut Store) -> Result<(), RunQueryError> {
        let Some(mut active) = self.running.take() else {
            return Ok(());
        };
        let result = if active.reply.is_closed() {
            Some(Err(ProtocolError::new(ErrorKind::Internal)))
        } else {
            match active.control.check().and_then(|()| active.job.advance()) {
                Ok(Some(response)) => Some(
                    active
                        .control
                        .check()
                        .map(|()| response)
                        .map_err(|error| ProtocolError::new(error.kind())),
                ),
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(%error, "could not compute query");
                    Some(Err(ProtocolError::new(error.kind())))
                }
            }
        };
        if let Some(result) = result {
            let grant = active.job.take_receipt_grant();
            let successful = result.is_ok();
            match active.job.finish() {
                Ok(reader) => store.return_query_reader(reader),
                Err(source) => {
                    let _ = active
                        .reply
                        .send(Err(ProtocolError::new(ErrorKind::Internal)));
                    return Err(source.recover_reader(store));
                }
            }
            if successful && let Some(grant) = grant {
                active.control.hold_receipt(grant);
            }
            if active.reply.send(result).is_err() {
                drop(active.control.take_receipt());
            }
        } else {
            self.running = Some(active);
        }
        Ok(())
    }
}

impl RunQueryError {
    fn recover_reader(self, store: &mut Store) -> Self {
        match store.reopen_query_reader() {
            Ok(()) => self,
            Err(source) => Self::RecoverReader {
                source,
                cleanup: Box::new(self),
            },
        }
    }
}
