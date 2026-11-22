//! Protocol framing and correlated calls, independent of the pipe platform.

use quartz_protocol::{
    AcceptedBody, ClientVersion, CoreRequest, CoreStatusRequest, CoreStatusResponse, Envelope,
    ErrorKind, Event, Hello, HelloResponse, HelloRole, JsonValue, Op, PROTOCOL_VERSION, ProtoRange,
    ProtoVersion, Request, RequestId, Response, ResponseOutcome, UiOpenRequest, decode_json,
    encode_json, frame_codec,
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
    #[snafu(display("expected a core response or event envelope"))]
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
            Self::Io { .. } | Self::Frame { .. } | Self::WriteTimeout { .. } => true,
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

/// A validated response identity or an event in wire order.
#[derive(Debug)]
pub enum IncomingFrame {
    Response(Response),
    Event(Event),
}

/// Owns cancellation-safe receive buffering and the outstanding request identities.
#[derive(Debug)]
pub struct AsyncCoreClient<T> {
    transport: T,
    codec: LengthDelimitedCodec,
    buffer: BytesMut,
    next_id: RequestId,
    pending: BTreeSet<RequestId>,
    events: VecDeque<Event>,
}
impl<T: AsyncRead + AsyncWrite + Unpin> AsyncCoreClient<T> {
    /// Wraps a transport; the caller must perform the handshake before other requests.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
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
    /// Returns encoding, write or deadline failures; discard the connection on write failure.
    pub async fn send_request(
        &mut self,
        op: Op,
        body: JsonValue,
    ) -> Result<RequestId, CallCoreError> {
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
        tokio::time::timeout(RESPONSE_TIMEOUT, self.transport.write_all(&wire))
            .await
            .context(call_core_error::WriteTimeoutSnafu)?
            .context(call_core_error::IoSnafu)?;
        self.pending.insert(id);
        Ok(id)
    }
    /// Receives one frame without an idle timeout; cancellation preserves partial frames.
    ///
    /// # Errors
    /// Rejects unknown response identities, malformed frames and disconnected transports.
    pub async fn receive(&mut self) -> Result<IncomingFrame, CallCoreError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(IncomingFrame::Event(event));
        }
        self.read_incoming().await
    }
    /// Retires an expired request; allocated identities are never reused (04 §3).
    pub fn retire_request(&mut self, id: RequestId) {
        self.pending.remove(&id);
    }

    async fn read_incoming(&mut self) -> Result<IncomingFrame, CallCoreError> {
        loop {
            match self.read_envelope_unbounded().await? {
                Envelope::Event(event) => return Ok(IncomingFrame::Event(event)),
                Envelope::Response(response) => {
                    let Some(id) = response.id else {
                        return Err(CallCoreError::UnsolicitedResponse { received: None });
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
    pub async fn request(&mut self, op: Op, body: &JsonValue) -> Result<JsonValue, CallCoreError> {
        let id = self.send_request(op, body.clone()).await?;
        let result = tokio::time::timeout(RESPONSE_TIMEOUT, async {
            loop {
                match self.read_incoming().await? {
                    IncomingFrame::Event(event) => {
                        if self.events.len() == EVENT_CAPACITY {
                            return Err(CallCoreError::EventOverflow);
                        }
                        self.events.push_back(event);
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
        })
        .await;
        if result.is_err() {
            self.retire_request(id);
        }
        result.context(call_core_error::ResponseTimeoutSnafu)?
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
            if let Some(payload) = self
                .codec
                .decode(&mut self.buffer)
                .context(call_core_error::FrameSnafu)?
            {
                return decode_json(&payload).map_err(CallCoreError::decode);
            }
            let read = self
                .transport
                .read_buf(&mut self.buffer)
                .await
                .context(call_core_error::IoSnafu)?;
            if read == 0 {
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
}
