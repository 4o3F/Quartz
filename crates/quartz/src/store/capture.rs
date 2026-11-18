//! Capture write conversion inside a caller-owned immediate transaction.

mod availability;
mod checkpoints;
mod errors;
mod gaps;
mod intervals;
mod recovery;
mod runs;
mod windows;
pub(crate) use errors::{CaptureColumn, DecodeCaptureRowError, ExecuteCaptureBatchError};
pub(crate) use recovery::{CaptureStartup, RecoverCaptureError};
#[cfg(test)]
mod tests;

use diesel::SqliteConnection;
use errors::{RowContext, TimeColumns};
use quartz_domain::capture::{CaptureWrite, RowId, RunId, SegmentId, WindowId, WriteBatch};
use std::collections::BTreeMap;

/// Windows session identity supplied by the platform shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowsSessionId(u32);
impl WindowsSessionId {
    pub(crate) fn new(value: u32) -> Self {
        Self(value)
    }
}
/// A SQLite primary key, kept distinct from run-local capture identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DbRowId(i64);
impl DbRowId {
    fn new(value: i64) -> Self {
        Self(value)
    }
    fn get(self) -> i64 {
        self.0
    }
    fn context(self, table: &'static str) -> RowContext {
        RowContext::new(table, Some(self.0))
    }
    fn require_open(
        self,
        table: &'static str,
        affected_rows: usize,
    ) -> Result<(), ExecuteCaptureBatchError> {
        if affected_rows == 1 {
            Ok(())
        } else {
            Err(ExecuteCaptureBatchError::MissingOpenRow {
                table,
                row_id: self.get(),
                affected_rows,
            })
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogicalKey {
    Segment(SegmentId),
    Window(WindowId),
    Interval(RowId),
    Availability(RowId),
}
/// Installation and process metadata supplied when opening a capture run.
#[derive(Debug, Clone)]
pub(crate) struct RunMetadata {
    pub device_id: String,
    pub win_session_id: WindowsSessionId,
    pub app_version: String,
}
/// Caller-owned mappings; publish the transaction's copy only after commit succeeds.
#[derive(Debug, Clone, Default)]
pub(crate) struct CaptureKeys {
    runs: BTreeMap<RunId, RunKeys>,
}
#[derive(Debug, Clone, Default)]
struct RunKeys {
    segments: BTreeMap<SegmentId, DbRowId>,
    windows: BTreeMap<WindowId, DbRowId>,
    intervals: BTreeMap<RowId, DbRowId>,
    availability: BTreeMap<RowId, DbRowId>,
}

/// Borrowed transaction view. No mapping changes survive a failed transaction.
pub(crate) struct Capture<'connection> {
    connection: &'connection mut SqliteConnection,
    keys: &'connection mut CaptureKeys,
    metadata: &'connection RunMetadata,
}
impl std::fmt::Debug for Capture<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture").finish_non_exhaustive()
    }
}
impl<'connection> Capture<'connection> {
    pub(super) fn new(
        connection: &'connection mut SqliteConnection,
        keys: &'connection mut CaptureKeys,
        metadata: &'connection RunMetadata,
    ) -> Self {
        Self {
            connection,
            keys,
            metadata,
        }
    }
    pub(crate) fn execute(&mut self, batch: &WriteBatch) -> Result<(), ExecuteCaptureBatchError> {
        self.execute_writes(batch.run, &batch.writes)
    }
    pub(crate) fn execute_writes(
        &mut self,
        run: RunId,
        writes: &[CaptureWrite],
    ) -> Result<(), ExecuteCaptureBatchError> {
        for write in writes {
            self.execute_write(run, write)?;
        }
        Ok(())
    }
    fn execute_write(
        &mut self,
        run: RunId,
        write: &CaptureWrite,
    ) -> Result<(), ExecuteCaptureBatchError> {
        use CaptureWrite::*;
        match write {
            OpenRun { run, at } => self.open_run(*run, *at),
            CloseRun { run, at, reason } => self.close_run(*run, *at, *reason),
            OpenClockSegment { segment, at } => self.open_clock_segment(run, *segment, *at),
            CloseClockSegment {
                segment,
                at,
                reason,
            } => self.close_clock_segment(run, *segment, *at, *reason),
            ObserveWindow { instance, at } => self.observe_window(run, instance, *at),
            RetireWindow { instance, at } => self.retire_window(run, *instance, *at),
            OpenInterval(row) => self.open_interval(run, row),
            CloseInterval { id, at, reason } => self.close_interval(run, *id, *at, *reason),
            OpenAvailability(row) => self.open_availability(run, row),
            CloseAvailability { id, at, reason } => self.close_availability(run, *id, *at, *reason),
            Checkpoint(point) => self.checkpoint(point),
            SettlePersistedRows { run, segment, at } => self.settle_persisted(*run, *segment, *at),
            SetDeadline { owner, at } => self.set_deadline(*owner, *at),
            ClearDeadline { owner } => self.clear_deadline(*owner),
            RecordGap(gap) => self.record_gap(run, gap),
        }
    }
    fn run_keys(&mut self, run: RunId) -> &mut RunKeys {
        self.keys.runs.entry(run).or_default()
    }
    fn segment(&mut self, run: RunId, key: SegmentId) -> Result<DbRowId, ExecuteCaptureBatchError> {
        self.run_keys(run)
            .segments
            .get(&key)
            .copied()
            .ok_or(ExecuteCaptureBatchError::MissingKey {
                run,
                key: LogicalKey::Segment(key),
            })
    }
    fn window(&mut self, run: RunId, key: WindowId) -> Result<DbRowId, ExecuteCaptureBatchError> {
        self.run_keys(run)
            .windows
            .get(&key)
            .copied()
            .ok_or(ExecuteCaptureBatchError::MissingKey {
                run,
                key: LogicalKey::Window(key),
            })
    }
    fn interval(&mut self, run: RunId, key: RowId) -> Result<DbRowId, ExecuteCaptureBatchError> {
        self.run_keys(run).intervals.get(&key).copied().ok_or(
            ExecuteCaptureBatchError::MissingKey {
                run,
                key: LogicalKey::Interval(key),
            },
        )
    }
    fn availability(
        &mut self,
        run: RunId,
        key: RowId,
    ) -> Result<DbRowId, ExecuteCaptureBatchError> {
        self.run_keys(run).availability.get(&key).copied().ok_or(
            ExecuteCaptureBatchError::MissingKey {
                run,
                key: LogicalKey::Availability(key),
            },
        )
    }
    fn run_id(run: RunId) -> Result<DbRowId, ExecuteCaptureBatchError> {
        CaptureColumn::new("capture_runs", "id")
            .integer(run.get())
            .map(DbRowId::new)
    }
    #[cfg(test)]
    fn timestamp(
        utc: i64,
        mono: i64,
    ) -> Result<quartz_domain::capture::Timestamp, DecodeCaptureRowError> {
        RowContext::new("test capture row", None).point(utc, mono, TimeColumns::Start)
    }
}
