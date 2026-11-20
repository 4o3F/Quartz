//! Stable IPC error codes and code-specific structured fields (04 §3).

use std::{convert::Infallible, fmt, str::FromStr};

use serde::{Deserialize, Serialize, Serializer};

use crate::{NegotiateProtoError, ProtoRange};

/// Stable IPC failure codes; distinct from startup [`crate::CoreErrorCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The envelope or operation body does not match its schema.
    BadRequest,
    /// The request operation is not recognized.
    UnknownOp,
    /// The operation is defined but not implemented by this core.
    NotImplemented,
    /// The first request must be hello.
    HandshakeRequired,
    /// The declared frame payload exceeds the byte limit.
    FrameTooLarge,
    /// The client and core protocol ranges do not overlap.
    ProtoMismatch,
    /// The declared role may not perform this operation.
    Forbidden,
    /// The requested date or time range is invalid.
    InvalidRange,
    /// Query inputs changed; pagination must restart.
    StaleCursor,
    /// The submitted base version differs from the current rule-set version.
    RuleSetStale,
    /// The host could not connect to the core.
    CoreOffline,
    /// The core could not complete the request because of an internal failure.
    Internal,
    /// The bounded query queue is full; retry later.
    Busy,
    /// A code added by a later core; receive-only and excluded from `ALL`.
    #[serde(other)]
    Unknown,
}

impl ErrorCode {
    /// Every stable code that this core can send.
    pub const ALL: [Self; 13] = [
        Self::BadRequest,
        Self::UnknownOp,
        Self::NotImplemented,
        Self::HandshakeRequired,
        Self::FrameTooLarge,
        Self::ProtoMismatch,
        Self::Forbidden,
        Self::InvalidRange,
        Self::StaleCursor,
        Self::RuleSetStale,
        Self::CoreOffline,
        Self::Internal,
        Self::Busy,
    ];

    /// Stable wire spelling; `unknown` is a diagnostic label and is never sent.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::UnknownOp => "unknown_op",
            Self::NotImplemented => "not_implemented",
            Self::HandshakeRequired => "handshake_required",
            Self::FrameTooLarge => "frame_too_large",
            Self::ProtoMismatch => "proto_mismatch",
            Self::Forbidden => "forbidden",
            Self::InvalidRange => "invalid_range",
            Self::StaleCursor => "stale_cursor",
            Self::RuleSetStale => "rule_set_stale",
            Self::CoreOffline => "core_offline",
            Self::Internal => "internal",
            Self::Busy => "busy",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ErrorCode {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "bad_request" => Self::BadRequest,
            "unknown_op" => Self::UnknownOp,
            "not_implemented" => Self::NotImplemented,
            "handshake_required" => Self::HandshakeRequired,
            "frame_too_large" => Self::FrameTooLarge,
            "proto_mismatch" => Self::ProtoMismatch,
            "forbidden" => Self::Forbidden,
            "invalid_range" => Self::InvalidRange,
            "stale_cursor" => Self::StaleCursor,
            "rule_set_stale" => Self::RuleSetStale,
            "core_offline" => Self::CoreOffline,
            "internal" => Self::Internal,
            "busy" => Self::Busy,
            _ => Self::Unknown,
        })
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Unknown => Err(serde::ser::Error::custom(
                "unknown IPC error codes are receive-only",
            )),
            Self::BadRequest
            | Self::UnknownOp
            | Self::NotImplemented
            | Self::HandshakeRequired
            | Self::FrameTooLarge
            | Self::ProtoMismatch
            | Self::Forbidden
            | Self::InvalidRange
            | Self::StaleCursor
            | Self::RuleSetStale
            | Self::CoreOffline
            | Self::Busy
            | Self::Internal => serializer.serialize_str(self.as_str()),
        }
    }
}

/// A persisted rule-set version used for optimistic concurrency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleSetVersion(pub u64);

/// Typed error fields serialized flat beside the `code` discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorKind {
    /// The envelope or operation body does not match its schema.
    BadRequest,
    /// The request operation is not recognized.
    UnknownOp,
    /// The operation is defined but not implemented by this core.
    NotImplemented,
    /// The first request must be hello.
    HandshakeRequired,
    /// The declared frame payload exceeds the byte limit.
    FrameTooLarge,
    /// The client and core protocol ranges do not overlap.
    ProtoMismatch {
        /// Core-supported inclusive protocol range.
        supported: ProtoRange,
    },
    /// The declared role may not perform this operation.
    Forbidden,
    /// The requested date or time range is invalid.
    InvalidRange,
    /// Query inputs changed; pagination must restart.
    StaleCursor,
    /// The submitted base version differs from the current rule-set version.
    RuleSetStale {
        /// Current version to read before resubmitting a change.
        current_version: RuleSetVersion,
    },
    /// The host could not connect to the core.
    CoreOffline,
    /// The core could not complete the request because of an internal failure.
    Internal,
    /// The bounded query queue is full; retry later.
    Busy,
    /// An unrecognized future code; additional fields are ignored by this client.
    #[serde(other, skip_serializing)]
    Unknown,
}

