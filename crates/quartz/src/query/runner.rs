//! Resumable query ownership and reply delivery on the database thread.
use super::{QueryJob, QueryResponse, RunQueryError, dispatch::QueryCommand};
use crate::store::Store;
use quartz_protocol::{ErrorKind, ProtocolError};

#[derive(Default)]
pub(crate) struct QueryRunner {
    running: Option<RunningQuery>,
}
struct RunningQuery {
    job: QueryJob,
    reply: tokio::sync::oneshot::Sender<Result<QueryResponse, ProtocolError>>,
}
impl QueryRunner {
    pub(crate) fn is_running(&self) -> bool {
        self.running.is_some()
    }
    pub(crate) fn start(
        &mut self,
        store: &mut Store,
        command: QueryCommand,
        rules: std::sync::Arc<quartz_domain::rules::registry::RuleSnapshot>,
    ) -> Result<(), RunQueryError> {
        let QueryCommand::Query { request, reply } = command else {
            if let QueryCommand::RuleInputs { reply } = command {
                let result = store
                    .rule_read_metadata()
                    .map(|metadata_version| crate::rules::read::ReadInputs {
                        snapshot: rules,
                        metadata_version,
                    })
                    .map_err(|_| ProtocolError::new(ErrorKind::Internal));
                let _ = reply.send(result);
            }
            return Ok(());
        };
        if reply.is_closed() {
            return Ok(());
        }
        let mut reader = store.take_query_reader();
        match QueryJob::start(&mut reader, request, rules) {
            Ok(job) => self.running = Some(RunningQuery { job, reply }),
            Err(error) => {
                if let Some(reader) = reader {
                    store.return_query_reader(reader);
                }
                tracing::warn!(%error, "could not start query");
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
            match active.job.advance() {
                Ok(Some(response)) => Some(Ok(response)),
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(%error, "could not compute query");
                    Some(Err(ProtocolError::new(error.kind())))
                }
            }
        };
        if let Some(result) = result {
            match active.job.finish() {
                Ok(reader) => store.return_query_reader(reader),
                Err(source) => {
                    let _ = active
                        .reply
                        .send(Err(ProtocolError::new(ErrorKind::Internal)));
                    return Err(source.recover_reader(store));
                }
            }
            let _ = active.reply.send(result);
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
