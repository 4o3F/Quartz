//! Framed protocol sessions over arbitrary asynchronous transport halves.
use super::{
    ConnectionSession, PublishedStatus, UiPort, outbox::OutboxTask, registry::RegistryHandle,
};
use crate::query::dispatch::QueryPort;
use futures_util::StreamExt;
use quartz_protocol::{
    Envelope, ErrorKind, ProtocolError, ResponseOutcome, decode_json, frame_codec,
};
use std::{io, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
};
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
    pub queries: QueryPort,
    pub registry: RegistryHandle,
    pub timeouts: ConnectionTimeouts,
    pub status: watch::Receiver<PublishedStatus>,
    pub ui: UiPort,
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
        let mut reader = FramedRead::new(read, frame_codec());
        let writer = OutboxTask::spawn(write);
        let outbox = writer.handle.clone();
        let mut registration = None;
        loop {
            let received = tokio::select! {
                () = outbox.closed() => return Ok(()),
                received = self.next_frame(&mut reader, session, hello_deadline) => received,
            };
            let mut decision = match received {
                None => break,
                Some(Ok(payload)) => match decode_json::<Envelope>(&payload) {
                    Ok(envelope) => session.receive(envelope, &self.status.borrow().status),
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
            if let Some(query) = decision.query.take() {
                let result = self.queries.call(query).await;
                decision.response.outcome = match result {
                    Ok(response) => match serde_json::to_value(response) {
                        Ok(body) => ResponseOutcome::Ok { body },
                        Err(_) => ResponseOutcome::Err {
                            error: ProtocolError::new(ErrorKind::Internal),
                        },
                    },
                    Err(error) => ResponseOutcome::Err { error },
                };
            }
            if let Some(command) = decision.command
                && let Err(error) = (self.ui)(command)
            {
                tracing::warn!(%error, "could not queue IPC UI command");
                decision.response.outcome = ResponseOutcome::Err {
                    error: ProtocolError::new(ErrorKind::Internal),
                };
            }
            if registration.is_none()
                && let Some(role) = session.role()
            {
                registration = Some(
                    self.registry
                        .register(role, outbox.clone())
                        .map_err(io::Error::other)?,
                );
            }
            if let Some(subscription) = decision.subscription {
                let id = registration
                    .as_ref()
                    .expect("subscription follows authenticated hello")
                    .id;
                self.registry
                    .subscribe(id, subscription, decision.response)
                    .await
                    .map_err(io::Error::other)?;
            } else {
                outbox
                    .response(decision.response)
                    .map_err(io::Error::other)?;
            }
            if session.closing() {
                return self.finish_closing(writer).await;
            }
        }
        drop(writer);
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
    async fn finish_closing(&self, writer: OutboxTask) -> io::Result<()> {
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
    use quartz_protocol::{HelloRole, PROTOCOL_VERSION, RequestId};
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
            queries: database.port.clone(),
            registry: RegistryHandle::start(status.subscribe(), database.versions.clone()),
            timeouts: ConnectionTimeouts {
                hello: HELLO_TIMEOUT,
                close_flush: TEST_CLOSE_TIMEOUT,
            },
            status: status.subscribe(),
            ui: Arc::new(|_| Ok(())),
        }
    }
    #[test]
    fn final_response_timeout_aborts_a_stalled_writer() {
        runtime().block_on(async {
            let database = TestDatabase::start();
            let status = StatusPublisher::new(quartz_protocol::InstanceId("memory-test".into()));
            let (write, _read) = tokio::io::duplex(1);
            let writer = OutboxTask::spawn(write);
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
                .finish_closing(writer)
                .await
                .expect("bounded close");
            tokio::task::yield_now().await;
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
}
