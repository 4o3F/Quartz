//! Bounded typed snapshot reads for the query pipeline; Diesel stays in storage.

use super::{
    ReadOnlyStore, Store,
    enums::{self, IntegerCode},
    query_error::ReadQueryError,
    rows::{
        ApplicationsRow, AvailabilityIntervalsRow, CaptureGapsRow, CaptureRunsRow,
        ClockSegmentsRow, IntervalsRow,
    },
    schema::{
        applications, availability_intervals, capture_gaps, capture_runs, clock_segments,
        intervals, titles,
    },
    versions::QuerySettings,
};
use quartz_domain::{
    capture::{GapReason, StopReason, limits::UTC_DAY_MS},
    facts::{
        ApplicationId, ApplicationRef, Availability, CaptureLevel, ClockSegment, ClockSegmentId,
        FactId, FactSpan, Foreground, ForegroundKind, InputRow, QueryRange,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Bounds each SQLite fact scan before the database loop services pending writes.
pub(crate) const READ_BATCH_ROWS: usize = 256;
use diesel::{
    BoolExpressionMethods, ExpressionMethods, JoinOnDsl, NullableExpressionMethods, QueryDsl,
    RunQueryDsl, SelectableHelper,
    connection::{AnsiTransactionManager, TransactionManager},
};
impl ReadOnlyStore {
    pub(crate) fn snapshot(self) -> ReadSnapshot {
        ReadSnapshot {
            reader: Some(self),
            transaction: SnapshotTransaction::Pending,
            cursor: ScanCursor::Clock(StartCursor::initial(ClockSegmentId(i64::from(i32::MIN)))),
            applications: BTreeMap::new(),
        }
    }
}
impl Store {
    pub(crate) fn take_query_reader(&mut self) -> Option<ReadOnlyStore> {
        self.reader.take()
    }
    pub(crate) fn return_query_reader(&mut self, reader: ReadOnlyStore) {
        self.reader = Some(reader);
    }
}
/// Owns the read transaction across DB-loop turns, rolling it back on every exit path.
pub(crate) struct ReadSnapshot {
    reader: Option<ReadOnlyStore>,
    transaction: SnapshotTransaction,
    cursor: ScanCursor,
    applications: BTreeMap<ApplicationId, Arc<ApplicationRef>>,
}
impl std::fmt::Debug for ReadSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadSnapshot")
            .field("cursor", &self.cursor)
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
struct StartCursor<Id> {
    start_utc_ms: i64,
    id: Id,
}
impl<Id> StartCursor<Id> {
    fn initial(id: Id) -> Self {
        Self {
            start_utc_ms: i64::MIN,
            id,
        }
    }
}
#[derive(Debug, Clone, Copy)]
struct StoredRunId(i64);
#[derive(Debug, Clone, Copy)]
struct AvailabilityCursor {
    run: StoredRunId,
    reason: Option<enums::AvailabilityReason>,
}
#[derive(Debug, Clone, Copy)]
enum ScanCursor {
    Clock(StartCursor<ClockSegmentId>),
    ForegroundClosed(StartCursor<FactId>),
    ForegroundOpen(StoredRunId),
    AvailabilityClosed(StartCursor<FactId>),
    AvailabilityOpen(AvailabilityCursor),
    Gaps(StartCursor<FactId>),
    Done,
}
impl ScanCursor {
    fn next(self) -> Self {
        match self {
            Self::Clock(_) => {
                Self::ForegroundClosed(StartCursor::initial(FactId(i64::from(i32::MIN))))
            }
            Self::ForegroundClosed(_) => Self::ForegroundOpen(StoredRunId(i64::from(i32::MIN))),
            Self::ForegroundOpen(_) => {
                Self::AvailabilityClosed(StartCursor::initial(FactId(i64::from(i32::MIN))))
            }
            Self::AvailabilityClosed(_) => Self::AvailabilityOpen(AvailabilityCursor {
                run: StoredRunId(i64::MIN),
                reason: None,
            }),
            Self::AvailabilityOpen(_) => {
                Self::Gaps(StartCursor::initial(FactId(i64::from(i32::MIN))))
            }
            Self::Gaps(_) | Self::Done => Self::Done,
        }
    }
}
#[derive(Debug, diesel::Queryable)]
struct ClockDetails {
    clock: ClockSegmentsRow,
    run: CaptureRunsRow,
}
#[derive(Debug, diesel::Queryable)]
struct ForegroundDetails {
    fact: IntervalsRow,
    title: Option<String>,
}

impl ReadSnapshot {
    fn read_applications(&mut self, rows: &[ForegroundDetails]) -> Result<(), ReadQueryError> {
        let missing: BTreeSet<ApplicationId> = rows
            .iter()
            .filter_map(|row| row.fact.app_id.map(ApplicationId))
            .filter(|id| !self.applications.contains_key(id))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let rows = applications::table
            .filter(applications::id.eq_any(missing.iter().map(|id| id.0)))
            .select(ApplicationsRow::as_select())
            .load::<ApplicationsRow>(self.connection())?;
        for row in rows {
            let id = ApplicationId(row.id.ok_or(ReadQueryError::InvalidFacts)?);
            self.applications.insert(
                id,
                Arc::new(ApplicationRef {
                    id,
                    identity_key: row.identity_key,
                    exe_name: row.exe_name,
                    display_name: row.display_name,
                    aumid: row.aumid,
                }),
            );
        }
        Ok(())
    }
    pub(super) fn connection(&mut self) -> &mut diesel::SqliteConnection {
        &mut self
            .reader
            .as_mut()
            .expect("live snapshot reader")
            .connection
    }
    pub(crate) fn settings(&mut self) -> Result<QuerySettings, ReadQueryError> {
        self.reader
            .as_mut()
            .expect("live snapshot reader")
            .query_settings()
    }
    pub(crate) fn begin(&mut self) -> Result<(), ReadQueryError> {
        AnsiTransactionManager::begin_transaction(self.connection())?;
        self.transaction = SnapshotTransaction::Started;
        // This SELECT pins the WAL snapshot before settings, versions, or facts are read.
        let _ = capture_runs::table
            .select(capture_runs::id)
            .limit(1)
            .load::<Option<i64>>(self.connection())?;
        Ok(())
    }
    pub(crate) fn finish(mut self) -> Result<ReadOnlyStore, ReadQueryError> {
        if self.transaction == SnapshotTransaction::Started {
            AnsiTransactionManager::rollback_transaction(self.connection())?;
            self.transaction = SnapshotTransaction::Pending;
        }
        Ok(self
            .reader
            .take()
            .expect("finished snapshot returns reader"))
    }
    /// Every SQL fact scan returns at most READ_BATCH_ROWS rows before yielding to writes.
    pub(crate) fn read_batch(
        &mut self,
        range: QueryRange,
    ) -> Result<Option<Vec<InputRow>>, ReadQueryError> {
        let result = match self.cursor {
            ScanCursor::Clock(cursor) => self.read_clock(range, cursor)?,
            ScanCursor::ForegroundClosed(cursor) => self.read_foreground_closed(range, cursor)?,
            ScanCursor::ForegroundOpen(cursor) => self.read_foreground_open(range, cursor)?,
            ScanCursor::AvailabilityClosed(cursor) => {
                self.read_availability_closed(range, cursor)?
            }
            ScanCursor::AvailabilityOpen(cursor) => self.read_availability_open(range, cursor)?,
            ScanCursor::Gaps(cursor) => self.read_gaps(range, cursor)?,
            ScanCursor::Done => return Ok(None),
        };
        if result.len() < READ_BATCH_ROWS {
            self.cursor = self.cursor.next();
        }
        Ok(Some(result))
    }
    fn read_clock(
        &mut self,
        range: QueryRange,
        cursor: StartCursor<ClockSegmentId>,
    ) -> Result<Vec<InputRow>, ReadQueryError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.start_utc_ms;
        let after_id = cursor.id.0;
        let mut result = Vec::new();

        let rows = clock_segments::table
            .inner_join(
                capture_runs::table.on(clock_segments::run_id.nullable().eq(capture_runs::id)),
            )
            .filter(clock_segments::start_utc_ms.lt(range.end))
            .filter(
                clock_segments::end_utc_ms
                    .gt(range.start)
                    .or(clock_segments::confirmed_utc_ms.gt(range.start)),
            )
            .filter(
                clock_segments::start_utc_ms
                    .gt(after_start)
                    .or(clock_segments::start_utc_ms
                        .eq(after_start)
                        .and(clock_segments::id.gt(after_id))),
            )
            .order((clock_segments::start_utc_ms, clock_segments::id))
            .limit(limit)
            .select((ClockSegmentsRow::as_select(), CaptureRunsRow::as_select()))
            .load::<ClockDetails>(self.connection())?;
        for details in rows {
            let row = details.clock;
            let run_end = Self::endpoint(details.run.ended_utc_ms, details.run.confirmed_utc_ms)?;
            let id = row.id.ok_or(ReadQueryError::InvalidFacts)?;
            self.cursor = ScanCursor::Clock(StartCursor {
                start_utc_ms: row.start_utc_ms,
                id: ClockSegmentId(id),
            });
            result.push(InputRow::Clock(ClockSegment {
                id: ClockSegmentId(id),
                start_utc_ms: row.start_utc_ms,
                end_utc_ms: Self::endpoint(row.end_utc_ms, row.confirmed_utc_ms)?.min(run_end),
                start_mono_ms: row.start_mono_ms,
                end_mono_ms: Self::endpoint(row.end_mono_ms, row.confirmed_mono_ms)?
                    .min(row.start_mono_ms + run_end - row.start_utc_ms),
            }));
        }

        Ok(result)
    }
    fn decode_foreground(
        &mut self,
        rows: Vec<ForegroundDetails>,
    ) -> Result<Vec<InputRow>, ReadQueryError> {
        self.read_applications(&rows)?;
        let mut result = Vec::with_capacity(rows.len());
        for details in rows {
            let row = details.fact;
            let id = row.id.ok_or(ReadQueryError::InvalidFacts)?;
            let level: CaptureLevel = row.capture_level.into();
            let app_id = row.app_id;
            let app = app_id.and_then(|id| self.applications.get(&ApplicationId(id)).cloned());
            let foreground = Foreground::new(
                FactSpan {
                    id: FactId(id),
                    segment: ClockSegmentId(row.clock_segment_id),
                    start_utc_ms: row.start_utc_ms,
                    end_utc_ms: Self::endpoint(row.end_utc_ms, row.confirmed_utc_ms)?,
                    start_mono_ms: row.start_mono_ms,
                    end_mono_ms: Self::endpoint(row.end_mono_ms, row.confirmed_mono_ms)?,
                },
                match row.kind {
                    enums::IntervalKind::Window => ForegroundKind::Window,
                    enums::IntervalKind::NoForeground => ForegroundKind::NoForeground,
                },
                level,
                quartz_domain::facts::ForegroundContent {
                    app,
                    title: details.title,
                    title_id: row.title_id.map(quartz_domain::facts::TitleId),
                },
            );
            if app_id.is_some()
                && foreground.visible_app().is_none()
                && foreground.capture_level() != CaptureLevel::Excluded
            {
                return Err(ReadQueryError::InvalidFacts);
            }
            result.push(InputRow::Foreground(foreground));
        }

        Ok(result)
    }
    fn read_foreground_closed(
        &mut self,
        range: QueryRange,
        cursor: StartCursor<FactId>,
    ) -> Result<Vec<InputRow>, ReadQueryError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.start_utc_ms;
        let after_id = cursor.id.0;

        let mut query = intervals::table
            .left_join(titles::table.on(intervals::title_id.eq(titles::id)))
            .into_boxed();
        query = query
            .filter(
                intervals::start_utc_ms.ge(range.start.saturating_sub(UTC_DAY_MS).max(after_start)),
            )
            .filter(intervals::end_utc_ms.gt(range.start))
            .filter(
                intervals::start_utc_ms
                    .gt(after_start)
                    .or(intervals::start_utc_ms
                        .eq(after_start)
                        .and(intervals::id.gt(after_id))),
            )
            .order((intervals::start_utc_ms, intervals::id));
        let rows = query
            .filter(intervals::start_utc_ms.lt(range.end))
            .limit(limit)
            .select((IntervalsRow::as_select(), titles::text.nullable()))
            .load::<ForegroundDetails>(self.connection())?;
        if let Some(row) = rows.last() {
            self.cursor = ScanCursor::ForegroundClosed(StartCursor {
                start_utc_ms: row.fact.start_utc_ms,
                id: FactId(row.fact.id.ok_or(ReadQueryError::InvalidFacts)?),
            });
        }
        self.decode_foreground(rows)
    }
    fn read_foreground_open(
        &mut self,
        range: QueryRange,
        cursor: StoredRunId,
    ) -> Result<Vec<InputRow>, ReadQueryError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_id = cursor.0;

        let mut query = intervals::table
            .left_join(titles::table.on(intervals::title_id.eq(titles::id)))
            .into_boxed();
        query = query
            .filter(intervals::end_utc_ms.is_null())
            .filter(intervals::confirmed_utc_ms.gt(range.start))
            .filter(intervals::run_id.gt(after_id))
            .order(intervals::run_id);
        let rows = query
            .filter(intervals::start_utc_ms.lt(range.end))
            .limit(limit)
            .select((IntervalsRow::as_select(), titles::text.nullable()))
            .load::<ForegroundDetails>(self.connection())?;
        if let Some(row) = rows.last() {
            self.cursor = ScanCursor::ForegroundOpen(StoredRunId(row.fact.run_id));
        }
        self.decode_foreground(rows)
    }
    fn decode_availability(
        &mut self,
        rows: Vec<AvailabilityIntervalsRow>,
    ) -> Result<Vec<InputRow>, ReadQueryError> {
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let id = row.id.ok_or(ReadQueryError::InvalidFacts)?;
            let state = match row.reason {
                enums::AvailabilityReason::Idle => Availability::Idle,
                enums::AvailabilityReason::Locked => Availability::Stopped(StopReason::Locked),
                enums::AvailabilityReason::Disconnected => {
                    Availability::Stopped(StopReason::Disconnected)
                }
                enums::AvailabilityReason::Suspended => {
                    Availability::Stopped(StopReason::Suspended)
                }
                enums::AvailabilityReason::Paused => Availability::Stopped(StopReason::Paused),
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

        Ok(result)
    }
    fn read_availability_closed(
        &mut self,
        range: QueryRange,
        cursor: StartCursor<FactId>,
    ) -> Result<Vec<InputRow>, ReadQueryError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.start_utc_ms;
        let after_id = cursor.id.0;

        let mut query = availability_intervals::table.into_boxed();
        query = query
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
            ));
        let rows = query
            .filter(availability_intervals::start_utc_ms.lt(range.end))
            .limit(limit)
            .select(AvailabilityIntervalsRow::as_select())
            .load::<AvailabilityIntervalsRow>(self.connection())?;
        if let Some(row) = rows.last() {
            self.cursor = ScanCursor::AvailabilityClosed(StartCursor {
                start_utc_ms: row.start_utc_ms,
                id: FactId(row.id.ok_or(ReadQueryError::InvalidFacts)?),
            });
        }
        self.decode_availability(rows)
    }
    fn read_availability_open(
        &mut self,
        range: QueryRange,
        cursor: AvailabilityCursor,
    ) -> Result<Vec<InputRow>, ReadQueryError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.run.0;
        let after_id = cursor.reason.map_or(i32::MIN, IntegerCode::code);

        let mut query = availability_intervals::table.into_boxed();
        query = query
            .filter(availability_intervals::end_utc_ms.is_null())
            .filter(availability_intervals::confirmed_utc_ms.gt(range.start))
            .filter(
                availability_intervals::run_id
                    .gt(after_start)
                    .or(availability_intervals::run_id
                        .eq(after_start)
                        .and(availability_intervals::reason.gt(after_id))),
            )
            .order((
                availability_intervals::run_id,
                availability_intervals::reason,
            ));
        let rows = query
            .filter(availability_intervals::start_utc_ms.lt(range.end))
            .limit(limit)
            .select(AvailabilityIntervalsRow::as_select())
            .load::<AvailabilityIntervalsRow>(self.connection())?;
        if let Some(row) = rows.last() {
            self.cursor = ScanCursor::AvailabilityOpen(AvailabilityCursor {
                run: StoredRunId(row.run_id),
                reason: Some(row.reason),
            });
        }
        self.decode_availability(rows)
    }
    fn read_gaps(
        &mut self,
        range: QueryRange,
        cursor: StartCursor<FactId>,
    ) -> Result<Vec<InputRow>, ReadQueryError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.start_utc_ms;
        let after_id = cursor.id.0;
        let mut result = Vec::new();

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
            let id = row.id.ok_or(ReadQueryError::InvalidFacts)?;
            self.cursor = ScanCursor::Gaps(StartCursor {
                start_utc_ms: row.start_utc_ms,
                id: FactId(id),
            });
            result.push(InputRow::Gap {
                span: FactSpan {
                    id: FactId(id),
                    segment: ClockSegmentId(row.clock_segment_id),
                    start_utc_ms: row.start_utc_ms,
                    end_utc_ms: row.end_utc_ms,
                    start_mono_ms: row.start_mono_ms,
                    end_mono_ms: row.end_mono_ms,
                },
                reason: match row.reason {
                    enums::GapReason::EventOverload => GapReason::EventOverload,
                    enums::GapReason::WriteQueueOverload => GapReason::WriteQueueOverload,
                    enums::GapReason::DatabaseError => GapReason::DatabaseError,
                },
            });
        }

        Ok(result)
    }
    fn endpoint(end: Option<i64>, confirmed: Option<i64>) -> Result<i64, ReadQueryError> {
        match (end, confirmed) {
            (Some(end), None) => Ok(end),
            (None, Some(confirmed)) => Ok(confirmed),
            _ => Err(ReadQueryError::InvalidFacts),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
