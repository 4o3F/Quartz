//! Pure host message validation, `browser.hello` parsing, `core_offline` framing, and ID remapping.

#[cfg(any(windows, test))]
use std::collections::BTreeMap;

#[cfg(any(windows, test))]
use quartz_protocol::{BrowserHello, BrowserStateRequest, ClientVersion, Hello, HelloRole, Op};
use quartz_protocol::{
    BrowserHostHello, Envelope, ErrorKind, Event, EventSeq, JsonValue, ProtocolError, RequestId,
    Response, ResponseOutcome, Topic, decode_json, encode_json_with_limit,
};
use serde::{Deserialize, Serialize};
use snafu::Snafu;

use crate::framing::{HOST_MAX_FRAME_LEN, encode_native_frame};

/// Extension-to-host handshake operation name (never forwarded as a core op; 05 §1.2).
pub(crate) const BROWSER_HELLO_OP: &str = "browser.hello";

/// Control event operation emitted to the extension when the core pipe cannot be opened (05 §2.2).
pub(crate) const CORE_OFFLINE_OP: &str = "core_offline";

/// Envelope discriminator for client requests.
const REQUEST_TAG: &str = "req";

/// Maximum concurrent in-flight requests remapped by the host, matching core's limit (04 §2).
#[cfg(any(windows, test))]
pub(crate) const MAX_IN_FLIGHT_REQUESTS: usize = 64;

/// Validated extension `browser.hello` request ready to translate into core `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedBrowserHello {
    /// Extension-assigned request ID for `browser.hello`.
    #[cfg(any(windows, test))]
    pub(crate) id: RequestId,
    /// Validated `browser.hello` body.
    #[cfg(any(windows, test))]
    pub(crate) hello: BrowserHostHello,
}

#[cfg(any(windows, test))]
impl ParsedBrowserHello {
    /// Translates the extension's `browser.hello` into a core `hello` with `role: "browser"`.
    #[must_use]
    pub(crate) fn into_core_hello(self) -> (RequestId, Hello) {
        (
            self.id,
            Hello {
                proto: self.hello.proto,
                client_version: ClientVersion(env!("CARGO_PKG_VERSION").into()),
                role: HelloRole::Browser {
                    browser: BrowserHello {
                        browser: self.hello.browser,
                        profile_instance_id: self.hello.profile_instance_id,
                    },
                },
            },
        )
    }
}

/// Failure parsing the required initial `browser.hello` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub(crate) enum ParseHostHelloError {
    /// Frame exceeds the 256 KiB protocol ceiling.
    #[snafu(display("browser hello frame exceeds {HOST_MAX_FRAME_LEN} bytes"))]
    HelloFrameTooLarge,
    /// Frame is not valid UTF-8 JSON or is not a request envelope.
    #[snafu(display("browser hello frame is not a valid request envelope"))]
    HelloEnvelopeInvalid,
    /// First request used an operation other than `browser.hello`.
    #[snafu(display("first host message must be browser.hello"))]
    ExpectedBrowserHello {
        /// Request ID from the rejected non-hello request.
        id: RequestId,
    },
    /// `browser.hello` body failed schema validation.
    #[snafu(display("browser.hello body is invalid"))]
    HelloBodyInvalid {
        /// Request ID from the malformed hello request.
        id: RequestId,
    },
}

impl ParseHostHelloError {
    /// Builds the stable protocol error response to return to the extension before closing.
    #[must_use]
    pub(crate) fn into_response(self) -> Response {
        match self {
            Self::HelloFrameTooLarge => error_response(None, ErrorKind::FrameTooLarge),
            Self::HelloEnvelopeInvalid => error_response(None, ErrorKind::BadRequest),
            Self::ExpectedBrowserHello { id } => {
                error_response(Some(id), ErrorKind::HandshakeRequired)
            }
            Self::HelloBodyInvalid { id } => error_response(Some(id), ErrorKind::BadRequest),
        }
    }
}

#[derive(Deserialize)]
struct RawFirstRequest {
    t: String,
    id: RequestId,
    op: String,
    body: JsonValue,
}

