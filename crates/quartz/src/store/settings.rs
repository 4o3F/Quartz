//! Typed JSON access to configuration and persisted scheduler deadlines.
use super::{Store, schema::settings, versions::BumpVersionError};
use crate::query::calendar::DayStartOffset;
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl, SqliteConnection};
use quartz_domain::capture::{SchedulerKind, UtcInstant};
use quartz_protocol::{
    FirstDayOfWeekSetting, HourCycleSetting, PrewarmEnabled, PrewarmOnBattery, PrewarmSettings,
    ResidentEnabled, ResidentSettings, SettingsSetRequest, UiLanguageSetting, UiSettings,
};
use serde::{Serialize, de::DeserializeOwned};

/// Every supported settings row, including the obsolete recovery marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingKey {
    MetadataVersion,
    StatsPolicyVersion,
    DeviceId,
    DayStartOffset,
    MaintenanceDeadline,
    IdleDeadline,
    RemovedPrivacyContextDeadline,
    MidnightDeadline,
    UiResidentEnabled,
    UiPrewarmEnabled,
    UiPrewarmOnBattery,
    UiLanguage,
    UiHourCycle,
    UiFirstDayOfWeek,
}
impl SettingKey {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MetadataVersion => "metadata_version",
            Self::StatsPolicyVersion => "stats_policy_version",
            Self::DeviceId => "device_id",
            Self::DayStartOffset => "day.start_offset_min",
            Self::MaintenanceDeadline => "capture.deadline.maintenance",
            Self::IdleDeadline => "capture.deadline.idle",
            Self::RemovedPrivacyContextDeadline => "capture.deadline.privacy_context",
            Self::MidnightDeadline => "capture.deadline.midnight",
            Self::UiResidentEnabled => "ui.resident.enabled",
            Self::UiPrewarmEnabled => "ui.prewarm.enabled",
            Self::UiPrewarmOnBattery => "ui.prewarm.on_battery",
            Self::UiLanguage => "ui.language",
            Self::UiHourCycle => "ui.hour_cycle",
            Self::UiFirstDayOfWeek => "ui.first_day_of_week",
        }
    }
    pub(crate) fn deadline(owner: SchedulerKind) -> Self {
        match owner {
            SchedulerKind::Maintenance => Self::MaintenanceDeadline,
            SchedulerKind::Idle => Self::IdleDeadline,
            SchedulerKind::Midnight => Self::MidnightDeadline,
        }
    }
}
impl std::fmt::Display for SettingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum AccessSettingError {
    #[snafu(display("could not version UI settings"))]
    Version { source: Box<BumpVersionError> },
    #[snafu(display("could not access setting"))]
    Database { source: diesel::result::Error },
    #[snafu(display("invalid JSON value for setting {key}"))]
    Json {
        key: SettingKey,
        source: serde_json::Error,
    },
    #[snafu(display("invalid scheduler deadline for setting {key}"))]
    Deadline {
        key: SettingKey,
        source: quartz_domain::capture::AdvanceTimeError,
    },
    #[snafu(display("day start offset must be between 0 and 720 minutes"))]
    DayStartOffset,
    #[snafu(display("statistics policy version must be nonnegative"))]
    StatsPolicyVersion,
    #[snafu(display("metadata version must be nonnegative"))]
    MetadataVersion,
}
impl From<diesel::result::Error> for AccessSettingError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}
pub(super) struct Settings<'connection> {
    connection: &'connection mut SqliteConnection,
}
impl<'connection> Settings<'connection> {
    pub(super) fn new(connection: &'connection mut SqliteConnection) -> Self {
        Self { connection }
    }
}
/// Missing day offset means local midnight (03 §1).
pub(crate) const DEFAULT_DAY_START_OFFSET_MIN: i64 = DayStartOffset::MIDNIGHT.minutes();
/// Maximum logical day offset in minutes (03 §1).
#[cfg(test)]
pub(crate) const MAX_DAY_START_OFFSET_MIN: i64 = DayStartOffset::MAX_MINUTES;
impl Settings<'_> {
    fn get<T: DeserializeOwned>(
        &mut self,
        key: SettingKey,
    ) -> Result<Option<T>, AccessSettingError> {
        self.raw(key)?
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|source| AccessSettingError::Json { key, source })
            })
            .transpose()
    }
    fn put<T: Serialize + ?Sized>(
        &mut self,
        key: SettingKey,
        value: &T,
    ) -> Result<(), AccessSettingError> {
        let value = serde_json::to_string(value)
            .map_err(|source| AccessSettingError::Json { key, source })?;
        self.put_raw(key, &value)?;
        Ok(())
    }
    fn raw(&mut self, key: SettingKey) -> Result<Option<String>, diesel::result::Error> {
        settings::table
            .find(key.as_str())
            .select(settings::value)
            .first(self.connection)
            .optional()
    }
    fn put_raw(&mut self, key: SettingKey, value: &str) -> Result<(), diesel::result::Error> {
        diesel::insert_into(settings::table)
            .values((settings::key.eq(key.as_str()), settings::value.eq(value)))
            .on_conflict(settings::key)
            .do_update()
            .set(settings::value.eq(value))
            .execute(self.connection)?;
        Ok(())
    }
    pub(super) fn contains(&mut self, key: SettingKey) -> Result<bool, diesel::result::Error> {
        Ok(self.raw(key)?.is_some())
    }
    pub(super) fn remove(&mut self, key: SettingKey) -> Result<(), diesel::result::Error> {
        diesel::delete(settings::table.find(key.as_str())).execute(self.connection)?;
        Ok(())
    }
    pub(super) fn device_id(&mut self) -> Result<Option<String>, AccessSettingError> {
        self.get(SettingKey::DeviceId)
    }
    pub(super) fn put_device_id(&mut self, value: &str) -> Result<(), AccessSettingError> {
        self.put(SettingKey::DeviceId, value)
    }
    /// Databases without a statistics policy counter start at version zero.
    pub(super) fn stats_policy_version(&mut self) -> Result<i64, AccessSettingError> {
        let value = self
            .get::<i64>(SettingKey::StatsPolicyVersion)?
            .unwrap_or(0);
        if value < 0 {
            return Err(AccessSettingError::StatsPolicyVersion);
        }
        Ok(value)
    }
    pub(super) fn metadata_version(&mut self) -> Result<i64, AccessSettingError> {
        let value = self.get(SettingKey::MetadataVersion)?.unwrap_or_default();
        if value < 0 {
            return Err(AccessSettingError::MetadataVersion);
        }
        Ok(value)
    }
    pub(super) fn put_metadata_version(&mut self, value: i64) -> Result<(), AccessSettingError> {
        if value < 0 {
            return Err(AccessSettingError::MetadataVersion);
        }
        self.put(SettingKey::MetadataVersion, &value)
    }
    pub(super) fn day_start_offset(&mut self) -> Result<DayStartOffset, AccessSettingError> {
        let value = self
            .get(SettingKey::DayStartOffset)?
            .unwrap_or(DEFAULT_DAY_START_OFFSET_MIN);
        DayStartOffset::try_from(value).map_err(|_| AccessSettingError::DayStartOffset)
    }
    #[cfg(test)]
    fn validate_day_start_offset(value: i64) -> Result<(), AccessSettingError> {
        DayStartOffset::try_from(value)
            .map(|_| ())
            .map_err(|_| AccessSettingError::DayStartOffset)
    }
    #[cfg(test)]
    fn put_day_start_offset(&mut self, value: i64) -> Result<(), AccessSettingError> {
        Self::validate_day_start_offset(value)?;
        self.put(SettingKey::DayStartOffset, &value)
    }
    pub(super) fn deadline(
        &mut self,
        owner: SchedulerKind,
    ) -> Result<Option<UtcInstant>, AccessSettingError> {
        let key = SettingKey::deadline(owner);
        self.get::<i64>(key)?
            .map(|value| {
                UtcInstant::try_from(value)
                    .map_err(|source| AccessSettingError::Deadline { key, source })
            })
            .transpose()
    }
    pub(super) fn put_deadline(
        &mut self,
        owner: SchedulerKind,
        at: UtcInstant,
    ) -> Result<(), AccessSettingError> {
        let key = SettingKey::deadline(owner);
        let millis =
            i64::try_from(at).map_err(|source| AccessSettingError::Deadline { key, source })?;
        self.put(key, &millis)
    }
    pub(super) fn first_day_of_week(
        &mut self,
    ) -> Result<FirstDayOfWeekSetting, AccessSettingError> {
        Ok(self
            .get::<FirstDayOfWeekSetting>(SettingKey::UiFirstDayOfWeek)?
            .unwrap_or_default())
    }
    pub(super) fn ui(&mut self) -> Result<UiSettings, AccessSettingError> {
        Ok(UiSettings {
            resident: ResidentSettings {
                enabled: self
                    .get::<ResidentEnabled>(SettingKey::UiResidentEnabled)?
                    .unwrap_or_default(),
            },
            prewarm: PrewarmSettings {
                enabled: self
                    .get::<PrewarmEnabled>(SettingKey::UiPrewarmEnabled)?
                    .unwrap_or_default(),
                on_battery: self
                    .get::<PrewarmOnBattery>(SettingKey::UiPrewarmOnBattery)?
                    .unwrap_or_default(),
            },
            language: self
                .get::<UiLanguageSetting>(SettingKey::UiLanguage)?
                .unwrap_or_default(),
            hour_cycle: self
                .get::<HourCycleSetting>(SettingKey::UiHourCycle)?
                .unwrap_or_default(),
            first_day_of_week: self.first_day_of_week()?,
        })
    }
    fn recover_ui_value<T: DeserializeOwned + Serialize + Default>(
        &mut self,
        key: SettingKey,
    ) -> Result<(), AccessSettingError> {
        match self.get::<T>(key) {
            Ok(_) => Ok(()),
            Err(error @ AccessSettingError::Json { .. }) => {
                tracing::warn!(%key, %error, "recovering corrupt UI setting to its default");
                self.put(key, &T::default())
            }
            Err(error) => Err(error),
        }
    }
    pub(super) fn update_ui(
        &mut self,
        request: SettingsSetRequest,
    ) -> Result<UiSettings, AccessSettingError> {
        if let Some(value) = request.resident.and_then(|resident| resident.enabled) {
            self.put(SettingKey::UiResidentEnabled, &value)?;
        }
        if let Some(prewarm) = request.prewarm {
            if let Some(value) = prewarm.enabled {
                self.put(SettingKey::UiPrewarmEnabled, &value)?;
            }
            if let Some(value) = prewarm.on_battery {
                self.put(SettingKey::UiPrewarmOnBattery, &value)?;
            }
        }
        if let Some(value) = request.language {
            self.put(SettingKey::UiLanguage, &value)?;
        }
        if let Some(value) = request.hour_cycle {
            self.put(SettingKey::UiHourCycle, &value)?;
        }
        if let Some(value) = request.first_day_of_week {
            self.put(SettingKey::UiFirstDayOfWeek, &value)?;
        }
        self.ui()
    }
    #[cfg(test)]
    pub(super) fn corrupt(
        &mut self,
        key: SettingKey,
        value: &str,
    ) -> Result<(), diesel::result::Error> {
        self.put_raw(key, value)
    }
    #[cfg(test)]
    pub(super) fn configuration_count(&mut self) -> Result<i64, diesel::result::Error> {
        settings::table
            .filter(settings::key.ne(SettingKey::MetadataVersion.as_str()))
            .count()
            .get_result(self.connection)
    }
}
impl Store {
    #[cfg(test)]
    pub(crate) fn test_day_offset_write(
        &mut self,
        offset_min: i64,
    ) -> Result<(), super::versions::BumpVersionError> {
        self.write(|tx| {
            Settings::new(tx.connection).put_day_start_offset(offset_min)?;
            tx.effects.bump_metadata(tx.connection)
        })
    }
    pub(crate) fn day_start_offset(&mut self) -> Result<DayStartOffset, AccessSettingError> {
        Settings::new(&mut self.writer).day_start_offset()
    }
    pub(crate) fn ui_settings(&mut self) -> Result<UiSettings, AccessSettingError> {
        Settings::new(&mut self.writer).ui()
    }
    pub(crate) fn recover_ui_settings(&mut self) -> Result<UiSettings, AccessSettingError> {
        self.write(|tx| {
            let mut settings = Settings::new(tx.connection);
            settings.recover_ui_value::<ResidentEnabled>(SettingKey::UiResidentEnabled)?;
            settings.recover_ui_value::<PrewarmEnabled>(SettingKey::UiPrewarmEnabled)?;
            settings.recover_ui_value::<PrewarmOnBattery>(SettingKey::UiPrewarmOnBattery)?;
            settings.recover_ui_value::<UiLanguageSetting>(SettingKey::UiLanguage)?;
            settings.recover_ui_value::<HourCycleSetting>(SettingKey::UiHourCycle)?;
            settings.recover_ui_value::<FirstDayOfWeekSetting>(SettingKey::UiFirstDayOfWeek)?;
            settings.ui()
        })
    }
    pub(crate) fn update_ui_settings(
        &mut self,
        request: SettingsSetRequest,
    ) -> Result<UiSettings, AccessSettingError> {
        self.write(|tx| {
            let before = Settings::new(tx.connection).ui()?;
            let updated = Settings::new(tx.connection).update_ui(request)?;
            if updated != before {
                tx.effects.bump_metadata(tx.connection).map_err(|source| {
                    AccessSettingError::Version {
                        source: Box::new(source),
                    }
                })?;
            }
            Ok(updated)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_directory::DataDirectory;

    #[test]
    fn typed_values_round_trip_for_every_active_key() {
        let directory = tempfile::tempdir().expect("isolated settings");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        let mut settings = Settings::new(&mut store.writer);
        assert_eq!(settings.ui().expect("defaults"), UiSettings::default());
        assert_eq!(
            settings.day_start_offset().expect("default day").minutes(),
            DEFAULT_DAY_START_OFFSET_MIN
        );
        settings.put_metadata_version(42).expect("metadata write");
        assert_eq!(settings.metadata_version().expect("metadata read"), 42);
        settings
            .put_device_id("installation-guid")
            .expect("device write");
        assert_eq!(
            settings.device_id().expect("device read").as_deref(),
            Some("installation-guid")
        );
        settings
            .put_day_start_offset(MAX_DAY_START_OFFSET_MIN)
            .expect("day write");
        assert_eq!(
            settings.day_start_offset().expect("day read").minutes(),
            MAX_DAY_START_OFFSET_MIN
        );
        assert!(
            settings
                .put_day_start_offset(MAX_DAY_START_OFFSET_MIN + 1)
                .is_err()
        );
        assert!(settings.put_metadata_version(-1).is_err());
        for owner in [
            SchedulerKind::Maintenance,
            SchedulerKind::Idle,
            SchedulerKind::Midnight,
        ] {
            let deadline = UtcInstant::try_from(123).expect("deadline");
            settings
                .put_deadline(owner, deadline)
                .expect("deadline write");
            assert_eq!(
                settings.deadline(owner).expect("deadline read"),
                Some(deadline)
            );
            settings
                .remove(SettingKey::deadline(owner))
                .expect("deadline clear");
            assert_eq!(settings.deadline(owner).expect("cleared deadline"), None);
        }
        for language in [
            UiLanguageSetting::Auto,
            UiLanguageSetting::ZhCn,
            UiLanguageSetting::En,
        ] {
            let ui = settings
                .update_ui(SettingsSetRequest {
                    language: Some(language),
                    ..Default::default()
                })
                .expect("language write");
            assert_eq!(ui.language, language);
        }
        for hour_cycle in [
            HourCycleSetting::Auto,
            HourCycleSetting::H12,
            HourCycleSetting::H24,
        ] {
            let ui = settings
                .update_ui(SettingsSetRequest {
                    hour_cycle: Some(hour_cycle),
                    ..Default::default()
                })
                .expect("clock write");
            assert_eq!(ui.hour_cycle, hour_cycle);
        }
        for first_day_of_week in [
            FirstDayOfWeekSetting::Auto,
            FirstDayOfWeekSetting::Monday,
            FirstDayOfWeekSetting::Tuesday,
            FirstDayOfWeekSetting::Wednesday,
            FirstDayOfWeekSetting::Thursday,
            FirstDayOfWeekSetting::Friday,
            FirstDayOfWeekSetting::Saturday,
            FirstDayOfWeekSetting::Sunday,
        ] {
            let ui = settings
                .update_ui(SettingsSetRequest {
                    first_day_of_week: Some(first_day_of_week),
                    ..Default::default()
                })
                .expect("weekday write");
            assert_eq!(ui.first_day_of_week, first_day_of_week);
        }
        settings
            .corrupt(SettingKey::RemovedPrivacyContextDeadline, "obsolete")
            .expect("removed key");
        assert!(
            settings
                .contains(SettingKey::RemovedPrivacyContextDeadline)
                .expect("presence without decoding")
        );
    }

    #[test]
    fn corrupt_ui_values_fail_typed_reads_and_startup_logs_and_repairs_them() {
        let directory = tempfile::tempdir().expect("isolated recovery");
        let log = tempfile::NamedTempFile::new().expect("isolated log");
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(log.reopen().expect("writer")))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
            for key in [
                SettingKey::UiLanguage,
                SettingKey::UiHourCycle,
                SettingKey::UiFirstDayOfWeek,
                SettingKey::UiPrewarmEnabled,
                SettingKey::UiPrewarmOnBattery,
            ] {
                Settings::new(&mut store.writer)
                    .corrupt(key, "\"unknown\"")
                    .expect("corruption");
                assert!(
                    matches!(store.ui_settings(), Err(AccessSettingError::Json { key: actual, .. }) if actual == key)
                );
                assert_eq!(
                    store.recover_ui_settings().expect("explicit recovery"),
                    UiSettings::default()
                );
                assert_eq!(
                    store.ui_settings().expect("persisted auto"),
                    UiSettings::default()
                );
            }
        });
        let logged = std::fs::read_to_string(log.path()).expect("warning log");
        assert!(logged.contains("WARN"));
        for key in [
            SettingKey::UiLanguage,
            SettingKey::UiHourCycle,
            SettingKey::UiFirstDayOfWeek,
            SettingKey::UiPrewarmEnabled,
            SettingKey::UiPrewarmOnBattery,
        ] {
            assert!(logged.contains(key.as_str()));
        }
        assert!(logged.contains("recovering corrupt UI setting to its default"));
    }

