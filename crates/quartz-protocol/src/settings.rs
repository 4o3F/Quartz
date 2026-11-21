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
/// All stored UI preferences, with auto for absent rows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiSettings {
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
    /// Effective catalog.
    pub ui_language: UiLanguage,
    /// Absent when an automatic regional value could not be read.
    pub locale: Option<LocaleSettings>,
}
/// Empty settings.get body.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsGetRequest {}
/// Partial settings.set body; supplied values must be valid enum strings.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsSetRequest {
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
            serde_json::json!({"language":null}),
            serde_json::json!({"language":"fr"}),
            serde_json::json!({"hour_cycle":24}),
            serde_json::json!({"first_day_of_week":"noday"}),
        ] {
            assert!(serde_json::from_value::<SettingsSetRequest>(body).is_err());
        }
    }
    #[test]
    fn explicit_preferences_override_system_and_auto_preserves_it() {
        let system = EffectiveUiSettings {
            ui_language: UiLanguage::English,
            locale: Some(LocaleSettings {
                hour_cycle: HourCycle::H24,
                first_day_of_week: Weekday::Monday,
            }),
        };
        assert_eq!(UiSettings::default().resolve(system), system);
        let settings = UiSettings {
            language: UiLanguageSetting::ZhCn,
            hour_cycle: HourCycleSetting::H12,
            first_day_of_week: FirstDayOfWeekSetting::Sunday,
        };
        assert_eq!(
            settings.resolve(system),
            EffectiveUiSettings {
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
