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
/// Global privacy mode request; enabling requires a future deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyModeSetRequest {
    /// Whether to install or cancel private mode.
    pub enabled: bool,
    /// Required when enabled.
    pub until_utc_ms: Option<i64>,
}
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
