//! Typed subscription snapshots (04 §7 and 10 §1).

use crate::{BrowserStream, StopReason};
use serde::{Deserialize, Serialize};

/// Supported production UI languages, independent of regional formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiLanguage {
    /// English.
    #[serde(rename = "en")]
    English,
    /// Simplified Chinese.
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
}

/// Clock format resolved from the Windows regional time pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HourCycle {
    /// Twelve-hour clock.
    H12,
    /// Twenty-four-hour clock.
    H24,
}

/// First calendar weekday resolved independently of UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    /// Monday.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday.
    Friday,
    /// Saturday.
    Saturday,
    /// Sunday.
    Sunday,
}

/// User regional settings resolved by the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocaleSettings {
    /// Twelve-hour or twenty-four-hour clock.
    pub hour_cycle: HourCycle,
    /// First calendar weekday.
    pub first_day_of_week: Weekday,
}

/// Sanitized application identity; excluded applications have no identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AppIdentity {
    /// Normalized executable path.
    Exe(String),
    /// Packaged application identity.
    Aumid(String),
    /// Unresolved application window class.
    Unresolved(String),
}

/// Foreground facts copied only from a privacy-filtered interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundState {
    /// Absent when privacy removes application identity.
    pub app: Option<AppIdentity>,
    /// Absent when privacy removes the title.
    pub title: Option<String>,
    /// Inclusive interval boundary in UTC milliseconds.
    pub interval_start_utc_ms: i64,
}

/// Latest state topic body; elapsed time is calculated by the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Absent whenever the core is not tracking a foreground interval.
    pub foreground: Option<ForegroundState>,
    /// Whether foreground recording is active.
    pub recording: bool,
    /// Same stable reason and priority as `core.status`.
    pub stop_reason: Option<StopReason>,
    /// Empty until M5 supplies browser connections.
    pub browser_streams: Vec<BrowserStream>,
    /// Resolved UI catalog language.
    pub ui_language: UiLanguage,
    /// Absent only when reading Windows regional settings failed; the core logs the failure.
    pub locale: Option<LocaleSettings>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_round_trip_preserves_nulls_and_stable_enum_names() {
        let wire = serde_json::json!({"foreground":{"app":{"kind":"exe","value":"c:\\app.exe"},"title":null,"interval_start_utc_ms":123},"recording":true,"stop_reason":null,"browser_streams":[],"ui_language":"zh-CN","locale":{"hour_cycle":"h24","first_day_of_week":"monday"}});
        let body: StateSnapshot = serde_json::from_value(wire.clone()).expect("state body");
        assert_eq!(serde_json::to_value(body).expect("state encoding"), wire);
    }
}
