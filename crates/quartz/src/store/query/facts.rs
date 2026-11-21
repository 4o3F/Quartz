//! Raw facts reuse the pipeline's bounded scans before dictionary enrichment.

use super::{ForegroundDetails, READ_BATCH_ROWS, ScanCursor, StartCursor};
use crate::store::{
    facts::{Fact, FactKind, ReadFactsError},
    rows::{AvailabilityIntervalsRow, CaptureGapsRow, CaptureRunsRow, TimedPoliciesRow},
    schema::{capture_runs, timed_policies},
};
use diesel::{BoolExpressionMethods, ExpressionMethods, QueryDsl, RunQueryDsl, SelectableHelper};
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
                .map(|row| row.clock.into_fact())
                .collect(),
            Self::ForegroundClosed(cursor) => {
                let rows = self.load_foreground_closed(connection, range, cursor)?;
                Self::foreground_facts(connection, rows)
            }
            Self::ForegroundOpen(cursor) => {
                let rows = self.load_foreground_open(connection, range, cursor)?;
                Self::foreground_facts(connection, rows)
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
    fn foreground_facts(
        connection: &mut diesel::SqliteConnection,
        rows: Vec<ForegroundDetails>,
    ) -> Result<Vec<Fact>, ReadFactsError> {
        let mut facts = rows
            .into_iter()
            .map(|row| row.fact.into_fact())
            .collect::<Result<Vec<_>, _>>()?;
        Fact::enrich(connection, &mut facts)?;
        Ok(facts)
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
