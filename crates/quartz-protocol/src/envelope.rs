//! Request, response, and event wire envelopes (04 §3).

use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;

use crate::{ErrorKind, Op, ProtocolError, Topic};

/// Client-assigned request identity, increasing within one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

/// Core-assigned event sequence within one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventSeq(pub u64);

/// Client request with an operation-specific raw JSON body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Client request identity, also preserved for an unknown operation.
    pub id: RequestId,
    /// Typed operation name; an unrecognized wire name becomes [`Op::Unknown`].
    pub op: Op,
    /// Operation body decoded separately into its typed schema.
    pub body: Value,
}

/// Mutually exclusive success and failure response data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseOutcome {
    /// Successful operation-specific raw JSON body.
    Ok {
        /// Data returned by the requested operation.
        body: Value,
    },
    /// Stable error code and code-specific fields.
    Err {
        /// Structured failure; diagnostic text is for logs only.
        error: ProtocolError,
    },
}

/// Response whose wire outcome is `ok` plus exactly one of `body` or `err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// `None` is reserved for connection errors with no known request ID:
    /// oversized frames or undecodable JSON (`frame_too_large` or `bad_request`).
    /// All successes and request-level errors must use `Some`.
    pub id: Option<RequestId>,
    /// Exactly one success body or structured failure.
    pub outcome: ResponseOutcome,
}

impl Response {
    fn validate_id(&self) -> Result<(), &'static str> {
        if self.id.is_some() {
            return Ok(());
        }
        match &self.outcome {
            ResponseOutcome::Err {
                error:
                    ProtocolError {
                        kind: ErrorKind::BadRequest | ErrorKind::FrameTooLarge | ErrorKind::Unknown,
                        ..
                    },
            } => Ok(()),
            ResponseOutcome::Ok { .. } | ResponseOutcome::Err { .. } => Err(
                "null response id requires a connection-level bad_request or frame_too_large error",
            ),
        }
    }
}

impl Serialize for Response {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate_id().map_err(serde::ser::Error::custom)?;
        let mut map = serializer.serialize_map(Some(RESPONSE_FIELD_COUNT))?;
        map.serialize_entry("id", &self.id)?;
        match &self.outcome {
            ResponseOutcome::Ok { body } => {
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("body", body)?;
            }
            ResponseOutcome::Err { error } => {
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("err", error)?;
            }
        }
        map.end()
    }
}

/// A response contains id, ok, and one outcome field; `t` belongs to the envelope.
const RESPONSE_FIELD_COUNT: usize = 3;

// A wrapper makes a missing id an error while still accepting an explicit null.
#[derive(Deserialize)]
struct ResponseId(Option<RequestId>);

#[derive(Debug, Default)]
enum JsonField {
    #[default]
    Missing,
    Present(Value),
}

impl<'de> Deserialize<'de> for JsonField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Value::deserialize(deserializer).map(Self::Present)
    }
}

#[derive(Deserialize)]
struct ResponseWire {
    id: ResponseId,
    ok: bool,
    #[serde(default)]
    body: JsonField,
    #[serde(default)]
    err: JsonField,
}

impl<'de> Deserialize<'de> for Response {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ResponseWire::deserialize(deserializer)?;
        let outcome = match (wire.ok, wire.body, wire.err) {
            (true, JsonField::Present(body), JsonField::Missing) => ResponseOutcome::Ok { body },
            (false, JsonField::Missing, JsonField::Present(error)) => {
                let error = serde_json::from_value(error).map_err(serde::de::Error::custom)?;
                ResponseOutcome::Err { error }
            }
            _ => {
                return Err(serde::de::Error::custom(
                    "response requires ok=true with body or ok=false with err",
                ));
            }
        };
        let response = Self {
            id: wire.id.0,
            outcome,
        };
        response.validate_id().map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

/// Core push on a typed subscription topic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Subscription or control channel for this event.
    pub topic: Topic,
    /// Event sequence assigned by the core.
    pub seq: EventSeq,
    /// Topic-specific raw JSON; control bodies use [`crate::ControlBody`].
    pub body: Value,
}

