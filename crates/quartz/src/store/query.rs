//! Bounded typed snapshot reads for the query pipeline; Diesel stays in storage.

use super::facts::UTC_DAY_MS;
use super::{
    ReadOnlyStore, Store,
    enums::{self, IntegerCode},
    rows::{
        AvailabilityIntervalsRow, CaptureGapsRow, CaptureRunsRow, ClockSegmentsRow,
        DataDayVersionsRow, IntervalsRow,
    },
    schema::{
        applications, availability_intervals, capture_gaps, capture_runs, clock_segments,
        data_day_versions, intervals, settings, titles,
    },
};
use crate::query::versions::{UtcDay, VersionKey, VersionSource, VersionState};
use crate::query::{
    RunQueryError,
    calendar::{DEFAULT_DAY_START_OFFSET_MIN, MAX_DAY_START_OFFSET_MIN},
    input::{
        Availability, CaptureLevel, ClockSegment, FactId, FactSpan, Foreground, ForegroundKind,
        InputRow, QueryRange,
    },
    pipeline::QUERY_BATCH_ROWS,
};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, JoinOnDsl, NullableExpressionMethods, QueryDsl,
    RunQueryDsl, SelectableHelper,
    connection::{AnsiTransactionManager, TransactionManager},
};
use quartz_protocol::{
    ApplicationId, ApplicationRef, ClockSegmentId, QueryGapReason, QueryStopReason, QueryVersions,
};

