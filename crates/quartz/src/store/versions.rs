//! Version rows and transaction-local invalidation effects.
use super::{
    ReadOnlyStore, Store,
    enums::{DataSource, IntegerCode},
    query::ReadSnapshot,
    query_error::ReadQueryError,
    rows::DataDayVersionsRow,
    schema::data_day_versions,
    settings::Settings,
};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl, SelectableHelper};
use quartz_domain::{capture::limits::UTC_DAY_MS, facts::QueryRange};
use std::{collections::BTreeMap, ops::RangeInclusive};

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum TestCommit {
    Commit,
    Rollback,
}
/// Failure to persist a fact-day or metadata version in the current transaction.
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum BumpVersionError {
    #[snafu(display("could not write query version"))]
    Database { source: diesel::result::Error },
    #[snafu(display("version range is reversed or exceeds the annotation span limit"))]
    InvalidRange,
    #[snafu(display("day version counter overflowed"))]
    DayVersion,
    #[snafu(display("annotation day-version work exceeds the transaction limit"))]
    AnnotationWork,
    #[snafu(display("could not access metadata version"))]
    Setting {
        source: crate::store::settings::AccessSettingError,
    },
    #[snafu(display("invalid metadata version in settings"))]
    MetadataVersion,
}
impl From<diesel::result::Error> for BumpVersionError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}

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
impl VersionSource {
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        use strum::VariantArray;
        DataSource::VARIANTS
            .iter()
            .map(|source| Self(source.code()))
    }
}
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
    pub day_start_offset: crate::query::calendar::DayStartOffset,
    pub metadata_version: i64,
    pub stats_policy_version: i64,
}

