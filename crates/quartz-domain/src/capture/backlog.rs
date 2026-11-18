//! Bounded, transaction-preserving write buffering and identity-free loss ranges.

use std::time::Duration;

use super::{
    BatchId, CaptureGap, CaptureWrite, GapReason, MonoInstant, RunId, SegmentRange, Timestamp,
    TitleActivity, limits, limits::WRITE_QUEUE_CAPACITY,
};
use std::collections::VecDeque;

/// A whole capture step; close/open pairs are never split by queue draining.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteBatch {
    /// Core acknowledgement key.
    pub id: BatchId,
    /// Owning capture run.
    pub run: RunId,
    /// Every segment-local range affected if this transaction cannot be retained.
    pub coverage: Vec<SegmentRange>,
    /// Capture writes kept in original order.
    pub writes: Vec<CaptureWrite>,
    /// Batching hint; every title boundary is still retained.
    pub title_storm: TitleActivity,
}
impl WriteBatch {
    fn replaceable(&self) -> bool {
        !self.writes.is_empty()
            && self
                .writes
                .iter()
                .all(|write| matches!(write, CaptureWrite::Checkpoint(_)))
    }
}
/// Shell-reported write failure kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BacklogFailure {
    /// The write-command queue could not accept more work.
    QueueFull,
    /// The database rejected or failed a transaction.
    Database,
}
impl BacklogFailure {
    fn gap_reason(self) -> GapReason {
        match self {
            Self::QueueFull => GapReason::WriteQueueOverload,
            Self::Database => GapReason::DatabaseError,
        }
    }
}
/// Buffer mutation result; discarded batches must be fed back to the capture core.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use]
pub struct BacklogOutcome {
    /// Identity-free ranges lost after coalescing and applying both limits.
    pub gaps: Vec<CaptureGap>,
    /// Batch acknowledgements superseded by a newer replaceable checkpoint.
    pub coalesced: Vec<BatchId>,
    /// Batch keys discarded as whole transaction units.
    pub discarded: Vec<BatchId>,
}
/// Failure buffer with 4,096-command and ten-minute limits, whichever is reached first.
#[derive(Debug)]
pub struct WriteBacklog {
    queue: VecDeque<QueuedBatch>,
    gaps: VecDeque<StoredGap>,
    structural: Vec<CaptureWrite>,
    failure: BacklogFailure,
    capacity: usize,
    retention: Duration,
    reservation: Reservation,
}
/// A database transaction retains its prefix while producers can replace the remaining suffix.
#[derive(Debug, Default)]
enum Reservation {
    #[default]
    None,
    Batches {
        through: BatchId,
    },
    Gap {
        stored: StoredGap,
        structural_commands: usize,
    },
}

