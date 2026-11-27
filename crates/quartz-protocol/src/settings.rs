//! Stored UI preferences and their effective presentation values.
use crate::{HourCycle, LocaleSettings, UiLanguage, Weekday};
use serde::{Deserialize, Serialize};

/// UI catalog preference; auto follows the preferred system languages.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiLanguageSetting {
    /// Follow Windows.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    /// Simplified Chinese.
    #[serde(rename = "zh-CN")]
    ZhCn,
    /// English.
    #[serde(rename = "en")]
    En,
}
impl UiLanguageSetting {
    /// Resolves an explicit preference before consulting the system language.
    #[must_use]
    pub fn resolve(self, system: UiLanguage) -> UiLanguage {
        match self {
            Self::Auto => system,
            Self::ZhCn => UiLanguage::SimplifiedChinese,
            Self::En => UiLanguage::English,
        }
    }
}
/// Stored clock preference.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HourCycleSetting {
    /// Follow Windows.
    #[default]
    Auto,
    /// Twelve-hour clock.
    H12,
    /// Twenty-four-hour clock.
    H24,
}
impl HourCycleSetting {
    fn resolve(self, system: Option<HourCycle>) -> Option<HourCycle> {
        match self {
            Self::Auto => system,
            Self::H12 => Some(HourCycle::H12),
            Self::H24 => Some(HourCycle::H24),
        }
    }
}
/// Stored first calendar weekday preference.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstDayOfWeekSetting {
    /// Follow Windows.
    #[default]
    Auto,
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
impl FirstDayOfWeekSetting {
    fn resolve(self, system: Option<Weekday>) -> Option<Weekday> {
        match self {
            Self::Auto => system,
            Self::Monday => Some(Weekday::Monday),
            Self::Tuesday => Some(Weekday::Tuesday),
            Self::Wednesday => Some(Weekday::Wednesday),
            Self::Thursday => Some(Weekday::Thursday),
            Self::Friday => Some(Weekday::Friday),
            Self::Saturday => Some(Weekday::Saturday),
            Self::Sunday => Some(Weekday::Sunday),
        }
    }
}
/// Retention window lengths in logical days (03 §6).
const RETENTION_DAYS_30: u32 = 30;
const RETENTION_DAYS_90: u32 = 90;
const RETENTION_DAYS_365: u32 = 365;

