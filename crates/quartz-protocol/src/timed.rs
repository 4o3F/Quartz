//! Timed capture controls and their admitted requested state (04 §6.3/§6.5).

use serde::{Deserialize, Serialize};

/// Required UTC deadline for a pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingPauseRequest {
    /// Expiration in UTC milliseconds, strictly after admission time.
    pub until_utc_ms: i64,
}
/// Required UTC deadline for keeping timing active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingKeepAliveRequest {
    /// Expiration in UTC milliseconds, subject to domain normalization.
    pub until_utc_ms: i64,
}
/// Cancels the current pause.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingResumeRequest {}
/// Installs a timed private policy or cancels the current policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PrivacyModeSetWire", into = "PrivacyModeSetWire")]
pub enum PrivacyModeSetRequest {
    /// Enabling requires a deadline; the core checks that it is in the future.
    Enable { until_utc_ms: i64 },
    /// Cancel the active private policy.
    Disable,
}
/// Boolean fields exist only at the documented JSON boundary.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivacyModeSetWire {
    enabled: bool,
    until_utc_ms: Option<i64>,
}
impl TryFrom<PrivacyModeSetWire> for PrivacyModeSetRequest {
    type Error = ParsePrivacyModeRequestError;
    fn try_from(wire: PrivacyModeSetWire) -> Result<Self, Self::Error> {
        match wire.enabled {
            true => wire
                .until_utc_ms
                .map(|until_utc_ms| Self::Enable { until_utc_ms })
                .ok_or(ParsePrivacyModeRequestError),
            false => Ok(Self::Disable),
        }
    }
}
impl From<PrivacyModeSetRequest> for PrivacyModeSetWire {
    fn from(request: PrivacyModeSetRequest) -> Self {
        match request {
            PrivacyModeSetRequest::Enable { until_utc_ms } => Self {
                enabled: true,
                until_utc_ms: Some(until_utc_ms),
            },
            PrivacyModeSetRequest::Disable => Self {
                enabled: false,
                until_utc_ms: None,
            },
        }
    }
}
/// Enabling private mode requires an integer UTC deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
#[snafu(display("enabling privacy mode requires until_utc_ms"))]
pub struct ParsePrivacyModeRequestError;
/// Privacy retention combination from 06 §2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode {
    /// Ordinary rule-driven retention.
    Normal,
    /// Drop titles and retain domains at most.
    NoTitles,
    /// Keep the application, drop titles, and exclude web activity.
    Private,
}
/// Active privacy policy and its expiration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedPrivacyMode {
    /// Applied retention combination.
    pub mode: PrivacyMode,
    /// Expiration in UTC milliseconds.
    pub until_utc_ms: i64,
}
/// Capture-owned active policies, shared by subscriptions and the tray.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedPolicies {
    /// Absent when no pause is active.
    pub pause_until_utc_ms: Option<i64>,
    /// Absent when global privacy mode is inactive.
    pub privacy_mode: Option<TimedPrivacyMode>,
    /// Absent when keep timing is inactive.
    pub keep_timing_until_utc_ms: Option<i64>,
}
/// Normalized requested policy, acknowledged after queue admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum RequestedTimedState {
    /// None cancels the pause.
    Pause { until_utc_ms: Option<i64> },
    /// None cancels global private mode.
    Privacy { mode: Option<TimedPrivacyMode> },
    /// Keep timing has no cancellation operation.
    KeepTiming { until_utc_ms: i64 },
}
/// Admission acknowledgement; effective state arrives on the state topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedPolicyResponse {
    /// Always true for an admitted command.
    #[serde(flatten)]
    pub accepted: crate::AcceptedBody,
    /// Domain-normalized requested state.
    pub requested: RequestedTimedState,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn privacy_wire_requires_a_deadline_when_enabled_and_retains_boolean_json() {
        for wire in [
            serde_json::json!({"enabled":true}),
            serde_json::json!({"enabled":true,"until_utc_ms":null}),
            serde_json::json!({"enabled":"true","until_utc_ms":1}),
            serde_json::json!({"until_utc_ms":1}),
        ] {
            assert!(
                serde_json::from_value::<PrivacyModeSetRequest>(wire.clone()).is_err(),
                "{wire}"
            );
        }
        for wire in [
            serde_json::json!({"enabled":true,"until_utc_ms":i64::MAX}),
            serde_json::json!({"enabled":false,"until_utc_ms":null}),
        ] {
            let request: PrivacyModeSetRequest =
                serde_json::from_value(wire.clone()).expect("valid wire");
            assert_eq!(serde_json::to_value(request).expect("wire snapshot"), wire);
        }
    }
}