/// IPC message discriminator and data; framing is handled by [`crate::Frame`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Envelope {
    /// Client-to-core request.
    #[serde(rename = "req")]
    Request(Request),
    /// Core-to-client response.
    #[serde(rename = "res")]
    Response(Response),
    /// Core-to-client push.
    #[serde(rename = "evt")]
    Event(Event),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlBody, ControlOp, ErrorCode, Frame};

    #[test]
    fn doc_envelope_examples_have_exact_wire_shapes() {
        // 04 §3 uses {...} placeholders; use an empty object for that body.
        let cases = [
            serde_json::json!({"t":"req","id":17,"op":"query.day","body":{}}),
            serde_json::json!({"t":"res","id":17,"ok":true,"body":{}}),
            serde_json::json!({"t":"res","id":17,"ok":false,"err":{"code":"invalid_range","msg":"..."}}),
            serde_json::json!({"t":"evt","topic":"state","seq":42,"body":{}}),
        ];
        for wire in cases {
            let envelope: Envelope = serde_json::from_value(wire.clone()).expect("04 envelope");
            assert_eq!(serde_json::to_value(&envelope).expect("encode"), wire);
            let frame = Frame::from_json(&envelope).expect("serialize frame");
            assert_eq!(
                frame.decode_json::<Envelope>().expect("parse frame"),
                envelope
            );
        }
    }

    #[test]
    fn unknown_fields_are_ignored_and_unknown_op_retains_id() {
        for wire in [
            serde_json::json!({"t":"req","id":17,"op":"future.op","body":{},"future":true}),
            serde_json::json!({"t":"res","id":17,"ok":true,"body":{},"future":true}),
            serde_json::json!({"t":"res","id":17,"ok":false,"err":{"code":"internal","future":true},"future":true}),
            serde_json::json!({"t":"evt","topic":"state","seq":42,"body":{},"future":true}),
        ] {
            let frame = Frame::from_json(&wire).expect("JSON frame");
            let envelope = frame
                .decode_json::<Envelope>()
                .expect("ignore unknown fields");
            if let Envelope::Request(request) = envelope {
                assert_eq!(request.id, RequestId(17));
                assert_eq!(request.op, Op::Unknown);
                assert!(
                    serde_json::to_value(request).is_err(),
                    "unknown requests are receive-only"
                );
            }
        }
    }

    #[test]
    fn null_connection_error_and_future_error_codes() {
        for code in ["frame_too_large", "bad_request"] {
            let wire = serde_json::json!({"t":"res","id":null,"ok":false,"err":{"code":code}});
            let envelope: Envelope =
                serde_json::from_value(wire.clone()).expect("connection error");
            assert_eq!(serde_json::to_value(&envelope).expect("serialize"), wire);
        }
        let unknown: Envelope = serde_json::from_str(
            r#"{"t":"res","id":17,"ok":false,"err":{"code":"future_error","extra":{}}}"#,
        )
        .expect("future error");
        let Envelope::Response(Response {
            outcome: ResponseOutcome::Err { error },
            ..
        }) = unknown
        else {
            panic!("expected error response")
        };
        assert_eq!(error.kind.code(), ErrorCode::Unknown);
        let future_connection_error: Envelope = serde_json::from_str(
            r#"{"t":"res","id":null,"ok":false,"err":{"code":"future_connection_error"}}"#,
        )
        .expect("future connection errors remain decodable");
        let Envelope::Response(Response {
            id: None,
            outcome: ResponseOutcome::Err { error },
        }) = future_connection_error
        else {
            panic!("expected connection-level error");
        };
        assert_eq!(error.kind.code(), ErrorCode::Unknown);
    }

    #[test]
    fn invalid_outcome_combinations_and_missing_ids_are_rejected() {
        for wire in [
            r#"{"t":"res","id":17,"ok":true,"err":{"code":"internal"}}"#,
            r#"{"t":"res","id":17,"ok":false,"body":{}}"#,
            r#"{"t":"res","id":17,"ok":true,"body":{},"err":{"code":"internal"}}"#,
            r#"{"t":"res","id":17,"ok":false,"body":{},"err":{"code":"internal"}}"#,
            r#"{"t":"res","id":null,"ok":true,"body":{}}"#,
            r#"{"t":"res","id":null,"ok":false,"err":{"code":"forbidden"}}"#,
            r#"{"t":"res","ok":false,"err":{"code":"bad_request"}}"#,
        ] {
            assert!(serde_json::from_str::<Envelope>(wire).is_err(), "{wire}");
        }
        let null_body = serde_json::json!({"t":"res","id":17,"ok":true,"body":null});
        let envelope: Envelope =
            serde_json::from_value(null_body.clone()).expect("explicit null body");
        assert_eq!(
            serde_json::to_value(envelope).expect("serialize"),
            null_body
        );
        let invalid = Response {
            id: None,
            outcome: ResponseOutcome::Ok {
                body: serde_json::json!({}),
            },
        };
        assert!(serde_json::to_value(invalid).is_err());
    }

    #[test]
    fn control_operations_are_typed_event_bodies_not_requests() {
        let body: ControlBody = serde_json::from_value(
            serde_json::json!({"op":"browser.ack","epoch":12,"upto_seq":345}),
        )
        .expect("05 control body");
        assert_eq!(body.op, ControlOp::BrowserAck);
        let event = Envelope::Event(Event {
            topic: Topic::Control,
            seq: EventSeq(10),
            body: serde_json::to_value(body).expect("serialize body"),
        });
        assert_eq!(
            serde_json::to_value(event).expect("serialize event"),
            serde_json::json!({"t":"evt","topic":"control","seq":10,"body":{"op":"browser.ack","epoch":12,"upto_seq":345}})
        );
        assert_eq!(
            serde_json::from_str::<Op>("\"ui.show\"").expect("unknown request"),
            Op::Unknown
        );
        assert_eq!(
            serde_json::from_str::<ControlOp>("\"core_offline\"").expect("05 host notification"),
            ControlOp::CoreOffline
        );
    }

    #[test]
    fn deep_envelope_body_is_rejected_without_stack_overflow() {
        let wire = format!(
            "{{\"t\":\"req\",\"id\":1,\"op\":\"hello\",\"body\":{}0{}}}",
            "[".repeat(10_000),
            "]".repeat(10_000)
        );
        assert!(
            Frame::new(wire.into_bytes())
                .decode_json::<Envelope>()
                .is_err()
        );
    }
}
