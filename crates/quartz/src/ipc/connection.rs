//! Framed protocol sessions over arbitrary asynchronous transport halves.
use super::{
    ConnectionSession, ProcessId, Services, outbox::OutboxTask, registry::RegistryHandle,
    session::Action,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use quartz_protocol::{Envelope, ErrorKind, ProtocolError, decode_json, frame_codec};
use std::{future::Future, io, pin::Pin, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::oneshot;
use tokio_util::{
    bytes::BytesMut,
    codec::{FramedRead, LengthDelimitedCodec, LengthDelimitedCodecError},
};

/// Bound the final error response and close grace period (04 §2 error-and-close).
const CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_millis(500);
/// Matches the client pending-request bound in architecture 01 §7.2.
const MAX_PENDING_REQUESTS: usize = 64;
/// Require a complete first hello frame within five seconds (04 §4).
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConnectionTimeouts {
    pub(crate) hello: Duration,
    pub(crate) close_flush: Duration,
}
impl Default for ConnectionTimeouts {
    fn default() -> Self {
        Self {
            hello: HELLO_TIMEOUT,
            close_flush: CLOSE_FLUSH_TIMEOUT,
        }
    }
}

type PendingReplies<'connection> =
    FuturesUnordered<Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'connection>>>;
type PendingTracker =
    std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<quartz_protocol::RequestId>>>;
struct PendingRegistration {
    id: quartz_protocol::RequestId,
    tracker: PendingTracker,
}
impl Drop for PendingRegistration {
    fn drop(&mut self) {
        self.tracker
            .lock()
            .expect("pending request lock")
            .remove(&self.id);
    }
}
enum PendingReply {
    Write(oneshot::Receiver<Result<crate::persistence::dispatch::WriteResponse, ProtocolError>>),
    Query(oneshot::Receiver<Result<crate::query::QueryResponse, ProtocolError>>),
    Rules(crate::query::dispatch::RuleReadReply),
    Subscribe(oneshot::Receiver<Result<(), super::outbox::QueueFrameError>>),
    Unsubscribe(oneshot::Receiver<()>),
}
impl PendingReply {
    async fn receive(
        self,
        id: quartz_protocol::RequestId,
    ) -> io::Result<Option<quartz_protocol::Response>> {
        let response = match self {
            Self::Write(received) => Some(
                match received
                    .await
                    .map_err(|_| ProtocolError::new(ErrorKind::Internal))
                    .and_then(|result| result)
                {
                    Ok(body) => Action::success_response(id, body),
                    Err(error) => Action::error_response(Some(id), error),
                },
            ),
            Self::Query(received) => Some(
                match received
                    .await
                    .map_err(|_| ProtocolError::new(ErrorKind::Internal))
                    .and_then(|result| result)
                {
                    Ok(body) => Action::success_response(id, body),
                    Err(error) => Action::error_response(Some(id), error),
                },
            ),
            Self::Rules(received) => Some(match received.receive().await {
                Ok(body) => Action::success_response(id, body),
                Err(error) => Action::error_response(Some(id), error),
            }),
            Self::Subscribe(received) => {
                received
                    .await
                    .map_err(io::Error::other)?
                    .map_err(io::Error::other)?;
                None
            }
            Self::Unsubscribe(received) => {
                received.await.map_err(io::Error::other)?;
                Some(Action::success_response(
                    id,
                    quartz_protocol::SubscribeResponse {},
                ))
            }
        };
        Ok(response)
    }
}

#[derive(Default)]
struct ProgressClockConfig {
    #[cfg(test)]
    clock: Option<std::sync::Arc<dyn Fn() -> Duration + Send + Sync>>,
}

pub(crate) struct Connection {
    pub services: Services,
    pub registry: RegistryHandle,
    pub timeouts: ConnectionTimeouts,
    pub peer_pid: ProcessId,
}
impl Connection {
    pub(crate) async fn run<R, W>(
        &self,
        read: R,
        write: W,
        session: &mut ConnectionSession,
        hello_deadline: tokio::time::Instant,
    ) -> io::Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        self.run_with_config(
            read,
            write,
            session,
            hello_deadline,
            &ProgressClockConfig::default(),
        )
        .await
    }
    #[cfg(test)]
    async fn run_with_progress_clock<R, W>(
        &self,
        read: R,
        write: W,
        session: &mut ConnectionSession,
        hello_deadline: tokio::time::Instant,
        clock: std::sync::Arc<dyn Fn() -> Duration + Send + Sync>,
    ) -> io::Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        self.run_with_config(
            read,
            write,
            session,
            hello_deadline,
            &ProgressClockConfig { clock: Some(clock) },
        )
        .await
    }
    async fn run_with_config<R, W>(
        &self,
        read: R,
        write: W,
        session: &mut ConnectionSession,
        hello_deadline: tokio::time::Instant,
        progress_clock: &ProgressClockConfig,
    ) -> io::Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let mut writer = OutboxTask::spawn(write);
        let result = self
            .run_session(read, &mut writer, session, hello_deadline, progress_clock)
            .await;
        result.and(writer.stop().await)
    }
    async fn run_session<R: AsyncRead + Unpin>(
        &self,
        read: R,
        writer: &mut OutboxTask,
        session: &mut ConnectionSession,
        hello_deadline: tokio::time::Instant,
        progress_clock: &ProgressClockConfig,
    ) -> io::Result<()> {
        let mut reader = FramedRead::new(read, frame_codec());
        let outbox = writer.handle.clone();
        let mut registration = None;
        let mut requests = PendingReplies::new();
        let tracker = PendingTracker::default();
        loop {
            let received = tokio::select! {
                () = outbox.closed() => return Ok(()),
                completed = requests.next(), if !requests.is_empty() => {
                    completed.expect("pending request exists")?;
                    continue;
                }
                received = self.next_frame(&mut reader, session, hello_deadline) => received,
            };
            let action = match received {
                None => break,
                Some(Ok(payload)) => match decode_json::<Envelope>(&payload) {
                    Ok(envelope) => {
                        session.receive(envelope, &self.services.status.borrow().status)
                    }
                    // Parse diagnostics can contain request body values; only the stable code is sent.
                    Err(_) => session.fail(ErrorKind::BadRequest),
                },
                Some(Err(error))
                    if error
                        .get_ref()
                        .and_then(|source| source.downcast_ref::<LengthDelimitedCodecError>())
                        .is_some() =>
                {
                    session.fail(ErrorKind::FrameTooLarge)
                }
                Some(Err(error))
                    if error.kind() == io::ErrorKind::Other && !reader.read_buffer().is_empty() =>
                {
                    // tokio-util 0.7.20 codec/decoder.rs::decode_eof uses Other for a buffered EOF tail.
                    tracing::debug!(%error, "dropped half frame at pipe EOF");
                    break;
                }
                Some(Err(error)) => return Err(error),
            };
            if registration.is_none()
                && let Some(peer) = session.peer(self.peer_pid)
            {
                registration = Some(
                    self.registry
                        .register(peer, outbox.clone())
                        .map_err(io::Error::other)?,
                );
            }
            let response = match action {
                Action::Reply(response) => Some(response),
                Action::FirstFrame { id } => {
                    let connection = registration.as_ref().expect("first frame follows hello").id;
                    Some(
                        match (self.services.ui)(super::UiCommand::FirstFrame { connection }) {
                            Ok(()) => Action::success_response(id, quartz_protocol::AcceptedBody),
                            Err(error) => {
                                tracing::warn!(%error, "could not queue UI first frame");
                                Action::error_response(
                                    Some(id),
                                    ProtocolError::new(ErrorKind::Internal),
                                )
                            }
                        },
                    )
                }

                Action::Hidden { id } => {
                    let connection = registration
                        .as_ref()
                        .expect("hidden notification follows hello")
                        .id;
                    Some(
                        match (self.services.ui)(super::UiCommand::Hidden { connection }) {
                            Ok(()) => Action::success_response(id, quartz_protocol::AcceptedBody),
                            Err(error) => {
                                tracing::warn!(%error, "could not queue UI hidden notification");
                                Action::error_response(
                                    Some(id),
                                    ProtocolError::new(ErrorKind::Internal),
                                )
                            }
                        },
                    )
                }

                Action::Timed { id, command } => Some(match self.services.timed.call(command) {
                    Ok(response) => Action::success_response(id, response),
                    Err(error) => Action::error_response(Some(id), error),
                }),
                Action::Ui { id, command } => Some(match (self.services.ui)(command) {
                    Ok(()) => Action::success_response(id, quartz_protocol::AcceptedBody),
                    Err(error) => {
                        tracing::warn!(%error, "could not queue IPC UI command");
                        Action::error_response(Some(id), ProtocolError::new(ErrorKind::Internal))
                    }
                }),
                Action::Preview { id, request } => {
                    let progress = Self::progress_sink(session, id, &outbox, progress_clock);
                    self.admit_request(id, &outbox, &mut requests, &tracker, || {
                        self.services
                            .queries
                            .submit_preview(request, progress)
                            .map(PendingReply::Query)
                    })
                }
                Action::Write { id, request } => {
                    self.admit_request(id, &outbox, &mut requests, &tracker, || {
                        self.services
                            .writes
                            .submit(request)
                            .map(PendingReply::Write)
                    })
                }
                Action::Query { id, request } => {
                    let progress = Self::progress_sink(session, id, &outbox, progress_clock);
                    self.admit_request(id, &outbox, &mut requests, &tracker, || {
                        self.services
                            .queries
                            .submit_with_progress(request, progress)
                            .map(PendingReply::Query)
                    })
                }
                Action::Rules { id, request } => {
                    self.admit_request(id, &outbox, &mut requests, &tracker, || {
                        self.services
                            .queries
                            .submit_rule_read(request)
                            .map(PendingReply::Rules)
                    })
                }
                Action::Subscribe { id, subscription } => {
                    let connection = registration.as_ref().expect("subscribe follows hello").id;
                    self.admit_request(id, &outbox, &mut requests, &tracker, || {
                        self.registry
                            .submit_subscribe(
                                connection,
                                subscription,
                                Action::success_response(id, quartz_protocol::SubscribeResponse {}),
                            )
                            .map(PendingReply::Subscribe)
                            .map_err(|_| ProtocolError::new(ErrorKind::Internal))
                    })
                }
                Action::Unsubscribe { id, subscription } => {
                    let connection = registration.as_ref().expect("unsubscribe follows hello").id;
                    self.admit_request(id, &outbox, &mut requests, &tracker, || {
                        self.registry
                            .submit_unsubscribe(connection, subscription)
                            .map(PendingReply::Unsubscribe)
                            .map_err(|_| ProtocolError::new(ErrorKind::Internal))
                    })
                }
            };
            if let Some(response) = response {
                outbox.response(response).map_err(io::Error::other)?;
            }
            if session.closing() {
                return self.finish_closing(writer).await;
            }
        }
        Ok(())
    }
    fn progress_sink(
        session: &ConnectionSession,
        id: quartz_protocol::RequestId,
        outbox: &super::outbox::OutboxHandle,
        progress_clock: &ProgressClockConfig,
    ) -> crate::query::progress::ProgressSink {
        if session.role() != Some(quartz_protocol::Role::Ui) {
            return crate::query::progress::ProgressSink::disabled();
        }
        #[cfg(test)]
        if let Some(clock) = &progress_clock.clock {
            return crate::query::progress::ProgressSink::for_outbox_with_clock(
                id,
                outbox.clone(),
                std::sync::Arc::clone(clock),
                crate::query::progress::PROGRESS_FIRST_DELAY,
                crate::query::progress::PROGRESS_INTERVAL,
            );
        }
        #[cfg(not(test))]
        let _ = progress_clock;
        crate::query::progress::ProgressSink::for_outbox(id, outbox.clone())
    }
    fn admit_request<'connection>(
        &'connection self,
        id: quartz_protocol::RequestId,
        outbox: &super::outbox::OutboxHandle,
        requests: &mut PendingReplies<'connection>,
        tracker: &PendingTracker,
        admit: impl FnOnce() -> Result<PendingReply, ProtocolError>,
    ) -> Option<quartz_protocol::Response> {
        if tracker.lock().expect("pending request lock").contains(&id) {
            return Some(Action::error_response(
                Some(id),
                ProtocolError::new(ErrorKind::BadRequest),
            ));
        }
        if requests.len() >= MAX_PENDING_REQUESTS {
            return Some(Action::error_response(
                Some(id),
                ProtocolError::new(ErrorKind::Busy),
            ));
        }
        // Admission occurs before reading the next frame, independently of future polling order.
        match admit() {
            Err(error) => Some(Action::error_response(Some(id), error)),
            Ok(reply) => {
                tracker.lock().expect("pending request lock").insert(id);
                let registration = PendingRegistration {
                    id,
                    tracker: tracker.clone(),
                };
                let outbox = outbox.clone();
                requests.push(Box::pin(async move {
                    let response = reply.receive(id).await?;
                    drop(registration);
                    if let Some(response) = response {
                        outbox.response(response).map_err(io::Error::other)?;
                    }
                    Ok(())
                }));
                None
            }
        }
    }
    async fn next_frame<R: AsyncRead + Unpin>(
        &self,
        reader: &mut FramedRead<R, LengthDelimitedCodec>,
        session: &ConnectionSession,
        hello_deadline: tokio::time::Instant,
    ) -> Option<io::Result<BytesMut>> {
        if session.hello_pending() {
            match tokio::time::timeout_at(hello_deadline, reader.next()).await {
                Ok(received) => received,
                Err(error) => {
                    tracing::debug!(%error, "pipe hello deadline expired; closing connection");
                    None
                }
            }
        } else {
            reader.next().await
        }
    }
    async fn finish_closing(&self, writer: &mut OutboxTask) -> io::Result<()> {
        let outcome = tokio::time::timeout(self.timeouts.close_flush, writer.finish()).await;
        match outcome {
            Ok(result) => result,
            Err(error) => {
                tracing::debug!(%error, "pipe final response deadline expired; closing connection");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        ipc::{AllowedRoles, StatusPublisher},
        query::dispatch::test_support::TestDatabase,
    };
    use futures_util::SinkExt;
    use quartz_protocol::{HelloRole, PROTOCOL_VERSION, RequestId, ResponseOutcome};
    use std::sync::Arc;
    use tokio::io::split;
    use tokio_util::codec::Framed;

    /// Keeps in-memory connection tests independent of production close deadlines.
    const TEST_CLOSE_TIMEOUT: Duration = Duration::from_millis(20);
    /// Holds a small framed request and response without relying on OS pipe buffers.
    const TEST_BUFFER_BYTES: usize = 4096;
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
    }
    fn connection(database: &TestDatabase, status: &StatusPublisher) -> Connection {
        Connection {
            services: Services {
                summary: None,
                timed: crate::ipc::TimedPolicyPort::unavailable(),
                queries: database.port.clone(),
                writes: crate::ipc::WriteChannel::new().port,
                versions: database.versions.clone(),
                status: status.subscribe(),
                ui: Arc::new(|_| Ok(())),
            },
            registry: RegistryHandle::start(
                status.subscribe(),
                database.versions.clone(),
                Arc::new(|_| Ok(())),
            ),
            peer_pid: ProcessId::new(std::process::id()),
            timeouts: ConnectionTimeouts {
                hello: HELLO_TIMEOUT,
                close_flush: TEST_CLOSE_TIMEOUT,
            },
        }
    }
    #[tokio::test]
    async fn buffered_writes_are_admitted_in_frame_order_before_replies() {
        let database = TestDatabase::start();
        let status = StatusPublisher::new(quartz_protocol::InstanceId("ordered-writes".into()));
        let writes = crate::ipc::WriteChannel::new();
        let mut connection = connection(&database, &status);
        connection.services.writes = writes.port;
        let mut client = MemoryClient::connect(
            connection,
            HelloRole::Ui {
                ui: quartz_protocol::UiHello {
                    state: quartz_protocol::UiState::Visible,
                },
            },
        )
        .await;
        client
            .send_batch([
                quartz_protocol::Request {
                    id: RequestId(2),
                    op: quartz_protocol::Op::SettingsSet,
                    body: serde_json::json!({"resident":{"enabled":false}}),
                },
                quartz_protocol::Request {
                    id: RequestId(3),
                    op: quartz_protocol::Op::SettingsSet,
                    body: serde_json::json!({"resident":{"enabled":true}}),
                },
            ])
            .await;
        let mut admitted = Vec::new();
        for expected in [
            quartz_protocol::ResidentEnabled::Disabled,
            quartz_protocol::ResidentEnabled::Enabled,
        ] {
            let command = tokio::time::timeout(HELLO_TIMEOUT, async {
                loop {
                    if let Ok(command) = writes.receiver.try_recv() {
                        break command;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("write admitted without awaiting prior reply");
            assert!(
                matches!(command.request, crate::persistence::dispatch::WriteRequest::SettingsSet(update) if update.resident.is_some_and(|resident| resident.enabled == Some(expected)))
            );
            admitted.push(command);
        }
        for command in admitted {
            command
                .reply
                .send(Ok(crate::persistence::dispatch::WriteResponse::Accepted(
                    quartz_protocol::AcceptedBody,
                )))
                .expect("fake write reply");
        }
        client.close().await;
    }

    #[tokio::test]
    async fn buffered_subscribe_then_unsubscribe_leaves_no_date_subscription() {
        let database = TestDatabase::start();
        let status =
            StatusPublisher::new(quartz_protocol::InstanceId("ordered-subscriptions".into()));
        let mut fixture = crate::store::query::test_support::QueryFixture::new(1);
        let snapshots =
            quartz_domain::rules::registry::RuleRegistry::from_state(Default::default())
                .expect("rules")
                .snapshots();
        let mut versions =
            crate::query::versions::VersionPublisher::open(&mut fixture.store, snapshots)
                .expect("versions");
        let mut connection = connection(&database, &status);
        connection.services.versions = versions.port.clone();
        connection.registry = connection.services.start_registry();
        let mut client = MemoryClient::connect(
            connection,
            HelloRole::Ui {
                ui: quartz_protocol::UiHello {
                    state: quartz_protocol::UiState::Visible,
                },
            },
        )
        .await;
        client
            .send_batch([
                quartz_protocol::Request {
                    id: RequestId(2),
                    op: quartz_protocol::Op::Subscribe,
                    body: serde_json::json!({"topics":["day:2025-01-02"]}),
                },
                quartz_protocol::Request {
                    id: RequestId(3),
                    op: quartz_protocol::Op::Unsubscribe,
                    body: serde_json::json!({"topics":["day:2025-01-02"]}),
                },
            ])
            .await;
        // Installation requests one date, then removal requests the empty date set.
        for _ in 0..2 {
            let command = tokio::time::timeout(HELLO_TIMEOUT, async {
                loop {
                    if let Ok(command) = versions.publisher.requests.try_recv() {
                        break command;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("ordered version subscription update");
            versions
                .publisher
                .receive(&mut fixture.store, command)
                .expect("version command");
        }
        let mut responses = std::collections::BTreeSet::new();
        while responses.len() < 2 {
            if let Envelope::Response(response) = client.next().await {
                assert!(matches!(response.outcome, ResponseOutcome::Ok { .. }));
                responses.insert(response.id);
            }
        }
        assert!(
            versions.port.updates.borrow().days.is_empty(),
            "unsubscribe must follow subscribe admission"
        );
        client.close().await;
    }

    #[tokio::test]
    async fn pending_request_limit_returns_busy_before_admission() {
        let database = TestDatabase::start();
        let status = StatusPublisher::new(quartz_protocol::InstanceId("request-limit".into()));
        let connection = connection(&database, &status);
        let writer = OutboxTask::spawn(tokio::io::sink());
        let mut requests = PendingReplies::new();
        for _ in 0..MAX_PENDING_REQUESTS {
            requests.push(Box::pin(std::future::pending()));
        }
        let response = connection
            .admit_request(
                RequestId(2),
                &writer.handle,
                &mut requests,
                &PendingTracker::default(),
                || panic!("over-limit request must not enter an application queue"),
            )
            .expect("busy response");
        assert!(
            matches!(response.outcome, ResponseOutcome::Err { error } if error.kind == ErrorKind::Busy)
        );
        assert_eq!(requests.len(), MAX_PENDING_REQUESTS);
    }

    #[tokio::test]
    async fn pending_query_and_write_do_not_delay_status_or_disconnect_cancellation() {
        let database = TestDatabase::start();
        let status = StatusPublisher::new(quartz_protocol::InstanceId("pending-disconnect".into()));
        let queries = crate::query::dispatch::QueryChannel::new();
        let writes = crate::ipc::WriteChannel::new();
        let mut connection = connection(&database, &status);
        connection.services.queries = queries.port;
        connection.services.writes = writes.port;
        let mut client = MemoryClient::connect(connection, HelloRole::Cli).await;
        for request in [
            quartz_protocol::Request {
                id: RequestId(2),
                op: quartz_protocol::Op::QueryDay,
                body: serde_json::json!({"local_date":"2025-01-02"}),
            },
            quartz_protocol::Request {
                id: RequestId(3),
                op: quartz_protocol::Op::SettingsGet,
                body: serde_json::json!({}),
            },
        ] {
            let payload =
                quartz_protocol::encode_json(&Envelope::Request(request)).expect("request");
            client
                .frames
                .send(payload.as_slice())
                .await
                .expect("queue request");
        }
        let pending_query = tokio::time::timeout(HELLO_TIMEOUT, async {
            loop {
                if let Ok(command) = queries.receiver.try_recv() {
                    break command;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("query admitted");
        let pending_write = tokio::time::timeout(HELLO_TIMEOUT, async {
            loop {
                if let Ok(command) = writes.receiver.try_recv() {
                    break command;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("later write admitted without query completion");
        tokio::time::timeout(
            TEST_CLOSE_TIMEOUT,
            client.request(4, quartz_protocol::Op::CoreStatus, serde_json::json!({})),
        )
        .await
        .expect("status is independent of pending work");
        tokio::time::timeout(TEST_CLOSE_TIMEOUT, client.close())
            .await
            .expect("disconnect is observed promptly");
        let crate::query::dispatch::QueryCommand::Query { reply, .. } = pending_query else {
            panic!("query command");
        };
        assert!(
            reply.is_closed(),
            "disconnect cancels read-only database work"
        );
        assert!(
            pending_write.reply.is_closed(),
            "disconnect drops the response without waiting for persistence"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn flushed_final_response_releases_connection_without_grace_delay() {
        let database = TestDatabase::start();
        let status = StatusPublisher::new(quartz_protocol::InstanceId("close-test".into()));
        let mut writer = OutboxTask::spawn(tokio::io::sink());
        writer
            .handle
            .response(Action::success_response(
                RequestId(1),
                quartz_protocol::AcceptedBody,
            ))
            .expect("final response");
        let started = tokio::time::Instant::now();
        connection(&database, &status)
            .finish_closing(&mut writer)
            .await
            .expect("finish");
        assert_eq!(
            tokio::time::Instant::now(),
            started,
            "flushed frame must release the slot immediately"
        );
        writer.stop().await.expect("stop");
    }

    #[test]
    fn final_response_timeout_aborts_a_stalled_writer() {
        runtime().block_on(async {
            let database = TestDatabase::start();
            let status = StatusPublisher::new(quartz_protocol::InstanceId("memory-test".into()));
            let (write, _read) = tokio::io::duplex(1);
            let mut writer = OutboxTask::spawn(write);
            writer
                .handle
                .response(quartz_protocol::Response {
                    id: Some(RequestId(1)),
                    outcome: ResponseOutcome::Ok {
                        body: serde_json::json!({}),
                    },
                })
                .expect("queue final response");
            let outbox = writer.handle.clone();
            connection(&database, &status)
                .finish_closing(&mut writer)
                .await
                .expect("bounded close");
            writer.stop().await.expect("cancel timed-out writer");
            assert!(
                outbox.is_closed(),
                "timed-out writer releases its transport"
            );
        });
    }
    /// Real session/registry/outbox actors with in-memory pipes and no processes or windows.
    pub(crate) struct MemoryClient {
        frames: Framed<tokio::io::DuplexStream, LengthDelimitedCodec>,
        task: tokio::task::JoinHandle<io::Result<()>>,
    }
    impl MemoryClient {
        pub(crate) async fn connect(connection: Connection, role: HelloRole) -> Self {
            Self::connect_inner(connection, role, None).await
        }
        async fn connect_with_progress_clock(
            connection: Connection,
            role: HelloRole,
            clock: Arc<dyn Fn() -> Duration + Send + Sync>,
        ) -> Self {
            Self::connect_inner(connection, role, Some(clock)).await
        }
        async fn connect_inner(
            connection: Connection,
            role: HelloRole,
            clock: Option<Arc<dyn Fn() -> Duration + Send + Sync>>,
        ) -> Self {
            let (server, client) = tokio::io::duplex(TEST_BUFFER_BYTES);
            let task = tokio::spawn(async move {
                let (read, write) = split(server);
                let mut session = ConnectionSession::new(AllowedRoles::Quartz);
                let deadline = tokio::time::Instant::now() + HELLO_TIMEOUT;
                match clock {
                    Some(clock) => {
                        connection
                            .run_with_progress_clock(read, write, &mut session, deadline, clock)
                            .await
                    }
                    None => connection.run(read, write, &mut session, deadline).await,
                }
            });
            let mut client = Self {
                frames: Framed::new(client, frame_codec()),
                task,
            };
            let hello = quartz_protocol::Hello {
                proto: quartz_protocol::ProtoRange::new(PROTOCOL_VERSION, PROTOCOL_VERSION)
                    .expect("range"),
                client_version: quartz_protocol::ClientVersion("memory".into()),
                role,
            };
            client
                .request(
                    1,
                    quartz_protocol::Op::Hello,
                    serde_json::to_value(hello).expect("hello"),
                )
                .await;
            client
        }
        async fn send_batch<const COUNT: usize>(
            &mut self,
            requests: [quartz_protocol::Request; COUNT],
        ) {
            use tokio::io::AsyncWriteExt;
            let mut frames = Vec::new();
            for request in requests {
                let payload = quartz_protocol::encode_json(&Envelope::Request(request))
                    .expect("request frame");
                frames.extend_from_slice(
                    &u32::try_from(payload.len())
                        .expect("test frame length")
                        .to_le_bytes(),
                );
                frames.extend_from_slice(&payload);
            }
            self.frames
                .get_mut()
                .write_all(&frames)
                .await
                .expect("one buffered write for all frames");
        }
        async fn next(&mut self) -> Envelope {
            let payload = tokio::time::timeout(HELLO_TIMEOUT, self.frames.next())
                .await
                .expect("frame deadline")
                .expect("connected")
                .expect("frame");
            decode_json(&payload).expect("envelope")
        }
        pub(crate) async fn request(
            &mut self,
            id: u64,
            op: quartz_protocol::Op,
            body: serde_json::Value,
        ) {
            let payload =
                quartz_protocol::encode_json(&Envelope::Request(quartz_protocol::Request {
                    id: RequestId(id),
                    op,
                    body,
                }))
                .expect("encode request");
            self.frames.send(payload.as_slice()).await.expect("send");
            let Envelope::Response(response) = self.next().await else {
                panic!("request response")
            };
            assert_eq!(response.id, Some(RequestId(id)));
            assert!(
                matches!(response.outcome, ResponseOutcome::Ok { .. }),
                "{response:?}"
            );
        }
        async fn control(&mut self, expected: quartz_protocol::ControlOp) {
            let Envelope::Event(event) = self.next().await else {
                panic!("control event")
            };
            assert_eq!(event.topic, quartz_protocol::Topic::Control);
            let body: quartz_protocol::ControlBody =
                serde_json::from_value(event.body).expect("control body");
            assert_eq!(body.op, expected);
        }
        async fn quiet(&mut self) {
            assert!(
                tokio::time::timeout(TEST_CLOSE_TIMEOUT, self.frames.next())
                    .await
                    .is_err(),
                "unselected connection must receive no control"
            );
        }
        pub(crate) async fn close(self) {
            drop(self.frames);
            self.task
                .await
                .expect("connection task")
                .expect("connection close");
        }
    }
    #[tokio::test]
    async fn ui_hello_close_and_targeted_control_use_the_same_connection_identity() {
        use crate::ipc::{UiCommand, registry::ConnectionId};
        use quartz_protocol::{ControlOp, UiHello, UiState};
        let database = TestDatabase::start();
        let status = StatusPublisher::new(quartz_protocol::InstanceId("ui-lifecycle".into()));
        let (send, mut commands) = tokio::sync::mpsc::channel(crate::ipc::MAIN_COMMAND_CAPACITY);
        let mut services = connection(&database, &status).services;
        services.ui =
            Arc::new(move |command| send.try_send(command).map_err(|_| crate::ipc::QueueUiError));
        let registry = services.start_registry();
        const VERIFIED_PID: ProcessId = ProcessId::new(42);
        let make_connection = || Connection {
            services: services.clone(),
            registry: registry.clone(),
            timeouts: ConnectionTimeouts::default(),
            peer_pid: VERIFIED_PID,
        };
        let mut cli = MemoryClient::connect(make_connection(), HelloRole::Cli).await;
        assert!(
            commands.try_recv().is_err(),
            "CLI hello emits no UI registration"
        );
        for phase in [UiState::Starting, UiState::Prewarming, UiState::Visible] {
            let mut first = MemoryClient::connect(
                make_connection(),
                HelloRole::Ui {
                    ui: UiHello { state: phase },
                },
            )
            .await;
            let UiCommand::Registered {
                connection: registered,
                pid,
                state,
                ..
            } = commands.recv().await.expect("UI registration")
            else {
                panic!("registered command")
            };
            assert_eq!(pid, VERIFIED_PID, "verified transport PID is reported");
            assert_eq!(state, phase);
            let mut second = MemoryClient::connect(
                make_connection(),
                HelloRole::Ui {
                    ui: UiHello {
                        state: UiState::Visible,
                    },
                },
            )
            .await;
            let UiCommand::Registered {
                connection: other, ..
            } = commands.recv().await.expect("second registration")
            else {
                panic!("registered command")
            };
            assert_ne!(registered, other);
            for op in [ControlOp::UiShow, ControlOp::UiActivate] {
                registry
                    .target_control(registered, op)
                    .expect("target control");
                first.control(op).await;
                second.quiet().await;
                cli.quiet().await;
            }
            first
                .request(2, quartz_protocol::Op::UiFirstFrame, serde_json::json!({}))
                .await;
            assert_eq!(
                commands.recv().await,
                Some(UiCommand::FirstFrame {
                    connection: registered
                })
            );
            first
                .request(3, quartz_protocol::Op::UiHidden, serde_json::json!({}))
                .await;
            assert_eq!(
                commands.recv().await,
                Some(UiCommand::Hidden {
                    connection: registered
                })
            );
            registry
                .target_control(registered, ControlOp::UiShutdown)
                .expect("expire selected prewarm connection");
            first.control(ControlOp::UiShutdown).await;
            second.quiet().await;
            cli.quiet().await;
            registry
                .control(ControlOp::UiShutdown)
                .expect("broadcast shutdown");
            first.control(ControlOp::UiShutdown).await;
            second.control(ControlOp::UiShutdown).await;
            cli.quiet().await;
            first.close().await;
            assert_eq!(
                commands.recv().await,
                Some(UiCommand::Unregistered {
                    connection: registered
                })
            );
            registry
                .target_control(registered, ControlOp::UiActivate)
                .expect("stale target");
            registry
                .target_control(ConnectionId::for_test(u64::MAX), ControlOp::UiShow)
                .expect("unknown target");
            registry
                .target_control(other, ControlOp::UiShutdown)
                .expect("targeted prewarm shutdown");
            second.control(ControlOp::UiShutdown).await;
            cli.quiet().await;
            registry
                .control(ControlOp::UiActivate)
                .expect("reject broadcast activation");
            second.quiet().await;
            second.close().await;
            assert_eq!(
                commands.recv().await,
                Some(UiCommand::Unregistered { connection: other })
            );
        }
        cli.close().await;
        registry.outboxes().await.expect("cleanup barrier");
        assert!(
            commands.try_recv().is_err(),
            "no duplicate unregister notifications"
        );
    }

    #[tokio::test]
    async fn ui_preview_executes_on_the_database_query_worker() {
        use quartz_protocol::{Op, Request};
        let database = TestDatabase::start();
        let status = StatusPublisher::new(quartz_protocol::InstanceId("preview-worker".into()));
        let mut client = MemoryClient::connect(
            connection(&database, &status),
            HelloRole::Ui {
                ui: quartz_protocol::UiHello {
                    state: quartz_protocol::UiState::Visible,
                },
            },
        )
        .await;
        client
            .send_batch([Request {
                id: RequestId(2),
                op: Op::RulesPreview,
                body: serde_json::json!({
                    "mutation": {
                        "op": "rules_text.import",
                        "body": {
                            "key": "user.worker",
                            "base_version": 0,
                            "text": "format='quartz-rules/1'\n[set]\nkey='user.worker'\nkind='classification'\nname='Worker'\n"
                        }
                    },
                    "start_date": "2025-01-02",
                    "end_date": "2025-01-03"
                }),
            }])
            .await;
        let Envelope::Response(response) = client.next().await else {
            panic!("preview response")
        };
        let ResponseOutcome::Ok { body } = response.outcome else {
            panic!("preview success")
        };
        let preview: quartz_protocol::RulesPreviewResponse =
            serde_json::from_value(body).expect("typed response");
        assert_eq!(preview.totals.after.active_ms, 4000);
        client.close().await;
    }

    #[tokio::test]
    async fn slow_multi_day_query_emits_monotonic_progress_only_for_ui_and_before_response() {
        use quartz_protocol::{Op, Request};
        use std::sync::atomic::{AtomicU64, Ordering};

        let database = TestDatabase::start();
        let status = StatusPublisher::new(quartz_protocol::InstanceId("query-progress".into()));
        let range_body = serde_json::json!({
            "start_date": "2025-01-02",
            "end_date": "2025-01-06",
            "bucket": "day",
            "dimensions": ["category"],
        });

        let ui_ticks = Arc::new(AtomicU64::new(0));
        let mut ui = MemoryClient::connect_with_progress_clock(
            connection(&database, &status),
            HelloRole::Ui {
                ui: quartz_protocol::UiHello {
                    state: quartz_protocol::UiState::Visible,
                },
            },
            Arc::new(move || {
                let step = ui_ticks.fetch_add(1, Ordering::Relaxed) + 1;
                Duration::from_millis(step * 250)
            }),
        )
        .await;
        ui.send_batch([Request {
            id: RequestId(2),
            op: Op::QueryRange,
            body: range_body.clone(),
        }])
        .await;
        let mut progress_done = Vec::new();
        let response = loop {
            match ui.next().await {
                Envelope::Progress(progress) => {
                    assert_eq!(progress.id, RequestId(2));
                    assert_eq!(progress.total, 4);
                    assert!(progress.done <= progress.total);
                    if let Some(&previous) = progress_done.last() {
                        assert!(progress.done > previous, "progress must be monotonic");
                    }
                    progress_done.push(progress.done);
                }
                Envelope::Response(response) => break response,
                other => panic!("expected progress or response, got {other:?}"),
            }
        };
        assert_eq!(response.id, Some(RequestId(2)));
        assert!(matches!(response.outcome, ResponseOutcome::Ok { .. }));
        assert_eq!(progress_done, [1, 2, 3]);
        // Single-day queries never emit progress even on a UI connection.
        ui.request(
            3,
            Op::QueryDay,
            serde_json::json!({"local_date":"2025-01-02"}),
        )
        .await;
        ui.close().await;

        let cli_ticks = Arc::new(AtomicU64::new(0));
        let mut cli = MemoryClient::connect_with_progress_clock(
            connection(&database, &status),
            HelloRole::Cli,
            Arc::new(move || {
                let step = cli_ticks.fetch_add(1, Ordering::Relaxed) + 1;
                Duration::from_millis(step * 250)
            }),
        )
        .await;
        cli.send_batch([Request {
            id: RequestId(2),
            op: Op::QueryRange,
            body: range_body,
        }])
        .await;
        let Envelope::Response(response) = cli.next().await else {
            panic!("CLI connection must never receive progress frames")
        };
        assert_eq!(response.id, Some(RequestId(2)));
        assert!(matches!(response.outcome, ResponseOutcome::Ok { .. }));
        cli.close().await;
    }
}