#[derive(Debug, Clone)]
pub(crate) struct QuerySettings {
    pub day_start_offset_min: i64,
    pub metadata_version: i64,
}
#[derive(Debug, diesel::Queryable)]
struct SettingRow {
    key: String,
    value: String,
}
impl ReadOnlyStore {
    pub(crate) fn query_settings(&mut self) -> Result<QuerySettings, RunQueryError> {
        let rows = settings::table
            .filter(settings::key.eq_any(["day.start_offset_min", "metadata_version"]))
            .select((settings::key, settings::value))
            .load::<SettingRow>(&mut self.connection)?;
        let mut offset = DEFAULT_DAY_START_OFFSET_MIN;
        let mut metadata = 0;
        for row in rows {
            match row.key.as_str() {
                "day.start_offset_min" => {
                    offset = serde_json::from_str(&row.value)
                        .map_err(|_| RunQueryError::InvalidSettings)?
                }
                "metadata_version" => {
                    metadata = serde_json::from_str(&row.value)
                        .map_err(|_| RunQueryError::InvalidSettings)?
                }
                _ => unreachable!("settings selected by explicit key list"),
            }
        }
        if !(DEFAULT_DAY_START_OFFSET_MIN..=MAX_DAY_START_OFFSET_MIN).contains(&offset)
            || metadata < 0
        {
            return Err(RunQueryError::InvalidSettings);
        }
        Ok(QuerySettings {
            day_start_offset_min: offset,
            metadata_version: metadata,
        })
    }
    pub(crate) fn snapshot(self) -> ReadSnapshot {
        ReadSnapshot {
            reader: Some(self),
            transaction: SnapshotTransaction::Pending,
            stage: ReadStage::Clock,
            after_start: i64::MIN,
            after_id: i64::from(i32::MIN),
        }
    }
}
impl Store {
    #[cfg(any(windows, test))]
    pub(crate) fn version_state(
        &mut self,
        classification_hash: &str,
    ) -> Result<VersionState, RunQueryError> {
        let settings = self
            ._reader
            .as_mut()
            .ok_or(RunQueryError::InvalidFacts)?
            .query_settings()?;
        let rows = data_day_versions::table
            .select(DataDayVersionsRow::as_select())
            .load::<DataDayVersionsRow>(&mut self._writer)?;
        Ok(VersionState {
            days: rows
                .into_iter()
                .map(|row| {
                    (
                        VersionKey {
                            source: VersionSource(row.source.code()),
                            day: UtcDay(row.utc_date),
                        },
                        row.version,
                    )
                })
                .collect(),
            settings,
            classification_hash: classification_hash.to_owned(),
        })
    }
    pub(crate) fn take_query_reader(&mut self) -> Option<ReadOnlyStore> {
        self._reader.take()
    }
    pub(crate) fn return_query_reader(&mut self, reader: ReadOnlyStore) {
        self._reader = Some(reader);
    }
}
/// Owns the read transaction across DB-loop turns, rolling it back on every exit path.
pub(crate) struct ReadSnapshot {
    reader: Option<ReadOnlyStore>,
    transaction: SnapshotTransaction,
    stage: ReadStage,
    after_start: i64,
    after_id: i64,
}
impl std::fmt::Debug for ReadSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadSnapshot")
            .field("stage", &self.stage)
            .finish_non_exhaustive()
    }
}
impl Drop for ReadSnapshot {
    fn drop(&mut self) {
        if self.transaction == SnapshotTransaction::Started
            && let Some(reader) = &mut self.reader
            && let Err(error) = AnsiTransactionManager::rollback_transaction(&mut reader.connection)
        {
            tracing::error!(%error, "could not roll back query snapshot");
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotTransaction {
    Pending,
    Started,
}
#[derive(Debug, Clone, Copy)]
enum ReadStage {
    Clock,
    ForegroundClosed,
    ForegroundOpen,
    AvailabilityClosed,
    AvailabilityOpen,
    Gaps,
    Done,
}
#[derive(Debug, diesel::Queryable)]
struct ClockDetails {
    clock: ClockSegmentsRow,
    run: CaptureRunsRow,
}
#[derive(Debug, diesel::Queryable)]
struct ForegroundDetails {
    fact: IntervalsRow,
    app_id: Option<i64>,
    exe_name: Option<String>,
    display_name: Option<String>,
    aumid: Option<String>,
    title: Option<String>,
}
impl ReadSnapshot {
    fn connection(&mut self) -> &mut diesel::SqliteConnection {
        &mut self
            .reader
            .as_mut()
            .expect("live snapshot reader")
            .connection
    }
    pub(crate) fn settings(&mut self) -> Result<QuerySettings, RunQueryError> {
        self.reader
            .as_mut()
            .expect("live snapshot reader")
            .query_settings()
    }
    pub(crate) fn versions(
        &mut self,
        range: QueryRange,
        settings: &QuerySettings,
        classification_hash: &str,
    ) -> Result<QueryVersions, RunQueryError> {
        let rows = data_day_versions::table
            .filter(data_day_versions::utc_date.ge(range.start.div_euclid(UTC_DAY_MS)))
            .filter(data_day_versions::utc_date.le((range.end - 1).div_euclid(UTC_DAY_MS)))
            .order((data_day_versions::source, data_day_versions::utc_date))
            .select(DataDayVersionsRow::as_select())
            .load::<DataDayVersionsRow>(self.connection())?;
        Ok(VersionState {
            days: rows
                .into_iter()
                .map(|row| {
                    (
                        VersionKey {
                            source: VersionSource(row.source.code()),
                            day: UtcDay(row.utc_date),
                        },
                        row.version,
                    )
                })
                .collect(),
            settings: settings.clone(),
            classification_hash: classification_hash.to_owned(),
        }
        .digest(range))
    }
    pub(crate) fn begin(&mut self) -> Result<(), RunQueryError> {
        AnsiTransactionManager::begin_transaction(self.connection())?;
        self.transaction = SnapshotTransaction::Started;
        // This SELECT pins the WAL snapshot before settings, versions, or facts are read.
        let _ = capture_runs::table
            .select(capture_runs::id)
            .limit(1)
            .load::<Option<i64>>(self.connection())?;
        Ok(())
    }
    pub(crate) fn finish(mut self) -> Result<ReadOnlyStore, RunQueryError> {
        if self.transaction == SnapshotTransaction::Started {
            AnsiTransactionManager::rollback_transaction(self.connection())?;
            self.transaction = SnapshotTransaction::Pending;
        }
        Ok(self
            .reader
            .take()
            .expect("finished snapshot returns reader"))
    }
    /// Every SQL fact scan returns at most QUERY_BATCH_ROWS rows before yielding to writes.
    pub(crate) fn read_batch(
        &mut self,
        range: QueryRange,
    ) -> Result<Option<Vec<InputRow>>, RunQueryError> {
        let limit = i64::try_from(QUERY_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = self.after_start;
        let after_id = self.after_id;
        let mut result = Vec::new();
        match self.stage {
            ReadStage::Clock => {
                let rows = clock_segments::table
                    .inner_join(
                        capture_runs::table
                            .on(clock_segments::run_id.nullable().eq(capture_runs::id)),
                    )
                    .filter(clock_segments::start_utc_ms.lt(range.end))
                    .filter(
                        clock_segments::end_utc_ms
                            .gt(range.start)
                            .or(clock_segments::confirmed_utc_ms.gt(range.start)),
                    )
                    .filter(
                        clock_segments::start_utc_ms.gt(after_start).or(
                            clock_segments::start_utc_ms
                                .eq(after_start)
                                .and(clock_segments::id.gt(after_id)),
                        ),
                    )
                    .order((clock_segments::start_utc_ms, clock_segments::id))
                    .limit(limit)
                    .select((ClockSegmentsRow::as_select(), CaptureRunsRow::as_select()))
                    .load::<ClockDetails>(self.connection())?;
                for details in rows {
                    let row = details.clock;
                    let run_end =
                        Self::endpoint(details.run.ended_utc_ms, details.run.confirmed_utc_ms)?;
                    self.after_start = row.start_utc_ms;
                    self.after_id = row.id.ok_or(RunQueryError::InvalidFacts)?;
                    result.push(InputRow::Clock(ClockSegment {
                        id: ClockSegmentId(self.after_id),
                        start_utc_ms: row.start_utc_ms,
                        end_utc_ms: Self::endpoint(row.end_utc_ms, row.confirmed_utc_ms)?
                            .min(run_end),
                        start_mono_ms: row.start_mono_ms,
                        end_mono_ms: Self::endpoint(row.end_mono_ms, row.confirmed_mono_ms)?
                            .min(row.start_mono_ms + run_end - row.start_utc_ms),
                    }));
                }
            }
            ReadStage::ForegroundClosed | ReadStage::ForegroundOpen => {
                let mut query = intervals::table
                    .left_join(applications::table.on(intervals::app_id.eq(applications::id)))
                    .left_join(titles::table.on(intervals::title_id.eq(titles::id)))
                    .into_boxed();
                query = match self.stage {
                    ReadStage::ForegroundClosed => query
                        .filter(
                            intervals::start_utc_ms
                                .ge(range.start.saturating_sub(UTC_DAY_MS).max(after_start)),
                        )
                        .filter(intervals::end_utc_ms.gt(range.start))
                        .filter(
                            intervals::start_utc_ms
                                .gt(after_start)
                                .or(intervals::start_utc_ms
                                    .eq(after_start)
                                    .and(intervals::id.gt(after_id))),
                        )
                        .order((intervals::start_utc_ms, intervals::id)),
                    // Match the partial open-row index's run ordering instead of scanning history.
                    ReadStage::ForegroundOpen => query
                        .filter(intervals::end_utc_ms.is_null())
                        .filter(intervals::confirmed_utc_ms.gt(range.start))
                        .filter(intervals::run_id.gt(after_id))
                        .order(intervals::run_id),
                    _ => unreachable!("foreground scan stage"),
                };
                let rows = query
                    .filter(intervals::start_utc_ms.lt(range.end))
                    .limit(limit)
                    .select((
                        IntervalsRow::as_select(),
                        applications::id.nullable(),
                        applications::exe_name.nullable(),
                        applications::display_name.nullable(),
                        applications::aumid.nullable(),
                        titles::text.nullable(),
                    ))
                    .load::<ForegroundDetails>(self.connection())?;
                for details in rows {
                    let row = details.fact;
                    let id = row.id.ok_or(RunQueryError::InvalidFacts)?;
                    self.after_start = row.start_utc_ms;
                    self.after_id = match self.stage {
                        ReadStage::ForegroundOpen => row.run_id,
                        _ => id,
                    };
                    let level = match row.capture_level {
                        enums::CaptureLevel::Full => CaptureLevel::Full,
                        enums::CaptureLevel::TitleHidden => CaptureLevel::TitleHidden,
                        enums::CaptureLevel::Excluded => CaptureLevel::Excluded,
                    };
                    let app = if level == CaptureLevel::Excluded {
                        None
                    } else {
                        match details.app_id {
                            Some(id) => Some(ApplicationRef {
                                id: ApplicationId(id),
                                exe_name: details.exe_name,
                                display_name: details
                                    .display_name
                                    .ok_or(RunQueryError::InvalidFacts)?,
                                aumid: details.aumid,
                            }),
                            None => None,
                        }
                    };
                    let title = if level == CaptureLevel::Full {
                        details.title
                    } else {
                        None
                    };
                    result.push(InputRow::Foreground(Foreground {
                        span: FactSpan {
                            id: FactId(id),
                            segment: ClockSegmentId(row.clock_segment_id),
                            start_utc_ms: row.start_utc_ms,
                            end_utc_ms: Self::endpoint(row.end_utc_ms, row.confirmed_utc_ms)?,
                            start_mono_ms: row.start_mono_ms,
                            end_mono_ms: Self::endpoint(row.end_mono_ms, row.confirmed_mono_ms)?,
                        },
                        kind: match row.kind {
                            enums::IntervalKind::Window => ForegroundKind::Window,
                            enums::IntervalKind::NoForeground => ForegroundKind::NoForeground,
                        },
                        level,
                        app,
                        title,
                    }));
                }
            }
            ReadStage::AvailabilityClosed | ReadStage::AvailabilityOpen => {
                let mut query = availability_intervals::table.into_boxed();
                query = match self.stage {
                    ReadStage::AvailabilityClosed => query
                        .filter(
                            availability_intervals::start_utc_ms
                                .ge(range.start.saturating_sub(UTC_DAY_MS).max(after_start)),
                        )
                        .filter(availability_intervals::end_utc_ms.gt(range.start))
                        .filter(
                            availability_intervals::start_utc_ms.gt(after_start).or(
                                availability_intervals::start_utc_ms
                                    .eq(after_start)
                                    .and(availability_intervals::id.gt(after_id)),
                            ),
                        )
                        .order((
                            availability_intervals::start_utc_ms,
                            availability_intervals::id,
                        )),
                    ReadStage::AvailabilityOpen => query
                        .filter(availability_intervals::end_utc_ms.is_null())
                        .filter(availability_intervals::confirmed_utc_ms.gt(range.start))
                        .filter(
                            availability_intervals::run_id.gt(after_start).or(
                                availability_intervals::run_id.eq(after_start).and(
                                    availability_intervals::reason.gt(i32::try_from(after_id)
                                        .map_err(|_| RunQueryError::InvalidFacts)?),
                                ),
                            ),
                        )
                        .order((
                            availability_intervals::run_id,
                            availability_intervals::reason,
                        )),
                    _ => unreachable!("availability scan stage"),
                };
                let rows = query
                    .filter(availability_intervals::start_utc_ms.lt(range.end))
                    .limit(limit)
                    .select(AvailabilityIntervalsRow::as_select())
                    .load::<AvailabilityIntervalsRow>(self.connection())?;
                for row in rows {
                    let id = row.id.ok_or(RunQueryError::InvalidFacts)?;
                    match self.stage {
                        ReadStage::AvailabilityOpen => {
                            self.after_start = row.run_id;
                            self.after_id = i64::from(row.reason.code());
                        }
                        _ => {
                            self.after_start = row.start_utc_ms;
                            self.after_id = id;
                        }
                    }
                    let state = match row.reason {
                        enums::AvailabilityReason::Idle => Availability::Idle,
                        enums::AvailabilityReason::Locked => {
                            Availability::Stopped(QueryStopReason::Locked)
                        }
                        enums::AvailabilityReason::Disconnected => {
                            Availability::Stopped(QueryStopReason::Disconnected)
                        }
                        enums::AvailabilityReason::Suspended => {
                            Availability::Stopped(QueryStopReason::Suspended)
                        }
                        enums::AvailabilityReason::Paused => {
                            Availability::Stopped(QueryStopReason::Paused)
                        }
                    };
                    result.push(InputRow::Availability {
                        span: FactSpan {
                            id: FactId(id),
                            segment: ClockSegmentId(row.clock_segment_id),
                            start_utc_ms: row.start_utc_ms,
                            end_utc_ms: Self::endpoint(row.end_utc_ms, row.confirmed_utc_ms)?,
                            start_mono_ms: row.start_mono_ms,
                            end_mono_ms: Self::endpoint(row.end_mono_ms, row.confirmed_mono_ms)?,
                        },
                        state,
                    });
                }
            }
            ReadStage::Gaps => {
                let rows = capture_gaps::table
                    .filter(capture_gaps::start_utc_ms.lt(range.end))
                    .filter(capture_gaps::end_utc_ms.gt(range.start))
                    .filter(
                        capture_gaps::start_utc_ms
                            .gt(after_start)
                            .or(capture_gaps::start_utc_ms
                                .eq(after_start)
                                .and(capture_gaps::id.gt(after_id))),
                    )
                    .order((capture_gaps::start_utc_ms, capture_gaps::id))
                    .limit(limit)
                    .select(CaptureGapsRow::as_select())
                    .load::<CaptureGapsRow>(self.connection())?;
                for row in rows {
                    self.after_start = row.start_utc_ms;
                    self.after_id = row.id.ok_or(RunQueryError::InvalidFacts)?;
                    result.push(InputRow::Gap {
                        span: FactSpan {
                            id: FactId(self.after_id),
                            segment: ClockSegmentId(row.clock_segment_id),
                            start_utc_ms: row.start_utc_ms,
                            end_utc_ms: row.end_utc_ms,
                            start_mono_ms: row.start_mono_ms,
                            end_mono_ms: row.end_mono_ms,
                        },
                        reason: match row.reason {
                            enums::GapReason::EventOverload => QueryGapReason::EventOverload,
                            enums::GapReason::WriteQueueOverload => {
                                QueryGapReason::WriteQueueOverload
                            }
                            enums::GapReason::DatabaseError => QueryGapReason::DatabaseError,
                        },
                    });
                }
            }
            ReadStage::Done => return Ok(None),
        }
        if result.len() < QUERY_BATCH_ROWS {
            self.stage = match self.stage {
                ReadStage::Clock => ReadStage::ForegroundClosed,
                ReadStage::ForegroundClosed => ReadStage::ForegroundOpen,
                ReadStage::ForegroundOpen => ReadStage::AvailabilityClosed,
                ReadStage::AvailabilityClosed => ReadStage::AvailabilityOpen,
                ReadStage::AvailabilityOpen => ReadStage::Gaps,
                ReadStage::Gaps | ReadStage::Done => ReadStage::Done,
            };
            self.after_start = i64::MIN;
            self.after_id = i64::from(i32::MIN);
        }
        Ok(Some(result))
    }
    fn endpoint(end: Option<i64>, confirmed: Option<i64>) -> Result<i64, RunQueryError> {
        match (end, confirmed) {
            (Some(end), None) => Ok(end),
            (None, Some(confirmed)) => Ok(confirmed),
            _ => Err(RunQueryError::InvalidFacts),
        }
    }
}

#[derive(Debug)]
pub(crate) struct CommittedVersions {
    pub facts: Vec<crate::query::versions::FactVersion>,
    pub metadata_version: Option<i64>,
}
impl Store {
    #[cfg(any(windows, test))]
    pub(crate) fn take_committed_versions(&mut self) -> CommittedVersions {
        CommittedVersions {
            facts: std::mem::take(&mut self.pending_versions)
                .into_iter()
                .map(|entry| crate::query::versions::FactVersion {
                    key: entry.0,
                    version: entry.1,
                })
                .collect(),
            metadata_version: self.pending_metadata_version.take(),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