impl Default for WriteBacklog {
    fn default() -> Self {
        Self::new()
    }
}
impl WriteBacklog {
    /// Creates the design-specified buffer without allocating its maximum capacity up front.
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            gaps: VecDeque::new(),
            structural: Vec::new(),
            failure: BacklogFailure::QueueFull,
            capacity: WRITE_QUEUE_CAPACITY,
            retention: limits::BACKLOG_RETENTION,
            reservation: Reservation::None,
        }
    }
    /// Pins the selected prefix until its database transaction commits or fails.
    pub fn reserve_batches(&mut self, through: BatchId) {
        assert!(matches!(self.reservation, Reservation::None));
        assert!(self.queue.iter().any(|queued| queued.batch.id == through));
        self.reservation = Reservation::Batches { through };
    }
    /// Pins the current gap and structural prefix while the database repairs it.
    pub fn reserve_gap(&mut self) {
        assert!(matches!(self.reservation, Reservation::None));
        let stored = self
            .first_gap()
            .expect("gap reservation requires a front gap");
        self.reservation = Reservation::Gap {
            stored,
            structural_commands: self.structural.len(),
        };
    }
    /// Releases a failed transaction without acknowledging its writes.
    pub fn release_reservation(&mut self) {
        self.reservation = Reservation::None;
    }
    fn reserved_batches(&self) -> usize {
        match self.reservation {
            Reservation::Batches { through } => {
                self.queue
                    .iter()
                    .position(|queued| queued.batch.id == through)
                    .expect("reserved prefix remains queued")
                    + 1
            }
            _ => 0,
        }
    }
    /// Records a failure without consuming the uncommitted front transaction.
    pub fn failed(&mut self, failure: BacklogFailure) {
        self.failure = failure;
    }
    /// Number of retained domain commands, rather than the number of transactions.
    pub fn command_count(&self) -> usize {
        self.queue
            .iter()
            .map(|queued| queued.batch.writes.len())
            .sum()
    }
    /// Returns the next retention or confirmation deadline, with no periodic polling while empty.
    pub fn next_deadline(&self) -> Option<MonoInstant> {
        let retention = self
            .queue
            .front()
            .map(|queued| queued.queued_at.mono.advance(self.retention));
        let confirmation = self
            .queue
            .iter()
            .flat_map(|queued| &queued.batch.writes)
            .filter_map(|write| match write {
                CaptureWrite::Checkpoint(point) => {
                    Some(point.commit_by_mono.advance(Duration::from_millis(1)))
                }
                _ => None,
            })
            .min();
        retention.into_iter().chain(confirmation).min()
    }
    /// Borrows retained batches in order, after all pending gaps have been persisted.
    pub fn batches(&self) -> impl Iterator<Item = &WriteBatch> {
        self.queue
            .iter()
            .take(if self.gaps.is_empty() {
                self.queue.len()
            } else {
                0
            })
            .map(|queued| &queued.batch)
    }
    /// Returns the enqueue clock of the oldest retained batch for the batching deadline.
    pub fn oldest_queued_at(&self) -> Option<Timestamp> {
        self.queue.front().map(|queued| queued.queued_at)
    }
    /// Returns the oldest whole transaction; remove it only after successful commit.
    pub fn front(&self) -> Option<&WriteBatch> {
        if self.gaps.is_empty() {
            self.queue.front().map(|queued| &queued.batch)
        } else {
            None
        }
    }
    /// Returns gaps before normal writes after recovery. The shell must acknowledge each persisted gap.
    pub fn first_gap(&self) -> Option<StoredGap> {
        self.gaps.front().copied()
    }
    /// Plans repair before the first loss range: settle persisted rows, then write its gap.
    /// Execute this list atomically before acknowledging the gap and draining normal batches.
    #[must_use]
    pub fn recovery_writes(&self) -> Vec<CaptureWrite> {
        let Some(StoredGap { run, gap }) = self.first_gap() else {
            return Vec::new();
        };
        let mut writes = self.structural.clone();
        writes.push(CaptureWrite::SettlePersistedRows {
            run,
            segment: gap.segment,
            at: gap.range.start,
        });
        writes.push(CaptureWrite::RecordGap(gap));
        writes
    }
    /// Acknowledges exactly the first persisted gap.
    #[must_use]
    pub fn committed_gap(&mut self, run: RunId, gap: CaptureGap) -> bool {
        if self.gaps.front() == Some(&StoredGap { run, gap }) {
            self.gaps.pop_front();
            match self.reservation {
                Reservation::Gap {
                    structural_commands,
                    ..
                } => {
                    self.structural.drain(..structural_commands);
                }
                _ => self.structural.clear(),
            }
            self.release_reservation();
            true
        } else {
            false
        }
    }
    /// Removes exactly the successfully committed front transaction.
    #[must_use]
    pub fn committed(&mut self, batch: BatchId) -> bool {
        if (self.gaps.is_empty() || matches!(self.reservation, Reservation::Batches { .. }))
            && self
                .queue
                .front()
                .is_some_and(|queued| queued.batch.id == batch)
        {
            self.queue.pop_front();
            if matches!(self.reservation, Reservation::Batches { through } if through == batch) {
                self.release_reservation();
            }
            true
        } else {
            false
        }
    }
    /// Enqueues one atomic transaction, coalescing replaceable checkpoints before checking limits.
    pub fn push(&mut self, mut batch: WriteBatch, now: Timestamp) -> BacklogOutcome {
        let mut outcome = BacklogOutcome::default();
        let mut queued_at = now;
        if batch.replaceable() && self.queue.len() > self.reserved_batches() {
            // A checkpoint only replaces a checkpoint for the same exact open-row set.
            // Intervening close/open transactions must retain their ordering.
            if let Some(QueuedBatch {
                batch: previous, ..
            }) = self.queue.back()
                && previous.replaceable()
                && previous.run == batch.run
                && previous.same_checkpoint_targets(&batch)
            {
                let QueuedBatch {
                    queued_at: previous_at,
                    batch: removed,
                } = self.queue.pop_back().expect("examined tail exists");
                queued_at = previous_at;
                batch.absorb_checkpoint(&removed);
                outcome.coalesced.push(removed.id);
            }
        }
        self.queue.push_back(QueuedBatch { queued_at, batch });
        self.enforce_limits(now, &mut outcome);
        outcome
    }
    /// Enforces the time limit even when no new writes arrive during database failure.
    pub fn advance(&mut self, now: Timestamp) -> BacklogOutcome {
        let mut outcome = BacklogOutcome::default();
        self.enforce_limits(now, &mut outcome);
        outcome
    }
    fn enforce_limits(&mut self, now: Timestamp, outcome: &mut BacklogOutcome) {
        let expired = self.queue.front().is_some_and(|queued| {
            now.mono
                .duration_since(queued.queued_at.mono)
                .expect("backlog input is monotonic")
                >= self.retention
        });
        let overdue_confirmation = self.queue.iter().any(|queued| queued.batch.writes.iter().any(|write| matches!(write, CaptureWrite::Checkpoint(point) if now.mono > point.commit_by_mono)));
        if self.command_count() <= self.capacity && !expired && !overdue_confirmation {
            return;
        }
        // All dependent transactions are discarded together: retaining a close whose
        // matching open was discarded would leave the database with dangling row keys.
        let mut discarded = self.queue.split_off(self.reserved_batches());
        while let Some(QueuedBatch { batch, .. }) = discarded.pop_front() {
            outcome.discarded.push(batch.id);
            self.structural
                .extend(batch.writes.into_iter().filter(|write| {
                    matches!(
                        write,
                        CaptureWrite::OpenRun { .. }
                            | CaptureWrite::CloseRun { .. }
                            | CaptureWrite::OpenClockSegment { .. }
                            | CaptureWrite::CloseClockSegment { .. }
                            | CaptureWrite::SetTimedState(_)
                            | CaptureWrite::SetDeadline { .. }
                            | CaptureWrite::ClearDeadline { .. }
                            | CaptureWrite::ClearTimedPolicy(_)
                    )
                }));
            for covered in batch.coverage {
                self.record_loss(
                    batch.run,
                    CaptureGap {
                        segment: covered.segment,
                        range: covered.range,
                        reason: self.failure.gap_reason(),
                    },
                    outcome,
                );
            }
        }
    }
    fn record_loss(&mut self, run: RunId, gap: CaptureGap, outcome: &mut BacklogOutcome) {
        if let Some(StoredGap {
            run: last_run,
            gap: last,
        }) = self.gaps.back_mut()
            && !matches!(self.reservation, Reservation::Gap { stored, .. } if stored.run == *last_run && stored.gap == *last)
            && *last_run == run
            && last.segment == gap.segment
            && last.reason == gap.reason
            && gap.range.start.mono <= last.range.end.mono
            && gap.range.start.utc <= last.range.end.utc
        {
            last.range.start.mono = last.range.start.mono.min(gap.range.start.mono);
            last.range.start.utc = last.range.start.utc.min(gap.range.start.utc);
            last.range.end.mono = last.range.end.mono.max(gap.range.end.mono);
            last.range.end.utc = last.range.end.utc.max(gap.range.end.utc);
        } else {
            self.gaps.push_back(StoredGap { run, gap });
        }
        outcome.gaps.push(gap);
    }
}
impl WriteBatch {
    fn absorb_checkpoint(&mut self, previous: &Self) {
        for covered in &previous.coverage {
            if let Some(current) = self
                .coverage
                .iter_mut()
                .find(|current| current.segment == covered.segment)
            {
                current.range.start.utc = current.range.start.utc.min(covered.range.start.utc);
                current.range.start.mono = current.range.start.mono.min(covered.range.start.mono);
            } else {
                self.coverage.insert(0, *covered);
            }
        }
        if let ([CaptureWrite::Checkpoint(current)], [CaptureWrite::Checkpoint(old)]) =
            (&mut *self.writes, &*previous.writes)
        {
            current.commit_by_mono = current.commit_by_mono.min(old.commit_by_mono);
        }
    }
    fn same_checkpoint_targets(&self, other: &Self) -> bool {
        match (&*self.writes, &*other.writes) {
            ([CaptureWrite::Checkpoint(left)], [CaptureWrite::Checkpoint(right)]) => {
                left.run == right.run
                    && left.segment == right.segment
                    && left.interval == right.interval
                    && left.availability == right.availability
            }
            _ => false,
        }
    }
}

#[derive(Debug)]
struct QueuedBatch {
    queued_at: Timestamp,
    batch: WriteBatch,
}
/// A run-qualified persisted loss range, returned before normal buffered writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredGap {
    /// Owning capture run.
    pub run: RunId,
    /// Segment-local identity-free range.
    pub gap: CaptureGap,
}