/// Retention period for stored window and browser tab titles (03 §6).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TitleRetention {
    /// Keep stored titles indefinitely.
    #[default]
    #[serde(rename = "forever")]
    Forever,
    /// Purge titles from closed intervals ending at or before 30 logical days ago.
    #[serde(rename = "30d")]
    Days30,
    /// Purge titles from closed intervals ending at or before 90 logical days ago.
    #[serde(rename = "90d")]
    Days90,
    /// Purge titles from closed intervals ending at or before 365 logical days ago.
    #[serde(rename = "365d")]
    Days365,
}
impl TitleRetention {
    /// Number of retained logical days before the current logical day, or `None` for forever.
    #[must_use]
    pub const fn days(self) -> Option<u32> {
        match self {
            Self::Forever => None,
            Self::Days30 => Some(RETENTION_DAYS_30),
            Self::Days90 => Some(RETENTION_DAYS_90),
            Self::Days365 => Some(RETENTION_DAYS_365),
        }
    }
}
/// Whether Quartz launches automatically when the user signs in to Windows (01 §8).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "bool", into = "bool")]
pub enum AutostartEnabled {
    /// The current executable is registered in the Windows Run key.
    Enabled,
    /// Autostart is not registered for the current executable.
    #[default]
    Disabled,
}
impl From<bool> for AutostartEnabled {
    fn from(value: bool) -> Self {
        if value { Self::Enabled } else { Self::Disabled }
    }
}
impl From<AutostartEnabled> for bool {
    fn from(value: AutostartEnabled) -> Self {
        value == AutostartEnabled::Enabled
    }
}
/// Whether the UI remains running without a window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "bool", into = "bool")]
pub enum ResidentEnabled {
    /// Keep the UI process ready for the next open request.
    #[default]
    Enabled,
    /// Start the UI on demand and quit after its last window closes.
    Disabled,
}
impl From<bool> for ResidentEnabled {
    fn from(value: bool) -> Self {
        if value { Self::Enabled } else { Self::Disabled }
    }
}
impl From<ResidentEnabled> for bool {
    fn from(value: ResidentEnabled) -> Self {
        value == ResidentEnabled::Enabled
    }
}
/// Stored and effective resident policy.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidentSettings {
    /// ui.resident.enabled, defaulting to true.
    pub enabled: ResidentEnabled,
}
/// Partial resident policy update.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidentUpdate {
    /// Optional enablement change; explicit null is rejected.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub enabled: Option<ResidentEnabled>,
}
/// Whether tray hover may launch a hidden UI process.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "bool", into = "bool")]
pub enum PrewarmEnabled {
    /// Hover may launch a prewarm process.
    #[default]
    Enabled,
    /// Hover leaves the UI process stopped.
    Disabled,
}
impl From<bool> for PrewarmEnabled {
    fn from(value: bool) -> Self {
        if value { Self::Enabled } else { Self::Disabled }
    }
}
impl From<PrewarmEnabled> for bool {
    fn from(value: PrewarmEnabled) -> Self {
        value == PrewarmEnabled::Enabled
    }
}
/// Whether prewarming is allowed while the machine runs on battery.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "bool", into = "bool")]
pub enum PrewarmOnBattery {
    /// Battery power permits prewarming.
    Allowed,
    /// Battery power prevents prewarming.
    #[default]
    Blocked,
}
impl From<bool> for PrewarmOnBattery {
    fn from(value: bool) -> Self {
        if value { Self::Allowed } else { Self::Blocked }
    }
}
impl From<PrewarmOnBattery> for bool {
    fn from(value: PrewarmOnBattery) -> Self {
        value == PrewarmOnBattery::Allowed
    }
}
/// Stored and effective prewarm policy; it does not depend on the system locale.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrewarmSettings {
    /// ui.prewarm.enabled, defaulting to true.
    pub enabled: PrewarmEnabled,
    /// ui.prewarm.on_battery, defaulting to false.
    pub on_battery: PrewarmOnBattery,
}
/// Partial prewarm updates; explicit null and non-boolean values are rejected.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrewarmUpdate {
    /// Optional enablement change.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub enabled: Option<PrewarmEnabled>,
    /// Optional battery permission change.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub on_battery: Option<PrewarmOnBattery>,
}
/// Stored UI preferences with defaults for absent rows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiSettings {
    /// Keep the UI process running without a window.
    #[serde(default)]
    pub resident: ResidentSettings,
    /// Tray hover prewarming policy.
    #[serde(default)]
    pub prewarm: PrewarmSettings,
    /// Catalog preference.
    pub language: UiLanguageSetting,
    /// Clock preference.
    pub hour_cycle: HourCycleSetting,
    /// Weekday preference.
    pub first_day_of_week: FirstDayOfWeekSetting,
}
impl UiSettings {
    /// Resolves preferences using the system values read at core startup.
    #[must_use]
    pub fn resolve(self, system: EffectiveUiSettings) -> EffectiveUiSettings {
        let hour_cycle = self
            .hour_cycle
            .resolve(system.locale.map(|locale| locale.hour_cycle));
        let first_day_of_week = self
            .first_day_of_week
            .resolve(system.locale.map(|locale| locale.first_day_of_week));
        EffectiveUiSettings {
            resident: self.resident,
            prewarm: self.prewarm,
            ui_language: self.language.resolve(system.ui_language),
            locale: match hour_cycle {
                Some(hour_cycle) => first_day_of_week.map(|first_day_of_week| LocaleSettings {
                    hour_cycle,
                    first_day_of_week,
                }),
                None => None,
            },
        }
    }
}
/// Resolved values shared by the tray, state publisher, and settings replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveUiSettings {
    /// Effective UI process lifetime policy.
    #[serde(default)]
    pub resident: ResidentSettings,
    /// Prewarming policy evaluated with the current power source on hover.
    #[serde(default)]
    pub prewarm: PrewarmSettings,
    /// Effective catalog.
    pub ui_language: UiLanguage,
    /// Absent when an automatic regional value could not be read.
    pub locale: Option<LocaleSettings>,
}
/// Empty settings.get body.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsGetRequest {}
/// Partial settings.set body with typed preferences and nested prewarm updates.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsSetRequest {
    /// Optional Windows autostart update; explicit null is rejected.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub autostart: Option<AutostartEnabled>,
    /// Optional title retention period update (`capture.title_retention`); explicit null is rejected.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub title_retention: Option<TitleRetention>,
    /// Optional partial update to ui.resident.*.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub resident: Option<ResidentUpdate>,
    /// Optional partial update to ui.prewarm.*.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub prewarm: Option<PrewarmUpdate>,
    /// Optional ui.language update.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub language: Option<UiLanguageSetting>,
    /// Optional ui.hour_cycle update.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub hour_cycle: Option<HourCycleSetting>,
    /// Optional ui.first_day_of_week update.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub first_day_of_week: Option<FirstDayOfWeekSetting>,
}
fn present_value<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
/// Reply for both settings operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsResponse {
    /// Stored preferences.
    pub ui: UiSettings,
    /// Effective presentation values.
    pub effective: EffectiveUiSettings,
    /// Current Windows autostart state read from the registry at request time.
    #[serde(default)]
    pub autostart: AutostartEnabled,
    /// Configured window and browser tab title retention period (03 §6).
    #[serde(default)]
    pub title_retention: TitleRetention,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setting_wire_values_and_partial_update_validation() {
        let update: SettingsSetRequest = serde_json::from_value(serde_json::json!({"language":"zh-CN", "hour_cycle":"h12", "first_day_of_week":"sunday", "future":true})).expect("typed update");
        assert_eq!(update.language, Some(UiLanguageSetting::ZhCn));
        assert_eq!(update.hour_cycle, Some(HourCycleSetting::H12));
        assert_eq!(
            update.first_day_of_week,
            Some(FirstDayOfWeekSetting::Sunday)
        );
        assert_eq!(
            serde_json::to_value(SettingsSetRequest::default()).expect("empty update"),
            serde_json::json!({})
        );
        for body in [
            serde_json::json!({"autostart":null}),
            serde_json::json!({"autostart":"yes"}),
            serde_json::json!({"language":null}),
            serde_json::json!({"language":"fr"}),
            serde_json::json!({"hour_cycle":24}),
            serde_json::json!({"first_day_of_week":"noday"}),
        ] {
            assert!(serde_json::from_value::<SettingsSetRequest>(body).is_err());
        }
    }
    #[test]
    fn autostart_wire_values_round_trip_and_default_additively() {
        let enabled: SettingsSetRequest =
            serde_json::from_value(serde_json::json!({"autostart": true})).expect("enable");
        assert_eq!(enabled.autostart, Some(AutostartEnabled::Enabled));
        assert_eq!(
            serde_json::to_value(enabled).expect("serialize enable"),
            serde_json::json!({"autostart": true})
        );
        let disabled: SettingsSetRequest =
            serde_json::from_value(serde_json::json!({"autostart": false})).expect("disable");
        assert_eq!(disabled.autostart, Some(AutostartEnabled::Disabled));
        assert_eq!(
            serde_json::to_value(disabled).expect("serialize disable"),
            serde_json::json!({"autostart": false})
        );
        let legacy_response: SettingsResponse = serde_json::from_value(serde_json::json!({
            "ui": {
                "language": "auto",
                "hour_cycle": "auto",
                "first_day_of_week": "auto"
            },
            "effective": {
                "ui_language": "en",
                "locale": null
            }
        }))
        .expect("legacy response without autostart");
        assert_eq!(legacy_response.autostart, AutostartEnabled::Disabled);
        assert_eq!(legacy_response.title_retention, TitleRetention::Forever);
    }
    #[test]
    fn title_retention_wire_values_round_trip_and_reject_null_and_unknown_values() {
        for (wire, expected, expected_days) in [
            ("forever", TitleRetention::Forever, None),
            ("30d", TitleRetention::Days30, Some(RETENTION_DAYS_30)),
            ("90d", TitleRetention::Days90, Some(RETENTION_DAYS_90)),
            ("365d", TitleRetention::Days365, Some(RETENTION_DAYS_365)),
        ] {
            let parsed: SettingsSetRequest =
                serde_json::from_value(serde_json::json!({"title_retention": wire}))
                    .expect("valid retention");
            assert_eq!(parsed.title_retention, Some(expected));
            assert_eq!(expected.days(), expected_days);
            assert_eq!(
                serde_json::to_value(parsed).expect("serialize retention"),
                serde_json::json!({"title_retention": wire})
            );
        }
        assert_eq!(TitleRetention::default(), TitleRetention::Forever);
        for body in [
            serde_json::json!({"title_retention": null}),
            serde_json::json!({"title_retention": "7d"}),
            serde_json::json!({"title_retention": 90}),
        ] {
            assert!(
                serde_json::from_value::<SettingsSetRequest>(body).is_err(),
                "invalid title_retention must fail"
            );
        }
    }
    #[test]
    fn explicit_preferences_override_system_and_auto_preserves_it() {
        let system = EffectiveUiSettings {
            resident: Default::default(),
            prewarm: Default::default(),
            ui_language: UiLanguage::English,
            locale: Some(LocaleSettings {
                hour_cycle: HourCycle::H24,
                first_day_of_week: Weekday::Monday,
            }),
        };
        assert_eq!(UiSettings::default().resolve(system), system);
        let settings = UiSettings {
            resident: Default::default(),
            prewarm: Default::default(),
            language: UiLanguageSetting::ZhCn,
            hour_cycle: HourCycleSetting::H12,
            first_day_of_week: FirstDayOfWeekSetting::Sunday,
        };
        assert_eq!(
            settings.resolve(system),
            EffectiveUiSettings {
                resident: Default::default(),
                prewarm: Default::default(),
                ui_language: UiLanguage::SimplifiedChinese,
                locale: Some(LocaleSettings {
                    hour_cycle: HourCycle::H12,
                    first_day_of_week: Weekday::Sunday
                })
            }
        );
        let unavailable = EffectiveUiSettings {
            locale: None,
            ..system
        };
        assert_eq!(UiSettings::default().resolve(unavailable).locale, None);
        assert!(settings.resolve(unavailable).locale.is_some());
        assert_eq!(
            UiLanguageSetting::En.resolve(UiLanguage::SimplifiedChinese),
            UiLanguage::English
        );
    }
}