/// Validates that the first extension message is a well-formed `browser.hello` request.
///
/// # Errors
/// Returns a typed error without retaining private fields when the frame is not `browser.hello`.
pub(crate) fn parse_browser_hello(
    payload: &[u8],
) -> Result<ParsedBrowserHello, ParseHostHelloError> {
    if payload.len() > HOST_MAX_FRAME_LEN {
        return Err(ParseHostHelloError::HelloFrameTooLarge);
    }
    let raw: RawFirstRequest =
        decode_json(payload).map_err(|_| ParseHostHelloError::HelloEnvelopeInvalid)?;
    if raw.t != REQUEST_TAG {
        return Err(ParseHostHelloError::HelloEnvelopeInvalid);
    }
    if raw.op != BROWSER_HELLO_OP {
        return Err(ParseHostHelloError::ExpectedBrowserHello { id: raw.id });
    }
    let hello: BrowserHostHello = serde_json::from_value(raw.body)
        .map_err(|_| ParseHostHelloError::HelloBodyInvalid { id: raw.id })?;
    #[cfg(not(any(windows, test)))]
    let _ = hello;
    Ok(ParsedBrowserHello {
        #[cfg(any(windows, test))]
        id: raw.id,
        #[cfg(any(windows, test))]
        hello,
    })
}

/// Validated `browser.state` request ready to forward to the core pipe.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedForwardRequest {
    /// Original extension request ID.
    pub(crate) ext_id: RequestId,
    /// Core operation (`Op::BrowserState`).
    pub(crate) op: Op,
    /// Validated request body JSON.
    pub(crate) body: JsonValue,
}

/// Failure validating a post-handshake extension frame before forwarding to core.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub(crate) enum ValidateForwardError {
    /// Frame exceeds the 256 KiB protocol ceiling.
    #[snafu(display("forwarded frame exceeds {HOST_MAX_FRAME_LEN} bytes"))]
    FrameTooLarge,
    /// Frame is not valid UTF-8 JSON or is not a request envelope.
    #[snafu(display("forwarded frame is not a valid request envelope"))]
    InvalidEnvelope,
    /// Operation is unrecognized by the protocol.
    #[snafu(display("forwarded operation is unknown"))]
    UnknownOp {
        /// Original extension request ID.
        id: RequestId,
    },
    /// Operation is not permitted for the browser host forwarder.
    #[snafu(display("forwarded operation is forbidden for browser host"))]
    ForbiddenOp {
        /// Original extension request ID.
        id: RequestId,
    },
    /// `browser.state` body failed schema or content validation.
    #[snafu(display("browser.state request body is invalid"))]
    InvalidBody {
        /// Original extension request ID.
        id: RequestId,
    },
}

#[cfg(any(windows, test))]
impl ValidateForwardError {
    /// Whether this error terminates the host connection (connection-level frame/JSON error).
    #[must_use]
    pub(crate) const fn closes_connection(self) -> bool {
        matches!(self, Self::FrameTooLarge | Self::InvalidEnvelope)
    }

    /// Builds the stable protocol error response to send back to the extension.
    #[must_use]
    pub(crate) fn into_response(self) -> Response {
        match self {
            Self::FrameTooLarge => error_response(None, ErrorKind::FrameTooLarge),
            Self::InvalidEnvelope => error_response(None, ErrorKind::BadRequest),
            Self::UnknownOp { id } => error_response(Some(id), ErrorKind::UnknownOp),
            Self::ForbiddenOp { id } => error_response(Some(id), ErrorKind::Forbidden),
            Self::InvalidBody { id } => error_response(Some(id), ErrorKind::BadRequest),
        }
    }
}

/// Validates a post-handshake frame from the extension, allowing only valid `browser.state` requests.
///
/// # Errors
/// Rejects oversized frames, non-request envelopes, non-`browser.state` ops, and invalid state bodies.
#[cfg(any(windows, test))]
pub(crate) fn validate_forward_request(
    payload: &[u8],
) -> Result<ValidatedForwardRequest, ValidateForwardError> {
    if payload.len() > HOST_MAX_FRAME_LEN {
        return Err(ValidateForwardError::FrameTooLarge);
    }
    let Envelope::Request(request) =
        decode_json::<Envelope>(payload).map_err(|_| ValidateForwardError::InvalidEnvelope)?
    else {
        return Err(ValidateForwardError::InvalidEnvelope);
    };
    match request.op {
        Op::BrowserState => {
            let state: BrowserStateRequest = serde_json::from_value(request.body)
                .map_err(|_| ValidateForwardError::InvalidBody { id: request.id })?;
            let body = serde_json::to_value(&state)
                .map_err(|_| ValidateForwardError::InvalidBody { id: request.id })?;
            Ok(ValidatedForwardRequest {
                ext_id: request.id,
                op: Op::BrowserState,
                body,
            })
        }
        Op::Unknown => Err(ValidateForwardError::UnknownOp { id: request.id }),
        _ => Err(ValidateForwardError::ForbiddenOp { id: request.id }),
    }
}

