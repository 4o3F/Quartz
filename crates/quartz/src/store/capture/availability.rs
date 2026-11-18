//! Independent availability masks and their UTC midnight pieces.

use super::{Capture, CaptureColumn, DbRowId, ExecuteCaptureBatchError, LogicalKey, TimeColumns};
use crate::store::{enums, rows::AvailabilityIntervalsRow, schema::availability_intervals};
use diesel::prelude::*;
use quartz_domain::capture::{
    AvailabilityEndReason, IdleRecord, OpenAvailability, RowId, RunId, TimeRange, Timestamp,
};

/// SQLite's guaranteed minimum bind limit; data model 03 §4.5.
const SQLITE_BIND_LIMIT: usize = 999;
/// Maximum parameters per availability row, including its generated ID (03 §2.4).
const AVAILABILITY_COLUMNS: usize = 16;

impl Capture<'_> {
    pub(super) fn open_availability(
        &mut self,
        run: RunId,
        open: &OpenAvailability,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let segment = self.segment(run, open.segment)?;
        let row = AvailabilityIntervalsRow::opened(Self::run_id(run)?, segment, open)?;
        let replacement = DbRowId::new(
            diesel::insert_into(availability_intervals::table)
                .values(&row)
                .returning(availability_intervals::id.assume_not_null())
                .get_result(self.connection)?,
        );
        if let Some(existing) = self.run_keys(run).availability.insert(open.id, replacement) {
            return Err(ExecuteCaptureBatchError::DuplicateKey {
                run,
                key: LogicalKey::Availability(open.id),
                existing: existing.get(),
                replacement: replacement.get(),
            });
        }
        self.bump_days(
            enums::DataSource::Availability,
            TimeRange {
                start: open.start,
                end: open.start,
            },
        )
    }
    pub(super) fn close_availability(
        &mut self,
        run: RunId,
        logical: RowId,
        at: Timestamp,
        reason: AvailabilityEndReason,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let id = self.availability(run, logical)?;
        let mut row = availability_intervals::table
            .filter(availability_intervals::id.eq(id.get()))
            .filter(availability_intervals::end_utc_ms.is_null())
            .select(AvailabilityIntervalsRow::as_select())
            .first(self.connection)?;
        let context = id.context("availability_intervals");
        let start = context.point(row.start_utc_ms, row.start_mono_ms, TimeColumns::Start)?;
        let range = TimeRange { start, end: at };
        let pieces = range
            .split_midnights()
            .map_err(|source| ExecuteCaptureBatchError::Range {
                field: context.field("end_utc_ms"),
                start,
                end: at,
                source,
            })?;
        let mut remaining = Vec::new();
        for (index, piece) in pieces.iter().enumerate() {
            row.close_piece(
                *piece,
                if index + 1 == pieces.len() {
                    reason
                } else {
                    AvailabilityEndReason::MidnightSplit
                },
            )?;
            if index == 0 {
                self.close_first_availability(id, &row)?;
            } else {
                row.id = None;
                remaining.push(row.clone());
            }
        }
        self.insert_availability_pieces(&remaining)?;
        self.run_keys(run).availability.remove(&logical);
        self.bump_days(enums::DataSource::Availability, range)
    }
    fn close_first_availability(
        &mut self,
        id: DbRowId,
        row: &AvailabilityIntervalsRow,
    ) -> Result<(), ExecuteCaptureBatchError> {
        diesel::update(
            availability_intervals::table.filter(availability_intervals::id.eq(id.get())),
        )
        .set((
            availability_intervals::end_utc_ms.eq(row.end_utc_ms),
            availability_intervals::end_mono_ms.eq(row.end_mono_ms),
            availability_intervals::confirmed_utc_ms.eq(None::<i64>),
            availability_intervals::confirmed_mono_ms.eq(None::<i64>),
            availability_intervals::end_reason.eq(row.end_reason),
            availability_intervals::exception.eq(row.exception),
        ))
        .execute(self.connection)?;
        Ok(())
    }
    fn insert_availability_pieces(
        &mut self,
        rows: &[AvailabilityIntervalsRow],
    ) -> Result<(), ExecuteCaptureBatchError> {
        for chunk in rows.chunks(SQLITE_BIND_LIMIT / AVAILABILITY_COLUMNS) {
            diesel::insert_into(availability_intervals::table)
                .values(chunk)
                .execute(self.connection)?;
        }
        Ok(())
    }
}
impl AvailabilityIntervalsRow {
    fn opened(
        run: DbRowId,
        segment: DbRowId,
        open: &OpenAvailability,
    ) -> Result<Self, ExecuteCaptureBatchError> {
        let utc =
            CaptureColumn::new("availability_intervals", "start_utc_ms").utc(open.start.utc)?;
        let mono =
            CaptureColumn::new("availability_intervals", "start_mono_ms").mono(open.start.mono)?;
        let mut row = Self {
            id: None,
            run_id: run.get(),
            clock_segment_id: segment.get(),
            reason: open.reason.into(),
            start_utc_ms: utc,
            start_mono_ms: mono,
            end_utc_ms: None,
            end_mono_ms: None,
            confirmed_utc_ms: Some(utc),
            confirmed_mono_ms: Some(mono),
            end_reason: None,
            last_input_utc_ms: None,
            detected_utc_ms: None,
            threshold_ms: None,
            exception: None,
            policy_version: i64::from(quartz_domain::capture::IdlePolicy::default().version),
        };
        if let Some(idle) = open.idle {
            row.apply_idle(idle)?;
        }
        Ok(row)
    }
    fn apply_idle(&mut self, idle: IdleRecord) -> Result<(), ExecuteCaptureBatchError> {
        self.last_input_utc_ms = Some(
            CaptureColumn::new("availability_intervals", "last_input_utc_ms")
                .utc(idle.last_input.utc)?,
        );
        self.detected_utc_ms = Some(
            CaptureColumn::new("availability_intervals", "detected_utc_ms")
                .utc(idle.detected.utc)?,
        );
        self.threshold_ms = Some(
            CaptureColumn::new("availability_intervals", "threshold_ms")
                .duration(idle.policy.threshold)?,
        );
        self.policy_version = i64::from(idle.policy.version);
        Ok(())
    }
    fn close_piece(
        &mut self,
        piece: TimeRange,
        reason: AvailabilityEndReason,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let context = super::RowContext::new("availability_intervals", self.id);
        self.start_utc_ms = context.field("start_utc_ms").utc(piece.start.utc)?;
        self.start_mono_ms = context.field("start_mono_ms").mono(piece.start.mono)?;
        self.end_utc_ms = Some(context.field("end_utc_ms").utc(piece.end.utc)?);
        self.end_mono_ms = Some(context.field("end_mono_ms").mono(piece.end.mono)?);
        self.confirmed_utc_ms = None;
        self.confirmed_mono_ms = None;
        self.end_reason = Some(reason.into());
        self.exception = enums::IdleException::from_ending(reason);
        Ok(())
    }
}
