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
    fn detail_queries_round_trip_through_database_dispatch() {
        let fixture = crate::store::query::test_support::QueryFixture::new(4);
        let start = fixture.start;
        let database = test_support::TestDatabase::with_fixture(fixture);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let response = database.port.call(QueryRequest::Unclassified(quartz_protocol::QueryUnclassifiedRequest {
                start_date: crate::store::query::test_support::FIXTURE_DATE.parse().expect("date"),
                end_date: "2025-01-03".parse().expect("date"),
                group_by: quartz_protocol::UnclassifiedGroupBy::App,
                search: None, cursor: None,
            })).await.expect("unclassified dispatcher response");
            assert!(matches!(response, QueryResponse::Unclassified(page) if page.items.len() == 4));
            let response = database.port.call(QueryRequest::SegmentDetail(quartz_protocol::QuerySegmentDetailRequest {
                start_utc_ms: start, end_utc_ms: start + 4000, app: None, window_instance: None, cursor: None,
            })).await.expect("segment dispatcher response");
            assert!(matches!(response, QueryResponse::SegmentDetail(page) if page.intervals.len() == 4));
            let response = database.port.call(QueryRequest::EntityDetail(quartz_protocol::QueryEntityDetailRequest {
                reference_range: None,
                entity: quartz_protocol::DetailEntity::App { id: quartz_protocol::ApplicationId(1) },
                local_date: crate::store::query::test_support::FIXTURE_DATE.parse().expect("date"), cursor: None,
            })).await.expect("entity dispatcher response");
            assert!(matches!(response, QueryResponse::EntityDetail(page) if page.daily.as_ref().is_some_and(|daily| daily.len() == quartz_protocol::ENTITY_DETAIL_DAYS)));
        });
    }
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
