//! Identity-free gaps, policy deadlines and source-day invalidation.

use super::{Capture, CaptureColumn, ExecuteCaptureBatchError};
use crate::store::{
    enums::DataSource,
    rows::CaptureGapsRow,
    schema::{capture_gaps, settings},
};
use diesel::prelude::{ExpressionMethods, QueryDsl, RunQueryDsl};
use quartz_domain::capture::{CaptureGap, RunId, SchedulerKind, TimeRange, UtcInstant};

impl Capture<'_> {
    pub(super) fn record_gap(
        &mut self,
        run: RunId,
        gap: &CaptureGap,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let segment = self.segment(run, gap.segment)?;
        CaptureColumn::new(capture_gaps::end_mono_ms).range(gap.range.start, gap.range.end)?;
        let row = CaptureGapsRow {
            id: None,
            run_id: Self::run_id(run)?.get(),
            clock_segment_id: segment.get(),
            start_utc_ms: CaptureColumn::new(capture_gaps::start_utc_ms)
                .utc(gap.range.start.utc)?,
            end_utc_ms: CaptureColumn::new(capture_gaps::end_utc_ms).utc(gap.range.end.utc)?,
            start_mono_ms: CaptureColumn::new(capture_gaps::start_mono_ms)
                .mono(gap.range.start.mono)?,
            end_mono_ms: CaptureColumn::new(capture_gaps::end_mono_ms).mono(gap.range.end.mono)?,
            reason: gap.reason.into(),
        };
        diesel::insert_into(capture_gaps::table)
            .values(&row)
            .execute(self.connection)?;
        self.bump_days(DataSource::Gaps, gap.range)
    }
    pub(super) fn bump_days(
        &mut self,
        source: DataSource,
        range: TimeRange,
    ) -> Result<(), ExecuteCaptureBatchError> {
        self.effects
            .bump_days(self.connection, source, range)
            .map_err(ExecuteCaptureBatchError::from)
    }
    pub(super) fn set_deadline(
        &mut self,
        owner: SchedulerKind,
        at: UtcInstant,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let value = CaptureColumn::new(settings::value).utc(at)?.to_string();
        diesel::insert_into(settings::table)
            .values((
                settings::key.eq(Self::deadline_key(owner)),
                settings::value.eq(&value),
            ))
            .on_conflict(settings::key)
            .do_update()
            .set(settings::value.eq(&value))
            .execute(self.connection)?;
        Ok(())
    }
    pub(super) fn clear_deadline(
        &mut self,
        owner: SchedulerKind,
    ) -> Result<(), ExecuteCaptureBatchError> {
        diesel::delete(settings::table.filter(settings::key.eq(Self::deadline_key(owner))))
            .execute(self.connection)?;
        Ok(())
    }
    pub(super) fn deadline_key(owner: SchedulerKind) -> &'static str {
        match owner {
            SchedulerKind::Maintenance => crate::store::versions::MAINTENANCE_DEADLINE_KEY,
            SchedulerKind::Idle => crate::store::versions::IDLE_DEADLINE_KEY,
            SchedulerKind::Midnight => crate::store::versions::MIDNIGHT_DEADLINE_KEY,
        }
    }
}
