//! Version rows, settings keys, and transaction-local invalidation effects.
use super::{
    ReadOnlyStore, Store,
    enums::{DataSource, IntegerCode},
    query::ReadSnapshot,
    query_error::ReadQueryError,
    rows::{DataDayVersionsRow, SettingsRow},
    schema::{data_day_versions, settings},
};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl, SelectableHelper};
use quartz_domain::{capture::limits::UTC_DAY_MS, facts::QueryRange};
use std::{collections::BTreeMap, ops::RangeInclusive};

/// Failure to persist a fact-day or metadata version in the current transaction.
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum BumpVersionError {
    #[snafu(display("could not write query version"))]
    Database { source: diesel::result::Error },
    #[snafu(display("version range ends before it starts"))]
    InvalidRange,
    #[snafu(display("could not decode metadata version"))]
    DecodeMetadata { source: serde_json::Error },
    #[snafu(display("invalid metadata version in settings"))]
    MetadataVersion,
}
impl From<diesel::result::Error> for BumpVersionError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}

pub(crate) const METADATA_VERSION_KEY: &str = "metadata_version";
pub(crate) const DEVICE_ID_KEY: &str = "device_id";
pub(crate) const DAY_START_OFFSET_KEY: &str = "day.start_offset_min";
/// Missing day offset means local midnight (03 §1).
pub(crate) const DEFAULT_DAY_START_OFFSET_MIN: i64 = 0;
/// Day offsets are limited to twelve hours (03 §1).
pub(crate) const MAX_DAY_START_OFFSET_MIN: i64 = 720;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct UtcDay(pub i64);
/// UTC days touched by a half-open UTC range; an empty range touches its start day.
#[derive(Debug)]
pub(crate) struct UtcDays;
impl UtcDays {
    /// The caller supplies an ordered range.
    pub(crate) fn covering(range: QueryRange) -> RangeInclusive<UtcDay> {
        assert!(range.start <= range.end, "UTC day range must be ordered");
        let last = if range.end == range.start {
            range.start
        } else {
            range.end - 1
        };
        UtcDay(range.start.div_euclid(UTC_DAY_MS))..=UtcDay(last.div_euclid(UTC_DAY_MS))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VersionSource(pub i32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VersionKey {
    pub source: VersionSource,
    pub day: UtcDay,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct FactVersion {
    pub key: VersionKey,
    pub version: i64,
}

impl From<DataDayVersionsRow> for FactVersion {
    fn from(row: DataDayVersionsRow) -> Self {
        Self {
            key: VersionKey {
                source: VersionSource(row.source.code()),
                day: UtcDay(row.utc_date),
            },
            version: row.version,
        }
    }
}
#[derive(Debug, Clone)]
pub(crate) struct StoredVersions {
    pub days: BTreeMap<VersionKey, i64>,
    pub settings: QuerySettings,
}
#[derive(Debug, Clone)]
pub(crate) struct QuerySettings {
    pub day_start_offset_min: i64,
    pub metadata_version: i64,
}

impl ReadOnlyStore {
    pub(crate) fn query_settings(&mut self) -> Result<QuerySettings, ReadQueryError> {
        let rows = settings::table
            .filter(settings::key.eq_any([DAY_START_OFFSET_KEY, METADATA_VERSION_KEY]))
            .select((settings::key, settings::value))
            .load::<SettingsRow>(&mut self.connection)?;
        let mut offset = DEFAULT_DAY_START_OFFSET_MIN;
        let mut metadata = 0;
        for row in rows {
            match row.key.as_str() {
                DAY_START_OFFSET_KEY => {
                    offset = serde_json::from_str(&row.value)
                        .map_err(|_| ReadQueryError::InvalidSettings)?
                }
                METADATA_VERSION_KEY => {
                    metadata = serde_json::from_str(&row.value)
                        .map_err(|_| ReadQueryError::InvalidSettings)?
                }
                _ => unreachable!("settings selected by explicit key list"),
            }
        }
        if !(DEFAULT_DAY_START_OFFSET_MIN..=MAX_DAY_START_OFFSET_MIN).contains(&offset)
            || metadata < 0
        {
            return Err(ReadQueryError::InvalidSettings);
        }
        Ok(QuerySettings {
            day_start_offset_min: offset,
            metadata_version: metadata,
        })
    }
}
impl Store {
    #[cfg(any(windows, test))]
    pub(crate) fn version_state(&mut self) -> Result<StoredVersions, ReadQueryError> {
        let settings = self
            .reader
            .as_mut()
            .ok_or(ReadQueryError::ReaderUnavailable)?
            .query_settings()?;
        let rows = data_day_versions::table
            .select(DataDayVersionsRow::as_select())
            .load::<DataDayVersionsRow>(&mut self.writer)?;
        Ok(StoredVersions {
            days: rows
                .into_iter()
                .map(FactVersion::from)
                .map(|fact| (fact.key, fact.version))
                .collect(),
            settings,
        })
    }
}
impl ReadSnapshot {
    pub(crate) fn versions(
        &mut self,
        range: QueryRange,
    ) -> Result<Vec<FactVersion>, ReadQueryError> {
        let days = UtcDays::covering(range);
        let rows = data_day_versions::table
            .filter(data_day_versions::utc_date.ge(days.start().0))
            .filter(data_day_versions::utc_date.le(days.end().0))
            .order((data_day_versions::source, data_day_versions::utc_date))
            .select(DataDayVersionsRow::as_select())
            .load::<DataDayVersionsRow>(self.connection())?;
        Ok(rows.into_iter().map(FactVersion::from).collect())
    }
}
/// Effects become publishable only after the enclosing SQLite transaction commits.
#[derive(Debug, Default)]
pub(crate) struct TxEffects {
    pub days: BTreeMap<VersionKey, i64>,
    pub metadata: Option<i64>,
}
impl TxEffects {
    pub(super) fn merge(&mut self, changes: Self) {
        self.days.extend(changes.days);
        if changes.metadata.is_some() {
            self.metadata = changes.metadata;
        }
    }
    pub(super) fn bump_days(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        source: DataSource,
        range: QueryRange,
    ) -> Result<(), BumpVersionError> {
        if range.end < range.start {
            return Err(BumpVersionError::InvalidRange);
        }
        let days = UtcDays::covering(range);
        for day in days.start().0..=days.end().0 {
            let version = diesel::insert_into(data_day_versions::table)
                .values((
                    data_day_versions::source.eq(source),
                    data_day_versions::utc_date.eq(day),
                    data_day_versions::version.eq(1_i64),
                ))
                .on_conflict((data_day_versions::source, data_day_versions::utc_date))
                .do_update()
                .set(data_day_versions::version.eq(data_day_versions::version + 1_i64))
                .returning(data_day_versions::version)
                .get_result(connection)?;
            self.days.insert(
                VersionKey {
                    source: VersionSource(source.code()),
                    day: UtcDay(day),
                },
                version,
            );
        }
        Ok(())
    }
    pub(super) fn bump_metadata(
        &mut self,
        connection: &mut diesel::SqliteConnection,
    ) -> Result<(), BumpVersionError> {
        let value = settings::table
            .filter(settings::key.eq(METADATA_VERSION_KEY))
            .select(settings::value)
            .first::<String>(connection)
            .optional()?;
        let version: i64 = match value {
            Some(value) => serde_json::from_str(&value)
                .map_err(|source| BumpVersionError::DecodeMetadata { source })?,
            None => 0,
        };
        let version = version
            .checked_add(1)
            .filter(|version| *version > 0)
            .ok_or(BumpVersionError::MetadataVersion)?;
        diesel::insert_into(settings::table)
            .values((
                settings::key.eq(METADATA_VERSION_KEY),
                settings::value.eq(version.to_string()),
            ))
            .on_conflict(settings::key)
            .do_update()
            .set(settings::value.eq(version.to_string()))
            .execute(connection)?;
        self.metadata = Some(version);
        Ok(())
    }
}
#[derive(Debug)]
pub(crate) struct CommittedVersions {
    pub facts: Vec<FactVersion>,
    pub metadata_version: Option<i64>,
}
impl Store {
    #[cfg(any(windows, test))]
    pub(crate) fn take_committed_versions(&mut self) -> CommittedVersions {
        let effects = std::mem::take(&mut self.pending_effects);
        CommittedVersions {
            facts: effects
                .days
                .into_iter()
                .map(|(key, version)| FactVersion { key, version })
                .collect(),
            metadata_version: effects.metadata,
        }
    }
}

pub(crate) const MAINTENANCE_DEADLINE_KEY: &str = "capture.deadline.maintenance";
pub(crate) const IDLE_DEADLINE_KEY: &str = "capture.deadline.idle";
/// Obsolete browser-context deferral key, recognized only to warn during recovery.
pub(crate) const REMOVED_PRIVACY_CONTEXT_DEADLINE_KEY: &str = "capture.deadline.privacy_context";
pub(crate) const MIDNIGHT_DEADLINE_KEY: &str = "capture.deadline.midnight";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_directory::DataDirectory;

    #[test]
    fn empty_ranges_cover_the_start_day_including_negative_utc() {
        assert_eq!(
            UtcDays::covering(QueryRange { start: 0, end: 0 }),
            UtcDay(0)..=UtcDay(0)
        );
        assert_eq!(
            UtcDays::covering(QueryRange {
                start: UTC_DAY_MS,
                end: UTC_DAY_MS
            }),
            UtcDay(1)..=UtcDay(1)
        );
        assert_eq!(
            UtcDays::covering(QueryRange { start: -1, end: -1 }),
            UtcDay(-1)..=UtcDay(-1)
        );
        assert_eq!(
            UtcDays::covering(QueryRange {
                start: i64::MIN,
                end: i64::MIN
            }),
            UtcDay(i64::MIN.div_euclid(UTC_DAY_MS))..=UtcDay(i64::MIN.div_euclid(UTC_DAY_MS))
        );
    }

    #[test]
    fn exact_day_end_belongs_to_the_previous_day() {
        assert_eq!(
            UtcDays::covering(QueryRange {
                start: 0,
                end: UTC_DAY_MS
            }),
            UtcDay(0)..=UtcDay(0)
        );
        assert_eq!(
            UtcDays::covering(QueryRange {
                start: -UTC_DAY_MS,
                end: 0
            }),
            UtcDay(-1)..=UtcDay(-1)
        );
    }

    #[test]
    fn multi_day_ranges_include_each_touched_day() {
        assert_eq!(
            UtcDays::covering(QueryRange {
                start: UTC_DAY_MS - 1,
                end: 3 * UTC_DAY_MS + 1
            }),
            UtcDay(0)..=UtcDay(3)
        );
    }

    #[test]
    fn write_publishes_only_committed_effects_and_retains_prior_effects_on_rollback() {
        let directory = tempfile::tempdir().expect("isolated versions");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("initial metadata");
        let failed: Result<(), BumpVersionError> = store.write(|tx| {
            tx.effects.bump_metadata(tx.connection)?;
            tx.effects.bump_days(
                tx.connection,
                DataSource::Intervals,
                QueryRange {
                    start: 0,
                    end: UTC_DAY_MS,
                },
            )?;
            Err(diesel::result::Error::RollbackTransaction.into())
        });
        assert!(failed.is_err());
        let changes = store.take_committed_versions();
        assert_eq!(changes.metadata_version, Some(1));
        assert!(changes.facts.is_empty());
        let stored = store.version_state().expect("rolled back versions");
        assert_eq!(stored.settings.metadata_version, 1);
        assert!(stored.days.is_empty());
        store
            .write::<_, BumpVersionError>(|tx| {
                tx.effects.bump_days(
                    tx.connection,
                    DataSource::Intervals,
                    QueryRange {
                        start: UTC_DAY_MS,
                        end: UTC_DAY_MS,
                    },
                )?;
                tx.effects.bump_metadata(tx.connection)
            })
            .expect("commit versions");
        let changes = store.take_committed_versions();
        assert_eq!(changes.metadata_version, Some(2));
        assert_eq!(changes.facts.len(), 1);
        assert_eq!(changes.facts[0].key.day, UtcDay(1));
        let failed = store.write(|tx| {
            tx.effects.bump_days(
                tx.connection,
                DataSource::Intervals,
                QueryRange {
                    start: UTC_DAY_MS,
                    end: 0,
                },
            )
        });
        assert!(matches!(failed, Err(BumpVersionError::InvalidRange)));
        assert!(store.take_committed_versions().facts.is_empty());
    }

    #[test]
    fn missing_version_reader_reports_reader_unavailable() {
        let directory = tempfile::tempdir().expect("isolated reader");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        let _reader = store.take_query_reader().expect("reader");
        assert!(matches!(
            store.version_state(),
            Err(ReadQueryError::ReaderUnavailable)
        ));
    }
}
