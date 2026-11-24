//! Atomic confirmations and settlement of persisted open rows.

use super::{Capture, CaptureColumn, DbRowId, ExecuteCaptureBatchError, TimeColumns};
use crate::store::{
    enums,
    rows::{AvailabilityIntervalsRow, ClockSegmentsRow, IntervalsRow},
    schema::{availability_intervals, capture_runs, clock_segments, intervals},
};
use diesel::NullableExpressionMethods;
use diesel::prelude::{ExpressionMethods, QueryDsl, RunQueryDsl, SelectableHelper};
use quartz_domain::capture::{
    AvailabilityEndReason, Checkpoint, IntervalEndReason, RunId, SegmentId, TimeRange, Timestamp,
};

impl Capture<'_> {
    pub(super) fn checkpoint(
        &mut self,
        point: &Checkpoint,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let run = Self::run_id(point.run)?;
        let segment = self.segment(point.run, point.segment)?;
        self.confirm_foreground(run, point.at)?;
        self.confirm_availability(run, point.at)?;
        self.confirm_run(run, point.at)?;
        self.confirm_segment(segment, point.at)
    }
    fn confirm_foreground(
        &mut self,
        run: DbRowId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let rows = intervals::table
            .filter(intervals::run_id.eq(run.get()))
            .filter(intervals::end_utc_ms.is_null())
            .select(IntervalsRow::as_select())
            .load(self.connection)?;
        for row in rows {
            let context = super::RowContext::new(row.id);
            let start = context.point(
                row.start_utc_ms,
                row.start_mono_ms,
                TimeColumns::new(intervals::start_utc_ms, intervals::start_mono_ms),
            )?;
            context.confirmation(
                start,
                at,
                TimeColumns::new(intervals::confirmed_utc_ms, intervals::confirmed_mono_ms),
            )?;
            self.bump_days(enums::DataSource::Intervals, TimeRange { start, end: at })?;
        }
        diesel::update(
            intervals::table
                .filter(intervals::run_id.eq(run.get()))
                .filter(intervals::end_utc_ms.is_null()),
        )
        .set((
            intervals::confirmed_utc_ms
                .eq(CaptureColumn::new(intervals::confirmed_utc_ms).utc(at.utc)?),
            intervals::confirmed_mono_ms
                .eq(CaptureColumn::new(intervals::confirmed_mono_ms).mono(at.mono)?),
        ))
        .execute(self.connection)?;
        Ok(())
    }
    fn confirm_availability(
        &mut self,
        run: DbRowId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let rows = availability_intervals::table
            .filter(availability_intervals::run_id.eq(run.get()))
            .filter(availability_intervals::end_utc_ms.is_null())
            .select(AvailabilityIntervalsRow::as_select())
            .load(self.connection)?;
        for row in rows {
            let context = super::RowContext::new(row.id);
            let start = context.point(
                context.required(
                    row.confirmed_utc_ms,
                    availability_intervals::confirmed_utc_ms,
                )?,
                context.required(
                    row.confirmed_mono_ms,
                    availability_intervals::confirmed_mono_ms,
                )?,
                TimeColumns::new(
                    availability_intervals::confirmed_utc_ms,
                    availability_intervals::confirmed_mono_ms,
                ),
            )?;
            context.confirmation(
                start,
                at,
                TimeColumns::new(
                    availability_intervals::confirmed_utc_ms,
                    availability_intervals::confirmed_mono_ms,
                ),
            )?;
            if start != at {
                self.bump_days(
                    enums::DataSource::Availability,
                    TimeRange { start, end: at },
                )?;
            }
        }
        diesel::update(
            availability_intervals::table
                .filter(availability_intervals::run_id.eq(run.get()))
                .filter(availability_intervals::end_utc_ms.is_null()),
        )
        .set((
            availability_intervals::confirmed_utc_ms
                .eq(CaptureColumn::new(availability_intervals::confirmed_utc_ms).utc(at.utc)?),
            availability_intervals::confirmed_mono_ms
                .eq(CaptureColumn::new(availability_intervals::confirmed_mono_ms).mono(at.mono)?),
        ))
        .execute(self.connection)?;
        Ok(())
    }
    fn confirm_run(&mut self, id: DbRowId, at: Timestamp) -> Result<(), ExecuteCaptureBatchError> {
        let count = diesel::update(
            capture_runs::table
                .filter(capture_runs::id.eq(id.get()))
                .filter(capture_runs::ended_utc_ms.is_null()),
        )
        .set(
            capture_runs::confirmed_utc_ms.eq(id
                .context()
                .field(capture_runs::confirmed_utc_ms)
                .utc(at.utc)?),
        )
        .execute(self.connection)?;
        id.require_open(capture_runs::table, count)
    }
    fn confirm_segment(
        &mut self,
        id: DbRowId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let context = id.context();
        let row = clock_segments::table
            .filter(clock_segments::id.eq(id.get()))
            .filter(clock_segments::end_utc_ms.is_null())
            .select(ClockSegmentsRow::as_select())
            .first::<ClockSegmentsRow>(self.connection)?;
        let start = context.point(
            context.required(row.confirmed_utc_ms, clock_segments::confirmed_utc_ms)?,
            context.required(row.confirmed_mono_ms, clock_segments::confirmed_mono_ms)?,
            TimeColumns::new(
                clock_segments::confirmed_utc_ms,
                clock_segments::confirmed_mono_ms,
            ),
        )?;
        let count = diesel::update(
            clock_segments::table
                .filter(clock_segments::id.eq(id.get()))
                .filter(clock_segments::end_utc_ms.is_null()),
        )
        .set((
            clock_segments::confirmed_utc_ms.eq(context
                .field(clock_segments::confirmed_utc_ms)
                .utc(at.utc)?),
            clock_segments::confirmed_mono_ms.eq(context
                .field(clock_segments::confirmed_mono_ms)
                .mono(at.mono)?),
        ))
        .execute(self.connection)?;
        id.require_open(clock_segments::table, count)?;
        self.bump_days(enums::DataSource::Intervals, TimeRange { start, end: at })
    }
    pub(super) fn settle_persisted(
        &mut self,
        run: RunId,
        segment: SegmentId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let segment = self.segment(run, segment)?;
        self.settle_foreground(run, segment, at)?;
        self.settle_availability(run, segment, at)
    }
    fn settle_foreground(
        &mut self,
        run: RunId,
        segment: DbRowId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let ids = intervals::table
            .filter(intervals::clock_segment_id.eq(segment.get()))
            .filter(intervals::end_utc_ms.is_null())
            .select(intervals::id.assume_not_null())
            .load::<i64>(self.connection)?;
        for id in ids {
            let row_id = DbRowId::new(id);
            let key = self
                .run_keys(run)
                .intervals
                .iter()
                .find(|(_, value)| **value == row_id)
                .map(|(key, _)| *key)
                .ok_or(ExecuteCaptureBatchError::MissingMapping {
                    run,
                    table: crate::store::identifiers::TableName::of(intervals::table),
                    row_id: row_id.get(),
                })?;
            self.close_interval(run, key, at, IntervalEndReason::CaptureGap)?;
        }
        Ok(())
    }
    fn settle_availability(
        &mut self,
        run: RunId,
        segment: DbRowId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let ids = availability_intervals::table
            .filter(availability_intervals::clock_segment_id.eq(segment.get()))
            .filter(availability_intervals::end_utc_ms.is_null())
            .select(availability_intervals::id.assume_not_null())
            .load::<i64>(self.connection)?;
        for id in ids {
            let row_id = DbRowId::new(id);
            let key = self
                .run_keys(run)
                .availability
                .iter()
                .find(|(_, value)| **value == row_id)
                .map(|(key, _)| *key)
                .ok_or(ExecuteCaptureBatchError::MissingMapping {
                    run,
                    table: crate::store::identifiers::TableName::of(availability_intervals::table),
                    row_id: row_id.get(),
                })?;
            self.close_availability(run, key, at, AvailabilityEndReason::PolicyChanged)?;
        }
        Ok(())
    }
}
