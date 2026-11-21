//! Bounded I/O and main-thread admission to the database writer.

use crate::store::{Store, annotations::ExecuteAnnotationWritesError};
use crossbeam_channel::{Receiver, Sender, bounded};
use quartz_domain::capture::UtcInstant;
use quartz_protocol::{ErrorKind, ProtocolError};
use tokio::sync::oneshot;

/// Wire body selected by the write operation.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum WriteResponse {
    Annotations(quartz_protocol::AnnotationsResponse),
    Accepted(quartz_protocol::AcceptedBody),
}
impl WriteRequest {
    pub(crate) fn decode(
        op: quartz_protocol::Op,
        body: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        match op {
            quartz_protocol::Op::AnnotationsCreate => serde_json::from_value(body)
                .map(AnnotationWrite::Create)
                .map(Self::Annotations),
            quartz_protocol::Op::AnnotationsUpdate => serde_json::from_value(body)
                .map(AnnotationWrite::Update)
                .map(Self::Annotations),
            quartz_protocol::Op::AnnotationsDelete => serde_json::from_value(body)
                .map(AnnotationWrite::Delete)
                .map(Self::Annotations),
            _ => unreachable!("write decoding follows annotation operation dispatch"),
        }
    }
    pub(crate) fn execute(
        self,
        store: &mut Store,
        now: UtcInstant,
    ) -> Result<WriteResponse, ProtocolError> {
        let result = match self {
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
                ExecuteAnnotationWritesError::Database { .. }
                | ExecuteAnnotationWritesError::Version { .. }
                | ExecuteAnnotationWritesError::Identity { .. } => ErrorKind::Internal,
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
    Annotations(AnnotationWrite),
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
#[derive(Debug)]
pub(crate) struct WriteCommand {
    pub request: WriteRequest,
    pub reply: oneshot::Sender<Result<WriteResponse, ProtocolError>>,
}
#[derive(Debug, Clone)]
pub(crate) struct WritePort {
    sender: Sender<WriteCommand>,
}
pub(crate) struct WriteChannel {
    pub port: WritePort,
    pub receiver: Receiver<WriteCommand>,
}
impl WriteChannel {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = bounded(WRITE_CHANNEL_CAPACITY);
        Self {
            port: WritePort { sender },
            receiver,
        }
    }
}
impl WritePort {
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
            .try_send(WriteCommand { request, reply })
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
