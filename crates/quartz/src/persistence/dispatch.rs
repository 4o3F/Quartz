//! Bounded I/O and main-thread admission to the database writer.

use crossbeam_channel::{Receiver, Sender, bounded};
use quartz_protocol::{ErrorKind, ProtocolError};
use tokio::sync::oneshot;

/// Limits pending user writes independently of capture persistence and queries.
pub(crate) const WRITE_CHANNEL_CAPACITY: usize = 8;

/// Production operations are added when their persistence implementation exists.
#[derive(Debug)]
pub(crate) enum WriteRequest {
    #[cfg(test)]
    BumpMetadata { outcome: TestCommit },
}
#[cfg(test)]
pub(crate) use crate::store::versions::TestCommit;
#[derive(Debug)]
pub(crate) struct WriteCommand {
    pub request: WriteRequest,
    pub reply: oneshot::Sender<Result<(), ProtocolError>>,
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "annotation IPC operations use this admission path next"
        )
    )]
    pub(crate) async fn call(&self, request: WriteRequest) -> Result<(), ProtocolError> {
        self.submit(request)?
            .await
            .map_err(|_| ProtocolError::new(ErrorKind::Internal))?
    }
    /// Admission never blocks. The main thread must not wait for the reply (01 §6): it keeps the
    /// returned receiver and polls it with `try_recv` after a wakeup.
    pub(crate) fn submit(
        &self,
        request: WriteRequest,
    ) -> Result<oneshot::Receiver<Result<(), ProtocolError>>, ProtocolError> {
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
