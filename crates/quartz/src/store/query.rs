//! Bounded typed snapshot reads for the query pipeline; Diesel stays in storage.

use super::{
    ReadOnlyStore, Store,
    enums::{self, IntegerCode},
    query_error::ReadQueryError,
    rows::{
        AnnotationsRow, ApplicationsRow, AvailabilityIntervalsRow, CaptureGapsRow, CaptureRunsRow,
        ClockSegmentsRow, IntervalsRow,
    },
    schema::{
        annotations, applications, availability_intervals, capture_gaps, capture_runs,
        clock_segments, intervals, titles, window_instances,
    },
    versions::QuerySettings,
};
use quartz_domain::{
    capture::{GapReason, StopReason, limits::UTC_DAY_MS},
    facts::{
        ApplicationId, ApplicationRef, Availability, CaptureLevel, ClockSegment, ClockSegmentId,
        FactId, FactSpan, Foreground, ForegroundKind, QueryRange,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

mod facts;
mod input;
pub(crate) use input::{InputRow, QueryAnnotation};

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
            cursor: ScanCursor::initial(),
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
pub(super) struct StartCursor<Id> {
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
pub(super) struct StoredRunId(i64);
#[derive(Debug, Clone, Copy)]
pub(super) struct AvailabilityCursor {
    run: StoredRunId,
    reason: Option<enums::AvailabilityReason>,
}
#[derive(Debug, Clone, Copy)]
pub(super) enum ScanCursor {
    Runs(StartCursor<FactId>),
    TimedPolicies,
    Clock(StartCursor<ClockSegmentId>),
    ForegroundClosed(StartCursor<FactId>),
    ForegroundOpen(StoredRunId),
    AvailabilityClosed(StartCursor<FactId>),
    AvailabilityOpen(AvailabilityCursor),
    Gaps(StartCursor<FactId>),
    Annotations(StartCursor<FactId>),
    Done,
}
impl ScanCursor {
    pub(super) fn initial() -> Self {
        Self::Runs(StartCursor::initial(FactId(i64::MIN)))
    }

    fn next(self) -> Self {
        match self {
            Self::Runs(_) => Self::Clock(StartCursor::initial(ClockSegmentId(i64::MIN))),
            Self::TimedPolicies => Self::Done,
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
            Self::Gaps(_) => Self::Annotations(StartCursor::initial(FactId(i64::MIN))),
            Self::Annotations(_) => Self::TimedPolicies,
            Self::Done => Self::Done,
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
    window_class: Option<String>,
}

/// Eight predicates share one prepared statement; padding repeats an existing ID.
const APPLICATION_LOOKUP_ARITY: usize = 8;

struct ApplicationLookup {
    ids: [ApplicationId; APPLICATION_LOOKUP_ARITY],
}
impl ApplicationLookup {
    fn new(ids: &[ApplicationId]) -> Self {
        let first = *ids.first().expect("application lookup chunks are nonempty");
        let mut bound = [first; APPLICATION_LOOKUP_ARITY];
        bound[..ids.len()].copy_from_slice(ids);
        Self { ids: bound }
    }
    fn load(
        &self,
        connection: &mut diesel::SqliteConnection,
    ) -> diesel::QueryResult<Vec<ApplicationsRow>> {
        applications::table
            .filter(
                applications::id
                    .eq(self.ids[0].0)
                    .or(applications::id.eq(self.ids[1].0))
                    .or(applications::id.eq(self.ids[2].0))
                    .or(applications::id.eq(self.ids[3].0))
                    .or(applications::id.eq(self.ids[4].0))
                    .or(applications::id.eq(self.ids[5].0))
                    .or(applications::id.eq(self.ids[6].0))
                    .or(applications::id.eq(self.ids[7].0)),
            )
            .select(ApplicationsRow::as_select())
            .load(connection)
    }
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
        let missing: Vec<_> = missing.into_iter().collect();
        for chunk in missing.chunks(APPLICATION_LOOKUP_ARITY) {
            let rows = ApplicationLookup::new(chunk).load(self.connection())?;
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
    pub(crate) fn first_day_of_week(
        &mut self,
    ) -> Result<quartz_protocol::FirstDayOfWeekSetting, ReadQueryError> {
        Ok(super::settings::Settings::new(self.connection()).first_day_of_week()?)
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
    /// Restarts the bounded scans within the same snapshot for the next logical day.
    pub(crate) fn restart_scan(&mut self) {
        self.cursor = ScanCursor::initial();
        // This dictionary belongs to the snapshot, not the resident read connection.
        self.applications.clear();
    }
    /// Every SQL fact scan returns at most READ_BATCH_ROWS rows before yielding to writes.
    pub(crate) fn read_batch(
        &mut self,
        range: QueryRange,
    ) -> Result<Option<Vec<InputRow>>, ReadQueryError> {
        let connection = &mut self
            .reader
            .as_mut()
            .expect("live snapshot reader")
            .connection;
        let result = match self.cursor {
            ScanCursor::Runs(_) | ScanCursor::TimedPolicies => Vec::new(),
            ScanCursor::Clock(cursor) => {
                Self::decode_clock(self.cursor.load_clock(connection, range, cursor)?)?
            }
            ScanCursor::ForegroundClosed(cursor) => {
                let rows = self
                    .cursor
                    .load_foreground_closed(connection, range, cursor)?;
                self.decode_foreground(rows)?
            }
            ScanCursor::ForegroundOpen(cursor) => {
                let rows = self
                    .cursor
                    .load_foreground_open(connection, range, cursor)?;
                self.decode_foreground(rows)?
            }
            ScanCursor::AvailabilityClosed(cursor) => {
                let rows = self
                    .cursor
                    .load_availability_closed(connection, range, cursor)?;
                self.decode_availability(rows)?
            }
            ScanCursor::AvailabilityOpen(cursor) => {
                let rows = self
                    .cursor
                    .load_availability_open(connection, range, cursor)?;
                self.decode_availability(rows)?
            }
            ScanCursor::Gaps(cursor) => {
                Self::decode_gaps(self.cursor.load_gaps(connection, range, cursor)?)?
            }
            ScanCursor::Annotations(cursor) => self.read_annotations(range, cursor)?,
            ScanCursor::Done => return Ok(None),
        };
        if result.len() < READ_BATCH_ROWS {
            self.cursor = self.cursor.next();
        }
        Ok(Some(result))
    }
    pub(crate) fn read_fact_batch(
        &mut self,
        range: QueryRange,
    ) -> Result<Option<Vec<super::facts::Fact>>, ReadQueryError> {
        Ok(self.cursor.read_facts(
            &mut self
                .reader
                .as_mut()
                .expect("live snapshot reader")
                .connection,
            range,
        )?)
    }
    fn read_annotations(
        &mut self,
        range: QueryRange,
        cursor: StartCursor<FactId>,
    ) -> Result<Vec<InputRow>, ReadQueryError> {
        let rows = annotations::table
            .filter(annotations::start_utc_ms.lt(range.end))
            .filter(annotations::end_utc_ms.gt(range.start))
            .filter(
                annotations::start_utc_ms
                    .gt(cursor.start_utc_ms)
                    .or(annotations::start_utc_ms
                        .eq(cursor.start_utc_ms)
                        .and(annotations::id.gt(cursor.id.0))),
            )
            .order(annotations::start_utc_ms)
            .then_order_by(annotations::id)
            .limit(i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64"))
            .select(AnnotationsRow::as_select())
            .load::<AnnotationsRow>(self.connection())?;
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            if row.end_utc_ms <= row.start_utc_ms
                || (row.kind == enums::AnnotationKind::CategoryOverride
                    && row.category_id.is_none())
            {
                return Err(ReadQueryError::InvalidFacts);
            }
            self.cursor = ScanCursor::Annotations(StartCursor {
                start_utc_ms: row.start_utc_ms,
                id: FactId(row.id.ok_or(ReadQueryError::InvalidFacts)?),
            });
            result.push(InputRow::Annotation(QueryAnnotation {
                id: quartz_protocol::AnnotationId(
                    row.uuid.parse().map_err(|_| ReadQueryError::InvalidFacts)?,
                ),
                kind: row.kind.into(),
                range: QueryRange {
                    start: row.start_utc_ms,
                    end: row.end_utc_ms,
                },
                category: row.category_id.map(quartz_domain::categories::CategoryId),
            }));
        }
        Ok(result)
    }
    fn decode_clock(rows: Vec<ClockDetails>) -> Result<Vec<InputRow>, ReadQueryError> {
        let mut result = Vec::with_capacity(rows.len());
        for details in rows {
            let row = details.clock;
            let run_end = Self::endpoint(details.run.ended_utc_ms, details.run.confirmed_utc_ms)?;
            let id = row.id.ok_or(ReadQueryError::InvalidFacts)?;
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
                    window_instance: row
                        .window_instance_id
                        .map(quartz_domain::facts::WindowInstanceId),
                    window_class: details.window_class,
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

    fn decode_gaps(rows: Vec<CaptureGapsRow>) -> Result<Vec<InputRow>, ReadQueryError> {
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let id = row.id.ok_or(ReadQueryError::InvalidFacts)?;
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

impl ScanCursor {
    fn load_clock(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
        cursor: StartCursor<ClockSegmentId>,
    ) -> Result<Vec<ClockDetails>, super::facts::ReadFactsError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.start_utc_ms;
        let after_id = cursor.id.0;

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
            .load::<ClockDetails>(connection)?;
        if let Some(row) = rows.last() {
            *self = Self::Clock(StartCursor {
                start_utc_ms: row.clock.start_utc_ms,
                id: ClockSegmentId(row.clock.id.ok_or(
                    super::facts::ReadFactsError::MissingId {
                        kind: super::facts::FactKind::ClockSegment,
                    },
                )?),
            });
        }
        Ok(rows)
    }
    fn load_foreground_closed(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
        cursor: StartCursor<FactId>,
    ) -> Result<Vec<ForegroundDetails>, super::facts::ReadFactsError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.start_utc_ms;
        let after_id = cursor.id.0;

        let rows = intervals::table
            .left_join(titles::table.on(intervals::title_id.eq(titles::id)))
            .left_join(
                window_instances::table.on(intervals::window_instance_id.eq(window_instances::id)),
            )
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
            .order((intervals::start_utc_ms, intervals::id))
            .filter(intervals::start_utc_ms.lt(range.end))
            .limit(limit)
            .select((
                IntervalsRow::as_select(),
                titles::text.nullable(),
                window_instances::window_class.nullable(),
            ))
            .load::<ForegroundDetails>(connection)?;
        if let Some(row) = rows.last() {
            *self = ScanCursor::ForegroundClosed(StartCursor {
                start_utc_ms: row.fact.start_utc_ms,
                id: FactId(row.fact.id.ok_or(super::facts::ReadFactsError::MissingId {
                    kind: super::facts::FactKind::Foreground,
                })?),
            });
        }
        Ok(rows)
    }
    fn load_foreground_open(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
        cursor: StoredRunId,
    ) -> Result<Vec<ForegroundDetails>, super::facts::ReadFactsError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_id = cursor.0;

        let rows = intervals::table
            .left_join(titles::table.on(intervals::title_id.eq(titles::id)))
            .left_join(
                window_instances::table.on(intervals::window_instance_id.eq(window_instances::id)),
            )
            .filter(intervals::end_utc_ms.is_null())
            .filter(intervals::confirmed_utc_ms.gt(range.start))
            .filter(intervals::run_id.gt(after_id))
            .order(intervals::run_id)
            .filter(intervals::start_utc_ms.lt(range.end))
            .limit(limit)
            .select((
                IntervalsRow::as_select(),
                titles::text.nullable(),
                window_instances::window_class.nullable(),
            ))
            .load::<ForegroundDetails>(connection)?;
        if let Some(row) = rows.last() {
            *self = ScanCursor::ForegroundOpen(StoredRunId(row.fact.run_id));
        }
        Ok(rows)
    }
    fn load_availability_closed(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
        cursor: StartCursor<FactId>,
    ) -> Result<Vec<AvailabilityIntervalsRow>, super::facts::ReadFactsError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.start_utc_ms;
        let after_id = cursor.id.0;

        let rows = availability_intervals::table
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
            ))
            .filter(availability_intervals::start_utc_ms.lt(range.end))
            .limit(limit)
            .select(AvailabilityIntervalsRow::as_select())
            .load::<AvailabilityIntervalsRow>(connection)?;
        if let Some(row) = rows.last() {
            *self = ScanCursor::AvailabilityClosed(StartCursor {
                start_utc_ms: row.start_utc_ms,
                id: FactId(row.id.ok_or(super::facts::ReadFactsError::MissingId {
                    kind: super::facts::FactKind::Availability,
                })?),
            });
        }
        Ok(rows)
    }
    fn load_availability_open(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
        cursor: AvailabilityCursor,
    ) -> Result<Vec<AvailabilityIntervalsRow>, super::facts::ReadFactsError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.run.0;
        let after_id = cursor.reason.map_or(i32::MIN, IntegerCode::code);

        let rows = availability_intervals::table
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
            ))
            .filter(availability_intervals::start_utc_ms.lt(range.end))
            .limit(limit)
            .select(AvailabilityIntervalsRow::as_select())
            .load::<AvailabilityIntervalsRow>(connection)?;
        if let Some(row) = rows.last() {
            *self = ScanCursor::AvailabilityOpen(AvailabilityCursor {
                run: StoredRunId(row.run_id),
                reason: Some(row.reason),
            });
        }
        Ok(rows)
    }
    fn load_gaps(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
        cursor: StartCursor<FactId>,
    ) -> Result<Vec<CaptureGapsRow>, super::facts::ReadFactsError> {
        let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
        let after_start = cursor.start_utc_ms;
        let after_id = cursor.id.0;

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
            .load::<CaptureGapsRow>(connection)?;
        if let Some(row) = rows.last() {
            *self = Self::Gaps(StartCursor {
                start_utc_ms: row.start_utc_ms,
                id: FactId(row.id.ok_or(super::facts::ReadFactsError::MissingId {
                    kind: super::facts::FactKind::CaptureGap,
                })?),
            });
        }
        Ok(rows)
    }
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
