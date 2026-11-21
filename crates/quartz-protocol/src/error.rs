//! Stable IPC error codes and code-specific structured fields (04 §3).

use serde::{Deserialize, Serialize};

use crate::ProtoRange;

/// A persisted rule-set version used for optimistic concurrency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleSetVersion(pub u64);

/// Typed error fields serialized flat beside the `code` discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
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
        /// Core release version to show with the incompatible protocol range.
        core_version: crate::CoreVersion,
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
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::UnknownOp => "unknown_op",
            Self::NotImplemented => "not_implemented",
            Self::HandshakeRequired => "handshake_required",
            Self::FrameTooLarge => "frame_too_large",
            Self::ProtoMismatch { .. } => "proto_mismatch",
            Self::Forbidden => "forbidden",
            Self::InvalidRange => "invalid_range",
            Self::StaleCursor => "stale_cursor",
            Self::RuleSetStale { .. } => "rule_set_stale",
            Self::CoreOffline => "core_offline",
            Self::Internal => "internal",
            Self::Busy => "busy",
            Self::Unknown => "unknown",
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
    fn error_wire_names_and_display_codes_agree() {
        for name in [
            "bad_request",
            "unknown_op",
            "not_implemented",
            "handshake_required",
            "frame_too_large",
            "proto_mismatch",
            "forbidden",
            "invalid_range",
            "stale_cursor",
            "rule_set_stale",
            "core_offline",
            "internal",
            "busy",
        ] {
            let wire = serde_json::json!({"code": name, "supported": {"min": 1, "max": 3}, "current_version": 7, "core_version": "test-core"});
            let kind: ErrorKind = serde_json::from_value(wire).expect("known error kind");
            assert_eq!(kind.code(), name);
            assert_eq!(
                serde_json::to_value(kind).expect("sendable kind")["code"],
                name
            );
        }
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
        let mismatch = ProtocolError::new(ErrorKind::ProtoMismatch {
            supported: range,
            core_version: crate::CoreVersion("test-core".into()),
        });
        let wire = serde_json::json!({"code":"proto_mismatch","supported":{"min":1,"max":3},"core_version":"test-core"});
        assert_eq!(serde_json::to_value(&mismatch).expect("serialize"), wire);
        assert_eq!(
            serde_json::from_value::<ProtocolError>(wire).expect("deserialize"),
            mismatch
        );
        assert!(
            serde_json::from_value::<ProtocolError>(
                serde_json::json!({"code":"proto_mismatch", "supported":{"min":1,"max":3}})
            )
            .is_err(),
            "core_version is required before protocol release"
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
        assert_eq!(unknown.kind.code(), "unknown");
        assert_eq!(unknown.msg.as_deref(), Some("diagnostic"));
        assert!(serde_json::to_value(&unknown).is_err());
    }
}
