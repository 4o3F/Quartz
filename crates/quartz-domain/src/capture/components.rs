//! Cohesive private capture state with named records and local operations.

use super::{
    AvailabilityEndReason, AvailabilityReason, BatchId, CaptureStep, CaptureWrite, Foreground,
    IdleDecision, IdleRecord, IdleStatus, IntervalEndReason, MonoInstant, ObservationId,
    ObservationSource, OpenAvailability, ReadLastInputError, RowId, RunId, SegmentId, TimeRange,
    Timestamp, ids::IdAllocator,
};
use crate::rules::registry::RuleSnapshot;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Debug)]
pub(super) struct ClockSegment {
    pub run: RunId,
    pub started: Timestamp,
    pub last_at: Timestamp,
    pub anchor: Timestamp,
    pub segment: SegmentId,
    pub maintenance_deadline: MonoInstant,
}
impl ClockSegment {
    pub(super) fn new(run: RunId, at: Timestamp, ids: &mut IdAllocator) -> Self {
        Self {
            run,
            started: at,
            last_at: at,
            anchor: at,
            segment: ids.segment(),
            maintenance_deadline: at.mono.advance(super::limits::CHECKPOINT_PERIOD),
        }
    }
    pub(super) fn reopen(&mut self, at: Timestamp, ids: &mut IdAllocator) {
        self.segment = ids.segment();
        self.anchor = at;
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
            idle: IdleSuppression::Eligible { not_before: at },
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
        at: Timestamp,
        anchor: Timestamp,
    ) -> AvailabilityAction {
        if let IdleStatus::Failed(error) = decision.status {
            return AvailabilityAction::Preserve(error);
        }
        if let IdleStatus::Suppressed(exception) = decision.status {
            self.idle = IdleSuppression::Suppressed {
                until: at.advance(
                    decision
                        .next_check_mono
                        .duration_since(at.mono)
                        .expect("idle deadline follows its sample"),
                ),
            };
            return AvailabilityAction::Close(AvailabilityEndReason::Exception(exception));
        }
        if let IdleSuppression::Suppressed { until } = self.idle {
            self.idle = IdleSuppression::Eligible {
                not_before: if until.mono <= at.mono { until } else { at },
            };
        }
        match decision.status {
            IdleStatus::Idle(mut record) => {
                if let IdleSuppression::Eligible { not_before } = self.idle
                    && record.start.mono < not_before.mono
                {
                    record.start = not_before;
                }
                if record.start.mono < anchor.mono || record.start.utc < anchor.utc {
                    record.start = anchor;
                }
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
#[derive(Debug)]
pub(super) struct DeferredObservation {
    pub observation: ObservationId,
    pub at: Timestamp,
    pub rules: Arc<RuleSnapshot>,
    pub source: ObservationSource,
    pub foreground: Foreground,
}
#[derive(Debug, Default)]
pub(super) struct DeferredBrowser {
    observations: VecDeque<DeferredObservation>,
}
#[derive(Debug)]
pub(super) enum BrowserResolution {
    Discard,
    Context {
        observation: ObservationId,
        browser: super::BrowserContext,
    },
}
impl DeferredBrowser {
    pub(super) fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }
    pub(super) fn is_full(&self) -> bool {
        self.observations.len() == super::limits::MAIN_QUEUE_CAPACITY
    }
    pub(super) fn first(&self) -> Option<&DeferredObservation> {
        self.observations.front()
    }
    pub(super) fn contains(&self, observation: ObservationId) -> bool {
        self.observations
            .iter()
            .any(|pending| pending.observation == observation)
    }
    pub(super) fn push(&mut self, observation: DeferredObservation) {
        self.observations.push_back(observation);
    }
    pub(super) fn pop(&mut self) -> Option<DeferredObservation> {
        self.observations.pop_front()
    }
    pub(super) fn expired(&self, at: Timestamp) -> bool {
        self.first().is_some_and(|pending| {
            pending.at.mono.advance(super::limits::PRIVACY_CONTEXT_WAIT) <= at.mono
        })
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
    ) {
        if let Some(mut row) = self.rows.remove(&reason) {
            let end = Timestamp {
                utc: at.utc.max(row.start.utc),
                mono: at.mono.max(row.start.mono),
            };
            let pieces = TimeRange {
                start: row.start,
                end,
            }
            .split_midnights()
            .expect("ordered segment endpoints");
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
    }
    pub(super) fn close_all(
        &mut self,
        at: Timestamp,
        reason: AvailabilityEndReason,
        ids: &mut IdAllocator,
        step: &mut CaptureStep,
    ) {
        let reasons: Vec<_> = self.rows.keys().copied().collect();
        for availability in reasons {
            self.close(availability, at, reason, ids, step);
        }
    }
}
#[derive(Debug, Clone, Copy)]
enum IdleSuppression {
    Eligible { not_before: Timestamp },
    Suppressed { until: Timestamp },
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
