//! Bounded raw fact scans with foreground dictionaries joined in one pass.

use super::{READ_BATCH_ROWS, ScanCursor, StartCursor};
use crate::store::{
    facts::{Fact, FactKind, ReadFactsError, WindowDetails},
    rows::{
        AvailabilityIntervalsRow, CaptureGapsRow, CaptureRunsRow, IntervalsRow, TimedPoliciesRow,
    },
    schema::{applications, capture_runs, intervals, timed_policies, titles, window_instances},
};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, JoinOnDsl, NullableExpressionMethods, QueryDsl,
    RunQueryDsl, SelectableHelper,
};
use quartz_domain::facts::{FactId, QueryRange};

impl ScanCursor {
    pub(in crate::store) fn read_facts(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
    ) -> Result<Option<Vec<Fact>>, ReadFactsError> {
        let facts = match *self {
            Self::Runs(cursor) => self
                .load_runs(connection, range, cursor)?
                .into_iter()
                .map(CaptureRunsRow::into_fact)
                .collect(),
            Self::Clock(cursor) => self
                .load_clock(connection, range, cursor)?
                .into_iter()
                .map(|row| row.into_fact())
                .collect(),
            Self::ForegroundClosed(_) | Self::ForegroundOpen(_) => {
                self.load_foreground_facts(connection, range)
            }
            Self::AvailabilityClosed(cursor) => self
                .load_availability_closed(connection, range, cursor)?
                .into_iter()
                .map(AvailabilityIntervalsRow::into_fact)
                .collect(),
            Self::AvailabilityOpen(cursor) => self
                .load_availability_open(connection, range, cursor)?
                .into_iter()
                .map(AvailabilityIntervalsRow::into_fact)
                .collect(),
            Self::Gaps(cursor) => self
                .load_gaps(connection, range, cursor)?
                .into_iter()
                .map(CaptureGapsRow::into_fact)
                .collect(),
            Self::Annotations(_) => Ok(Vec::new()),
            Self::TimedPolicies => self
                .load_policies(connection, range)?
                .into_iter()
                .map(TimedPoliciesRow::into_fact)
                .collect(),
            Self::Done => return Ok(None),
        }?;
        if facts.len() < READ_BATCH_ROWS {
            *self = self.next();
        }
        Ok(Some(facts))
    }
    fn load_foreground_facts(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
    ) -> Result<Vec<Fact>, ReadFactsError> {
        let mut query = intervals::table
            .left_join(applications::table.on(intervals::app_id.eq(applications::id)))
            .left_join(titles::table.on(intervals::title_id.eq(titles::id)))
            .left_join(
                window_instances::table.on(intervals::window_instance_id.eq(window_instances::id)),
            )
            .filter(intervals::start_utc_ms.lt(range.end))
            .into_boxed();
        match *self {
            Self::ForegroundClosed(cursor) => {
                query = query
                    .filter(
                        intervals::start_utc_ms.ge(range
                            .start
                            .saturating_sub(quartz_domain::capture::limits::UTC_DAY_MS)
                            .max(cursor.start_utc_ms)),
                    )
                    .filter(intervals::end_utc_ms.gt(range.start))
                    .filter(
                        intervals::start_utc_ms
                            .gt(cursor.start_utc_ms)
                            .or(intervals::start_utc_ms
                                .eq(cursor.start_utc_ms)
                                .and(intervals::id.gt(cursor.id.0))),
                    )
                    .order(intervals::start_utc_ms)
                    .then_order_by(intervals::id);
            }
            Self::ForegroundOpen(cursor) => {
                query = query
                    .filter(intervals::end_utc_ms.is_null())
                    .filter(intervals::confirmed_utc_ms.gt(range.start))
                    .filter(intervals::run_id.gt(cursor.0))
                    .order(intervals::run_id);
            }
            _ => unreachable!("raw foreground partition"),
        }
        let rows = query
            .limit(i64::try_from(READ_BATCH_ROWS).expect("batch fits i64"))
            .select((
                IntervalsRow::as_select(),
                (
                    applications::display_name.nullable(),
                    applications::exe_path.nullable(),
                    applications::exe_name.nullable(),
                    titles::text.nullable(),
                    window_instances::hwnd.nullable(),
                    window_instances::generation.nullable(),
                    window_instances::pid.nullable(),
                    window_instances::window_class.nullable(),
                ),
            ))
            .load::<RawForeground>(connection)?;
        if let Some(row) = rows.last() {
            *self = match *self {
                Self::ForegroundClosed(_) => Self::ForegroundClosed(StartCursor {
                    start_utc_ms: row.fact.start_utc_ms,
                    id: FactId(row.fact.id.ok_or(ReadFactsError::MissingId {
                        kind: FactKind::Foreground,
                    })?),
                }),
                Self::ForegroundOpen(_) => {
                    Self::ForegroundOpen(super::StoredRunId(row.fact.run_id))
                }
                _ => unreachable!("raw foreground partition"),
            };
        }
        rows.into_iter().map(RawForeground::into_fact).collect()
    }
    fn load_runs(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
        cursor: StartCursor<FactId>,
    ) -> Result<Vec<CaptureRunsRow>, ReadFactsError> {
        let rows = capture_runs::table
            .filter(capture_runs::started_utc_ms.lt(range.end))
            .filter(
                capture_runs::ended_utc_ms
                    .gt(range.start)
                    .or(capture_runs::ended_utc_ms
                        .is_null()
                        .and(capture_runs::confirmed_utc_ms.gt(range.start))),
            )
            .filter(
                capture_runs::started_utc_ms.gt(cursor.start_utc_ms).or(
                    capture_runs::started_utc_ms
                        .eq(cursor.start_utc_ms)
                        .and(capture_runs::id.gt(cursor.id.0)),
                ),
            )
            .order(capture_runs::started_utc_ms)
            .then_order_by(capture_runs::id)
            .limit(i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64"))
            .select(CaptureRunsRow::as_select())
            .load::<CaptureRunsRow>(connection)?;
        if let Some(row) = rows.last() {
            *self = Self::Runs(StartCursor {
                start_utc_ms: row.started_utc_ms,
                id: FactId(row.id.ok_or(ReadFactsError::MissingId {
                    kind: FactKind::Run,
                })?),
            });
        }
        Ok(rows)
    }
    fn load_policies(
        &mut self,
        connection: &mut diesel::SqliteConnection,
        range: QueryRange,
    ) -> Result<Vec<TimedPoliciesRow>, ReadFactsError> {
        let rows = timed_policies::table
            .filter(timed_policies::started_utc_ms.lt(range.end))
            .filter(timed_policies::until_utc_ms.gt(range.start))
            .order(timed_policies::started_utc_ms)
            .then_order_by(timed_policies::kind)
            .limit(i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64"))
            .select(TimedPoliciesRow::as_select())
            .load::<TimedPoliciesRow>(connection)?;
        Ok(rows)
    }
}

#[derive(diesel::Queryable)]
struct RawForeground {
    fact: IntervalsRow,
    window: WindowDetails,
}
impl RawForeground {
    fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let mut fact = self.fact.into_fact()?;
        fact.set_window(self.window);
        Ok(fact)
    }
}
