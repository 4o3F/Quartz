//! Protocol framing and correlated calls, independent of the pipe platform.

use quartz_protocol::{
    AcceptedBody, ClientVersion, CoreRequest, CoreStatusRequest, CoreStatusResponse, Envelope,
    ErrorKind, Event, Hello, HelloResponse, HelloRole, JsonValue, Op, PROTOCOL_VERSION, Progress,
    ProtoRange, ProtoVersion, Request, RequestId, Response, ResponseOutcome, UiOpenRequest,
    decode_json, encode_json, frame_codec,
};
use snafu::{ResultExt, Snafu};
use std::{
    collections::{BTreeSet, VecDeque},
    io,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    runtime::{Builder, Runtime},
};
use tokio_util::{
    bytes::BytesMut,
    codec::{Decoder, Encoder, LengthDelimitedCodec},
};

/// Bounds each send and complete response, including interleaved events (04 §4).
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
/// CLI runtime ceiling (01 §7.2).
const CLIENT_BLOCKING_THREADS: usize = 1;
/// A bounded event backlog for sequential CLI calls; UI consumes frames continuously (04 §7).
const EVENT_CAPACITY: usize = 64;

/// Failures while connecting, framing, or calling a core operation.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum CallCoreError {
    #[snafu(display("could not build core client runtime"))]
    Runtime { source: io::Error },
    #[cfg(windows)]
    #[snafu(display("{source}"))]
    Open { source: crate::OpenCorePipeError },
    #[cfg(windows)]
    #[snafu(display("could not wait for Quartz core pipe"))]
    Wait { source: crate::WaitCorePipeError },
    #[snafu(display("core negotiated an unexpected protocol version: {received:?}"))]
    HandshakeVersion {
        received: ProtoVersion,
        core_version: quartz_protocol::CoreVersion,
    },
    #[snafu(display("core rejected the request: {}", kind.code()))]
    Protocol { kind: ErrorKind },
    #[snafu(display("core ended the connection: {}", kind.code()))]
    ConnectionProtocol { kind: ErrorKind },
    #[snafu(display(
        "core connection must be discarded after an interrupted send or connection error"
    ))]
    DiscardConnection,
    #[snafu(display("could not exchange core IPC frame"))]
    Io { source: io::Error },
    #[snafu(display("core response deadline expired"))]
    ResponseTimeout { source: tokio::time::error::Elapsed },
    #[snafu(display("invalid core response"))]
    Decode {
        source: quartz_protocol::DecodeJsonError,
    },
    #[snafu(display("invalid core response body"))]
    DecodeBody { source: quartz_protocol::JsonError },
    #[snafu(display("invalid core transport frame"))]
    Frame { source: io::Error },
    #[snafu(display("could not encode core request"))]
    Encode {
        source: quartz_protocol::EncodeFrameError,
    },
    #[snafu(display("could not encode core request body"))]
    EncodeBody { source: quartz_protocol::JsonError },
    #[snafu(display("core transport write deadline expired"))]
    WriteTimeout { source: tokio::time::error::Elapsed },
    #[snafu(display("expected a core response, progress, or event envelope"))]
    UnexpectedEnvelope,
    #[snafu(display("core response id {received:?} does not match request {expected:?}"))]
    ResponseIdMismatch {
        expected: RequestId,
        received: Option<RequestId>,
    },
    #[snafu(display("core sent an unsolicited response: {received:?}"))]
    UnsolicitedResponse { received: Option<RequestId> },
    #[snafu(display("core event backlog is full"))]
    EventOverflow,
    #[snafu(display("core client request counter exhausted"))]
    RequestIdExhausted,
}
impl CallCoreError {
    /// Preserves a body decoding failure at the typed request boundary.
    pub fn decode_body(source: quartz_protocol::JsonError) -> Self {
        Self::DecodeBody { source }
    }
    /// Preserves a body encoding failure at the typed request boundary.
    pub fn encode_body(source: quartz_protocol::JsonError) -> Self {
        Self::EncodeBody { source }
    }
    /// Preserves the frame decoder's concrete source.
    pub fn decode(source: quartz_protocol::DecodeJsonError) -> Self {
        Self::Decode { source }
    }
    /// Preserves the frame encoder's concrete source.
    pub fn encode(source: quartz_protocol::EncodeFrameError) -> Self {
        Self::Encode { source }
    }
    /// Whether framing or transport must be discarded before another call (04 §2–§4).
    pub fn ends_connection(&self) -> bool {
        match self {
            Self::Io { .. }
            | Self::Frame { .. }
            | Self::WriteTimeout { .. }
            | Self::ConnectionProtocol { .. }
            | Self::DiscardConnection => true,
            Self::Runtime { .. }
            | Self::HandshakeVersion { .. }
            | Self::Protocol { .. }
            | Self::ResponseTimeout { .. }
            | Self::Decode { .. }
            | Self::DecodeBody { .. }
            | Self::Encode { .. }
            | Self::EncodeBody { .. }
            | Self::UnexpectedEnvelope
            | Self::ResponseIdMismatch { .. }
            | Self::UnsolicitedResponse { .. }
            | Self::EventOverflow
            | Self::RequestIdExhausted => false,
            #[cfg(windows)]
            Self::Open { .. } | Self::Wait { .. } => false,
        }
    }
}

