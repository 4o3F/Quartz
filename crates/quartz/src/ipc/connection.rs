//! Framed protocol sessions over arbitrary asynchronous transport halves.
use super::{
    ConnectionSession, ProcessId, Services, outbox::OutboxTask, registry::RegistryHandle,
    session::Action,
};
use futures_util::StreamExt;
use quartz_protocol::{Envelope, ErrorKind, ProtocolError, decode_json, frame_codec};
use std::{io, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::{
    bytes::BytesMut,
    codec::{FramedRead, LengthDelimitedCodec, LengthDelimitedCodecError},
};

/// Bound the final error response and close grace period (04 §2 error-and-close).
const CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_millis(500);
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
        let mut writer = OutboxTask::spawn(write);
        let result = self
            .run_session(read, &mut writer, session, hello_deadline)
            .await;
        result.and(writer.stop().await)
    }
    async fn run_session<R: AsyncRead + Unpin>(
        &self,
        read: R,
        writer: &mut OutboxTask,
        session: &mut ConnectionSession,
        hello_deadline: tokio::time::Instant,
    ) -> io::Result<()> {
        let mut reader = FramedRead::new(read, frame_codec());
        let outbox = writer.handle.clone();
        let mut registration = None;
        loop {
            let received = tokio::select! {
                () = outbox.closed() => return Ok(()),
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
                Action::Write { id, request } => {
                    Some(match self.services.writes.call(request).await {
                        Ok(response) => Action::success_response(id, response),
                        Err(error) => Action::error_response(Some(id), error),
                    })
                }
                Action::Query { id, request } => {
                    Some(match self.services.queries.call(request).await {
                        Ok(response) => Action::success_response(id, response),
                        Err(error) => Action::error_response(Some(id), error),
                    })
                }
                Action::Unsubscribe { id, subscription } => {
                    let connection = registration.as_ref().expect("unsubscribe follows hello").id;
                    self.registry
                        .unsubscribe(connection, subscription)
                        .await
                        .map_err(io::Error::other)?;
                    Some(Action::success_response(
                        id,
                        quartz_protocol::SubscribeResponse {},
                    ))
                }
                Action::Subscribe { id, subscription } => {
                    let connection = registration
                        .as_ref()
                        .expect("subscription follows authenticated hello")
                        .id;
                    self.registry
                        .subscribe(
                            connection,
                            subscription,
                            Action::success_response(id, quartz_protocol::SubscribeResponse {}),
                        )
                        .await
                        .map_err(io::Error::other)?;
                    None
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
        let deadline = tokio::time::Instant::now() + self.timeouts.close_flush;
        let outcome = tokio::time::timeout(self.timeouts.close_flush, writer.finish()).await;
        match outcome {
            Ok(result) => {
                // Allow the final response to be read before DisconnectNamedPipe discards unread bytes.
                tokio::time::sleep_until(deadline).await;
                result
            }
            Err(error) => {
                tracing::debug!(%error, "pipe final response deadline expired; closing connection");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
    #[test]
    fn memory_transport_performs_hello_and_dispatches_query() {
        runtime().block_on(async {
            let database = TestDatabase::start();
            let status = StatusPublisher::new(quartz_protocol::InstanceId("memory-test".into()));
            let connection = connection(&database, &status);
            let (server, client) = tokio::io::duplex(TEST_BUFFER_BYTES);
            let task = tokio::spawn(async move {
                let (read, write) = split(server);
                let mut session = ConnectionSession::new(AllowedRoles::Quartz);
                connection
                    .run(
                        read,
                        write,
                        &mut session,
                        tokio::time::Instant::now() + HELLO_TIMEOUT,
                    )
                    .await
            });
            let mut client = Framed::new(client, frame_codec());
            let hello = quartz_protocol::Hello {
                proto: quartz_protocol::ProtoRange::new(PROTOCOL_VERSION, PROTOCOL_VERSION)
                    .expect("protocol"),
                client_version: quartz_protocol::ClientVersion("test".into()),
                role: HelloRole::Cli,
            };
            let request = |id, op, body| {
                quartz_protocol::Envelope::Request(quartz_protocol::Request {
                    id: RequestId(id),
                    op,
                    body,
                })
            };
            let payload = quartz_protocol::encode_json(&request(
                1,
                quartz_protocol::Op::Hello,
                serde_json::to_value(hello).expect("hello"),
            ))
            .expect("encode");
            client.send(payload.as_slice()).await.expect("send hello");
            let received = client.next().await.expect("hello response").expect("frame");
            let Envelope::Response(response) = decode_json(&received).expect("response") else {
                panic!("expected hello response")
            };
            assert!(matches!(response.outcome, ResponseOutcome::Ok { .. }));
            let payload = quartz_protocol::encode_json(&request(
                2,
                quartz_protocol::Op::QueryDay,
                serde_json::json!({"local_date":"2025-01-02"}),
            ))
            .expect("encode query");
            client.send(payload.as_slice()).await.expect("send query");
            let received = client.next().await.expect("query response").expect("frame");
            let Envelope::Response(response) = decode_json(&received).expect("response") else {
                panic!("expected query response")
            };
            assert_eq!(response.id, Some(RequestId(2)));
            assert!(
                matches!(response.outcome, ResponseOutcome::Ok { .. }),
                "{response:?}"
            );
            drop(client);
            task.await
                .expect("connection task")
                .expect("connection close");
        });
    }
    /// Real session/registry/outbox actors with in-memory pipes and no processes or windows.
    struct MemoryClient {
        frames: Framed<tokio::io::DuplexStream, LengthDelimitedCodec>,
        task: tokio::task::JoinHandle<io::Result<()>>,
    }
    impl MemoryClient {
        async fn connect(connection: Connection, role: HelloRole) -> Self {
            let (server, client) = tokio::io::duplex(TEST_BUFFER_BYTES);
            let task = tokio::spawn(async move {
                let (read, write) = split(server);
                connection
                    .run(
                        read,
                        write,
                        &mut ConnectionSession::new(AllowedRoles::Quartz),
                        tokio::time::Instant::now() + HELLO_TIMEOUT,
                    )
                    .await
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
        async fn next(&mut self) -> Envelope {
            let payload = tokio::time::timeout(HELLO_TIMEOUT, self.frames.next())
                .await
                .expect("frame deadline")
                .expect("connected")
                .expect("frame");
            decode_json(&payload).expect("envelope")
        }
        async fn request(&mut self, id: u64, op: quartz_protocol::Op, body: serde_json::Value) {
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
        async fn close(self) {
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
}
