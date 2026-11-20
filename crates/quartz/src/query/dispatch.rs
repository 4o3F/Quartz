//! Non-blocking I/O-to-database query admission with asynchronous replies.

use super::{QueryRequest, QueryResponse};
use crossbeam_channel::{Receiver, Sender, bounded};
use quartz_protocol::{ErrorKind, ProtocolError};
use tokio::sync::oneshot;

/// Limits queued queries independently of the capture write queue (01 §6).
pub(crate) const QUERY_CHANNEL_CAPACITY: usize = 8;
#[derive(Debug)]
pub(crate) struct QueryCommand {
    pub request: QueryRequest,
    pub reply: oneshot::Sender<Result<QueryResponse, ProtocolError>>,
}
#[derive(Debug, Clone)]
pub(crate) struct QueryPort {
    sender: Sender<QueryCommand>,
}
pub(crate) struct QueryChannel {
    pub port: QueryPort,
    pub receiver: Receiver<QueryCommand>,
}
impl QueryChannel {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = bounded(QUERY_CHANNEL_CAPACITY);
        Self {
            port: QueryPort { sender },
            receiver,
        }
    }
}
impl QueryPort {
    pub(crate) async fn call(&self, request: QueryRequest) -> Result<QueryResponse, ProtocolError> {
        self.submit(request)?
            .await
            .map_err(|_| ProtocolError::new(ErrorKind::Internal))?
    }
    pub(crate) fn submit(
        &self,
        request: QueryRequest,
    ) -> Result<oneshot::Receiver<Result<QueryResponse, ProtocolError>>, ProtocolError> {
        let (reply, received) = oneshot::channel();
        self.sender
            .try_send(QueryCommand { request, reply })
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
pub(crate) mod test_support;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_query_channel_reports_busy_without_waiting_for_database() {
        let channel = QueryChannel::new();
        let request = QueryRequest::Day(quartz_protocol::QueryDayRequest {
            local_date: "2025-01-02".parse().expect("date"),
            cursor: None,
        });
        channel.saturate(request.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert_eq!(
            runtime
                .block_on(channel.port.call(request))
                .expect_err("full channel")
                .kind,
            ErrorKind::Busy
        );
    }
}
