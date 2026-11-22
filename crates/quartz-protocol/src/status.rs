//! Typed core status and UI acknowledgements (04 §6.1).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{CoreVersion, InstanceId};

/// Empty request body for `core.status`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreStatusRequest {}

/// Empty request body for `ui.open`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiOpenRequest {}

/// Empty request body for `ui.first_frame`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiFirstFrameRequest {}

/// UI notification that its last window has closed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiHiddenRequest {}

/// An acknowledgement that always serializes as `{"accepted":true}`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AcceptedBody;

impl Serialize for AcceptedBody {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire {
            accepted: bool,
        }
        Wire { accepted: true }.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for AcceptedBody {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            accepted: bool,
        }
        if Wire::deserialize(deserializer)?.accepted {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("accepted must be true"))
        }
    }
}

/// Stable reason why foreground recording is currently stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(strum::IntoStaticStr, strum::VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum StopReason {
    /// The first capture state has not arrived.
    Starting,
    /// Persistence failed.
    PersistenceFailed,
    /// Session registration or querying is pending.
    SessionUnavailable,
    /// Capture coverage is untrusted.
    CaptureGap,
    /// The session is locked.
    Locked,
    /// The session is disconnected.
    Disconnected,
    /// The computer is suspended.
    Suspended,
    /// The user paused capture.
    Paused,
    /// No foreground window can be recorded.
    NoForeground,
    /// The capture run has ended.
    Ended,
}

/// Persistence availability, independent of foreground recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(strum::IntoStaticStr, strum::VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum DatabaseState {
    /// The database accepts writes.
    Ok,
    /// The database failed to persist writes.
    PersistenceFailed,
}

/// Identity of a connected browser stream; populated by M5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BrowserStreamId(pub String);

/// Browser connection entry; M5 supplies connected entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserStream {
    /// Registered stream identity.
    pub browser_stream: BrowserStreamId,
}

/// Immutable status published by the main thread, without a database read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreStatusResponse {
    /// Whether the core is recording foreground facts.
    pub recording: bool,
    /// Absent while recording; otherwise the current stable reason.
    pub stop_reason: Option<StopReason>,
    /// Current persistence state.
    pub database: DatabaseState,
    /// Connected browser streams, empty until M5.
    pub browser_streams: Vec<BrowserStream>,
    /// Identity generated for this core start.
    pub instance_id: InstanceId,
    /// Core program version.
    pub core_version: CoreVersion,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_display_names_match_wire_names() {
        use strum::VariantArray;
        for reason in StopReason::VARIANTS {
            let name: &'static str = (*reason).into();
            assert_eq!(serde_json::to_value(reason).expect("stop reason"), name);
        }
        for database in DatabaseState::VARIANTS {
            let name: &'static str = (*database).into();
            assert_eq!(
                serde_json::to_value(database).expect("database state"),
                name
            );
        }
    }

    #[test]
    fn accepted_is_always_true() {
        assert_eq!(
            serde_json::to_value(AcceptedBody).expect("valid protocol fixture"),
            json!({"accepted":true})
        );
        assert_eq!(
            serde_json::from_value::<AcceptedBody>(json!({"accepted":true}))
                .expect("valid protocol fixture"),
            AcceptedBody
        );
        assert!(serde_json::from_value::<AcceptedBody>(json!({"accepted":false})).is_err());
    }

    #[test]
    fn status_round_trip_and_empty_requests() {
        let wire = json!({"recording":false,"stop_reason":"session_unavailable","database":"ok","browser_streams":[],"instance_id":"test-instance","core_version":"0.0.0"});
        let status: CoreStatusResponse =
            serde_json::from_value(wire.clone()).expect("valid protocol fixture");
        assert_eq!(
            serde_json::to_value(status).expect("valid protocol fixture"),
            wire
        );
        assert_eq!(
            serde_json::to_value(CoreStatusRequest {}).expect("valid protocol fixture"),
            json!({})
        );
        assert!(serde_json::from_value::<CoreStatusRequest>(json!(null)).is_err());
        assert!(serde_json::from_value::<CoreStatusResponse>(json!({"recording":false,"stop_reason":"arbitrary text","database":"ok","browser_streams":[],"instance_id":"x","core_version":"v"})).is_err());
    }
}