/// Failure inserting a request ID remapping entry.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub(crate) enum RemapIdError {
    /// The host already has the maximum number of in-flight requests.
    #[snafu(display("in-flight request map is full"))]
    Full,
    /// The core request ID is already tracked.
    #[snafu(display("duplicate core request id {core_id:?}"))]
    DuplicateCoreId {
        /// Core request ID that collided.
        core_id: RequestId,
    },
}

/// Tracks in-flight core request IDs and remaps core responses back to extension request IDs.
#[cfg(any(windows, test))]
#[derive(Debug, Default)]
pub(crate) struct RequestIdMap {
    core_to_ext: BTreeMap<RequestId, RequestId>,
}

#[cfg(any(windows, test))]
impl RequestIdMap {
    /// Returns the number of currently in-flight requests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.core_to_ext.len()
    }

    /// Returns whether no requests are currently in-flight.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.core_to_ext.is_empty()
    }

    /// Whether the map has reached [`MAX_IN_FLIGHT_REQUESTS`].
    #[must_use]
    pub(crate) fn is_full(&self) -> bool {
        self.core_to_ext.len() >= MAX_IN_FLIGHT_REQUESTS
    }

    /// Records a mapping from a core-assigned request ID back to the extension's request ID.
    ///
    /// # Errors
    /// Rejects insertion when at capacity or when `core_id` is already present.
    pub(crate) fn insert(
        &mut self,
        core_id: RequestId,
        ext_id: RequestId,
    ) -> Result<(), RemapIdError> {
        if self.is_full() {
            return Err(RemapIdError::Full);
        }
        if self.core_to_ext.contains_key(&core_id) {
            return Err(RemapIdError::DuplicateCoreId { core_id });
        }
        self.core_to_ext.insert(core_id, ext_id);
        Ok(())
    }

    /// Remaps a core response's request ID back to the corresponding extension request ID.
    ///
    /// Connection-level error responses (`id: None`) pass through unchanged. Responses with an
    /// unrecognized core ID return `None`.
    #[must_use]
    pub(crate) fn remap_response(&mut self, response: Response) -> Option<Response> {
        let Some(core_id) = response.id else {
            return Some(response);
        };
        let ext_id = self.core_to_ext.remove(&core_id)?;
        Some(Response {
            id: Some(ext_id),
            outcome: response.outcome,
        })
    }
}

/// Builds a typed protocol error response without diagnostic text.
#[must_use]
pub(crate) fn error_response(id: Option<RequestId>, kind: ErrorKind) -> Response {
    Response {
        id,
        outcome: ResponseOutcome::Err {
            error: ProtocolError::new(kind),
        },
    }
}

/// Encodes a response envelope as UTF-8 JSON within the 256 KiB host ceiling.
#[must_use]
pub(crate) fn encode_response_payload(response: &Response) -> Vec<u8> {
    encode_json_with_limit(&Envelope::Response(response.clone()), HOST_MAX_FRAME_LEN)
        .expect("host error and forwarded responses fit within 256 KiB")
}

#[derive(Serialize)]
struct CoreOfflineBody {
    op: &'static str,
}

/// Encodes the exact `core_offline` control event JSON payload (05 §2.2).
#[must_use]
pub(crate) fn core_offline_payload() -> Vec<u8> {
    let body = serde_json::to_value(CoreOfflineBody {
        op: CORE_OFFLINE_OP,
    })
    .expect("core_offline body serializes");
    let event = Envelope::Event(Event {
        topic: Topic::Control,
        seq: EventSeq(1),
        body,
    });
    encode_json_with_limit(&event, HOST_MAX_FRAME_LEN)
        .expect("core_offline event fits within 256 KiB")
}