impl Settings<'_> {
    fn query_settings(&mut self) -> Result<QuerySettings, ReadQueryError> {
        Ok(QuerySettings {
            day_start_offset: self.day_start_offset()?,
            metadata_version: self.metadata_version()?,
            stats_policy_version: self.stats_policy_version()?,
        })
    }
}
impl ReadOnlyStore {
    pub(crate) fn query_settings(&mut self) -> Result<QuerySettings, ReadQueryError> {
        Settings::new(&mut self.connection).query_settings()
    }
}
impl Store {
    #[cfg(any(windows, test))]
    pub(crate) fn query_settings(&mut self) -> Result<QuerySettings, ReadQueryError> {
        Settings::new(&mut self.writer).query_settings()
    }
    #[cfg(any(windows, test))]
    pub(crate) fn range_versions(
        &mut self,
        range: QueryRange,
    ) -> Result<BTreeMap<VersionKey, i64>, ReadQueryError> {
        let days = UtcDays::covering(range);
        let rows = data_day_versions::table
            .filter(data_day_versions::utc_date.ge(days.start().0))
            .filter(data_day_versions::utc_date.le(days.end().0))
            .order((data_day_versions::source, data_day_versions::utc_date))
            .select(DataDayVersionsRow::as_select())
            .load::<DataDayVersionsRow>(&mut self.writer)?;
        Ok(rows
            .into_iter()
            .map(FactVersion::from)
            .map(|fact| (fact.key, fact.version))
            .collect())
    }
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
    annotation_day_writes: i64,
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
        if source == DataSource::Annotations {
            if range
                .end
                .checked_sub(range.start)
                .is_none_or(|duration| duration > crate::query::MAX_QUERY_RANGE_MS)
            {
                return Err(BumpVersionError::InvalidRange);
            }
            let work = days.end().0 - days.start().0 + 1;
            self.annotation_day_writes = self
                .annotation_day_writes
                .checked_add(work)
                .filter(|total| *total <= super::annotations::MAX_ANNOTATION_DAY_WRITES)
                .ok_or(BumpVersionError::AnnotationWork)?;
        }
        for day in days.start().0..=days.end().0 {
            self.bump_day(connection, source, UtcDay(day))?;
        }
        Ok(())
    }
    pub(super) fn bump_day(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        source: DataSource,
        day: UtcDay,
    ) -> Result<(), BumpVersionError> {
        let previous = data_day_versions::table
            .filter(data_day_versions::source.eq(source))
            .filter(data_day_versions::utc_date.eq(day.0))
            .select(data_day_versions::version)
            .first::<i64>(connection)
            .optional()?;
        let version = match previous {
            Some(version) => version.checked_add(1).ok_or(BumpVersionError::DayVersion)?,
            None => 1,
        };
        diesel::insert_into(data_day_versions::table)
            .values((
                data_day_versions::source.eq(source),
                data_day_versions::utc_date.eq(day.0),
                data_day_versions::version.eq(version),
            ))
            .on_conflict((data_day_versions::source, data_day_versions::utc_date))
            .do_update()
            .set(data_day_versions::version.eq(version))
            .execute(connection)?;
        self.days.insert(
            VersionKey {
                source: VersionSource(source.code()),
                day,
            },
            version,
        );
        Ok(())
    }
    pub(super) fn bump_metadata(
        &mut self,
        connection: &mut diesel::SqliteConnection,
    ) -> Result<(), BumpVersionError> {
        let mut settings = Settings::new(connection);
        let version = settings.metadata_version()?;
        let version = version
            .checked_add(1)
            .ok_or(BumpVersionError::MetadataVersion)?;
        settings.put_metadata_version(version)?;
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
    #[cfg(test)]
    pub(crate) fn test_metadata_write(
        &mut self,
        outcome: TestCommit,
    ) -> Result<(), BumpVersionError> {
        self.write(|tx| {
            tx.effects.bump_metadata(tx.connection)?;
            match outcome {
                TestCommit::Commit => Ok(()),
                TestCommit::Rollback => Err(diesel::result::Error::RollbackTransaction.into()),
            }
        })
    }
    #[cfg(any(windows, test))]
    pub(crate) fn has_committed_metadata(&self) -> bool {
        self.pending_effects.metadata.is_some()
    }
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

impl From<super::settings::AccessSettingError> for BumpVersionError {
    fn from(source: crate::store::settings::AccessSettingError) -> Self {
        Self::Setting { source }
    }
}

impl Store {
    /// Reads the single metadata counter without scanning fact-day inputs.
    pub(crate) fn rule_read_metadata(
        &mut self,
    ) -> Result<i64, super::settings::AccessSettingError> {
        Settings::new(&mut self.writer).metadata_version()
    }
}

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
    #[test]
    fn day_counter_overflow_rolls_back_rows_and_effects() {
        let directory = tempfile::tempdir().expect("isolated versions");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        diesel::insert_into(data_day_versions::table)
            .values(DataDayVersionsRow {
                source: DataSource::Annotations,
                utc_date: 1,
                version: i64::MAX,
            })
            .execute(&mut store.writer)
            .expect("maximum valid counter");
        let result = store.write(|tx| {
            tx.effects.bump_metadata(tx.connection)?;
            tx.effects.bump_days(
                tx.connection,
                DataSource::Annotations,
                QueryRange {
                    start: 0,
                    end: 2 * UTC_DAY_MS,
                },
            )
        });
        assert!(
            matches!(result, Err(BumpVersionError::DayVersion)),
            "overflow must reject the transaction"
        );
        let rows = data_day_versions::table
            .select(DataDayVersionsRow::as_select())
            .load::<DataDayVersionsRow>(&mut store.writer)
            .expect("versions");
        assert_eq!(rows.len(), 1, "first day's insertion must roll back");
        assert_eq!(rows[0].version, i64::MAX);
        let effects = store.take_committed_versions();
        assert!(effects.facts.is_empty());
        assert_eq!(effects.metadata_version, None);
        assert_eq!(store.rule_read_metadata().expect("metadata"), 0);
    }

    #[test]
    fn day_bumps_reject_unbounded_work_before_sql() {
        use diesel::connection::SimpleConnection;
        let directory = tempfile::tempdir().expect("isolated versions");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store
            .writer
            .batch_execute("PRAGMA query_only = ON")
            .expect("deny writes");
        let mut effects = TxEffects::default();
        let result = effects.bump_days(
            &mut store.writer,
            DataSource::Annotations,
            QueryRange {
                start: 0,
                end: i64::MAX,
            },
        );
        assert!(matches!(result, Err(BumpVersionError::InvalidRange)));
        assert!(store.take_committed_versions().facts.is_empty());
    }
    #[test]
    fn capture_day_bumps_allow_ranges_longer_than_one_year() {
        let directory = tempfile::tempdir().expect("isolated capture versions");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        let range = QueryRange {
            start: -1,
            end: crate::query::MAX_QUERY_RANGE_MS + UTC_DAY_MS,
        };
        let days = UtcDays::covering(range);
        for source in [
            DataSource::Intervals,
            DataSource::Availability,
            DataSource::Web,
            DataSource::Gaps,
        ] {
            store
                .write::<_, BumpVersionError>(|tx| {
                    tx.effects.bump_days(tx.connection, source, range)
                })
                .expect("long capture range");
            let effects = store.take_committed_versions();
            assert_eq!(
                i64::try_from(effects.facts.len()).expect("day count"),
                days.end().0 - days.start().0 + 1
            );
            for fact in effects.facts {
                assert_eq!(fact.key.source, VersionSource(source.code()));
                assert_eq!(fact.version, 1);
                assert!(days.contains(&fact.key.day));
            }
        }
        let before = store.version_state().expect("capture versions").days;
        assert!(matches!(
            store.write(|tx| tx
                .effects
                .bump_days(tx.connection, DataSource::Annotations, range)),
            Err(BumpVersionError::InvalidRange)
        ));
        assert_eq!(
            store
                .version_state()
                .expect("unchanged capture versions")
                .days,
            before
        );
        assert!(store.take_committed_versions().facts.is_empty());
    }

    #[test]
    fn annotation_work_across_day_bumps_rolls_back_the_transaction() {
        let directory = tempfile::tempdir().expect("isolated annotation work");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        let days_per_call = crate::query::MAX_QUERY_RANGE_MS / UTC_DAY_MS;
        let calls = super::super::annotations::MAX_ANNOTATION_DAY_WRITES / days_per_call + 1;
        let mut successful_calls = 0;
        let result = store.write::<_, BumpVersionError>(|tx| {
            tx.effects.bump_metadata(tx.connection)?;
            for _ in 0..calls {
                tx.effects.bump_days(
                    tx.connection,
                    DataSource::Annotations,
                    QueryRange {
                        start: 0,
                        end: crate::query::MAX_QUERY_RANGE_MS,
                    },
                )?;
                successful_calls += 1;
            }
            Ok(())
        });
        assert!(matches!(result, Err(BumpVersionError::AnnotationWork)));
        assert_eq!(
            successful_calls,
            calls - 1,
            "earlier increments must have reached SQLite"
        );
        let stored = store.version_state().expect("rolled back state");
        assert!(stored.days.is_empty());
        assert_eq!(stored.settings.metadata_version, 0);
        let effects = store.take_committed_versions();
        assert!(effects.facts.is_empty());
        assert_eq!(effects.metadata_version, None);
    }
}
