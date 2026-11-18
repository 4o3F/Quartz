//! Capture write conversion inside a caller-owned immediate transaction.

mod availability;
mod checkpoints;
mod errors;
mod gaps;
mod intervals;
mod recovery;
mod runs;
mod timed;
mod windows;
pub(crate) use errors::{CaptureColumn, DecodeCaptureRowError, ExecuteCaptureBatchError};
#[cfg(any(windows, test))]
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
impl std::fmt::Display for LogicalKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Segment(id) => write!(formatter, "segment {}", id.get()),
            Self::Window(id) => write!(formatter, "window {}", id.get()),
            Self::Interval(id) => write!(formatter, "interval {}", id.get()),
            Self::Availability(id) => write!(formatter, "availability {}", id.get()),
        }
    }
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
        match write {
            CaptureWrite::OpenRun { run, at } => self.open_run(*run, *at),
            CaptureWrite::CloseRun { run, at, reason } => self.close_run(*run, *at, *reason),
            CaptureWrite::OpenClockSegment { segment, at } => {
                self.open_clock_segment(run, *segment, *at)
            }
            CaptureWrite::CloseClockSegment {
                segment,
                at,
                reason,
            } => self.close_clock_segment(run, *segment, *at, *reason),
            CaptureWrite::ObserveWindow { instance, at } => self.observe_window(run, instance, *at),
            CaptureWrite::RetireWindow { instance, at } => self.retire_window(run, *instance, *at),
            CaptureWrite::OpenInterval(row) => self.open_interval(run, row),
            CaptureWrite::CloseInterval { id, at, reason } => {
                self.close_interval(run, *id, *at, *reason)
            }
            CaptureWrite::OpenAvailability(row) => self.open_availability(run, row),
            CaptureWrite::CloseAvailability { id, at, reason } => {
                self.close_availability(run, *id, *at, *reason)
            }
            CaptureWrite::Checkpoint(point) => self.checkpoint(point),
            CaptureWrite::SettlePersistedRows { run, segment, at } => {
                self.settle_persisted(*run, *segment, *at)
            }
            CaptureWrite::SetDeadline { owner, at } => self.set_deadline(*owner, *at),
            CaptureWrite::SetTimedState(state) => self.set_timed_state(state),
            CaptureWrite::ClearTimedPolicy(kind) => self.clear_timed_policy(*kind),
            CaptureWrite::ClearDeadline { owner } => self.clear_deadline(*owner),
            CaptureWrite::RecordGap(gap) => self.record_gap(run, gap),
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