    #[test]
    fn partial_update_rolls_back_when_another_stored_ui_value_is_corrupt() {
        let directory = tempfile::tempdir().expect("isolated atomic settings");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        Settings::new(&mut store.writer)
            .corrupt(SettingKey::UiHourCycle, "null")
            .expect("corruption");
        assert!(
            store
                .update_ui_settings(SettingsSetRequest {
                    language: Some(UiLanguageSetting::ZhCn),
                    ..Default::default()
                })
                .is_err()
        );
        assert!(
            !Settings::new(&mut store.writer)
                .contains(SettingKey::UiLanguage)
                .expect("rolled back language")
        );
    }
    #[test]
    fn scheduler_deadlines_reject_precision_loss() {
        let directory = tempfile::tempdir().expect("isolated deadline precision");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        let timestamp: jiff::Timestamp = "2025-01-02T00:00:00.000001Z"
            .parse()
            .expect("submillisecond timestamp");
        let mut settings = Settings::new(&mut store.writer);
        assert!(matches!(
            settings.put_deadline(SchedulerKind::Idle, UtcInstant::new(timestamp)),
            Err(AccessSettingError::Deadline {
                source: quartz_domain::capture::AdvanceTimeError::Submillisecond,
                ..
            })
        ));
        assert_eq!(
            settings
                .deadline(SchedulerKind::Idle)
                .expect("unchanged deadline"),
            None
        );
    }
    #[test]
    fn prewarm_settings_persist_defaults_partial_updates_and_metadata_versions() {
        use quartz_protocol::PrewarmUpdate;
        let directory = tempfile::tempdir().expect("isolated prewarm settings");
        let data = DataDirectory::new(directory.path());
        let mut store = Store::open(&data).expect("store");
        assert_eq!(
            store.ui_settings().expect("defaults").prewarm,
            PrewarmSettings::default()
        );
        let before = Settings::new(&mut store.writer)
            .metadata_version()
            .expect("version");
        let update = SettingsSetRequest {
            prewarm: Some(PrewarmUpdate {
                enabled: Some(PrewarmEnabled::Disabled),
                ..Default::default()
            }),
            ..Default::default()
        };
        let saved = store
            .update_ui_settings(update)
            .expect("save disabled prewarm");
        assert_eq!(saved.prewarm.enabled, PrewarmEnabled::Disabled);
        assert_eq!(saved.prewarm.on_battery, PrewarmOnBattery::Blocked);
        assert_eq!(
            Settings::new(&mut store.writer)
                .metadata_version()
                .expect("version"),
            before + 1
        );
        store.update_ui_settings(update).expect("idempotent update");
        assert_eq!(
            Settings::new(&mut store.writer)
                .metadata_version()
                .expect("version"),
            before + 1
        );
        store
            .update_ui_settings(SettingsSetRequest {
                prewarm: Some(PrewarmUpdate {
                    on_battery: Some(PrewarmOnBattery::Allowed),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .expect("battery update");
        assert_eq!(
            Settings::new(&mut store.writer)
                .metadata_version()
                .expect("version"),
            before + 2
        );
        drop(store);
        let mut reopened = Store::open(&data).expect("reopen");
        assert_eq!(
            reopened.ui_settings().expect("persisted").prewarm,
            PrewarmSettings {
                enabled: PrewarmEnabled::Disabled,
                on_battery: PrewarmOnBattery::Allowed
            }
        );
        Settings::new(&mut reopened.writer)
            .corrupt(SettingKey::UiPrewarmEnabled, "null")
            .expect("corrupt prewarm setting");
        assert!(reopened.ui_settings().is_err());
        assert_eq!(
            reopened
                .recover_ui_settings()
                .expect("repair")
                .prewarm
                .enabled,
            PrewarmEnabled::Enabled
        );
    }
    #[test]
    fn prewarm_update_rolls_back_when_versioning_fails() {
        let directory = tempfile::tempdir().expect("isolated version failure");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        Settings::new(&mut store.writer)
            .put_metadata_version(i64::MAX)
            .expect("exhaust metadata version");
        let result = store.update_ui_settings(SettingsSetRequest {
            prewarm: Some(quartz_protocol::PrewarmUpdate {
                enabled: Some(PrewarmEnabled::Disabled),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert!(matches!(result, Err(AccessSettingError::Version { .. })));
        assert_eq!(
            store.ui_settings().expect("rolled back").prewarm,
            PrewarmSettings::default()
        );
    }
    #[test]
    fn resident_setting_defaults_versions_persists_and_recovers() {
        let directory = tempfile::tempdir().expect("resident settings");
        let data = DataDirectory::new(directory.path());
        let mut store = Store::open(&data).expect("store");
        assert_eq!(
            store.ui_settings().expect("default").resident.enabled,
            ResidentEnabled::Enabled
        );
        let before = Settings::new(&mut store.writer)
            .metadata_version()
            .expect("version");
        let request = SettingsSetRequest {
            resident: Some(quartz_protocol::ResidentUpdate {
                enabled: Some(ResidentEnabled::Disabled),
            }),
            ..Default::default()
        };
        store.update_ui_settings(request).expect("update");
        store.update_ui_settings(request).expect("idempotent");
        assert_eq!(
            Settings::new(&mut store.writer)
                .metadata_version()
                .expect("version"),
            before + 1
        );
        drop(store);
        let mut store = Store::open(&data).expect("reopen");
        assert_eq!(
            store.ui_settings().expect("stored").resident.enabled,
            ResidentEnabled::Disabled
        );
        Settings::new(&mut store.writer)
            .corrupt(SettingKey::UiResidentEnabled, "null")
            .expect("corrupt");
        assert!(store.ui_settings().is_err());
        assert_eq!(
            store
                .recover_ui_settings()
                .expect("recovered")
                .resident
                .enabled,
            ResidentEnabled::Enabled
        );
    }
}
