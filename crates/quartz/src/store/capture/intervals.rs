//! Sanitized foreground row writes.

use super::{Capture, CaptureColumn, DbRowId, ExecuteCaptureBatchError, LogicalKey, TimeColumns};
use crate::store::{
    enums,
    rows::IntervalsRow,
    schema::{intervals, titles, window_instances},
};
use diesel::prelude::*;
use quartz_domain::capture::{
    CapturedFields, IntervalEndReason, IntervalFact, OpenInterval, PrivacyExplanation,
    PrivacyLabel, RowId, RunId, TimeRange, Timestamp, WindowId,
};

impl Capture<'_> {
    pub(super) fn open_interval(
        &mut self,
        run: RunId,
        open: &OpenInterval,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let segment = self.segment(run, open.segment)?;
        let mut row = IntervalsRow::opened(Self::run_id(run)?, segment, open)?;
        match &open.fact {
            IntervalFact::NoForeground(reason) => row.no_foreground_reason = Some((*reason).into()),
            IntervalFact::Window { instance, fields } => {
                self.interval_window(run, *instance, fields, &mut row)?
            }
        }
        let replacement = DbRowId::new(
            diesel::insert_into(intervals::table)
                .values(&row)
                .returning(intervals::id.assume_not_null())
                .get_result(self.connection)?,
        );
        if let Some(existing) = self.run_keys(run).intervals.insert(open.id, replacement) {
            return Err(ExecuteCaptureBatchError::DuplicateKey {
                run,
                key: LogicalKey::Interval(open.id),
                existing: existing.get(),
                replacement: replacement.get(),
            });
        }
        self.bump_days(
            enums::DataSource::Intervals,
            TimeRange {
                start: open.start,
                end: open.start,
            },
        )
    }
    fn interval_window(
        &mut self,
        run: RunId,
        instance: Option<WindowId>,
        fields: &CapturedFields,
        row: &mut IntervalsRow,
    ) -> Result<(), ExecuteCaptureBatchError> {
        row.kind = enums::IntervalKind::Window;
        row.title_status = fields.title_status.into();
        row.capture_level = fields.level.into();
        if let Some(instance) = instance {
            let id = self.window(run, instance)?;
            row.window_instance_id = Some(id.get());
            row.app_id = Some(
                window_instances::table
                    .filter(window_instances::id.eq(id.get()))
                    .select(window_instances::app_id)
                    .first(self.connection)?,
            );
        }
        row.title_id = fields
            .title
            .as_deref()
            .map(|text| self.title(text).map(DbRowId::get))
            .transpose()?;
        row.apply_privacy(fields.privacy.as_ref());
        Ok(())
    }
    fn title(&mut self, text: &str) -> Result<DbRowId, ExecuteCaptureBatchError> {
        diesel::insert_into(titles::table)
            .values(titles::text.eq(text))
            .on_conflict(titles::text)
            .do_update()
            .set(titles::text.eq(text))
            .returning(titles::id.assume_not_null())
            .get_result(self.connection)
            .map(DbRowId::new)
            .map_err(ExecuteCaptureBatchError::from)
    }
    pub(super) fn close_interval(
        &mut self,
        run: RunId,
        logical: RowId,
        at: Timestamp,
        reason: IntervalEndReason,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let id = self.interval(run, logical)?;
        let row = intervals::table
            .filter(intervals::id.eq(id.get()))
            .filter(intervals::end_utc_ms.is_null())
            .select(IntervalsRow::as_select())
            .first(self.connection)?;
        let context = id.context("intervals");
        let start = context.point(row.start_utc_ms, row.start_mono_ms, TimeColumns::Start)?;
        let duration = context.field("duration_ms").range(start, at)?;
        diesel::update(intervals::table.filter(intervals::id.eq(id.get())))
            .set((
                intervals::end_utc_ms.eq(context.field("end_utc_ms").utc(at.utc)?),
                intervals::end_mono_ms.eq(context.field("end_mono_ms").mono(at.mono)?),
                intervals::duration_ms.eq(context.field("duration_ms").duration(duration)?),
                intervals::end_reason.eq(enums::IntervalEndReason::from(reason)),
                intervals::confirmed_utc_ms.eq(None::<i64>),
                intervals::confirmed_mono_ms.eq(None::<i64>),
            ))
            .execute(self.connection)?;
        self.run_keys(run).intervals.remove(&logical);
        self.bump_days(enums::DataSource::Intervals, TimeRange { start, end: at })
    }
}
impl IntervalsRow {
    fn opened(
        run: DbRowId,
        segment: DbRowId,
        open: &OpenInterval,
    ) -> Result<Self, ExecuteCaptureBatchError> {
        let utc = CaptureColumn::new("intervals", "start_utc_ms").utc(open.start.utc)?;
        let mono = CaptureColumn::new("intervals", "start_mono_ms").mono(open.start.mono)?;
        Ok(Self {
            id: None,
            run_id: run.get(),
            sequence: CaptureColumn::new("intervals", "sequence").integer(open.id.get())?,
            clock_segment_id: segment.get(),
            kind: enums::IntervalKind::NoForeground,
            no_foreground_reason: None,
            app_id: None,
            window_instance_id: None,
            title_id: None,
            title_status: enums::TitleStatus::Empty,
            capture_level: enums::CaptureLevel::Full,
            privacy_rule_id: None,
            privacy_label_key: None,
            privacy_label: None,
            session_key: CaptureColumn::new("intervals", "session_key")
                .integer(open.session.get())?,
            start_utc_ms: utc,
            start_mono_ms: mono,
            end_utc_ms: None,
            end_mono_ms: None,
            confirmed_utc_ms: Some(utc),
            confirmed_mono_ms: Some(mono),
            duration_ms: None,
            end_reason: None,
        })
    }
    fn apply_privacy(&mut self, privacy: Option<&PrivacyExplanation>) {
        if let Some(privacy) = privacy {
            self.privacy_rule_id = Some(privacy.rule_id.clone());
            match &privacy.label {
                Some(PrivacyLabel::Key(key)) => self.privacy_label_key = Some(key.clone()),
                Some(PrivacyLabel::Text(text)) => self.privacy_label = Some(text.clone()),
                None => {}
            }
        }
    }
}