/// Encodes the complete length-prefixed `core_offline` Native Messaging frame bytes (05 §2.2).
#[must_use]
pub(crate) fn core_offline_frame() -> Vec<u8> {
    encode_native_frame(&core_offline_payload()).expect("core_offline frame fits Native Messaging")
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::{BrowserKind, PROTOCOL_VERSION, ProtoRange};

    const TEST_UUID: &str = "a286737e-3855-43cb-98ee-c366344372d0";

    #[test]
    fn first_message_must_be_browser_hello_and_translates_to_core_hello() {
        let valid = serde_json::json!({
            "t": "req",
            "id": 7,
            "op": "browser.hello",
            "body": {
                "profile_instance_id": TEST_UUID,
                "browser": "edge",
                "proto": { "min": 1, "max": 1 }
            }
        });
        let payload = serde_json::to_vec(&valid).expect("hello JSON");
        let parsed = parse_browser_hello(&payload).expect("valid browser.hello");
        assert_eq!(parsed.id, RequestId(7));
        assert_eq!(parsed.hello.browser, BrowserKind::Edge);
        assert_eq!(parsed.hello.profile_instance_id.to_string(), TEST_UUID);
        assert_eq!(
            parsed.hello.proto,
            ProtoRange::new(PROTOCOL_VERSION, PROTOCOL_VERSION).expect("proto range")
        );

        let (ext_id, core_hello) = parsed.into_core_hello();
        assert_eq!(ext_id, RequestId(7));
        assert_eq!(
            core_hello.role,
            HelloRole::Browser {
                browser: BrowserHello {
                    browser: BrowserKind::Edge,
                    profile_instance_id: TEST_UUID.parse().expect("uuid"),
                }
            }
        );

        // First message with a different op fails with HandshakeRequired preserving id.
        let wrong_op = serde_json::to_vec(&serde_json::json!({
            "t": "req",
            "id": 3,
            "op": "browser.state",
            "body": { "focused": false, "window": null, "tab": null }
        }))
        .expect("state JSON");
        assert_eq!(
            parse_browser_hello(&wrong_op),
            Err(ParseHostHelloError::ExpectedBrowserHello { id: RequestId(3) })
        );

        // Malformed browser.hello body fails with InvalidBody preserving id.
        let bad_body = serde_json::to_vec(&serde_json::json!({
            "t": "req",
            "id": 4,
            "op": "browser.hello",
            "body": { "profile_instance_id": "bad", "browser": "chrome", "proto": { "min": 1, "max": 1 } }
        }))
        .expect("bad body JSON");
        assert_eq!(
            parse_browser_hello(&bad_body),
            Err(ParseHostHelloError::HelloBodyInvalid { id: RequestId(4) })
        );

        // Non-request or invalid JSON fails with InvalidEnvelope.
        assert_eq!(
            parse_browser_hello(b"{garbage"),
            Err(ParseHostHelloError::HelloEnvelopeInvalid)
        );
        assert_eq!(
            parse_browser_hello(br#"{"t":"evt","id":1,"op":"browser.hello","body":{}}"#),
            Err(ParseHostHelloError::HelloEnvelopeInvalid)
        );
    }

    #[test]
    fn core_offline_frame_has_exact_little_endian_prefix_and_json_bytes() {
        let expected_json =
            br#"{"t":"evt","topic":"control","seq":1,"body":{"op":"core_offline"}}"#;
        assert_eq!(core_offline_payload(), expected_json);

        let frame = core_offline_frame();
        let expected_len = u32::try_from(expected_json.len())
            .expect("json len fits u32")
            .to_le_bytes();
        assert_eq!(&frame[..4], &expected_len);
        assert_eq!(&frame[4..], expected_json);
    }

    #[test]
    fn forward_validation_allows_only_valid_browser_state_requests() {
        let valid_state = serde_json::to_vec(&serde_json::json!({
            "t": "req",
            "id": 2,
            "op": "browser.state",
            "body": {
                "focused": true,
                "window": { "id": 7, "state": "normal" },
                "tab": {
                    "id": 91,
                    "domain": "github.com",
                    "title": "4o3F/quartz",
                    "title_status": "ok",
                    "audible": false
                }
            }
        }))
        .expect("state JSON");
        let forwarded = validate_forward_request(&valid_state).expect("valid browser.state");
        assert_eq!(forwarded.ext_id, RequestId(2));
        assert_eq!(forwarded.op, Op::BrowserState);

        let forbidden_ops = [
            ("core.status", serde_json::json!({})),
            ("query.day", serde_json::json!({"local_date": "2026-11-27"})),
            ("hello", serde_json::json!({})),
            ("settings.get", serde_json::json!({})),
        ];
        for (op, body) in forbidden_ops {
            let wire = serde_json::to_vec(&serde_json::json!({
                "t": "req",
                "id": 9,
                "op": op,
                "body": body
            }))
            .expect("forbidden op JSON");
            assert_eq!(
                validate_forward_request(&wire),
                Err(ValidateForwardError::ForbiddenOp { id: RequestId(9) }),
                "{op} must be forbidden"
            );
        }

        let unknown = serde_json::to_vec(&serde_json::json!({
            "t": "req",
            "id": 10,
            "op": "browser.unknown",
            "body": {}
        }))
        .expect("unknown op JSON");
        assert_eq!(
            validate_forward_request(&unknown),
            Err(ValidateForwardError::UnknownOp { id: RequestId(10) })
        );

        let bad_domain = serde_json::to_vec(&serde_json::json!({
            "t": "req",
            "id": 11,
            "op": "browser.state",
            "body": {
                "focused": true,
                "window": null,
                "tab": {
                    "id": 1,
                    "domain": "https://github.com/secret",
                    "title": "ok",
                    "title_status": "ok",
                    "audible": false
                }
            }
        }))
        .expect("bad domain JSON");
        assert_eq!(
            validate_forward_request(&bad_domain),
            Err(ValidateForwardError::InvalidBody { id: RequestId(11) })
        );

        assert_eq!(
            validate_forward_request(&vec![b' '; HOST_MAX_FRAME_LEN + 1]),
            Err(ValidateForwardError::FrameTooLarge)
        );
    }

    #[test]
    fn request_id_map_remaps_out_of_order_responses_and_enforces_capacity() {
        let mut map = RequestIdMap::default();
        assert!(map.is_empty());

        map.insert(RequestId(2), RequestId(100))
            .expect("first remap");
        map.insert(RequestId(3), RequestId(200))
            .expect("second remap");
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.insert(RequestId(2), RequestId(300)),
            Err(RemapIdError::DuplicateCoreId {
                core_id: RequestId(2),
            })
        );

        // Out-of-order response for core ID 3 maps back to extension ID 200.
        let second = map
            .remap_response(Response {
                id: Some(RequestId(3)),
                outcome: ResponseOutcome::Ok {
                    body: serde_json::json!({"accepted": true}),
                },
            })
            .expect("remapped second");
        assert_eq!(second.id, Some(RequestId(200)));

        // Unknown core ID is dropped.
        assert!(
            map.remap_response(Response {
                id: Some(RequestId(999)),
                outcome: ResponseOutcome::Ok {
                    body: serde_json::json!({}),
                },
            })
            .is_none()
        );

        // Connection-level null-id error passes through unchanged.
        let conn_err = map
            .remap_response(error_response(None, ErrorKind::FrameTooLarge))
            .expect("connection error passes through");
        assert_eq!(conn_err.id, None);

        // First response maps back to extension ID 100.
        let first = map
            .remap_response(error_response(Some(RequestId(2)), ErrorKind::Busy))
            .expect("remapped first");
        assert_eq!(first.id, Some(RequestId(100)));
        assert!(map.is_empty());

        // Enforce MAX_IN_FLIGHT_REQUESTS capacity.
        for index in 0..MAX_IN_FLIGHT_REQUESTS {
            let id = u64::try_from(index + 1).expect("index fits u64");
            map.insert(RequestId(id), RequestId(id + 1000))
                .expect("within capacity");
        }
        assert!(map.is_full());
        assert_eq!(
            map.insert(RequestId(999), RequestId(1999)),
            Err(RemapIdError::Full)
        );
    }
}