/// A validated response, progress update, or event in wire order.
#[derive(Debug)]
pub enum IncomingFrame {
    Response(Response),
    Progress(Progress),
    Event(Event),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionState {
    Ready,
    Discard,
}

/// Owns cancellation-safe receive buffering and the outstanding request identities.
#[derive(Debug)]
pub struct AsyncCoreClient<T> {
    transport: T,
    state: ConnectionState,
    codec: LengthDelimitedCodec,
    buffer: BytesMut,
    next_id: RequestId,
    pending: BTreeSet<RequestId>,
    events: VecDeque<Event>,
}
/// Retires a sequential call on completion, early return, or future cancellation.
struct PendingRequest<'client, T> {
    client: &'client mut AsyncCoreClient<T>,
    id: RequestId,
}
impl<T> Drop for PendingRequest<'_, T> {
    fn drop(&mut self) {
        self.client.pending.remove(&self.id);
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncCoreClient<T> {
    /// Wraps a transport; the caller must perform the handshake before other requests.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            state: ConnectionState::Ready,
            codec: frame_codec(),
            buffer: BytesMut::new(),
            next_id: RequestId(1),
            pending: BTreeSet::new(),
            events: VecDeque::new(),
        }
    }
    /// Negotiates protocol and role; returns the core start identity and release version.
    ///
    /// # Errors
    /// Returns a typed rejection, malformed response or transport failure.
    pub async fn handshake(&mut self, role: HelloRole) -> Result<HelloResponse, CallCoreError> {
        let response = self
            .call(&Hello {
                proto: ProtoRange::new(PROTOCOL_VERSION, PROTOCOL_VERSION)
                    .expect("single protocol version"),
                client_version: ClientVersion(env!("CARGO_PKG_VERSION").into()),
                role,
            })
            .await?;
        if response.proto != PROTOCOL_VERSION {
            return Err(CallCoreError::HandshakeVersion {
                received: response.proto,
                core_version: response.core_version,
            });
        }
        Ok(response)
    }
    /// Calls an operation with its statically associated response schema.
    ///
    /// # Errors
    /// Returns send, response, decoding or deadline errors.
    pub async fn call<R: CoreRequest>(
        &mut self,
        request: &R,
    ) -> Result<R::Response, CallCoreError> {
        let body = request.body().map_err(CallCoreError::encode_body)?;
        R::response(self.request(R::OP, &body).await?).map_err(CallCoreError::decode_body)
    }
    /// Sends one request without waiting; `receive` correlates its eventual response.
    ///
    /// # Errors
    /// Returns encoding, write or deadline failures. Writing and flushing share one deadline.
    /// Cancelling during either step leaves the client unusable; subsequent sends and receives
    /// return [`CallCoreError::DiscardConnection`]. Drop the client and reconnect.
    pub async fn send_request(
        &mut self,
        op: Op,
        body: JsonValue,
    ) -> Result<RequestId, CallCoreError> {
        self.ensure_connected()?;
        let id = self.next_id;
        self.next_id = RequestId(
            id.0.checked_add(1)
                .ok_or(CallCoreError::RequestIdExhausted)?,
        );
        let payload = encode_json(&Envelope::Request(Request { id, op, body }))
            .map_err(CallCoreError::encode)?;
        let mut wire = BytesMut::new();
        self.codec
            .encode(payload.as_slice(), &mut wire)
            .context(call_core_error::FrameSnafu)?;
        // Keep the connection unusable until the entire frame has been flushed, including
        // when the send future is dropped before it can report an error.
        self.state = ConnectionState::Discard;
        tokio::time::timeout(RESPONSE_TIMEOUT, async {
            self.transport.write_all(&wire).await?;
            self.transport.flush().await
        })
        .await
        .context(call_core_error::WriteTimeoutSnafu)?
        .context(call_core_error::IoSnafu)?;
        self.state = ConnectionState::Ready;
        self.pending.insert(id);
        Ok(id)
    }
    /// Receives one frame without an idle timeout; cancellation preserves partial frames.
    ///
    /// # Errors
    /// Rejects unknown response identities, malformed frames and disconnected transports.
    pub async fn receive(&mut self) -> Result<IncomingFrame, CallCoreError> {
        self.ensure_connected()?;
        if let Some(event) = self.events.pop_front() {
            return Ok(IncomingFrame::Event(event));
        }
        self.read_incoming().await
    }
    /// Retires an expired request; allocated identities are never reused (04 §3).
    pub fn retire_request(&mut self, id: RequestId) {
        self.pending.remove(&id);
    }

    fn ensure_connected(&self) -> Result<(), CallCoreError> {
        match self.state {
            ConnectionState::Ready => Ok(()),
            ConnectionState::Discard => Err(CallCoreError::DiscardConnection),
        }
    }
    async fn read_incoming(&mut self) -> Result<IncomingFrame, CallCoreError> {
        self.ensure_connected()?;
        loop {
            match self.read_envelope_unbounded().await? {
                Envelope::Event(event) => return Ok(IncomingFrame::Event(event)),
                Envelope::Progress(progress) => {
                    if self.pending.contains(&progress.id) {
                        return Ok(IncomingFrame::Progress(progress));
                    }
                    if progress.id.0 > 0 && progress.id < self.next_id {
                        tracing::debug!(id = ?progress.id, "discarded late core progress");
                        continue;
                    }
                    let expected = self.pending.first().copied().ok_or(
                        CallCoreError::UnsolicitedResponse {
                            received: Some(progress.id),
                        },
                    )?;
                    return Err(CallCoreError::ResponseIdMismatch {
                        expected,
                        received: Some(progress.id),
                    });
                }
                Envelope::Response(response) => {
                    let Some(id) = response.id else {
                        self.state = ConnectionState::Discard;
                        return match response.outcome {
                            ResponseOutcome::Err { error } => {
                                Err(CallCoreError::ConnectionProtocol { kind: error.kind })
                            }
                            ResponseOutcome::Ok { .. } => {
                                Err(CallCoreError::UnsolicitedResponse { received: None })
                            }
                        };
                    };
                    if self.pending.remove(&id) {
                        return Ok(IncomingFrame::Response(response));
                    }
                    if id.0 > 0 && id < self.next_id {
                        // The allocation range and pending set identify retired IDs without an unbounded tombstone list.
                        tracing::debug!(?id, "discarded late or duplicate core response");
                        continue;
                    }
                    let expected = self
                        .pending
                        .first()
                        .copied()
                        .ok_or(CallCoreError::UnsolicitedResponse { received: Some(id) })?;
                    return Err(CallCoreError::ResponseIdMismatch {
                        expected,
                        received: Some(id),
                    });
                }
                Envelope::Request(_) => return Err(CallCoreError::UnexpectedEnvelope),
            }
        }
    }
    /// Executes a sequential raw operation, buffering interleaved events in wire order.
    ///
    /// # Errors
    /// Returns correlation, protocol, deadline or bounded-backlog errors.
    /// Cancelling after the send completes retires the request; late responses are discarded.
    /// Cancelling during the send requires discarding the connection (see [`Self::send_request`]).
    pub async fn request(&mut self, op: Op, body: &JsonValue) -> Result<JsonValue, CallCoreError> {
        let id = self.send_request(op, body.clone()).await?;
        let pending = PendingRequest { client: self, id };
        let mut deadline = tokio::time::Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let frame = tokio::time::timeout_at(deadline, pending.client.read_incoming())
                .await
                .context(call_core_error::ResponseTimeoutSnafu)??;
            match frame {
                IncomingFrame::Event(event) => {
                    if pending.client.events.len() == EVENT_CAPACITY {
                        return Err(CallCoreError::EventOverflow);
                    }
                    pending.client.events.push_back(event);
                }
                IncomingFrame::Progress(progress) => {
                    if progress.id != id {
                        return Err(CallCoreError::ResponseIdMismatch {
                            expected: id,
                            received: Some(progress.id),
                        });
                    }
                    deadline = tokio::time::Instant::now() + RESPONSE_TIMEOUT;
                }
                IncomingFrame::Response(response) => {
                    if response.id != Some(id) {
                        return Err(CallCoreError::ResponseIdMismatch {
                            expected: id,
                            received: response.id,
                        });
                    }
                    return Self::response_body(response);
                }
            }
        }
    }
    /// Extracts a successful body while preserving structured protocol failures.
    pub fn response_body(response: Response) -> Result<JsonValue, CallCoreError> {
        match response.outcome {
            ResponseOutcome::Ok { body } => Ok(body),
            ResponseOutcome::Err { error } => Err(CallCoreError::Protocol { kind: error.kind }),
        }
    }
    async fn read_envelope_unbounded(&mut self) -> Result<Envelope, CallCoreError> {
        loop {
            if let Some(payload) = self.codec.decode(&mut self.buffer).map_err(|source| {
                self.state = ConnectionState::Discard;
                CallCoreError::Frame { source }
            })? {
                return decode_json(&payload).map_err(CallCoreError::decode);
            }
            let read = self
                .transport
                .read_buf(&mut self.buffer)
                .await
                .map_err(|source| {
                    self.state = ConnectionState::Discard;
                    CallCoreError::Io { source }
                })?;
            if read == 0 {
                self.state = ConnectionState::Discard;
                return Err(CallCoreError::Io {
                    source: io::ErrorKind::UnexpectedEof.into(),
                });
            }
        }
    }
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) async fn read_envelope(&mut self) -> Result<Envelope, CallCoreError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Envelope::Event(event));
        }
        tokio::time::timeout(RESPONSE_TIMEOUT, self.read_envelope_unbounded())
            .await
            .context(call_core_error::ResponseTimeoutSnafu)?
    }
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) async fn write_test_bytes(&mut self, bytes: &[u8]) -> Result<(), CallCoreError> {
        tokio::time::timeout(RESPONSE_TIMEOUT, self.transport.write_all(bytes))
            .await
            .context(call_core_error::ResponseTimeoutSnafu)?
            .context(call_core_error::IoSnafu)
    }
    /// Requests the current core status.
    pub async fn status(&mut self) -> Result<CoreStatusResponse, CallCoreError> {
        self.call(&CoreStatusRequest {}).await
    }
    /// Requests a page of stored facts.
    pub async fn facts(
        &mut self,
        request: &quartz_protocol::QueryFactsRequest,
    ) -> Result<quartz_protocol::QueryFactsResponse, CallCoreError> {
        self.call(request).await
    }
    /// Asks the core to open or activate the UI.
    pub async fn open_ui(&mut self) -> Result<AcceptedBody, CallCoreError> {
        self.call(&UiOpenRequest {}).await
    }
}

