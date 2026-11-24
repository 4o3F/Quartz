//! Snapshot range reads and decoding of stored endpoints.

use super::super::{
    ReadOnlyStore, enums,
    identifiers::ColumnName,
    rows::{AvailabilityIntervalsRow, CaptureGapsRow, IntervalsRow},
    schema::{
        applications, availability_intervals, capture_gaps, capture_runs, clock_segments,
        intervals, timed_policies, titles, web_intervals, window_instances,
    },
};
use super::{
    Fact, FactDetails, FactEnd, FactEndReason, FactExtent, FactKind, FactRange, FactReason,
    ReadFactsError, RowCounts,
};
use diesel::{Connection, ExpressionMethods, QueryDsl, RunQueryDsl, SelectableHelper};
use quartz_domain::capture::limits::UTC_DAY_MS;

#[derive(Debug, diesel::Queryable)]
struct NullableExtent {
    from_utc_ms: Option<i64>,
    to_utc_ms: Option<i64>,
}
impl ReadOnlyStore {
    /// A transaction pins one WAL snapshot across all three tables.
    pub(crate) fn fact_range(&mut self, from: i64, to: i64) -> Result<FactRange, ReadFactsError> {
        self.connection
            .transaction(|connection| Self::load_range(connection, from, to))
    }
    pub(super) fn load_range(
        connection: &mut diesel::SqliteConnection,
        from: i64,
        to: i64,
    ) -> Result<FactRange, ReadFactsError> {
        // Keep closed and open reads separate: open availability may span many days.
        let mut foreground = Vec::new();
        for rows in [
            intervals::table
                .filter(intervals::start_utc_ms.ge(from.saturating_sub(UTC_DAY_MS)))
                .filter(intervals::start_utc_ms.lt(to))
                .filter(intervals::end_utc_ms.gt(from))
                .select(IntervalsRow::as_select())
                .load::<IntervalsRow>(connection)?,
            intervals::table
                .filter(intervals::end_utc_ms.is_null())
                .filter(intervals::start_utc_ms.lt(to))
                .filter(intervals::confirmed_utc_ms.gt(from))
                .select(IntervalsRow::as_select())
                .load::<IntervalsRow>(connection)?,
        ] {
            for row in rows {
                foreground.push(row.into_fact()?);
            }
        }
        let mut availability = Vec::new();
        for rows in [
            availability_intervals::table
                .filter(availability_intervals::start_utc_ms.ge(from.saturating_sub(UTC_DAY_MS)))
                .filter(availability_intervals::start_utc_ms.lt(to))
                .filter(availability_intervals::end_utc_ms.gt(from))
                .select(AvailabilityIntervalsRow::as_select())
                .load::<AvailabilityIntervalsRow>(connection)?,
            availability_intervals::table
                .filter(availability_intervals::end_utc_ms.is_null())
                .filter(availability_intervals::start_utc_ms.lt(to))
                .filter(availability_intervals::confirmed_utc_ms.gt(from))
                .select(AvailabilityIntervalsRow::as_select())
                .load::<AvailabilityIntervalsRow>(connection)?,
        ] {
            for row in rows {
                availability.push(row.into_fact()?);
            }
        }
        let gaps = capture_gaps::table
            .filter(capture_gaps::start_utc_ms.lt(to))
            .filter(capture_gaps::end_utc_ms.gt(from))
            .select(CaptureGapsRow::as_select())
            .load::<CaptureGapsRow>(connection)?
            .into_iter()
            .map(CaptureGapsRow::into_fact)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FactRange {
            foreground,
            availability,
            gaps,
        })
    }
    pub(crate) fn extent(&mut self) -> Result<Option<FactExtent>, ReadFactsError> {
        use diesel::dsl::{max, min};
        let extent = capture_runs::table
            .filter(capture_runs::ended_utc_ms.is_not_null())
            .select((
                min(capture_runs::started_utc_ms),
                max(capture_runs::ended_utc_ms),
            ))
            .first::<NullableExtent>(&mut self.connection)?;
        match (extent.from_utc_ms, extent.to_utc_ms) {
            (Some(from_utc_ms), Some(to_utc_ms)) => Ok(Some(FactExtent {
                from_utc_ms,
                to_utc_ms,
            })),
            (None, None) => Ok(None),
            (Some(_), None) | (None, Some(_)) => Err(ReadFactsError::InvalidExtent),
        }
    }
    pub(crate) fn row_counts(&mut self) -> Result<RowCounts, ReadFactsError> {
        Ok(RowCounts {
            capture_runs: capture_runs::table
                .count()
                .get_result(&mut self.connection)?,
            clock_segments: clock_segments::table
                .count()
                .get_result(&mut self.connection)?,
            applications: applications::table
                .count()
                .get_result(&mut self.connection)?,
            window_instances: window_instances::table
                .count()
                .get_result(&mut self.connection)?,
            titles: titles::table.count().get_result(&mut self.connection)?,
            intervals: intervals::table.count().get_result(&mut self.connection)?,
            availability_intervals: availability_intervals::table
                .count()
                .get_result(&mut self.connection)?,
            capture_gaps: capture_gaps::table
                .count()
                .get_result(&mut self.connection)?,
            timed_policies: timed_policies::table
                .count()
                .get_result(&mut self.connection)?,
            web_intervals: web_intervals::table
                .count()
                .get_result(&mut self.connection)?,
        })
    }
}
impl IntervalsRow {
    pub(in crate::store) fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let kind = match self.kind {
            enums::IntervalKind::Window => FactKind::Foreground,
            enums::IntervalKind::NoForeground => FactKind::NoForeground,
        };
        let mut fact = Fact::new(
            kind,
            self.id,
            self.start_utc_ms,
            self.end_utc_ms,
            self.confirmed_utc_ms,
        )?;
        fact.run_id = Some(self.run_id);
        fact.segment_id = Some(self.clock_segment_id);
        fact.start_mono_ms = Some(self.start_mono_ms);
        fact.end_mono_ms = self.end_mono_ms;
        fact.confirmed_mono_ms = self.confirmed_mono_ms;
        fact.duration_ms = Some(match fact.end {
            FactEnd::Closed(_) => self.duration_ms.ok_or(ReadFactsError::MissingColumn {
                kind,
                id: fact.id,
                column: ColumnName::of(intervals::duration_ms),
            })?,
            FactEnd::Open { .. } => fact.monotonic_duration()?,
        });
        fact.end_reason = self.end_reason.map(FactEndReason::Interval);
        fact.no_foreground_reason = self.no_foreground_reason;
        fact.capture_level = Some(self.capture_level);
        fact.title_status = Some(self.title_status);
        fact.details = Some(FactDetails::Interval {
            sequence: self.sequence,
            session_key: self.session_key,
            app_id: self.app_id,
            window_instance_id: self.window_instance_id,
            privacy_rule_id: self.privacy_rule_id,
            privacy_label_key: self.privacy_label_key,
            privacy_label: self.privacy_label,
        });
        Ok(fact)
    }
}
impl AvailabilityIntervalsRow {
    pub(in crate::store) fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let mut fact = Fact::new(
            FactKind::Availability,
            self.id,
            self.start_utc_ms,
            self.end_utc_ms,
            self.confirmed_utc_ms,
        )?;
        fact.run_id = Some(self.run_id);
        fact.segment_id = Some(self.clock_segment_id);
        fact.start_mono_ms = Some(self.start_mono_ms);
        fact.end_mono_ms = self.end_mono_ms;
        fact.confirmed_mono_ms = self.confirmed_mono_ms;
        fact.duration_ms = Some(fact.monotonic_duration()?);
        fact.end_reason = self.end_reason.map(FactEndReason::Availability);
        fact.reason = Some(FactReason::Availability(self.reason));
        fact.exception = self.exception;
        fact.details = Some(FactDetails::Availability {
            last_input_utc_ms: self.last_input_utc_ms,
            detected_utc_ms: self.detected_utc_ms,
            threshold_ms: self.threshold_ms,
            policy_version: self.policy_version,
        });
        Ok(fact)
    }
}
impl CaptureGapsRow {
    pub(in crate::store) fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let mut fact = Fact::new(
            FactKind::CaptureGap,
            self.id,
            self.start_utc_ms,
            Some(self.end_utc_ms),
            None,
        )?;
        fact.run_id = Some(self.run_id);
        fact.segment_id = Some(self.clock_segment_id);
        fact.start_mono_ms = Some(self.start_mono_ms);
        fact.end_mono_ms = Some(self.end_mono_ms);
        fact.duration_ms = Some(fact.monotonic_duration()?);
        fact.reason = Some(FactReason::Gap(self.reason));
        Ok(fact)
    }
}
