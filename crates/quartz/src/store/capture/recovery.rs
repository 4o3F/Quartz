//! Rebuild recovery values and logical mappings from durable rows.

use super::{
    Capture, CaptureKeys, DbRowId, DecodeCaptureRowError, ExecuteCaptureBatchError, RowContext,
    RunKeys, RunMetadata, TimeColumns,
};
use crate::store::{
    enums::IntegerCode,
    rows::{AvailabilityIntervalsRow, CaptureRunsRow, ClockSegmentsRow, IntervalsRow},
    schema::{availability_intervals, capture_runs, clock_segments, intervals, settings, titles},
};
use diesel::prelude::{
    ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl, SelectableHelper,
};
use quartz_domain::capture::{
    self as domain, ConfirmedAvailability, ConfirmedInterval, ConfirmedSegment, RecoverableRun,
    RowId, RunId, SegmentId, Timestamp, WindowId,
};
use snafu::{ResultExt, Snafu};
use std::time::Duration;

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum LoadRecoverableRunError {
    #[snafu(display("could not load capture recovery rows"))]
    Database { source: diesel::result::Error },
    #[snafu(display("could not decode capture recovery rows"))]
    Decode { source: DecodeCaptureRowError },
}
impl From<diesel::result::Error> for LoadRecoverableRunError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}
impl From<DecodeCaptureRowError> for LoadRecoverableRunError {
    fn from(source: DecodeCaptureRowError) -> Self {
        Self::Decode { source }
    }
}
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum LoadCaptureStartupError {
    #[snafu(display("could not read capture startup state"))]
    Database { source: diesel::result::Error },
    #[snafu(display("could not decode capture startup values"))]
    Decode { source: DecodeCaptureRowError },
}
impl From<diesel::result::Error> for LoadCaptureStartupError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}
impl From<DecodeCaptureRowError> for LoadCaptureStartupError {
    fn from(source: DecodeCaptureRowError) -> Self {
        Self::Decode { source }
    }
}
#[derive(Debug)]
pub(crate) struct PersistedDeadline {
    pub owner: domain::SchedulerKind,
    pub at: domain::UtcInstant,
}
#[derive(Debug)]
pub(crate) struct CaptureStartup {
    pub next_run: RunId,
    pub last_segment: Option<SegmentId>,
    pub deadlines: Vec<PersistedDeadline>,
    pub policies: Vec<domain::TimedState>,
}
#[derive(Debug)]
struct RecoveryRowIds {
    next: i64,
    context: RowContext,
}
impl RecoveryRowIds {
    /// Interval sequences begin at one when a run has no persisted intervals.
    const FIRST_SEQUENCE: i64 = 1;
    fn allocate(&mut self) -> Result<RowId, DecodeCaptureRowError> {
        let id = self.next()?;
        self.next = self
            .context
            .next_identifier(self.next, intervals::sequence)?;
        Ok(id)
    }
    fn next(&self) -> Result<RowId, DecodeCaptureRowError> {
        self.context
            .unsigned(self.next, intervals::sequence)
            .map(RowId::from_persisted)
    }
}
impl Capture<'_> {
    pub(crate) fn load_startup(&mut self) -> Result<CaptureStartup, LoadCaptureStartupError> {
        let last_run = capture_runs::table
            .select(diesel::dsl::max(capture_runs::id))
            .first::<Option<i64>>(self.connection)?;
        let context = RowContext::new(last_run);
        let next = match last_run {
            Some(id) => context.next_identifier(id, capture_runs::id)?,
            None => 1,
        };
        let last_segment = clock_segments::table
            .select(diesel::dsl::max(clock_segments::id))
            .first::<Option<i64>>(self.connection)?;
        let segment_context = RowContext::new(last_segment);
        Ok(CaptureStartup {
            next_run: RunId::new(context.unsigned(next, capture_runs::id)?),
            last_segment: last_segment
                .map(|id| {
                    segment_context
                        .unsigned(id, clock_segments::id)
                        .map(SegmentId::from_persisted)
                })
                .transpose()?,
            deadlines: self.load_deadlines()?,
            policies: self.load_timed_states()?,
        })
    }
    fn load_deadlines(&mut self) -> Result<Vec<PersistedDeadline>, LoadCaptureStartupError> {
        // This removed pre-M5 feature used a settings key, not an integer enum code.
        // Ignore it without decoding its value so obsolete data cannot block startup.
        let removed_key = crate::store::versions::REMOVED_PRIVACY_CONTEXT_DEADLINE_KEY;
        if settings::table
            .find(removed_key)
            .select(settings::key)
            .first::<String>(self.connection)
            .optional()?
            .is_some()
        {
            tracing::warn!(
                key = removed_key,
                "ignoring persisted deadline for removed browser-context deferral"
            );
        }
        let mut deadlines = Vec::new();
        for owner in [
            domain::SchedulerKind::Maintenance,
            domain::SchedulerKind::Idle,
            domain::SchedulerKind::Midnight,
        ] {
            let key = Self::deadline_key(owner);
            if let Some(value) = settings::table
                .find(key)
                .select(settings::value)
                .first::<String>(self.connection)
                .optional()?
            {
                let millis =
                    value
                        .parse::<i64>()
                        .map_err(|source| DecodeCaptureRowError::Deadline {
                            owner,
                            value,
                            source,
                        })?;
                let at = RowContext::new(None).utc(millis, settings::value)?;
                deadlines.push(PersistedDeadline { owner, at });
            }
        }
        Ok(deadlines)
    }
    pub(crate) fn load_recoverable(
        &mut self,
    ) -> Result<Vec<RecoverableRun>, LoadRecoverableRunError> {
        let runs = capture_runs::table
            .filter(capture_runs::ended_utc_ms.is_null())
            .order(capture_runs::id)
            .select(CaptureRunsRow::as_select())
            .load(self.connection)?;
        runs.into_iter().map(|row| self.recover_run(row)).collect()
    }
    fn recover_run(
        &mut self,
        row: CaptureRunsRow,
    ) -> Result<RecoverableRun, LoadRecoverableRunError> {
        let context = RowContext::new(row.id);
        let id = context.required(row.id, capture_runs::id)?;
        let run = RunId::new(context.unsigned(id, capture_runs::id)?);
        self.keys.record(run);
        self.keys.runs.insert(run, RunKeys::default());
        let confirmed = self.load_run_confirmation(&row)?;
        let segments = self.load_open_segments(run, DbRowId::new(id))?;
        let mut ids = self.recovery_row_ids(DbRowId::new(id))?;
        let intervals = self.recover_intervals(run, DbRowId::new(id))?;
        let availability = self.recover_availability(run, DbRowId::new(id), &mut ids)?;
        Ok(RecoverableRun {
            run,
            confirmed,
            next_row: ids.next()?,
            intervals,
            availability,
            segments,
        })
    }
    fn load_run_confirmation(
        &mut self,
        row: &CaptureRunsRow,
    ) -> Result<Timestamp, LoadRecoverableRunError> {
        let run_context = RowContext::new(row.id);
        let id = run_context.required(row.id, capture_runs::id)?;
        let latest = clock_segments::table
            .filter(clock_segments::run_id.eq(id))
            .order(clock_segments::id.desc())
            .select(ClockSegmentsRow::as_select())
            .first::<ClockSegmentsRow>(self.connection)?;
        let segment_context = RowContext::new(latest.id);
        let mono = match latest.end_utc_ms {
            None => segment_context.mono(
                segment_context
                    .required(latest.confirmed_mono_ms, clock_segments::confirmed_mono_ms)?,
                clock_segments::confirmed_mono_ms,
            )?,
            Some(_) => segment_context.mono(
                segment_context.required(latest.end_mono_ms, clock_segments::end_mono_ms)?,
                clock_segments::end_mono_ms,
            )?,
        };
        Ok(Timestamp {
            utc: run_context.utc(
                run_context.required(row.confirmed_utc_ms, capture_runs::confirmed_utc_ms)?,
                capture_runs::confirmed_utc_ms,
            )?,
            mono,
        })
    }
    fn load_open_segments(
        &mut self,
        run: RunId,
        id: DbRowId,
    ) -> Result<Vec<ConfirmedSegment>, LoadRecoverableRunError> {
        let rows = clock_segments::table
            .filter(clock_segments::run_id.eq(id.get()))
            .filter(clock_segments::end_utc_ms.is_null())
            .order(clock_segments::id)
            .select(ClockSegmentsRow::as_select())
            .load(self.connection)?;
        let mut segments = Vec::new();
        for row in rows {
            let context = RowContext::new(row.id);
            let id = context.required(row.id, clock_segments::id)?;
            let segment = SegmentId::from_persisted(context.unsigned(id, clock_segments::id)?);
            self.run_keys(run)
                .segments
                .insert(segment, DbRowId::new(id));
            let start = context.point(
                row.start_utc_ms,
                row.start_mono_ms,
                TimeColumns::new(clock_segments::start_utc_ms, clock_segments::start_mono_ms),
            )?;
            segments.push(ConfirmedSegment {
                segment,
                confirmed: context.confirmed(
                    start,
                    row.confirmed_utc_ms,
                    row.confirmed_mono_ms,
                    TimeColumns::new(
                        clock_segments::confirmed_utc_ms,
                        clock_segments::confirmed_mono_ms,
                    ),
                )?,
            });
        }
        Ok(segments)
    }
    fn recovery_row_ids(
        &mut self,
        run: DbRowId,
    ) -> Result<RecoveryRowIds, LoadRecoverableRunError> {
        let maximum = intervals::table
            .filter(intervals::run_id.eq(run.get()))
            .select(diesel::dsl::max(intervals::sequence))
            .first::<Option<i64>>(self.connection)?;
        let context = RowContext::new(None);
        Ok(RecoveryRowIds {
            next: match maximum {
                Some(sequence) => context.next_identifier(sequence, intervals::sequence)?,
                None => RecoveryRowIds::FIRST_SEQUENCE,
            },
            context,
        })
    }
    fn recover_intervals(
        &mut self,
        run: RunId,
        id: DbRowId,
    ) -> Result<Vec<ConfirmedInterval>, LoadRecoverableRunError> {
        let rows = intervals::table
            .filter(intervals::run_id.eq(id.get()))
            .filter(intervals::end_utc_ms.is_null())
            .select(IntervalsRow::as_select())
            .load(self.connection)?;
        rows.into_iter()
            .map(|row| self.recover_interval(run, row))
            .collect()
    }
    fn recover_interval(
        &mut self,
        run: RunId,
        row: IntervalsRow,
    ) -> Result<ConfirmedInterval, LoadRecoverableRunError> {
        let context = RowContext::new(row.id);
        let id = RowId::from_persisted(context.unsigned(row.sequence, intervals::sequence)?);
        self.run_keys(run)
            .intervals
            .insert(id, DbRowId::new(context.required(row.id, intervals::id)?));
        let segment = self.remember_segment(
            run,
            row.clock_segment_id,
            context,
            intervals::clock_segment_id,
        )?;
        let start = context.point(
            row.start_utc_ms,
            row.start_mono_ms,
            TimeColumns::new(intervals::start_utc_ms, intervals::start_mono_ms),
        )?;
        let confirmed = context.confirmed(
            start,
            row.confirmed_utc_ms,
            row.confirmed_mono_ms,
            TimeColumns::new(intervals::confirmed_utc_ms, intervals::confirmed_mono_ms),
        )?;
        let session = domain::SessionKey::from_persisted(
            context.unsigned(row.session_key, intervals::session_key)?,
        );
        let fact = self.recover_interval_fact(run, row)?;
        Ok(ConfirmedInterval {
            row: domain::OpenInterval {
                id,
                segment,
                session,
                start,
                fact,
            },
            confirmed,
        })
    }
    fn remember_segment(
        &mut self,
        run: RunId,
        id: i64,
        context: RowContext,
        column: impl crate::store::identifiers::SchemaColumn,
    ) -> Result<SegmentId, DecodeCaptureRowError> {
        let segment = SegmentId::from_persisted(context.unsigned(id, column)?);
        self.run_keys(run)
            .segments
            .insert(segment, DbRowId::new(id));
        Ok(segment)
    }
    fn recover_interval_fact(
        &mut self,
        run: RunId,
        row: IntervalsRow,
    ) -> Result<domain::IntervalFact, LoadRecoverableRunError> {
        use crate::store::enums::IntervalKind;
        let context = RowContext::new(row.id);
        match row.kind {
            IntervalKind::NoForeground => Ok(domain::IntervalFact::NoForeground(
                context
                    .required(row.no_foreground_reason, intervals::no_foreground_reason)?
                    .into(),
            )),
            IntervalKind::Window => {
                let instance = row
                    .window_instance_id
                    .map(|id| self.remember_window(run, id, context))
                    .transpose()?;
                let title = self.load_title(row.title_id)?;
                Ok(domain::IntervalFact::Window {
                    instance,
                    fields: row.captured_fields(title, context)?,
                })
            }
        }
    }
    fn remember_window(
        &mut self,
        run: RunId,
        id: i64,
        context: RowContext,
    ) -> Result<WindowId, DecodeCaptureRowError> {
        let key = WindowId::from_persisted(context.unsigned(id, intervals::window_instance_id)?);
        self.run_keys(run).windows.insert(key, DbRowId::new(id));
        Ok(key)
    }
    fn load_title(&mut self, id: Option<i64>) -> Result<Option<String>, LoadRecoverableRunError> {
        id.map(|id| {
            titles::table
                .filter(titles::id.eq(id))
                .select(titles::text)
                .first(self.connection)
        })
        .transpose()
        .context(load_recoverable_run_error::DatabaseSnafu)
    }
    fn recover_availability(
        &mut self,
        run: RunId,
        id: DbRowId,
        ids: &mut RecoveryRowIds,
    ) -> Result<Vec<ConfirmedAvailability>, LoadRecoverableRunError> {
        let rows = availability_intervals::table
            .filter(availability_intervals::run_id.eq(id.get()))
            .filter(availability_intervals::end_utc_ms.is_null())
            .select(AvailabilityIntervalsRow::as_select())
            .load(self.connection)?;
        rows.into_iter()
            .map(|row| self.recover_mask(run, row, ids))
            .collect()
    }
    fn recover_mask(
        &mut self,
        run: RunId,
        row: AvailabilityIntervalsRow,
        ids: &mut RecoveryRowIds,
    ) -> Result<ConfirmedAvailability, LoadRecoverableRunError> {
        let context = RowContext::new(row.id);
        let id = ids.allocate()?;
        self.run_keys(run).availability.insert(
            id,
            DbRowId::new(context.required(row.id, availability_intervals::id)?),
        );
        let segment = self.remember_segment(
            run,
            row.clock_segment_id,
            context,
            availability_intervals::clock_segment_id,
        )?;
        let start = context.point(
            row.start_utc_ms,
            row.start_mono_ms,
            TimeColumns::new(
                availability_intervals::start_utc_ms,
                availability_intervals::start_mono_ms,
            ),
        )?;
        let confirmed = context.confirmed(
            start,
            row.confirmed_utc_ms,
            row.confirmed_mono_ms,
            TimeColumns::new(
                availability_intervals::confirmed_utc_ms,
                availability_intervals::confirmed_mono_ms,
            ),
        )?;
        let idle = row.idle_record(context, start)?;
        Ok(ConfirmedAvailability {
            row: domain::OpenAvailability {
                id,
                segment,
                reason: row.reason.into(),
                start,
                idle,
            },
            confirmed,
        })
    }
}
impl IntervalsRow {
    fn captured_fields(
        self,
        title: Option<String>,
        context: RowContext,
    ) -> Result<domain::CapturedFields, DecodeCaptureRowError> {
        let title_status =
            self.title_status
                .try_into()
                .map_err(|source| DecodeCaptureRowError::Enum {
                    field: context.field(intervals::title_status),
                    value: self.title_status.code(),
                    source,
                })?;
        let privacy = self
            .privacy_rule_id
            .map(|rule_id| domain::PrivacyExplanation {
                rule_id,
                label: self
                    .privacy_label_key
                    .map(domain::PrivacyLabel::Key)
                    .or_else(|| self.privacy_label.map(domain::PrivacyLabel::Text)),
            });
        Ok(domain::CapturedFields {
            level: self.capture_level.into(),
            title,
            title_status,
            privacy,
            // Recovery replays foreground rows; M5 supplies fresh web context.
            web: quartz_domain::rules::WebAction::None,
        })
    }
}
impl AvailabilityIntervalsRow {
    fn idle_record(
        &self,
        context: RowContext,
        start: Timestamp,
    ) -> Result<Option<domain::IdleRecord>, DecodeCaptureRowError> {
        if self.reason != crate::store::enums::AvailabilityReason::Idle {
            return Ok(None);
        }
        let last = context.required(
            self.last_input_utc_ms,
            availability_intervals::last_input_utc_ms,
        )?;
        let detected = context.required(
            self.detected_utc_ms,
            availability_intervals::detected_utc_ms,
        )?;
        let threshold =
            context.required(self.threshold_ms, availability_intervals::threshold_ms)?;
        Ok(Some(domain::IdleRecord {
            last_input: context.prior_point(
                start,
                last,
                availability_intervals::last_input_utc_ms,
            )?,
            start,
            detected: context.later_point(
                start,
                detected,
                availability_intervals::detected_utc_ms,
            )?,
            policy: domain::IdlePolicy {
                threshold: Duration::from_millis(
                    context.unsigned(threshold, availability_intervals::threshold_ms)?,
                ),
                version: context
                    .policy_version(self.policy_version, availability_intervals::policy_version)?,
            },
        }))
    }
}

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum RecoverCaptureError {
    #[snafu(display("could not load previous capture runs"))]
    Load { source: LoadRecoverableRunError },
    #[snafu(display("could not settle previous capture runs"))]
    Execute { source: ExecuteCaptureBatchError },
    #[snafu(display("could not load capture startup values"))]
    Startup { source: LoadCaptureStartupError },
    #[snafu(display("could not begin, commit or roll back capture recovery"))]
    Transaction { source: diesel::result::Error },
}
#[derive(Debug)]
enum RecoveryTransactionError {
    Operation { source: RecoverCaptureError },
    Transaction { source: diesel::result::Error },
}
impl From<diesel::result::Error> for RecoveryTransactionError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Transaction { source }
    }
}
impl Capture<'_> {
    fn recover(&mut self) -> Result<CaptureStartup, RecoverCaptureError> {
        let runs = self
            .load_recoverable()
            .context(recover_capture_error::LoadSnafu)?;
        for run in runs {
            let run_id = run.run;
            self.execute_writes(run.run, &run.settle())
                .context(recover_capture_error::ExecuteSnafu)?;
            self.keys.record(run_id);
            self.keys.runs.remove(&run_id);
        }
        self.load_startup()
            .context(recover_capture_error::StartupSnafu)
    }
}
impl crate::store::Store {
    pub(crate) fn recover_capture(
        &mut self,
        keys: &mut CaptureKeys,
        metadata: &RunMetadata,
    ) -> Result<CaptureStartup, RecoverCaptureError> {
        self.capture_transaction(keys, metadata, |capture| {
            capture
                .recover()
                .map_err(|source| RecoveryTransactionError::Operation { source })
        })
        .map_err(|error| match error {
            RecoveryTransactionError::Operation { source } => source,
            RecoveryTransactionError::Transaction { source } => {
                RecoverCaptureError::Transaction { source }
            }
        })
    }
}
