//! Run and clock segment writes.

use super::{Capture, CaptureColumn, DbRowId, ExecuteCaptureBatchError, LogicalKey};
use crate::store::{
    enums,
    rows::{CaptureRunsRow, ClockSegmentsRow},
    schema::{capture_runs, clock_segments},
};
use diesel::NullableExpressionMethods;
use diesel::prelude::{ExpressionMethods, QueryDsl, RunQueryDsl, SelectableHelper};
use quartz_domain::capture::{
    ClockEndReason, RunEndReason, RunId, SegmentId, TimeRange, Timestamp,
};

impl Capture<'_> {
    pub(super) fn open_run(
        &mut self,
        run: RunId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let utc = CaptureColumn::new(capture_runs::started_utc_ms).utc(at.utc)?;
        let row = CaptureRunsRow {
            id: Some(Self::run_id(run)?.get()),
            device_id: self.metadata.device_id.clone(),
            win_session_id: i64::from(self.metadata.win_session_id.0),
            app_version: self.metadata.app_version.clone(),
            started_utc_ms: utc,
            confirmed_utc_ms: Some(utc),
            ended_utc_ms: None,
            end_reason: None,
        };
        diesel::insert_into(capture_runs::table)
            .values(&row)
            .execute(self.connection)?;
        Ok(())
    }
    pub(super) fn close_run(
        &mut self,
        run: RunId,
        at: Timestamp,
        reason: RunEndReason,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let id = Self::run_id(run)?;
        let count = diesel::update(
            capture_runs::table
                .filter(capture_runs::id.eq(id.get()))
                .filter(capture_runs::ended_utc_ms.is_null()),
        )
        .set((
            capture_runs::ended_utc_ms
                .eq(id.context().field(capture_runs::ended_utc_ms).utc(at.utc)?),
            capture_runs::confirmed_utc_ms.eq(None::<i64>),
            capture_runs::end_reason.eq(enums::RunEndReason::from(reason)),
        ))
        .execute(self.connection)?;
        id.require_open(capture_runs::table, count)
    }
    pub(super) fn open_clock_segment(
        &mut self,
        run: RunId,
        segment: SegmentId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let utc = CaptureColumn::new(clock_segments::start_utc_ms).utc(at.utc)?;
        let mono = CaptureColumn::new(clock_segments::start_mono_ms).mono(at.mono)?;
        let row = ClockSegmentsRow {
            id: None,
            run_id: Self::run_id(run)?.get(),
            start_utc_ms: utc,
            start_mono_ms: mono,
            confirmed_utc_ms: Some(utc),
            confirmed_mono_ms: Some(mono),
            end_utc_ms: None,
            end_mono_ms: None,
            end_reason: None,
        };
        let replacement = DbRowId::new(
            diesel::insert_into(clock_segments::table)
                .values(&row)
                .returning(clock_segments::id.assume_not_null())
                .get_result(self.connection)?,
        );
        if let Some(existing) = self.run_keys(run).segments.insert(segment, replacement) {
            return Err(ExecuteCaptureBatchError::DuplicateKey {
                run,
                key: LogicalKey::Segment(segment),
                existing: existing.get(),
                replacement: replacement.get(),
            });
        }
        self.bump_days(
            enums::DataSource::Intervals,
            TimeRange { start: at, end: at },
        )
    }
    pub(super) fn close_clock_segment(
        &mut self,
        run: RunId,
        segment: SegmentId,
        at: Timestamp,
        reason: ClockEndReason,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let id = self.segment(run, segment)?;
        let context = id.context();
        let row = clock_segments::table
            .filter(clock_segments::id.eq(id.get()))
            .filter(clock_segments::end_utc_ms.is_null())
            .select(ClockSegmentsRow::as_select())
            .first::<ClockSegmentsRow>(self.connection)?;
        let start = context.point(
            row.start_utc_ms,
            row.start_mono_ms,
            super::TimeColumns::new(clock_segments::start_utc_ms, clock_segments::start_mono_ms),
        )?;
        let count = diesel::update(
            clock_segments::table
                .filter(clock_segments::id.eq(id.get()))
                .filter(clock_segments::end_utc_ms.is_null()),
        )
        .set((
            clock_segments::end_utc_ms.eq(context.field(clock_segments::end_utc_ms).utc(at.utc)?),
            clock_segments::end_mono_ms
                .eq(context.field(clock_segments::end_mono_ms).mono(at.mono)?),
            clock_segments::confirmed_utc_ms.eq(None::<i64>),
            clock_segments::confirmed_mono_ms.eq(None::<i64>),
            clock_segments::end_reason.eq(enums::ClockEndReason::from(reason)),
        ))
        .execute(self.connection)?;
        id.require_open(clock_segments::table, count)?;
        self.run_keys(run).closed_segments.insert(segment);
        self.bump_days(enums::DataSource::Intervals, TimeRange { start, end: at })
    }
}
