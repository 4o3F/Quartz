//! Crash settlement plans from persisted confirmation values, without database access.

use super::{
    AvailabilityEndReason, CaptureWrite, ClockEndReason, IntervalEndReason, OpenAvailability,
    OpenInterval, RowId, RunEndReason, RunId, SegmentId, TimeRange, Timestamp,
};

/// Persisted foreground row and its last durable confirmation.
#[derive(Debug, Clone)]
pub struct ConfirmedInterval {
    /// Persisted open row, using the shell's recovered logical-to-database mapping.
    pub row: OpenInterval,
    /// Last confirmed UTC and monotonic position for this row.
    pub confirmed: Timestamp,
}
/// Persisted availability row and its last durable confirmation.
#[derive(Debug, Clone)]
pub struct ConfirmedAvailability {
    /// Persisted mask including original idle evidence, when present.
    pub row: OpenAvailability,
    /// Last confirmed UTC and monotonic position for this row.
    pub confirmed: Timestamp,
}
/// An old open clock segment at its own durable confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmedSegment {
    /// Persisted segment logical key.
    pub segment: SegmentId,
    /// Segment's last confirmed clock pair.
    pub confirmed: Timestamp,
}
/// Complete bounded input for settling one old run before starting a fresh run.
#[derive(Debug, Clone)]
pub struct RecoverableRun {
    /// Old run identity supplied by the store shell.
    pub run: RunId,
    /// Run's own last confirmed position; only its UTC is persisted on the run table.
    pub confirmed: Timestamp,
    /// Logical key greater than all row keys already assigned in this old run.
    pub next_row: RowId,
    /// All persisted foreground rows left open.
    pub intervals: Vec<ConfirmedInterval>,
    /// All persisted availability masks left open.
    pub availability: Vec<ConfirmedAvailability>,
    /// Old open clock segments, closed after their rows.
    pub segments: Vec<ConfirmedSegment>,
}
impl RecoverableRun {
    /// Settles each row at its own confirmed endpoint in one transaction, retaining midnight limits.
    ///
    /// Inputs are validated persisted values: confirmations cannot precede starts, and
    /// next_row must exceed all existing logical keys. The shell clears confirmation
    /// columns when executing each close and supplies no titles from memory queues.
    #[must_use]
    pub fn settle(self) -> Vec<CaptureWrite> {
        let mut writes = Vec::new();
        let mut ids = super::ids::IdAllocator::resume_row(self.next_row);
        for confirmed in self.intervals {
            confirmed.settle(&mut ids, &mut writes);
        }
        for confirmed in self.availability {
            confirmed.settle(&mut ids, &mut writes);
        }
        for segment in self.segments {
            writes.push(CaptureWrite::CloseClockSegment {
                segment: segment.segment,
                at: segment.confirmed,
                reason: ClockEndReason::CrashRecovered,
            });
        }
        writes.push(CaptureWrite::CloseRun {
            run: self.run,
            at: self.confirmed,
            reason: RunEndReason::CrashRecovered,
        });
        writes
    }
}

impl ConfirmedInterval {
    fn settle(self, ids: &mut super::ids::IdAllocator, writes: &mut Vec<CaptureWrite>) {
        let mut row = self.row;
        let pieces = TimeRange {
            start: row.start,
            end: self.confirmed,
        }
        .split_midnights()
        .expect("ordered segment endpoints");
        for (index, piece) in pieces.iter().enumerate() {
            if index > 0 {
                row.id = ids.row();
                row.start = piece.start;
                writes.push(CaptureWrite::OpenInterval(row.clone()));
            }
            writes.push(CaptureWrite::CloseInterval {
                id: row.id,
                at: piece.end,
                reason: if index + 1 == pieces.len() {
                    IntervalEndReason::CrashRecovered
                } else {
                    IntervalEndReason::MidnightSplit
                },
            });
        }
    }
}

impl ConfirmedAvailability {
    fn settle(self, ids: &mut super::ids::IdAllocator, writes: &mut Vec<CaptureWrite>) {
        let mut row = self.row;
        let pieces = TimeRange {
            start: row.start,
            end: self.confirmed,
        }
        .split_midnights()
        .expect("ordered segment endpoints");
        for (index, piece) in pieces.iter().enumerate() {
            if index > 0 {
                row.id = ids.row();
                row.start = piece.start;
                writes.push(CaptureWrite::OpenAvailability(row.clone()));
            }
            writes.push(CaptureWrite::CloseAvailability {
                id: row.id,
                at: piece.end,
                reason: if index + 1 == pieces.len() {
                    AvailabilityEndReason::CrashRecovered
                } else {
                    AvailabilityEndReason::MidnightSplit
                },
            });
        }
    }
}
