//! Cohesive private capture state with named records and local operations.

use super::{
    AvailabilityEndReason, AvailabilityReason, BatchId, CaptureStep, CaptureWrite, IdleDecision,
    IdleRecord, IdleStatus, IntervalEndReason, MonoInstant, OpenAvailability, ReadLastInputError,
    RowId, RunId, SegmentId, TimeRange, Timestamp, ids::IdAllocator,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub(super) struct ClockSegment {
    pub run: RunId,
    /// Last accepted fact boundary in the segment mapping.
    pub last_at: Timestamp,
    /// Raw reading used to evaluate UTC policy deadlines.
    pub observed: Timestamp,
    /// Startup boundary following any previously recorded stop notifications.
    pub startup_boundary: Timestamp,
    pub anchor: Timestamp,
    pub segment: SegmentId,
    pub maintenance_deadline: MonoInstant,
}
impl ClockSegment {
    pub(super) fn new(run: RunId, at: Timestamp, ids: &mut IdAllocator) -> Self {
        Self {
            run,
            last_at: at,
            observed: at,
            startup_boundary: at,
            anchor: at,
            segment: ids.segment(),
            maintenance_deadline: at.mono.advance(super::limits::CHECKPOINT_PERIOD),
        }
    }
    /// Uses one UTC mapping for all fact boundaries within the active segment.
    pub(super) fn ordered_boundary(
        &self,
        at: Timestamp,
    ) -> Result<Timestamp, super::AdvanceTimeError> {
        self.anchor.checked_project(at.mono)
    }
    pub(super) fn reopen(&mut self, at: Timestamp, ids: &mut IdAllocator) {
        self.segment = ids.segment();
        self.anchor = at;
        self.startup_boundary = at;
    }
}
#[derive(Debug)]
pub(super) struct BatchLedger {
    pub emitted: BatchId,
    pub committed: BatchId,
    finalized: BatchId,
    settlements: BTreeSet<BatchId>,
}
#[derive(Debug, Clone, Copy)]
pub(super) enum BatchSettlement {
    Committed,
    Discarded,
}
impl BatchLedger {
    pub(super) fn new() -> Self {
        Self {
            emitted: IdAllocator::origin_batch(),
            committed: IdAllocator::origin_batch(),
            finalized: IdAllocator::origin_batch(),
            settlements: BTreeSet::new(),
        }
    }
    pub(super) fn accepts(&self, batch: BatchId) -> bool {
        batch > self.finalized && batch <= self.emitted && !self.settlements.contains(&batch)
    }
    pub(super) fn settle(&mut self, batch: BatchId, outcome: BatchSettlement) {
        self.settlements.insert(batch);
        if matches!(outcome, BatchSettlement::Committed) {
            self.committed = self.committed.max(batch);
        }
        while self
            .settlements
            .remove(&IdAllocator::next_batch(self.finalized))
        {
            self.finalized = IdAllocator::next_batch(self.finalized);
        }
    }
    pub(super) fn emit(&mut self, ids: &mut IdAllocator) -> BatchId {
        self.emitted = ids.batch();
        self.emitted
    }
}
#[derive(Debug)]
pub(super) struct AvailabilitySet {
    rows: BTreeMap<AvailabilityReason, OpenAvailability>,
    idle: IdleSuppression,
}
impl AvailabilitySet {
    pub(super) fn new(at: Timestamp) -> Self {
        Self {
            rows: BTreeMap::new(),
            idle: IdleSuppression::Eligible {
                not_before: at.mono,
            },
        }
    }
    pub(super) fn open_ids(&self) -> Vec<RowId> {
        self.rows.values().map(|row| row.id).collect()
    }
    pub(super) fn evidence(&self) -> Vec<AvailabilityEvidence> {
        self.rows
            .values()
            .map(|row| AvailabilityEvidence {
                reason: row.reason,
                idle: row.idle,
            })
            .collect()
    }
    pub(super) fn apply_idle(
        &mut self,
        decision: IdleDecision,
        at: MonoInstant,
        anchor: MonoInstant,
    ) -> AvailabilityAction {
        if let IdleStatus::Failed(error) = decision.status {
            return AvailabilityAction::Preserve(error);
        }
        if let IdleStatus::Suppressed(exception) = decision.status {
            self.idle = IdleSuppression::Suppressed {
                until: decision.next_check_mono,
            };
            return AvailabilityAction::Close(AvailabilityEndReason::Exception(exception));
        }
        if let IdleSuppression::Suppressed { until } = self.idle {
            self.idle = IdleSuppression::Eligible {
                not_before: until.min(at),
            };
        }
        match decision.status {
            IdleStatus::Idle(mut record) => {
                if let IdleSuppression::Eligible { not_before } = self.idle {
                    record.start.mono = record.start.mono.max(not_before);
                }
                record.start.mono = record.start.mono.max(anchor);
                AvailabilityAction::Open(record)
            }
            IdleStatus::Active => AvailabilityAction::Close(AvailabilityEndReason::Resumed),
            IdleStatus::Failed(_) | IdleStatus::Suppressed(_) => {
                unreachable!("failure and suppression handled above")
            }
        }
    }
    pub(super) fn reopen_evidence(&mut self) -> Vec<AvailabilityEvidence> {
        let active = self
            .rows
            .values()
            .map(|row| AvailabilityEvidence {
                reason: row.reason,
                idle: row.idle,
            })
            .collect();
        self.rows.clear();
        active
    }
}
#[derive(Debug, Clone, Copy)]
pub(super) struct AvailabilityEvidence {
    pub reason: AvailabilityReason,
    pub idle: Option<IdleRecord>,
}
#[derive(Debug, Clone, Copy)]
pub(super) struct WaitingRange {
    pub segment: SegmentId,
    pub range: TimeRange,
    pub reason: IntervalEndReason,
}
#[derive(Debug, Default)]
pub(super) struct ReviewHistory {
    pub ranges: Vec<WaitingRange>,
}
impl ReviewHistory {
    pub(super) fn take(&mut self) -> Vec<WaitingRange> {
        std::mem::take(&mut self.ranges)
    }
}
#[derive(Debug, Clone, Copy)]
pub(super) enum SessionBoundary {
    New,
    Continue(super::SessionKey),
}
#[derive(Debug)]
pub(super) struct StepContext {
    pub previous_segment: SegmentId,
    pub previous_at: Timestamp,
    pub previous_waiting: Vec<super::SegmentRange>,
    pub old_state: super::CaptureState,
}

impl AvailabilitySet {
    pub(super) fn open(
        &mut self,
        reason: AvailabilityReason,
        position: SegmentPoint,
        idle: Option<IdleRecord>,
        ids: &mut IdAllocator,
        step: &mut CaptureStep,
    ) {
        if self.rows.contains_key(&reason) {
            return;
        }
        let row = OpenAvailability {
            id: ids.row(),
            segment: position.segment,
            reason,
            start: position.at,
            idle,
        };
        step.writes
            .push(CaptureWrite::OpenAvailability(row.clone()));
        self.rows.insert(reason, row);
    }
    pub(super) fn close(
        &mut self,
        reason: AvailabilityReason,
        at: Timestamp,
        end_reason: AvailabilityEndReason,
        ids: &mut IdAllocator,
        step: &mut CaptureStep,
    ) -> Result<(), super::AdvanceTimeError> {
        if let Some(mut row) = self.rows.remove(&reason) {
            let end = Timestamp {
                utc: at.utc.max(row.start.utc),
                mono: at.mono.max(row.start.mono),
            };
            let pieces = TimeRange {
                start: row.start,
                end,
            }
            .split_midnights()?;
            for (index, piece) in pieces.iter().enumerate() {
                if index > 0 {
                    row.id = ids.row();
                    row.start = piece.start;
                    step.writes
                        .push(CaptureWrite::OpenAvailability(row.clone()));
                }
                step.writes.push(CaptureWrite::CloseAvailability {
                    id: row.id,
                    at: piece.end,
                    reason: if index + 1 == pieces.len() {
                        end_reason
                    } else {
                        AvailabilityEndReason::MidnightSplit
                    },
                });
            }
        }
        Ok(())
    }
    pub(super) fn close_all(
        &mut self,
        at: Timestamp,
        reason: AvailabilityEndReason,
        ids: &mut IdAllocator,
        step: &mut CaptureStep,
    ) -> Result<(), super::AdvanceTimeError> {
        let reasons: Vec<_> = self.rows.keys().copied().collect();
        for availability in reasons {
            self.close(availability, at, reason, ids, step)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy)]
enum IdleSuppression {
    Eligible { not_before: MonoInstant },
    Suppressed { until: MonoInstant },
}
#[derive(Debug)]
pub(super) enum AvailabilityAction {
    Open(IdleRecord),
    Close(AvailabilityEndReason),
    Preserve(ReadLastInputError),
}
#[derive(Debug, Clone, Copy)]
pub(super) struct SegmentPoint {
    pub segment: SegmentId,
    pub at: Timestamp,
}
impl ClockSegment {
    pub(super) fn position(&self, at: Timestamp) -> SegmentPoint {
        SegmentPoint {
            segment: self.segment,
            at,
        }
    }
}
