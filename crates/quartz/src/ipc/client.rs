//! Asynchronous core requests and a current-thread blocking facade for CLI callers.

#[cfg(test)]
pub(crate) use crate::platform::windows::ipc::CONNECT_TIMEOUT;
#[cfg(test)]
use crate::platform::windows::ipc::PipeName;
use crate::platform::windows::ipc::{OpenCorePipeError, PipeClient};
use futures_util::{SinkExt, StreamExt};
use quartz_protocol::{
    AcceptedBody, ClientVersion, CoreStatusRequest, CoreStatusResponse, Envelope, ErrorKind, Hello,
    HelloResponse, HelloRole, Op, PROTOCOL_VERSION, ProtoRange, ProtoVersion, Request, RequestId,
    ResponseOutcome, UiOpenRequest, decode_json, encode_json, frame_codec,
};
use snafu::{ResultExt, Snafu};
use std::{io, time::Duration};
use tokio::{
    net::windows::named_pipe::NamedPipeClient,
    runtime::{Builder, Runtime},
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

/// Bounds each response and client send, including a stalled peer (04 §4).
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
/// The CLI facade performs pipe I/O on its caller, with a minimal blocking-pool ceiling (01 §7.2).
const CLIENT_BLOCKING_THREADS: usize = 1;

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum CallCoreError {
    #[snafu(display("could not build core client runtime"))]
    Runtime { source: io::Error },
    #[snafu(display("{source}"))]
    Connect { source: OpenCorePipeError },
    #[snafu(display("core negotiated an unexpected protocol version: {received:?}"))]
    HandshakeVersion { received: ProtoVersion },
    #[snafu(display("core rejected the request: {}", kind.code()))]
    Protocol { kind: ErrorKind },
    #[snafu(display("could not exchange core IPC frame"))]
    Io { source: io::Error },
    #[snafu(display("core response deadline expired"))]
    ResponseTimeout { source: tokio::time::error::Elapsed },
    #[snafu(display("invalid core response"))]
    Decode {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[snafu(display("could not encode core request"))]
    Encode {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[snafu(display("expected a core response envelope"))]
    UnexpectedEnvelope,
    #[snafu(display("core response id {received:?} does not match request {expected:?}"))]
    ResponseIdMismatch {
        expected: RequestId,
        received: Option<RequestId>,
    },
}

impl CallCoreError {
    fn decode(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Decode {
            source: Box::new(source),
        }
    }
    fn encode(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Encode {
            source: Box::new(source),
        }
    }
}

/// Core protocol client that can run on the M3 UI background runtime.
#[derive(Debug)]
pub(crate) struct AsyncCoreClient {
    framed: Framed<NamedPipeClient, LengthDelimitedCodec>,
    next_id: RequestId,
}
impl AsyncCoreClient {
    pub(crate) async fn connect(role: HelloRole) -> Result<Self, CallCoreError> {
        Self::from_pipe(
            PipeClient::connect()
                .await
                .context(call_core_error::ConnectSnafu)?,
        )
        .handshake(role)
        .await
    }
    #[cfg(test)]
    async fn open_named(name: &PipeName) -> Result<Self, CallCoreError> {
        Ok(Self::from_pipe(
            PipeClient::open_named(name)
                .await
                .context(call_core_error::ConnectSnafu)?,
        ))
    }
    fn from_pipe(pipe: PipeClient) -> Self {
        Self {
            framed: Framed::new(pipe.into_inner(), frame_codec()),
            next_id: RequestId(1),
        }
    }
    async fn handshake(mut self, role: HelloRole) -> Result<Self, CallCoreError> {
        let hello = Hello {
            proto: ProtoRange::new(PROTOCOL_VERSION, PROTOCOL_VERSION)
                .expect("single protocol version"),
            client_version: ClientVersion(env!("CARGO_PKG_VERSION").into()),
            role,
        };
        let response: HelloResponse =
            serde_json::from_value(self.request(Op::Hello, &hello).await?)
                .map_err(CallCoreError::decode)?;
        if response.proto != PROTOCOL_VERSION {
            return Err(CallCoreError::HandshakeVersion {
                received: response.proto,
            });
        }
        Ok(self)
    }
    pub(crate) async fn status(&mut self) -> Result<CoreStatusResponse, CallCoreError> {
        serde_json::from_value(self.request(Op::CoreStatus, &CoreStatusRequest {}).await?)
            .map_err(CallCoreError::decode)
    }
    pub(crate) async fn open_ui(&mut self) -> Result<AcceptedBody, CallCoreError> {
        serde_json::from_value(self.request(Op::UiOpen, &UiOpenRequest {}).await?)
            .map_err(CallCoreError::decode)
    }
    pub(crate) async fn request(
        &mut self,
        op: Op,
        body: &impl serde::Serialize,
    ) -> Result<serde_json::Value, CallCoreError> {
        let id = self.next_id;
        self.next_id = RequestId(
            id.0.checked_add(1)
                .expect("u64 counter cannot overflow in practice"),
        );
        let body = serde_json::to_value(body).map_err(CallCoreError::encode)?;
        let payload = encode_json(&Envelope::Request(Request { id, op, body }))
            .map_err(CallCoreError::encode)?;
        tokio::time::timeout(RESPONSE_TIMEOUT, self.framed.send(payload.as_slice()))
            .await
            .context(call_core_error::ResponseTimeoutSnafu)?
            .context(call_core_error::IoSnafu)?;
        let Envelope::Response(response) = self.read_envelope().await? else {
            return Err(CallCoreError::UnexpectedEnvelope);
        };
        if response.id != Some(id) {
            return Err(CallCoreError::ResponseIdMismatch {
                expected: id,
                received: response.id,
            });
        }
        match response.outcome {
            ResponseOutcome::Ok { body } => Ok(body),
            ResponseOutcome::Err { error } => Err(CallCoreError::Protocol { kind: error.kind }),
        }
    }
    async fn read_envelope(&mut self) -> Result<Envelope, CallCoreError> {
        let payload = tokio::time::timeout(RESPONSE_TIMEOUT, self.framed.next())
            .await
            .context(call_core_error::ResponseTimeoutSnafu)?
            .ok_or_else(|| CallCoreError::Io {
                source: io::ErrorKind::UnexpectedEof.into(),
            })?
            .map_err(CallCoreError::decode)?;
        decode_json(&payload).map_err(CallCoreError::decode)
    }
}

/// Owns one async client and its caller-thread runtime for a CLI invocation.
#[derive(Debug)]
pub(crate) struct CoreClient {
    // Close the client before dropping the runtime that registered its pipe.
    client: AsyncCoreClient,
    runtime: Runtime,
}
impl CoreClient {
    fn runtime() -> Result<Runtime, CallCoreError> {
        Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .max_blocking_threads(CLIENT_BLOCKING_THREADS)
            .build()
            .context(call_core_error::RuntimeSnafu)
    }
    pub(crate) fn connect() -> Result<Self, CallCoreError> {
        let runtime = Self::runtime()?;
        let client = runtime.block_on(AsyncCoreClient::connect(HelloRole::Cli))?;
        Ok(Self { client, runtime })
    }
    pub(crate) fn status(&mut self) -> Result<CoreStatusResponse, CallCoreError> {
        self.runtime.block_on(self.client.status())
    }
    pub(crate) fn open_ui(&mut self) -> Result<AcceptedBody, CallCoreError> {
        self.runtime.block_on(self.client.open_ui())
    }
    #[cfg(test)]
    pub(crate) fn connect_named(name: &PipeName, role: HelloRole) -> Result<Self, CallCoreError> {
        let runtime = Self::runtime()?;
        let client = runtime.block_on(async {
            AsyncCoreClient::open_named(name)
                .await?
                .handshake(role)
                .await
        })?;
        Ok(Self { client, runtime })
    }
    #[cfg(test)]
    pub(crate) fn open_named(name: &PipeName) -> Result<Self, CallCoreError> {
        let runtime = Self::runtime()?;
        let client = runtime.block_on(AsyncCoreClient::open_named(name))?;
        Ok(Self { client, runtime })
    }
    #[cfg(test)]
    pub(crate) fn request(
        &mut self,
        op: Op,
        body: &impl serde::Serialize,
    ) -> Result<serde_json::Value, CallCoreError> {
        self.runtime.block_on(self.client.request(op, body))
    }
    #[cfg(test)]
    pub(crate) fn read_envelope(&mut self) -> Result<Envelope, CallCoreError> {
        self.runtime.block_on(self.client.read_envelope())
    }
    #[cfg(test)]
    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CallCoreError> {
        use tokio::io::AsyncWriteExt;
        self.runtime
            .block_on(async {
                tokio::time::timeout(
                    RESPONSE_TIMEOUT,
                    self.client.framed.get_mut().write_all(bytes),
                )
                .await
            })
            .context(call_core_error::ResponseTimeoutSnafu)?
            .context(call_core_error::IoSnafu)
    }
}
pub(crate) fn print_status() -> Result<(), CallCoreError> {
    let status = CoreClient::connect()?.status()?;
    println!("{}", StatusReport { status: &status });
    Ok(())
}

struct StatusReport<'status> {
    status: &'status CoreStatusResponse,
}
impl std::fmt::Display for StatusReport<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = self.status;
        writeln!(formatter, "recording: {}", status.recording)?;
        writeln!(
            formatter,
            "stop_reason: {}",
            status
                .stop_reason
                .map_or("none", Into::<&'static str>::into)
        )?;
        writeln!(
            formatter,
            "database: {}",
            Into::<&'static str>::into(status.database)
        )?;
        writeln!(
            formatter,
            "browser_streams: {}",
            status.browser_streams.len()
        )?;
        writeln!(formatter, "instance_id: {}", status.instance_id.0)?;
        write!(formatter, "core_version: {}", status.core_version.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::{DecodeJsonError, EncodeFrameError};
    struct ConnectedClient {
        _server: crate::platform::windows::ipc::PipeServer,
        client: CoreClient,
        _queries: crossbeam_channel::Receiver<crate::query::dispatch::QueryCommand>,
        _versions: tokio::sync::watch::Sender<crate::query::versions::VersionState>,
    }
    impl ConnectedClient {
        fn new() -> Self {
            use crate::platform::windows::ipc::{PipeServer, ProcessIdentity};
            let suffix = windows::core::GUID::new().expect("unique test pipe");
            let name = PipeName::for_test(suffix);
            let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId(format!(
                "{suffix:?}"
            )));
            let queries = crate::query::dispatch::QueryChannel::new();
            let (versions, received) =
                tokio::sync::watch::channel(crate::query::versions::VersionState::empty());
            let server = PipeServer::start(
                crate::platform::windows::ipc::PipeServerConfig {
                    owner: ProcessIdentity::current().expect("identity"),
                    name: name.clone(),
                    timeouts: crate::platform::windows::ipc::ConnectionTimeouts::default(),
                    queries: queries.port,
                    versions: received,
                },
                status.subscribe(),
                std::sync::Arc::new(|_| Ok(())),
            )
            .expect("test server");
            Self {
                _server: server,
                _queries: queries.receiver,
                _versions: versions,
                client: CoreClient::connect_named(&name, HelloRole::Cli).expect("test client"),
            }
        }
    }
    fn inject(client: &mut CoreClient, payload: &[u8]) {
        use tokio_util::{bytes::BytesMut, codec::Encoder};
        let mut wire = BytesMut::new();
        frame_codec()
            .encode(payload, &mut wire)
            .expect("test frame");
        client
            .client
            .framed
            .read_buffer_mut()
            .extend_from_slice(&wire);
    }
    #[test]
    fn malformed_response_keeps_the_decoder_source() {
        use std::error::Error;
        let mut fixture = ConnectedClient::new();
        inject(&mut fixture.client, b"{garbage");
        let error = fixture.client.status().expect_err("invalid envelope");
        assert!(matches!(error, CallCoreError::Decode { .. }));
        assert!(
            error
                .source()
                .expect("decoder source")
                .downcast_ref::<DecodeJsonError>()
                .is_some()
        );
    }
    #[test]
    fn malformed_body_keeps_the_serde_source() {
        use std::error::Error;
        let mut fixture = ConnectedClient::new();
        let id = fixture.client.client.next_id;
        inject(
            &mut fixture.client,
            &encode_json(&serde_json::json!({"t":"res","id":id,"ok":true,"body":null}))
                .expect("response"),
        );
        let error = fixture.client.status().expect_err("invalid body");
        assert!(matches!(error, CallCoreError::Decode { .. }));
        assert!(
            error
                .source()
                .expect("serde source")
                .downcast_ref::<serde_json::Error>()
                .is_some()
        );
    }
    #[test]
    fn mismatched_response_id_is_rejected() {
        let mut fixture = ConnectedClient::new();
        inject(
            &mut fixture.client,
            &encode_json(&serde_json::json!({"t":"res","id":99,"ok":true,"body":{}}))
                .expect("response"),
        );
        assert!(matches!(
            fixture.client.status(),
            Err(CallCoreError::ResponseIdMismatch {
                expected: RequestId(2),
                received: Some(RequestId(99))
            })
        ));
    }
    #[test]
    fn oversized_request_keeps_the_encoder_source() {
        use std::error::Error;
        let mut fixture = ConnectedClient::new();
        let error = fixture
            .client
            .request(Op::UiOpen, &"x".repeat(quartz_protocol::MAX_FRAME_LEN))
            .expect_err("oversize request");
        assert!(matches!(error, CallCoreError::Encode { .. }));
        assert!(
            error
                .source()
                .expect("encoder source")
                .downcast_ref::<EncodeFrameError>()
                .is_some()
        );
    }
    #[test]
    fn status_report_has_stable_keys_and_enum_values() {
        let status =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("test-instance".into()));
        let receiver = status.subscribe();
        let snapshot = receiver.borrow();
        assert_eq!(
            StatusReport {
                status: &snapshot.status
            }
            .to_string(),
            concat!(
                "recording: false\n",
                "stop_reason: starting\n",
                "database: ok\n",
                "browser_streams: 0\n",
                "instance_id: test-instance\n",
                "core_version: ",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}
