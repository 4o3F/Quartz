//! Stored capture records carried by `query.facts`, without local-time presentation.

use serde::{Deserialize, Serialize};

/// Database primary key within one fact kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactRowId(pub i64);
/// Stored identity of the device that produced a capture run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CaptureDeviceId(pub String);
/// Database identity of a capture run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CaptureRunId(pub i64);
/// Stored foreground session identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactSessionKey(pub i64);
/// Database identity of a captured window instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowInstanceId(pub i64);
/// Revision-qualified privacy rule identity retained on a fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StoredPrivacyRuleId(pub String);
/// Stored AvailabilityReason label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactAvailabilityReason {
    Idle,
    Locked,
    Disconnected,
    Suspended,
    Paused,
}
/// Stored GapReason label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactGapReason {
    EventOverload,
    WriteQueueOverload,
    DatabaseError,
}
/// Stored NoForegroundReason label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactNoForegroundReason {
    Empty,
    UnknownDesktop,
}
/// Stored CaptureLevel label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactCaptureLevel {
    Full,
    TitleHidden,
    Excluded,
}
/// Stored TitleStatus label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactTitleStatus {
    Ok,
    Empty,
    Unreadable,
    Truncated,
    Redacted,
    Purged,
}
/// Stored IdleException label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactIdleException {
    KeepAlive,
    AppExempt,
    Audible,
}
/// Stored TimedPolicyKind label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactTimedPolicyKind {
    Pause,
    Privacy,
    KeepTiming,
    AppExemption,
    Audible,
}
/// Stored PrivacyMode label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactPrivacyMode {
    Normal,
    Private,
    NoTitles,
}
/// Stored closure reason shared by capture row kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::IntoStaticStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum FactEndReason {
    Shutdown,
    UserExit,
    CrashRecovered,
    Update,
    TimeChange,
    Resume,
    Drift,
    RunEnd,
    Switch,
    Title,
    NoForeground,
    Locked,
    Suspended,
    Paused,
    Disconnected,
    ClockJump,
    MidnightSplit,
    PolicyChanged,
    CaptureGap,
    Resumed,
}
impl FactEndReason {
    /// Stable dump label.
    pub fn label(self) -> &'static str {
        self.into()
    }
}
/// Row kind and its stored fields. Titles are present only when stored for that row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactFields {
    Run {
        device_id: CaptureDeviceId,
        win_session_id: i64,
        app_version: String,
    },
    ClockSegment,
    Foreground {
        #[serde(flatten)]
        interval: FactInterval,
    },
    NoForeground {
        #[serde(flatten)]
        interval: FactInterval,
    },
    Availability {
        reason: FactAvailabilityReason,
        exception: Option<FactIdleException>,
        last_input_utc_ms: Option<i64>,
        detected_utc_ms: Option<i64>,
        threshold_ms: Option<i64>,
        policy_version: i64,
    },
    CaptureGap {
        reason: FactGapReason,
    },
    TimedPolicy {
        policy_kind: FactTimedPolicyKind,
        privacy_mode: Option<FactPrivacyMode>,
        application_id: Option<crate::ApplicationId>,
    },
}
impl FactFields {
    /// Stable row label used for chronological tie ordering and text output.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Run { .. } => "run",
            Self::ClockSegment => "clock_segment",
            Self::Foreground { .. } => "foreground",
            Self::NoForeground { .. } => "no_foreground",
            Self::Availability { .. } => "availability",
            Self::CaptureGap { .. } => "capture_gap",
            Self::TimedPolicy { .. } => "timed_policy",
        }
    }
}
/// Stored foreground fields, including nullable dictionary values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactInterval {
    pub no_foreground_reason: Option<FactNoForegroundReason>,
    pub capture_level: FactCaptureLevel,
    pub title_status: FactTitleStatus,
    pub sequence: i64,
    pub session_key: FactSessionKey,
    pub app_id: Option<crate::ApplicationId>,
    pub window_instance_id: Option<WindowInstanceId>,
    pub privacy_rule_id: Option<StoredPrivacyRuleId>,
    pub privacy_label_key: Option<String>,
    pub privacy_label: Option<String>,
    pub app_display_name: Option<String>,
    pub exe: Option<String>,
    pub exe_name: Option<String>,
    pub hwnd: Option<i64>,
    pub window_generation: Option<i64>,
    pub pid: Option<i64>,
    pub window_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
/// Raw fact with full stored endpoints; exactly one UTC endpoint is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactRecord {
    #[serde(flatten)]
    pub fields: FactFields,
    pub id: FactRowId,
    pub run_id: Option<CaptureRunId>,
    pub segment_id: Option<crate::ClockSegmentId>,
    pub start_utc_ms: i64,
    pub end_utc_ms: Option<i64>,
    pub confirmed_utc_ms: Option<i64>,
    pub start_mono_ms: Option<i64>,
    pub end_mono_ms: Option<i64>,
    pub confirmed_mono_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub end_reason: Option<FactEndReason>,
}
impl FactRecord {
    /// Authoritative stored endpoint for a closed or confirmed open row.
    pub fn endpoint(&self) -> Option<i64> {
        self.end_utc_ms.or(self.confirmed_utc_ms)
    }
}