impl ErrorKind {
    /// Returns the stable code without inspecting diagnostic text.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::BadRequest => ErrorCode::BadRequest,
            Self::UnknownOp => ErrorCode::UnknownOp,
            Self::NotImplemented => ErrorCode::NotImplemented,
            Self::HandshakeRequired => ErrorCode::HandshakeRequired,
            Self::FrameTooLarge => ErrorCode::FrameTooLarge,
            Self::ProtoMismatch { .. } => ErrorCode::ProtoMismatch,
            Self::Forbidden => ErrorCode::Forbidden,
            Self::InvalidRange => ErrorCode::InvalidRange,
            Self::StaleCursor => ErrorCode::StaleCursor,
            Self::RuleSetStale { .. } => ErrorCode::RuleSetStale,
            Self::CoreOffline => ErrorCode::CoreOffline,
            Self::Internal => ErrorCode::Internal,
            Self::Busy => ErrorCode::Busy,
            Self::Unknown => ErrorCode::Unknown,
        }
    }
}

impl From<NegotiateProtoError> for ErrorKind {
    fn from(value: NegotiateProtoError) -> Self {
        Self::ProtoMismatch {
            supported: value.supported,
        }
    }
}

/// IPC failure body with optional diagnostic text for logs only.
///
/// No code may branch on `msg`; behavior and presentation use the typed error kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    /// Stable code and its required structured fields.
    #[serde(flatten)]
    pub kind: ErrorKind,
    /// Optional log diagnostic; never use this text for decisions or localization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
}

impl ProtocolError {
    /// Constructs an error without diagnostic text.
    #[must_use]
    pub fn new(kind: ErrorKind) -> Self {
        Self { kind, msg: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_sendable_code_and_wire_spelling_is_stable() {
        let mut seen = std::collections::BTreeSet::new();
        for code in ErrorCode::ALL {
            match code {
                ErrorCode::BadRequest
                | ErrorCode::UnknownOp
                | ErrorCode::NotImplemented
                | ErrorCode::HandshakeRequired
                | ErrorCode::FrameTooLarge
                | ErrorCode::ProtoMismatch
                | ErrorCode::Forbidden
                | ErrorCode::InvalidRange
                | ErrorCode::StaleCursor
                | ErrorCode::RuleSetStale
                | ErrorCode::CoreOffline
                | ErrorCode::Busy
                | ErrorCode::Internal => {}
                ErrorCode::Unknown => panic!("unknown code must not be in ALL"),
            }
            assert!(seen.insert(code.as_str()), "duplicate {code}");
            assert_eq!(code.to_string(), code.as_str());
            assert_eq!(
                code.as_str()
                    .parse::<ErrorCode>()
                    .expect("infallible parsing"),
                code
            );
            assert_eq!(
                serde_json::to_value(code).expect("known code"),
                serde_json::json!(code.as_str())
            );
            assert_eq!(
                serde_json::from_value::<ErrorCode>(serde_json::json!(code.as_str()))
                    .expect("deserialize"),
                code
            );
        }
        assert_eq!(ErrorCode::ALL.len(), 13);
        assert_eq!(
            serde_json::from_str::<ErrorCode>("\"future_error\"").expect("unknown accepted"),
            ErrorCode::Unknown
        );
        assert_eq!(
            "future_error"
                .parse::<ErrorCode>()
                .expect("unknown accepted"),
            ErrorCode::Unknown
        );
        assert!(serde_json::to_value(ErrorCode::Unknown).is_err());
    }

    #[test]
    fn defined_but_unimplemented_has_stable_code() {
        let error = ProtocolError::new(ErrorKind::NotImplemented);
        let wire = serde_json::json!({"code":"not_implemented"});
        assert_eq!(serde_json::to_value(&error).expect("serialize"), wire);
        assert_eq!(
            serde_json::from_value::<ProtocolError>(wire).expect("deserialize"),
            error
        );
    }

    #[test]
    fn structured_error_fields_are_required_and_flat() {
        let range = ProtoRange::new(crate::ProtoVersion(1), crate::ProtoVersion(3)).expect("range");
        let mismatch = ProtocolError::new(ErrorKind::ProtoMismatch { supported: range });
        let wire = serde_json::json!({"code":"proto_mismatch","supported":{"min":1,"max":3}});
        assert_eq!(serde_json::to_value(&mismatch).expect("serialize"), wire);
        assert_eq!(
            serde_json::from_value::<ProtocolError>(wire).expect("deserialize"),
            mismatch
        );
        let stale = ProtocolError::new(ErrorKind::RuleSetStale {
            current_version: RuleSetVersion(7),
        });
        let wire = serde_json::json!({"code":"rule_set_stale","current_version":7});
        assert_eq!(serde_json::to_value(&stale).expect("serialize"), wire);
        assert_eq!(
            serde_json::from_value::<ProtocolError>(wire).expect("deserialize"),
            stale
        );
        for code in ["proto_mismatch", "rule_set_stale"] {
            assert!(
                serde_json::from_value::<ProtocolError>(serde_json::json!({"code":code})).is_err()
            );
        }
        let unknown: ProtocolError = serde_json::from_value(
            serde_json::json!({"code":"future_error","new_field":{},"msg":"diagnostic"}),
        )
        .expect("future error");
        assert_eq!(unknown.kind.code(), ErrorCode::Unknown);
        assert_eq!(unknown.msg.as_deref(), Some("diagnostic"));
        assert!(serde_json::to_value(&unknown).is_err());
    }
}