/// CLI facade owning a client and a caller-thread runtime; no background threads.
#[derive(Debug)]
pub struct CoreClient<T> {
    client: AsyncCoreClient<T>,
    runtime: Runtime,
}
impl<T: AsyncRead + AsyncWrite + Unpin> CoreClient<T> {
    fn runtime() -> Result<Runtime, CallCoreError> {
        Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .max_blocking_threads(CLIENT_BLOCKING_THREADS)
            .build()
            .context(call_core_error::RuntimeSnafu)
    }
    /// Opens and optionally handshakes on a minimal current-thread runtime.
    pub fn open(
        connect: impl std::future::Future<Output = Result<T, CallCoreError>>,
        role: Option<HelloRole>,
    ) -> Result<Self, CallCoreError> {
        let runtime = Self::runtime()?;
        let client = runtime.block_on(async {
            let mut client = AsyncCoreClient::new(connect.await?);
            if let Some(role) = role {
                client.handshake(role).await?;
            }
            Ok(client)
        })?;
        Ok(Self { client, runtime })
    }
    /// Executes a request with its associated response schema on the caller's runtime.
    ///
    /// # Errors
    /// Returns request encoding, transport, deadline, protocol, or response decoding failures.
    pub fn call<R: CoreRequest>(&mut self, request: &R) -> Result<R::Response, CallCoreError> {
        self.runtime.block_on(self.client.call(request))
    }
    /// Requests current core status.
    pub fn status(&mut self) -> Result<CoreStatusResponse, CallCoreError> {
        self.runtime.block_on(self.client.status())
    }
    /// Requests one facts page.
    pub fn facts(
        &mut self,
        request: &quartz_protocol::QueryFactsRequest,
    ) -> Result<quartz_protocol::QueryFactsResponse, CallCoreError> {
        self.runtime.block_on(self.client.facts(request))
    }
    /// Requests UI open/activation through the core.
    pub fn open_ui(&mut self) -> Result<AcceptedBody, CallCoreError> {
        self.runtime.block_on(self.client.open_ui())
    }
    /// Sends a raw operation in isolated protocol tests.
    #[cfg(any(test, feature = "test-util"))]
    pub fn request(&mut self, op: Op, body: &JsonValue) -> Result<JsonValue, CallCoreError> {
        self.runtime.block_on(self.client.request(op, body))
    }
    /// Reads an uncorrelated frame in isolated server tests.
    #[cfg(any(test, feature = "test-util"))]
    pub fn read_envelope(&mut self) -> Result<Envelope, CallCoreError> {
        self.runtime.block_on(self.client.read_envelope())
    }
    /// Writes raw framing test bytes to an isolated server.
    #[cfg(any(test, feature = "test-util"))]
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CallCoreError> {
        self.runtime.block_on(self.client.write_test_bytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::{DecodeJsonError, EncodeFrameError};
    struct ConnectedClient {
        client: CoreClient<tokio::io::DuplexStream>,
        _peer: tokio::io::DuplexStream,
    }
    impl ConnectedClient {
        fn new() -> Self {
            let runtime = CoreClient::<tokio::io::DuplexStream>::runtime().expect("test runtime");
            const TEST_BUFFER_BYTES: usize = 4096;
            let (stream, peer) = tokio::io::duplex(TEST_BUFFER_BYTES);
            let mut client = AsyncCoreClient::new(stream);
            client.next_id = RequestId(2);
            Self {
                client: CoreClient { client, runtime },
                _peer: peer,
            }
        }
    }
    fn inject(fixture: &mut ConnectedClient, payload: &[u8]) {
        use tokio_util::{bytes::BytesMut, codec::Encoder};
        let mut wire = BytesMut::new();
        frame_codec()
            .encode(payload, &mut wire)
            .expect("test frame");
        use tokio::io::AsyncWriteExt;
        fixture
            .client
            .runtime
            .block_on(fixture._peer.write_all(&wire))
            .expect("inject framed response");
    }
    #[test]
    fn malformed_response_keeps_the_decoder_source() {
        use std::error::Error;
        let mut fixture = ConnectedClient::new();
        inject(&mut fixture, b"{garbage");
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
            &mut fixture,
            &encode_json(&serde_json::json!({"t":"res","id":id,"ok":true,"body":null}))
                .expect("response"),
        );
        let error = fixture.client.status().expect_err("invalid body");
        assert!(matches!(error, CallCoreError::DecodeBody { .. }));
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
            &mut fixture,
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
            .request(
                Op::UiOpen,
                &serde_json::json!("x".repeat(quartz_protocol::MAX_FRAME_LEN)),
            )
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
    #[tokio::test]
    async fn events_remain_ordered_while_responses_are_correlated_by_identity() {
        let (stream, mut peer) = tokio::io::duplex(4096);
        let mut client = AsyncCoreClient::new(stream);
        let first = client
            .send_request(Op::CoreStatus, serde_json::json!({}))
            .await
            .expect("first request");
        let second = client
            .send_request(Op::UiOpen, serde_json::json!({}))
            .await
            .expect("second request");
        let frames = [
            serde_json::json!({"t":"evt","topic":"control","seq":1,"body":{"op":"ui.show"}}),
            serde_json::json!({"t":"res","id":second,"ok":true,"body":{"accepted":true}}),
            serde_json::json!({"t":"evt","topic":"control","seq":2,"body":{"op":"ui.activate"}}),
            serde_json::json!({"t":"res","id":first,"ok":true,"body":{}}),
        ];
        let mut wire = BytesMut::new();
        for frame in frames {
            client
                .codec
                .encode(encode_json(&frame).expect("JSON").as_slice(), &mut wire)
                .expect("frame");
        }
        peer.write_all(&wire)
            .await
            .expect("interleaved wire frames");
        let IncomingFrame::Event(event) = client.receive().await.expect("first event") else {
            panic!("event");
        };
        assert_eq!(event.seq, quartz_protocol::EventSeq(1));
        let IncomingFrame::Response(response) = client
            .receive()
            .await
            .expect("second response arrives first")
        else {
            panic!("response");
        };
        assert_eq!(response.id, Some(second));
        let IncomingFrame::Event(event) = client.receive().await.expect("second event") else {
            panic!("event");
        };
        assert_eq!(event.seq, quartz_protocol::EventSeq(2));
        let IncomingFrame::Response(response) =
            client.receive().await.expect("first response arrives last")
        else {
            panic!("response");
        };
        assert_eq!(response.id, Some(first));
        assert!(client.pending.is_empty());
    }

    #[tokio::test]
    async fn sequential_calls_keep_events_before_the_response() {
        let (stream, mut peer) = tokio::io::duplex(4096);
        let mut client = AsyncCoreClient::new(stream);
        let mut wire = BytesMut::new();
        for value in [
            serde_json::json!({"t":"evt","topic":"control","seq":1,"body":{"op":"ui.show"}}),
            serde_json::json!({"t":"evt","topic":"control","seq":2,"body":{"op":"ui.activate"}}),
            serde_json::json!({"t":"res","id":1,"ok":true,"body":{"accepted":true}}),
        ] {
            client
                .codec
                .encode(encode_json(&value).expect("JSON").as_slice(), &mut wire)
                .expect("frame");
        }
        peer.write_all(&wire).await.expect("wire");
        assert_eq!(client.open_ui().await.expect("response"), AcceptedBody);
        for expected in [1, 2] {
            let IncomingFrame::Event(event) = client.receive().await.expect("buffered event")
            else {
                panic!("event");
            };
            assert_eq!(event.seq.0, expected);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_receive_preserves_a_partial_frame() {
        let (stream, mut peer) = tokio::io::duplex(4096);
        let mut client = AsyncCoreClient::new(stream);
        let event =
            serde_json::json!({"t":"evt","topic":"control","seq":1,"body":{"op":"ui.show"}});
        let mut wire = BytesMut::new();
        client
            .codec
            .encode(encode_json(&event).expect("JSON").as_slice(), &mut wire)
            .expect("frame");
        let header_prefix = wire.split_to(2);
        peer.write_all(&header_prefix).await.expect("half header");
        assert!(
            tokio::time::timeout(Duration::from_millis(1), client.receive())
                .await
                .is_err()
        );
        peer.write_all(&wire).await.expect("remaining frame");
        assert!(matches!(
            client.receive().await,
            Ok(IncomingFrame::Event(_))
        ));
    }
    struct WireFrames;
    impl WireFrames {
        fn encode(values: impl IntoIterator<Item = serde_json::Value>) -> BytesMut {
            let mut wire = BytesMut::new();
            for value in values {
                frame_codec()
                    .encode(encode_json(&value).expect("JSON").as_slice(), &mut wire)
                    .expect("frame");
            }
            wire
        }
        fn response(id: u64) -> serde_json::Value {
            serde_json::json!({"t":"res","id":id,"ok":true,"body":{"accepted":true}})
        }
        fn progress(id: u64, done: u32, total: u32) -> serde_json::Value {
            serde_json::json!({"t":"prg","id":id,"done":done,"total":total})
        }
    }

    #[tokio::test]
    async fn cancelled_sequential_request_retires_id_and_discards_late_response() {
        const BUFFER_BYTES: usize = 4096;
        let (stream, mut peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(stream);
        let body = serde_json::json!({});
        {
            let request = client.request(Op::UiOpen, &body);
            tokio::pin!(request);
            let mut sent = [0; BUFFER_BYTES];
            tokio::select! {
                result = &mut request => panic!("request completed before its response: {result:?}"),
                result = peer.read(&mut sent) => assert!(result.expect("sent frame") > 0),
            }
        }
        peer.write_all(&WireFrames::encode([
            WireFrames::response(1),
            WireFrames::response(2),
        ]))
        .await
        .expect("late and current responses");
        assert_eq!(
            client
                .open_ui()
                .await
                .expect("next call ignores late response"),
            AcceptedBody
        );
        assert!(client.pending.is_empty());
    }

    #[tokio::test]
    async fn event_overflow_retires_request_before_the_next_sequential_call() {
        const BUFFER_BYTES: usize = 16384;
        let (stream, mut peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(stream);
        let events = (0..=EVENT_CAPACITY).map(|seq| {
            serde_json::json!({
                "t":"evt","topic":"control","seq":seq,"body":{"op":"ui.show"}
            })
        });
        peer.write_all(&WireFrames::encode(events))
            .await
            .expect("events");
        assert!(matches!(
            client.open_ui().await,
            Err(CallCoreError::EventOverflow)
        ));
        peer.write_all(&WireFrames::encode([
            WireFrames::response(1),
            WireFrames::response(2),
        ]))
        .await
        .expect("late and current responses");
        assert_eq!(client.open_ui().await.expect("next response"), AcceptedBody);
        assert!(client.pending.is_empty());
    }

    #[tokio::test]
    async fn null_id_errors_preserve_the_code_and_end_the_connection() {
        for kind in [
            ErrorKind::BadRequest,
            ErrorKind::FrameTooLarge,
            ErrorKind::Unknown,
        ] {
            const BUFFER_BYTES: usize = 4096;
            let (stream, mut peer) = tokio::io::duplex(BUFFER_BYTES);
            let mut client = AsyncCoreClient::new(stream);
            let code = if kind == ErrorKind::Unknown {
                "future_error"
            } else {
                kind.code()
            };
            peer.write_all(&WireFrames::encode([serde_json::json!({
                "t":"res","id":null,"ok":false,"err":{"code":code}
            })]))
            .await
            .expect("connection error");
            let error = client.receive().await.expect_err("connection ended");
            assert!(error.ends_connection(), "{error:?}");
            assert!(
                matches!(error, CallCoreError::ConnectionProtocol { kind: received } if received == kind)
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn buffered_transport_flushes_the_request_before_waiting_for_a_response() {
        const BUFFER_BYTES: usize = 4096;
        let (stream, peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(tokio::io::BufStream::new(stream));
        let server = tokio::spawn(async move {
            let mut peer = AsyncCoreClient::new(peer);
            let envelope = peer.read_envelope().await.expect("flushed request");
            assert!(matches!(envelope, Envelope::Request(_)));
            peer.transport
                .write_all(&WireFrames::encode([WireFrames::response(1)]))
                .await
                .expect("response");
        });
        assert_eq!(
            client.open_ui().await.expect("buffered response"),
            AcceptedBody
        );
        server.await.expect("server task");
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_partial_send_prevents_further_transport_use() {
        const BUFFER_BYTES: usize = 1;
        let (stream, mut peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(stream);
        {
            let send = client.send_request(Op::UiOpen, serde_json::json!({}));
            tokio::pin!(send);
            let mut prefix = [0; BUFFER_BYTES];
            tokio::select! {
                result = &mut send => panic!("partial frame unexpectedly completed: {result:?}"),
                result = peer.read_exact(&mut prefix) => { result.expect("partial frame"); },
            }
        }
        let error = client
            .send_request(Op::UiOpen, serde_json::json!({}))
            .await
            .expect_err("cancelled send poisons client");
        assert!(matches!(error, CallCoreError::DiscardConnection));
        assert!(error.ends_connection());
        assert!(
            tokio::time::timeout(RESPONSE_TIMEOUT, client.receive())
                .await
                .expect("receive rejects immediately")
                .expect_err("receive also rejects disposed connection")
                .ends_connection()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn flush_shares_the_send_deadline_and_failure_poisoning() {
        const BUFFER_BYTES: usize = 1;
        let (stream, _peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(tokio::io::BufStream::new(stream));
        let started = tokio::time::Instant::now();
        assert!(matches!(
            client.send_request(Op::UiOpen, serde_json::json!({})).await,
            Err(CallCoreError::WriteTimeout { .. })
        ));
        assert_eq!(started.elapsed(), RESPONSE_TIMEOUT);
        assert!(
            tokio::time::timeout(RESPONSE_TIMEOUT, client.receive())
                .await
                .expect("receive rejects immediately")
                .expect_err("failed send poisons client")
                .ends_connection()
        );
    }
    struct FailRead;
    impl AsyncRead for FailRead {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            _buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
        }
    }
    impl AsyncWrite for FailRead {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            std::task::Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    impl WireFrames {
        async fn assert_discarded<T: AsyncRead + AsyncWrite + Unpin>(
            client: &mut AsyncCoreClient<T>,
        ) {
            assert!(matches!(
                client.send_request(Op::UiOpen, serde_json::json!({})).await,
                Err(CallCoreError::DiscardConnection)
            ));
            assert!(matches!(
                client.receive().await,
                Err(CallCoreError::DiscardConnection)
            ));
        }
    }
    #[tokio::test]
    async fn read_io_error_poisons_subsequent_send_and_receive() {
        let mut client = AsyncCoreClient::new(FailRead);
        assert!(matches!(
            client.receive().await,
            Err(CallCoreError::Io { .. })
        ));
        WireFrames::assert_discarded(&mut client).await;
    }
    #[tokio::test]
    async fn read_eof_poisons_subsequent_send_and_receive() {
        const BUFFER_BYTES: usize = 4096;
        let (stream, peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(stream);
        drop(peer);
        assert!(matches!(
            client.receive().await,
            Err(CallCoreError::Io { .. })
        ));
        WireFrames::assert_discarded(&mut client).await;
    }
    #[tokio::test]
    async fn oversized_frame_poisons_subsequent_send_and_receive() {
        const BUFFER_BYTES: usize = 4096;
        let (stream, mut peer) = tokio::io::duplex(BUFFER_BYTES);
        peer.write_all(
            &u32::try_from(quartz_protocol::MAX_FRAME_LEN + 1)
                .expect("frame length")
                .to_le_bytes(),
        )
        .await
        .expect("oversized frame header");
        let mut client = AsyncCoreClient::new(stream);
        assert!(matches!(
            client.receive().await,
            Err(CallCoreError::Frame { .. })
        ));
        WireFrames::assert_discarded(&mut client).await;
    }
    #[tokio::test(start_paused = true)]
    async fn response_timeout_retires_id_and_preserves_connection_for_the_next_call() {
        const BUFFER_BYTES: usize = 4096;
        let (stream, mut peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(stream);
        let started = tokio::time::Instant::now();
        assert!(matches!(
            client.open_ui().await,
            Err(CallCoreError::ResponseTimeout { .. })
        ));
        assert_eq!(started.elapsed(), RESPONSE_TIMEOUT);
        assert!(client.pending.is_empty());
        let mut buffer = [0; BUFFER_BYTES];
        assert!(peer.read(&mut buffer).await.expect("fully sent request") > 0);
        peer.write_all(&WireFrames::encode([
            WireFrames::response(1),
            WireFrames::response(2),
        ]))
        .await
        .expect("late and next responses");
        assert_eq!(
            client.open_ui().await.expect("next call remains usable"),
            AcceptedBody
        );
        assert!(client.pending.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn progress_extends_response_deadline_and_timeout_resumes_when_progress_stops() {
        const BUFFER_BYTES: usize = 4096;
        const PROGRESS_STEP: Duration = Duration::from_millis(1_500);

        let (stream, mut peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(stream);
        let server = tokio::spawn(async move {
            let mut buffer = [0; BUFFER_BYTES];
            assert!(peer.read(&mut buffer).await.expect("request") > 0);
            tokio::time::sleep(PROGRESS_STEP).await;
            peer.write_all(&WireFrames::encode([WireFrames::progress(1, 1, 3)]))
                .await
                .expect("first progress");
            tokio::time::sleep(PROGRESS_STEP).await;
            peer.write_all(&WireFrames::encode([WireFrames::progress(1, 2, 3)]))
                .await
                .expect("second progress");
            tokio::time::sleep(PROGRESS_STEP).await;
            peer.write_all(&WireFrames::encode([WireFrames::response(1)]))
                .await
                .expect("final response");
        });
        let started = tokio::time::Instant::now();
        assert_eq!(
            client
                .open_ui()
                .await
                .expect("request succeeds past single timeout when progress arrives"),
            AcceptedBody
        );
        assert_eq!(started.elapsed(), PROGRESS_STEP * 3);
        assert!(client.pending.is_empty());
        server.await.expect("server task");

        let (stream, mut peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(stream);
        let server = tokio::spawn(async move {
            let mut buffer = [0; BUFFER_BYTES];
            assert!(peer.read(&mut buffer).await.expect("request") > 0);
            tokio::time::sleep(PROGRESS_STEP).await;
            peer.write_all(&WireFrames::encode([WireFrames::progress(1, 1, 3)]))
                .await
                .expect("single progress");
            std::future::pending::<()>().await;
        });
        let started = tokio::time::Instant::now();
        assert!(matches!(
            client.open_ui().await,
            Err(CallCoreError::ResponseTimeout { .. })
        ));
        assert_eq!(started.elapsed(), PROGRESS_STEP + RESPONSE_TIMEOUT);
        assert!(client.pending.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn receive_surfaces_pending_progress_and_rejects_unknown_progress_id() {
        const BUFFER_BYTES: usize = 4096;
        let (stream, mut peer) = tokio::io::duplex(BUFFER_BYTES);
        let mut client = AsyncCoreClient::new(stream);
        let first = client
            .send_request(Op::UiOpen, serde_json::json!({}))
            .await
            .expect("first request");
        peer.write_all(&WireFrames::encode([
            WireFrames::progress(first.0, 2, 5),
            WireFrames::response(first.0),
        ]))
        .await
        .expect("progress then response");
        let IncomingFrame::Progress(progress) = client.receive().await.expect("progress frame")
        else {
            panic!("expected progress frame");
        };
        assert_eq!(
            progress,
            Progress {
                id: first,
                done: 2,
                total: 5,
            }
        );
        assert!(client.pending.contains(&first));
        let IncomingFrame::Response(response) = client.receive().await.expect("response frame")
        else {
            panic!("expected response frame");
        };
        assert_eq!(response.id, Some(first));
        assert!(client.pending.is_empty());

        peer.write_all(&WireFrames::encode([WireFrames::progress(99, 1, 5)]))
            .await
            .expect("unsolicited progress");
        assert!(matches!(
            client.receive().await,
            Err(CallCoreError::UnsolicitedResponse {
                received: Some(RequestId(99))
            })
        ));

        let second = client
            .send_request(Op::UiOpen, serde_json::json!({}))
            .await
            .expect("second request");
        peer.write_all(&WireFrames::encode([WireFrames::progress(99, 1, 5)]))
            .await
            .expect("unknown id progress");
        assert!(matches!(
            client.receive().await,
            Err(CallCoreError::ResponseIdMismatch {
                expected,
                received: Some(RequestId(99)),
            }) if expected == second
        ));
    }

    #[test]
    fn sync_client_ignores_progress_frames_and_returns_final_response() {
        let mut fixture = ConnectedClient::new();
        let id = fixture.client.client.next_id;
        inject(
            &mut fixture,
            &encode_json(&WireFrames::progress(id.0, 1, 4)).expect("progress"),
        );
        inject(
            &mut fixture,
            &encode_json(&WireFrames::response(id.0)).expect("response"),
        );
        assert_eq!(
            fixture.client.open_ui().expect("final response"),
            AcceptedBody
        );
    }
}
