//! Capture state transitions and transaction plans.

use std::time::Duration;

use super::foreground::{
    EvaluatedForeground, EvaluationContext, ForegroundReading, ForegroundTracker, RebindDecision,
    Retention, ReviewExpectation,
};
use super::{
    AvailabilityEndReason, AvailabilityReason, BatchId, BrowserContext, CaptureEffect,
    CaptureEvent, CaptureGap, CaptureInput, CaptureState, CaptureStep, CaptureWrite, Checkpoint,
    ClockChange, ClockEndReason, Deadline, DeadlineKind, DeadlineScheduler, Foreground,
    ForegroundGeneration, GapReason, IdleRecord, IdleSample, IntervalEndReason, IntervalFact,
    NoForegroundReason, ObservationId, ObservationSource, OpenInterval, PolicyObservation,
    PrivacyContext, RowId, RunEndReason, RunId, SchedulerKind, SegmentRange, SessionStatus,
    StopChange, StopReason, TimeRange, TimedPolicyKind, Timer, Timestamp, WebRowExtension,
    WindowId, WindowInstances, ids::IdAllocator, limits,
};
use crate::rules::registry::{RuleSnapshot, SnapshotEpoch};
use std::collections::BTreeSet;
use std::sync::Arc;

use super::{
    AdvanceTimeError,
    components::{
        AvailabilityAction, AvailabilityEvidence, AvailabilitySet, BatchLedger, BatchSettlement,
        BrowserResolution, ClockSegment, DeferredBrowser, DeferredObservation, ReviewHistory,
        SessionBoundary, StepContext, WaitingRange,
    },
    phase::{GapSpan, Phase, ResumeTarget},
};

/// Invalid capture work units are rejected before any state transition.
#[derive(Debug, snafu::Snafu)]
pub enum StepCaptureError {
    /// The input clock precedes the last accepted work unit.
    #[snafu(display("capture monotonic time moved backward"))]
    BackwardMonotonicTime,
    /// The batch was never emitted, was already settled, or precedes the finalized frontier.
    #[snafu(display("capture batch acknowledgement is outside the emitted sequence"))]
    InvalidAcknowledgement,
    /// A clock calculation cannot be represented by the domain time types.
    #[snafu(display("capture time is outside the supported range"))]
    Time {
        /// Original checked arithmetic error.
        source: AdvanceTimeError,
    },
}
/// Capture startup could not represent its initial deadlines.
#[derive(Debug, snafu::Snafu)]
#[snafu(display("could not start capture"))]
pub struct StartCaptureError {
    /// Original checked time failure.
    source: AdvanceTimeError,
}
/// Started run and the initial transaction and shell commands.
#[derive(Debug)]
#[must_use]
pub struct StartedCapture {
    /// Stateful pure core.
    pub core: CaptureCore,
    /// Initial run/segment writes and session query.
    pub step: CaptureStep,
}
/// Sans-IO capture state owned by one capture event loop.
#[derive(Debug)]
pub struct CaptureCore {
    clock: ClockSegment,
    ledger: BatchLedger,
    ids: IdAllocator,
    phase: Phase,
    tracker: ForegroundTracker,
    availability: AvailabilitySet,
    pending_availability: Vec<AvailabilityEvidence>,
    review_history: ReviewHistory,
    deferred: DeferredBrowser,
    epoch: Option<SnapshotEpoch>,
    windows: WindowInstances,
    allowed_windows: BTreeSet<WindowId>,
    deadlines: DeadlineScheduler,
    timed: super::timed::TimedStates,
    persisted_deadlines: BTreeSet<SchedulerKind>,
}
impl CaptureCore {
    /// Starts a run and its first segment, requesting session initialization before foreground capture.
    pub fn start(run: RunId, at: Timestamp) -> Result<StartedCapture, StartCaptureError> {
        at.mono
            .checked_add(limits::CHECKPOINT_PERIOD)
            .map_err(|source| StartCaptureError { source })?;
        let midnight = at
            .next_midnight()
            .map_err(|source| StartCaptureError { source })?;
        Deadline::Utc(midnight)
            .mono_at(at)
            .map_err(|source| StartCaptureError { source })?;
        let mut ids = IdAllocator::default();
        let clock = ClockSegment::new(run, at, &mut ids);
        let mut deadlines = DeadlineScheduler::default();
        deadlines.register(
            SchedulerKind::Maintenance,
            Deadline::Mono(clock.maintenance_deadline),
        );
        deadlines.register(SchedulerKind::Midnight, Deadline::Utc(midnight));
        let mut core = Self {
            clock,
            ledger: BatchLedger::new(),
            ids,
            phase: Phase::startup(),
            tracker: ForegroundTracker::new(),
            availability: AvailabilitySet::new(at),
            pending_availability: Vec::new(),
            review_history: ReviewHistory::default(),
            deferred: DeferredBrowser::default(),
            epoch: None,
            windows: WindowInstances::default(),
            allowed_windows: BTreeSet::new(),
            deadlines,
            timed: super::timed::TimedStates::default(),
            persisted_deadlines: BTreeSet::new(),
        };
        let mut step = CaptureStep {
            writes: vec![
                CaptureWrite::OpenRun { run, at },
                CaptureWrite::OpenClockSegment {
                    segment: core.clock.segment,
                    at,
                },
            ],
            effects: vec![CaptureEffect::QuerySession],
            ..CaptureStep::default()
        };
        core.finish(at, &mut step);
        Ok(StartedCapture { core, step })
    }

    /// Produces one atomic write plan; persistence acknowledgements never allocate row identifiers.
    pub fn step(&mut self, input: CaptureInput) -> Result<CaptureStep, StepCaptureError> {
        let mut step = CaptureStep::default();
        if self.phase.ended() {
            return Ok(step);
        }
        let CaptureInput { at, rules, event } = input;
        let stale = match &event {
            CaptureEvent::Foreground { generation, .. } => *generation < self.tracker.generation(),
            CaptureEvent::DestroyWindow { generation, .. } => {
                *generation != self.tracker.generation()
            }
            _ => false,
        };
        if stale {
            return Ok(step);
        }
        self.validate_input(at, &event)?;
        let context = self.prepare_step(at, &rules, &event, &mut step);
        self.dispatch(event, at, &rules, &mut step);
        self.confirm_boundary(at, &mut step);
        self.append_coverage(at, context, &mut step);
        Ok(step)
    }
    fn prepare_step(
        &mut self,
        at: Timestamp,
        rules: &RuleSnapshot,
        event: &CaptureEvent,
        step: &mut CaptureStep,
    ) -> StepContext {
        let previous_segment = self.clock.segment;
        let mut previous_at = self
            .deferred
            .first()
            .map_or(self.clock.last_at, |pending| pending.at);
        if let Some(waiting) = self.phase.waiting()
            && waiting.mono < previous_at.mono
        {
            previous_at = waiting;
        }
        let previous_waiting = self
            .review_history
            .ranges
            .iter()
            .map(|waiting| SegmentRange {
                segment: waiting.segment,
                range: waiting.range,
            })
            .collect::<Vec<_>>();
        let context = StepContext {
            previous_segment,
            previous_at,
            previous_waiting,
            old_state: self.state(),
        };
        // Deferred titles are drained before unrelated boundaries so confirmations never
        // extend an obsolete foreground through an unprocessed title transition.
        if !matches!(
            event,
            CaptureEvent::Foreground { .. }
                | CaptureEvent::PrivacyContext { .. }
                | CaptureEvent::Wake
        ) {
            self.flush_deferred(BrowserResolution::Discard, step);
        }
        if !matches!(
            event,
            CaptureEvent::PersistenceGap { .. } | CaptureEvent::BatchDiscarded(_)
        ) {
            self.restore_availability(at, step);
        }
        self.adopt_rules(at, rules, event, step);
        // Every input can arrive after midnight even if its timer notification was delayed.
        if self.deferred.is_empty()
            && !matches!(
                event,
                CaptureEvent::EventOverload | CaptureEvent::PersistenceGap { .. }
            )
        {
            self.split_foreground(at, step);
        }
        context
    }
    fn adopt_rules(
        &mut self,
        at: Timestamp,
        rules: &RuleSnapshot,
        event: &CaptureEvent,
        step: &mut CaptureStep,
    ) {
        let epoch_changed = self.epoch.is_some_and(|epoch| rules.epoch() > epoch);
        self.epoch = Some(match self.epoch {
            Some(epoch) => epoch.max(rules.epoch()),
            None => rules.epoch(),
        });
        if epoch_changed && !matches!(event, CaptureEvent::PolicyChanged(_)) {
            self.flush_deferred(BrowserResolution::Discard, step);
            self.policy_boundary(at, PolicyObservation::Refresh, rules, step);
        }
    }
    fn dispatch(
        &mut self,
        event: CaptureEvent,
        at: Timestamp,
        rules: &Arc<RuleSnapshot>,
        step: &mut CaptureStep,
    ) {
        match event {
            CaptureEvent::Session(status) => self.session(status, at, step),
            CaptureEvent::Foreground {
                generation,
                source,
                foreground,
            } => self.on_foreground(generation, source, foreground, at, rules, step),
            CaptureEvent::PrivacyContext {
                observation,
                browser,
            } => self.on_privacy_context(observation, browser, step),
            CaptureEvent::Destroy(id) => self.destroy(id, at, step),
            CaptureEvent::DestroyWindow { handle, .. } => {
                if let Some(id) = self.windows.live_id(handle) {
                    self.destroy(id, at, step);
                }
            }
            CaptureEvent::Stop(change) => self.stop(change, at, step),
            CaptureEvent::ClockChanged(change) => self.clock_boundary(
                at,
                match change {
                    ClockChange::TimeChange => ClockEndReason::TimeChange,
                    ClockChange::Resume => ClockEndReason::Resume,
                },
                step,
            ),
            CaptureEvent::Wake => self.wake(at, rules, step),
            CaptureEvent::Idle(sample) => self.idle(sample, at, step),
            CaptureEvent::PolicyChanged(foreground) => {
                self.policy_boundary(at, foreground, rules, step)
            }
            CaptureEvent::RegisterDeadline { owner, deadline } => {
                self.register_deadline(owner, deadline, at, rules, step)
            }
            CaptureEvent::SetTimedState(state) => {
                self.set_timed_state(state, super::timed::GrantEvidence::Live, at, rules, step)
            }
            CaptureEvent::RestoreTimedState(state) => self.set_timed_state(
                state,
                super::timed::GrantEvidence::Restored,
                at,
                rules,
                step,
            ),
            CaptureEvent::CancelTimedPolicy(owner) => {
                self.cancel_timed_state(owner, at, rules, step)
            }
            CaptureEvent::CancelDeadline(owner) => {
                self.deadlines.cancel(owner);
                self.persisted_deadlines.remove(&owner);
                step.writes.push(CaptureWrite::ClearDeadline { owner });
            }
            CaptureEvent::EventOverload => {
                self.begin_gap(self.clock.last_at, GapReason::EventOverload, step)
            }
            CaptureEvent::PersistenceGap {
                reason,
                lost_through,
            } => self.persistence_gap(reason, lost_through, step),
            CaptureEvent::Committed(batch) => self.settle_batch(batch, BatchSettlement::Committed),
            CaptureEvent::BatchDiscarded(batch) => {
                self.settle_batch(batch, BatchSettlement::Discarded)
            }
            CaptureEvent::Shutdown => self.shutdown(at, step),
        }
    }
    fn on_foreground(
        &mut self,
        generation: ForegroundGeneration,
        source: ObservationSource,
        mut foreground: Foreground,
        at: Timestamp,
        rules: &Arc<RuleSnapshot>,
        step: &mut CaptureStep,
    ) {
        if let Foreground::Window { privacy, .. } = &mut foreground
            && let Some(mode) = self.timed.privacy(at)
        {
            privacy.mode = mode;
        }
        let waiting = matches!(
            &foreground,
            Foreground::Window {
                privacy: PrivacyContext {
                    browser: BrowserContext::Pending,
                    ..
                },
                ..
            }
        ) && rules.privacy().requires_web_privacy();
        if waiting && self.phase.ready() && self.phase.reasons().is_empty() {
            if self.deferred.is_full() {
                self.flush_deferred(BrowserResolution::Discard, step);
            }
            if self.deferred.is_empty() {
                self.deadlines.register(
                    SchedulerKind::PrivacyContext,
                    Deadline::Mono(at.mono.advance(limits::PRIVACY_CONTEXT_WAIT)),
                );
            }
            self.rebind(generation, &foreground, step);
            let observation = self.ids.observation();
            step.effects.push(CaptureEffect::AwaitBrowserContext {
                observation,
                generation,
            });
            self.deferred.push(DeferredObservation {
                observation,
                at,
                rules: Arc::clone(rules),
                generation,
                source,
                foreground,
            });
        } else {
            // A fresh reading cannot authorize older, separately withheld snapshots.
            self.flush_deferred(BrowserResolution::Discard, step);
            self.foreground(
                ForegroundReading {
                    generation,
                    source,
                    foreground,
                    at,
                },
                EvaluationContext::fresh(rules),
                step,
            );
        }
    }
    fn on_privacy_context(
        &mut self,
        observation: ObservationId,
        browser: BrowserContext,
        step: &mut CaptureStep,
    ) {
        if self.deferred.contains(observation) {
            self.flush_deferred(
                BrowserResolution::Context {
                    observation,
                    browser,
                },
                step,
            );
        }
    }
    fn destroy(&mut self, id: WindowId, at: Timestamp, step: &mut CaptureStep) {
        self.windows.retire(id);
        if self.allowed_windows.remove(&id) {
            step.writes
                .push(CaptureWrite::RetireWindow { instance: id, at });
        }
        if self.tracker.instance() == Some(id) {
            self.phase.request_review();
            step.effects.push(CaptureEffect::ReviewForeground);
        }
    }
    fn set_timed_state(
        &mut self,
        state: super::TimedState,
        evidence: super::timed::GrantEvidence,
        at: Timestamp,
        rules: &RuleSnapshot,
        step: &mut CaptureStep,
    ) {
        let state = state
            .normalized()
            .expect("timed state validated before transition");
        let owner = state.owner();
        let until = state.until;
        if owner == TimedPolicyKind::Privacy {
            self.tracker.expire_privacy();
        }
        self.timed.install(state.clone(), evidence);
        step.writes.push(CaptureWrite::SetTimedState(state));
        if owner == TimedPolicyKind::Pause && until > at.utc {
            self.stop(StopChange::Begin(StopReason::Paused), at, step);
        }
        self.deadlines.register(owner, Deadline::Utc(until));
        self.wake(at, rules, step);
        if until > at.utc {
            self.policy_boundary(at, PolicyObservation::Refresh, rules, step);
        }
        step.effects.push(CaptureEffect::CheckIdle);
    }
    fn cancel_timed_state(
        &mut self,
        owner: TimedPolicyKind,
        at: Timestamp,
        rules: &RuleSnapshot,
        step: &mut CaptureStep,
    ) {
        self.deadlines.cancel(owner);
        self.timed.remove(owner);
        step.writes.push(CaptureWrite::ClearTimedPolicy(owner));
        if owner == TimedPolicyKind::Pause {
            self.stop(StopChange::End(StopReason::Paused), at, step);
        }
        if owner == TimedPolicyKind::Privacy {
            self.tracker.expire_privacy();
        }
        self.policy_boundary(at, PolicyObservation::Refresh, rules, step);
        step.effects.push(CaptureEffect::CheckIdle);
    }
    fn register_deadline(
        &mut self,
        owner: SchedulerKind,
        deadline: Deadline,
        at: Timestamp,
        rules: &RuleSnapshot,
        step: &mut CaptureStep,
    ) {
        self.deadlines.register(owner, deadline);
        if let Deadline::Utc(utc) = deadline {
            self.persisted_deadlines.insert(owner);
            step.writes
                .push(CaptureWrite::SetDeadline { owner, at: utc });
        }
        self.wake(at, rules, step);
    }
    fn persistence_gap(
        &mut self,
        reason: GapReason,
        lost_through: Timestamp,
        step: &mut CaptureStep,
    ) {
        self.phase.take_interval();
        self.phase.take_waiting();
        self.review_history.ranges.clear();
        for evidence in self.availability.reopen_evidence() {
            self.pending_availability
                .retain(|pending| pending.reason != evidence.reason);
            self.pending_availability.push(evidence);
        }
        let boundary = Timestamp {
            utc: lost_through.utc.max(self.clock.anchor.utc),
            mono: lost_through.mono.max(self.clock.anchor.mono),
        };
        self.begin_gap(boundary, reason, step);
    }
    fn restore_availability(&mut self, at: Timestamp, step: &mut CaptureStep) {
        // Loss feedback must not allocate another batch while an in-flight transaction fills the budget.
        // Rebuild masks on subsequent work or a durable acknowledgement instead.
        let boundary = self.clock.anchor.project(at.mono);
        for AvailabilityEvidence { reason, idle } in std::mem::take(&mut self.pending_availability)
        {
            self.open_availability(reason, boundary, idle, step);
        }
    }
    fn shutdown(&mut self, at: Timestamp, step: &mut CaptureStep) {
        self.finish_waiting(at, IntervalEndReason::Shutdown);
        self.record_waiting_gaps(step);
        self.close_interval(at, IntervalEndReason::Shutdown, step);
        self.close_all_availability(at, AvailabilityEndReason::Shutdown, step);
        self.close_gap(at, step);
        step.writes.push(CaptureWrite::CloseClockSegment {
            segment: self.clock.segment,
            at,
            reason: ClockEndReason::RunEnd,
        });
        step.writes.push(CaptureWrite::CloseRun {
            run: self.clock.run,
            at,
            reason: RunEndReason::Shutdown,
        });
        self.phase = Phase::Ended;
    }
    fn confirm_boundary(&mut self, at: Timestamp, step: &mut CaptureStep) {
        let boundary = step.writes.iter().any(|write| {
            matches!(
                write,
                CaptureWrite::OpenInterval(_)
                    | CaptureWrite::CloseInterval { .. }
                    | CaptureWrite::OpenAvailability(_)
                    | CaptureWrite::CloseAvailability { .. }
                    | CaptureWrite::ObserveWindow { .. }
                    | CaptureWrite::OpenClockSegment { .. }
            )
        });
        if boundary
            && !self.phase.ended()
            && !step
                .writes
                .iter()
                .any(|write| matches!(write, CaptureWrite::Checkpoint(_)))
        {
            self.checkpoint(at, step);
        }
    }
    fn append_coverage(&mut self, at: Timestamp, context: StepContext, step: &mut CaptureStep) {
        if !step.writes.is_empty() {
            if self.review_history.ranges.is_empty() {
                step.coverage.extend(context.previous_waiting);
            }
            let mut segment = context.previous_segment;
            let mut start = context.previous_at;
            for write in &step.writes {
                match write {
                    CaptureWrite::CloseClockSegment {
                        segment: closed,
                        at: end,
                        ..
                    } if *closed == segment => {
                        step.coverage.push(SegmentRange {
                            segment,
                            range: TimeRange { start, end: *end },
                        });
                    }
                    CaptureWrite::OpenClockSegment {
                        segment: opened,
                        at: boundary,
                    } => {
                        segment = *opened;
                        start = *boundary;
                    }
                    _ => {}
                }
            }
            if !self.phase.ended() {
                step.coverage.push(SegmentRange {
                    segment: self.clock.segment,
                    range: TimeRange { start, end: at },
                });
            }
        }
        self.clock.last_at = at;
        if context.old_state != self.state() {
            step.effects.push(CaptureEffect::StateChanged(self.state()));
        }
        self.finish(at, step);
    }

    /// Returns only sanitized state, suitable for subscriptions and tray rendering.
    pub fn state(&self) -> CaptureState {
        if self.phase.ready()
            && self.phase.reasons().is_empty()
            && self.phase.gap().is_none()
            && !self.deferred.is_empty()
        {
            CaptureState::AwaitingBrowserContext
        } else {
            self.phase.state()
        }
    }
    /// Last transaction acknowledged by the store shell.
    pub fn committed_batch(&self) -> BatchId {
        self.ledger.committed
    }

    fn validate_input(&self, at: Timestamp, event: &CaptureEvent) -> Result<(), StepCaptureError> {
        if at.mono < self.clock.last_at.mono {
            return Err(StepCaptureError::BackwardMonotonicTime);
        }
        if let CaptureEvent::Committed(batch) | CaptureEvent::BatchDiscarded(batch) = event
            && !self.ledger.accepts(*batch)
        {
            return Err(StepCaptureError::InvalidAcknowledgement);
        }
        let validate_time = || -> Result<(), AdvanceTimeError> {
            at.utc.checked_add(limits::UTC_DAY * 2)?;
            at.utc
                .checked_sub(Duration::from_millis(u64::from(u32::MAX)))?;
            let utc_range =
                Duration::try_from(jiff::Timestamp::MAX.duration_since(jiff::Timestamp::MIN))
                    .expect("ordered jiff bounds");
            at.mono.checked_add_rounded_up(utc_range)?;
            self.clock.anchor.checked_project(at.mono)?;
            if let CaptureEvent::RegisterDeadline { deadline, .. } = event {
                deadline.mono_at(at)?;
            }
            if let CaptureEvent::SetTimedState(state) | CaptureEvent::RestoreTimedState(state) =
                event
            {
                let state = state.clone().normalized()?;
                Deadline::Utc(state.until).mono_at(at)?;
            }
            if let CaptureEvent::Idle(sample) = event {
                at.mono.checked_add(sample.policy.threshold)?;
                for grant in &sample.exceptions {
                    grant.started_utc.checked_add(limits::EXCEPTION_CAP)?;
                }
            }
            Ok(())
        };
        validate_time().map_err(|source| StepCaptureError::Time { source })
    }
    fn settle_batch(&mut self, batch: BatchId, outcome: BatchSettlement) {
        self.ledger.settle(batch, outcome);
    }
    fn finish(&mut self, at: Timestamp, step: &mut CaptureStep) {
        if !step.writes.is_empty() {
            if step.coverage.is_empty() {
                step.coverage.push(SegmentRange {
                    segment: self.clock.segment,
                    range: TimeRange { start: at, end: at },
                });
            }
            step.batch = Some(self.ledger.emit(&mut self.ids));
        }
        step.effects
            .push(CaptureEffect::ScheduleTimer(if self.phase.ended() {
                Timer::Cancel
            } else {
                match self
                    .deadlines
                    .next(at)
                    .expect("deadline arithmetic validated before the transition")
                {
                    Some(at) => Timer::Arm(at),
                    None => Timer::Cancel,
                }
            }));
    }
    fn finish_waiting(&mut self, at: Timestamp, reason: IntervalEndReason) {
        if let Some(start) = self.phase.take_waiting()
            && at.mono > start.mono
        {
            self.review_history.ranges.push(WaitingRange {
                segment: self.clock.segment,
                range: TimeRange { start, end: at },
                reason,
            });
        }
    }
    fn record_waiting_gaps(&mut self, step: &mut CaptureStep) {
        for WaitingRange { segment, range, .. } in self.review_history.take() {
            step.writes.push(CaptureWrite::RecordGap(CaptureGap {
                segment,
                range,
                reason: GapReason::EventOverload,
            }));
        }
    }
    fn flush_waiting(&mut self, step: &mut CaptureStep) {
        for WaitingRange {
            segment,
            range,
            reason,
        } in self.review_history.take()
        {
            let session = self.ids.session();
            let pieces = range.split_midnights().expect("ordered segment endpoints");
            for (index, piece) in pieces.iter().enumerate() {
                let row = OpenInterval {
                    id: self.row_id(),
                    segment,
                    session,
                    start: piece.start,
                    fact: IntervalFact::NoForeground(NoForegroundReason::Empty),
                };
                let id = row.id;
                step.writes.push(CaptureWrite::OpenInterval(row));
                step.writes.push(CaptureWrite::CloseInterval {
                    id,
                    at: piece.end,
                    reason: if index + 1 == pieces.len() {
                        reason
                    } else {
                        IntervalEndReason::MidnightSplit
                    },
                });
            }
        }
    }
    fn row_id(&mut self) -> RowId {
        self.ids.row()
    }
    fn open_interval(
        &mut self,
        fact: IntervalFact,
        at: Timestamp,
        session: SessionBoundary,
        step: &mut CaptureStep,
    ) {
        let session = match session {
            SessionBoundary::New => self.ids.session(),
            SessionBoundary::Continue(session) => session,
        };
        let row = OpenInterval {
            id: self.row_id(),
            segment: self.clock.segment,
            session,
            start: at,
            fact,
        };
        step.writes.push(CaptureWrite::OpenInterval(row.clone()));
        self.phase.open_interval(row);
    }
    fn close_interval(&mut self, at: Timestamp, reason: IntervalEndReason, step: &mut CaptureStep) {
        if let Some(row) = self.phase.take_interval() {
            let at = Timestamp {
                utc: at.utc.max(row.start.utc),
                mono: at.mono.max(row.start.mono),
            };
            step.writes.push(CaptureWrite::CloseInterval {
                id: row.id,
                at,
                reason,
            });
        }
    }
    fn split_foreground(&mut self, at: Timestamp, step: &mut CaptureStep) {
        while let Some(row) = &self.phase.interval() {
            let midnight = row
                .start
                .next_midnight()
                .expect("validated UTC calendar has a next day");
            let end = self.clock.anchor.project(at.mono);
            if midnight > end.utc || midnight <= row.start.utc {
                break;
            }
            let boundary = self.clock.anchor.advance(
                midnight
                    .duration_since(self.clock.anchor.utc)
                    .expect("midnight follows the segment anchor"),
            );
            let fact = row.fact.clone();
            let session = row.session;
            self.close_interval(boundary, IntervalEndReason::MidnightSplit, step);
            self.open_interval(fact, boundary, SessionBoundary::Continue(session), step);
        }
    }
    fn open_availability(
        &mut self,
        reason: AvailabilityReason,
        at: Timestamp,
        idle: Option<IdleRecord>,
        step: &mut CaptureStep,
    ) {
        self.availability
            .open(reason, self.clock.position(at), idle, &mut self.ids, step);
    }
    fn close_availability(
        &mut self,
        reason: AvailabilityReason,
        at: Timestamp,
        end: AvailabilityEndReason,
        step: &mut CaptureStep,
    ) {
        self.availability
            .close(reason, at, end, &mut self.ids, step);
    }
    fn close_all_availability(
        &mut self,
        at: Timestamp,
        end: AvailabilityEndReason,
        step: &mut CaptureStep,
    ) {
        self.availability.close_all(at, end, &mut self.ids, step);
    }
    fn session(&mut self, status: SessionStatus, at: Timestamp, step: &mut CaptureStep) {
        let write_start = step.writes.len();
        match status {
            SessionStatus::Unavailable => self.session_unavailable(at, step),
            SessionStatus::Ready(reasons) => self.session_ready(reasons, at, step),
        }
        for write in &mut step.writes[write_start..] {
            if let CaptureWrite::Checkpoint(point) = write {
                point.at = at;
                point.commit_by_mono = at.mono.advance(limits::BATCH_WAIT);
            }
        }
    }
    fn session_unavailable(&mut self, at: Timestamp, step: &mut CaptureStep) {
        if self.phase.ready() {
            self.close_interval(at, IntervalEndReason::Disconnected, step);
            self.open_availability(
                AvailabilityReason::Stopped(StopReason::Disconnected),
                at,
                None,
                step,
            );
            let mut reasons = self.phase.reasons();
            reasons.insert(StopReason::Disconnected);
            self.phase.stop(reasons, self.phase.resume_target());
        }
        self.phase.session_unavailable();
        step.effects.push(CaptureEffect::QuerySession);
    }
    fn session_ready(
        &mut self,
        reasons: BTreeSet<StopReason>,
        at: Timestamp,
        step: &mut CaptureStep,
    ) {
        let boundary = if self.phase.initialized() {
            at
        } else {
            self.clock.started
        };
        self.phase.session_ready();
        for reason in [StopReason::Locked, StopReason::Disconnected] {
            self.stop(
                if reasons.contains(&reason) {
                    StopChange::Begin(reason)
                } else {
                    StopChange::End(reason)
                },
                boundary,
                step,
            );
        }
        for reason in reasons {
            self.stop(StopChange::Begin(reason), boundary, step);
        }
        if self.phase.reasons().is_empty()
            && self.phase.interval().is_none()
            && self.phase.gap().is_none()
        {
            self.phase.wait(boundary);
        }
        self.phase.request_review();
        step.effects.push(CaptureEffect::ReviewForeground);
    }
    fn stop(&mut self, change: StopChange, at: Timestamp, step: &mut CaptureStep) {
        match change {
            StopChange::Begin(reason) => self.begin_stop(reason, at, step),
            StopChange::End(reason) => self.end_stop(reason, at, step),
        }
    }
    fn begin_stop(&mut self, reason: StopReason, at: Timestamp, step: &mut CaptureStep) {
        let mut reasons = self.phase.reasons();
        if !reasons.insert(reason) {
            return;
        }
        let resume = self.phase.resume_target();
        self.finish_waiting(at, reason.end_reason());
        self.close_gap(at, step);
        self.close_interval(at, reason.end_reason(), step);
        self.phase.stop(reasons, resume);
        self.open_availability(AvailabilityReason::Stopped(reason), at, None, step);
        self.checkpoint(at, step);
    }
    fn end_stop(&mut self, reason: StopReason, at: Timestamp, step: &mut CaptureStep) {
        if reason == StopReason::Suspended && self.phase.reasons().contains(&reason) {
            self.clock_boundary(at, ClockEndReason::Resume, step);
        }
        let mut reasons = self.phase.reasons();
        if !reasons.remove(&reason) {
            return;
        }
        let resume = self.phase.resume_target();
        self.phase.stop(reasons, resume);
        self.close_availability(
            AvailabilityReason::Stopped(reason),
            at,
            AvailabilityEndReason::Resumed,
            step,
        );
        if self.phase.reasons().is_empty() && self.phase.ready() {
            match resume {
                ResumeTarget::Gap(reason) => self.phase.mark_gap(GapSpan { since: at, reason }),
                ResumeTarget::Review => self.phase.wait(at),
            }
            step.effects.push(CaptureEffect::ReviewForeground);
        }
    }
    fn rebind(
        &mut self,
        generation: ForegroundGeneration,
        foreground: &Foreground,
        step: &mut CaptureStep,
    ) {
        if let RebindDecision::Rebind(target) = self.tracker.rebind(generation, foreground) {
            step.effects
                .push(CaptureEffect::RebindHooks { generation, target });
        }
    }

    fn foreground(
        &mut self,
        reading: ForegroundReading,
        context: EvaluationContext<'_>,
        step: &mut CaptureStep,
    ) {
        self.rebind(reading.generation, &reading.foreground, step);
        if !self.phase.ready() || !self.phase.reasons().is_empty() {
            return;
        }
        if self.phase.needs_review() && reading.source != ObservationSource::Review {
            return;
        }
        let expected = if self.phase.needs_review() {
            ReviewExpectation::Expected
        } else {
            ReviewExpectation::Unexpected
        };
        let application = match &reading.foreground {
            Foreground::Window { facts, .. } => Some(&facts.app),
            Foreground::None(_) => None,
        };
        if self.tracker.app() != application {
            step.effects.push(CaptureEffect::CheckIdle);
        }
        self.review_foreground(reading.at, reading.source, step);
        self.split_foreground(reading.at, step);
        let observed = reading.evaluate(&mut self.windows, context);
        self.apply_foreground(observed, expected, step);
    }
    fn review_foreground(
        &mut self,
        at: Timestamp,
        source: ObservationSource,
        step: &mut CaptureStep,
    ) {
        if source != ObservationSource::Review {
            return;
        }
        if at.drifted_from(self.clock.anchor) {
            self.clock_boundary(at, ClockEndReason::Drift, step);
        }
        self.close_gap(at, step);
        self.finish_waiting(at, IntervalEndReason::NoForeground);
        self.flush_waiting(step);
        self.phase.reviewed();
    }
    fn apply_foreground(
        &mut self,
        observed: EvaluatedForeground,
        expected: ReviewExpectation,
        step: &mut CaptureStep,
    ) {
        let fact = observed.fact();
        let same_window =
            observed.instance().map(|instance| instance.id) == self.tracker.instance();
        let same_title = observed.fingerprint() == self.tracker.fingerprint();
        let same =
            self.phase.interval().is_some_and(|row| row.fact == fact) && same_window && same_title;
        if same && observed.source == ObservationSource::Hook {
            self.tracker.remember(observed);
            return;
        }
        if let Some(instance) = observed.allowed_instance() {
            self.allowed_windows.insert(instance.id);
            step.writes.push(CaptureWrite::ObserveWindow {
                instance: instance.clone(),
                at: observed.at,
            });
        }
        if !same {
            if observed.source == ObservationSource::Review
                && expected == ReviewExpectation::Unexpected
                && self.tracker.current().is_some()
            {
                step.effects.push(CaptureEffect::MissedEvent);
            }
            let reason = self.foreground_end_reason(&observed);
            if reason == IntervalEndReason::Title
                && let Some(instance) = observed.instance()
            {
                step.title_storm = self.tracker.title_activity(instance.id, observed.at.mono);
            }
            self.close_interval(observed.at, reason, step);
            self.open_interval(fact, observed.at, SessionBoundary::New, step);
        }
        self.tracker.remember(observed);
    }
    fn foreground_end_reason(&self, observed: &EvaluatedForeground) -> IntervalEndReason {
        let same_window =
            observed.instance().map(|instance| instance.id) == self.tracker.instance();
        let same_title = observed.fingerprint() == self.tracker.fingerprint();
        match observed.fact() {
            IntervalFact::NoForeground(_) => IntervalEndReason::NoForeground,
            _ if same_window
                && !same_title
                && self
                    .phase
                    .interval()
                    .is_some_and(|row| matches!(row.fact, IntervalFact::Window { .. })) =>
            {
                IntervalEndReason::Title
            }
            _ => IntervalEndReason::Switch,
        }
    }
    fn policy_boundary(
        &mut self,
        at: Timestamp,
        foreground: PolicyObservation,
        rules: &RuleSnapshot,
        step: &mut CaptureStep,
    ) {
        let old = self.phase.take_interval();
        let cached = matches!(foreground, PolicyObservation::Refresh);
        let retention = match (&old, cached) {
            (
                Some(OpenInterval {
                    fact: IntervalFact::Window { fields, .. },
                    ..
                }),
                true,
            ) => Retention::Cached(fields.clone()),
            _ => Retention::Fresh,
        };
        let start = step.writes.len();
        self.reevaluate_policy(at, foreground, rules, retention, step);
        if let Some(old) = old {
            let position = step.writes[start..]
                .iter()
                .position(|write| matches!(write, CaptureWrite::OpenInterval(_)))
                .map_or(step.writes.len(), |position| start + position);
            step.writes.insert(
                position,
                CaptureWrite::CloseInterval {
                    id: old.id,
                    at,
                    reason: IntervalEndReason::PolicyChanged,
                },
            );
        }
    }
    fn reevaluate_policy(
        &mut self,
        at: Timestamp,
        foreground: PolicyObservation,
        rules: &RuleSnapshot,
        retention: Retention,
        step: &mut CaptureStep,
    ) {
        if !self.phase.ready() || !self.phase.reasons().is_empty() || self.phase.gap().is_some() {
            return;
        }
        let cached = matches!(foreground, PolicyObservation::Refresh);
        if cached && self.phase.needs_review() {
            self.phase.wait(at);
        } else if let Some(mut foreground) = match foreground {
            PolicyObservation::Refresh => self.tracker.current(),
            PolicyObservation::Observed(foreground) => Some(foreground),
        } {
            if let Foreground::Window { privacy, .. } = &mut foreground
                && let Some(mode) = self.timed.privacy(at)
            {
                privacy.mode = mode;
            }
            let needs_review = self.phase.needs_review();
            let fingerprint = self.tracker.fingerprint();
            self.foreground(
                ForegroundReading {
                    generation: self.tracker.generation(),
                    source: ObservationSource::Review,
                    foreground,
                    at,
                },
                EvaluationContext { rules, retention },
                step,
            );
            if needs_review {
                self.phase.request_review();
            }
            if cached {
                self.tracker.restore_fingerprint(fingerprint);
            }
        } else {
            self.open_interval(
                IntervalFact::NoForeground(NoForegroundReason::Empty),
                at,
                SessionBoundary::New,
                step,
            );
        }
        step.effects.push(CaptureEffect::ReviewForeground);
    }
    fn checkpoint(&mut self, at: Timestamp, step: &mut CaptureStep) {
        if let Some(start) = self.phase.waiting()
            && start.mono < at.mono
        {
            self.finish_waiting(at, IntervalEndReason::CaptureGap);

            self.phase.mark_gap(GapSpan {
                since: at,
                reason: GapReason::EventOverload,
            });
            self.phase.request_review();
            step.effects.push(CaptureEffect::ReviewForeground);
        }
        // A review wait settled before a review succeeded is unknown coverage.
        self.record_waiting_gaps(step);
        if let Some(GapSpan {
            since: start,
            reason,
        }) = self.phase.gap()
            && start.mono < at.mono
        {
            step.writes.push(CaptureWrite::RecordGap(CaptureGap {
                segment: self.clock.segment,
                range: TimeRange { start, end: at },
                reason,
            }));
            self.phase.mark_gap(GapSpan { since: at, reason });
        }
        step.writes.push(CaptureWrite::Checkpoint(Checkpoint {
            run: self.clock.run,
            segment: self.clock.segment,
            interval: self.phase.interval().as_ref().map(|row| row.id),
            availability: self.availability.open_ids(),
            at,
            commit_by_mono: at.mono.advance(limits::BATCH_WAIT),
            web_extension: WebRowExtension::PendingM5,
        }));
    }
    fn clock_boundary(&mut self, at: Timestamp, reason: ClockEndReason, step: &mut CaptureStep) {
        let old_end = self.clock.anchor.project(at.mono);
        self.finish_waiting(old_end, IntervalEndReason::ClockJump);
        self.close_interval(old_end, IntervalEndReason::ClockJump, step);
        let active = self.availability.evidence();
        // A resume supplies wall time for the suspended mask, while foreground ends
        // through the old segment's monotonic projection.
        let availability_end = if reason == ClockEndReason::Resume && at.utc >= old_end.utc {
            at
        } else {
            old_end
        };
        self.close_all_availability(availability_end, AvailabilityEndReason::ClockJump, step);
        let gap = self.phase.gap().map(|gap| gap.reason);
        self.close_gap(old_end, step);
        step.writes.push(CaptureWrite::CloseClockSegment {
            segment: self.clock.segment,
            at: old_end,
            reason,
        });
        self.clock.reopen(at, &mut self.ids);
        step.writes.push(CaptureWrite::OpenClockSegment {
            segment: self.clock.segment,
            at,
        });
        for AvailabilityEvidence { reason, idle } in active {
            self.open_availability(reason, at, idle, step);
        }
        if let Some(reason) = gap {
            self.phase.mark_gap(GapSpan { since: at, reason });
        } else if self.phase.ready() && self.phase.reasons().is_empty() {
            self.phase.wait(at);
        }
        self.phase.request_review();
        self.deadlines.register(
            SchedulerKind::Midnight,
            Deadline::Utc(
                at.next_midnight()
                    .expect("validated UTC calendar has a next day"),
            ),
        );
        step.effects.push(CaptureEffect::ReviewForeground);
    }
    fn idle(&mut self, sample: IdleSample, at: Timestamp, step: &mut CaptureStep) {
        if !self.phase.initialized() {
            return;
        }
        let mut sample = sample;
        let app = if self.phase.ready()
            && !self.phase.needs_review()
            && self.phase.reasons().is_empty()
            && self.deferred.is_empty()
        {
            self.tracker.app()
        } else {
            None
        };
        sample.exceptions.extend(self.timed.grants(at, app));
        let decision = sample.decide(at);
        self.deadlines.register(
            SchedulerKind::Idle,
            Deadline::Mono(decision.next_check_mono),
        );
        match self
            .availability
            .apply_idle(decision, at, self.clock.anchor)
        {
            AvailabilityAction::Preserve(error) => {
                step.effects.push(CaptureEffect::IdleSampleFailed(error))
            }
            AvailabilityAction::Open(record) => {
                self.open_availability(AvailabilityReason::Idle, record.start, Some(record), step)
            }
            AvailabilityAction::Close(reason) => {
                self.close_availability(AvailabilityReason::Idle, at, reason, step)
            }
        }
    }
    fn begin_gap(&mut self, at: Timestamp, reason: GapReason, step: &mut CaptureStep) {
        if self.phase.gap().is_none() && self.phase.reasons().is_empty() {
            let at = Timestamp {
                utc: at.utc.max(self.clock.anchor.utc),
                mono: at.mono.max(self.clock.anchor.mono),
            };
            self.finish_waiting(at, IntervalEndReason::CaptureGap);
            self.record_waiting_gaps(step);
            self.split_foreground(at, step);
            self.close_interval(at, IntervalEndReason::CaptureGap, step);
            self.phase.mark_gap(GapSpan { since: at, reason });
        }
        if !self.phase.reasons().is_empty() {
            self.phase
                .stop(self.phase.reasons(), ResumeTarget::Gap(reason));
        }
        self.phase.request_review();
        self.tracker.forget();
        step.effects.push(CaptureEffect::DiscardQueuedObservations);
        step.effects.push(CaptureEffect::ReviewForeground);
    }
    fn close_gap(&mut self, at: Timestamp, step: &mut CaptureStep) {
        if let Some(GapSpan {
            since: start,
            reason,
        }) = self.phase.take_gap()
        {
            let end = Timestamp {
                utc: at.utc.max(start.utc),
                mono: at.mono.max(start.mono),
            };
            step.writes.push(CaptureWrite::RecordGap(CaptureGap {
                segment: self.clock.segment,
                range: TimeRange { start, end },
                reason,
            }));
        }
    }
    fn wake(&mut self, at: Timestamp, rules: &RuleSnapshot, step: &mut CaptureStep) {
        if self.deferred.expired(at) {
            self.flush_deferred(BrowserResolution::Discard, step);
        }
        let due = self.deadlines.take_due(at);
        for owner in due {
            match owner {
                DeadlineKind::Scheduler(owner) => self.wake_scheduler(owner, at, step),
                DeadlineKind::Policy(owner) => self.expire_policy(owner, at, rules, step),
            }
        }
    }
    fn wake_scheduler(&mut self, owner: SchedulerKind, at: Timestamp, step: &mut CaptureStep) {
        if self.persisted_deadlines.remove(&owner) {
            step.writes.push(CaptureWrite::ClearDeadline { owner });
        }
        match owner {
            SchedulerKind::Maintenance => {
                self.flush_deferred(BrowserResolution::Discard, step);
                if at.mono > self.clock.maintenance_deadline.advance(limits::BATCH_WAIT) {
                    self.begin_gap(self.clock.last_at, GapReason::EventOverload, step);
                }
                self.checkpoint(at, step);
                self.clock.maintenance_deadline = at.mono.advance(limits::CHECKPOINT_PERIOD);
                step.effects.push(CaptureEffect::ReviewForeground);
                self.deadlines.register(
                    owner,
                    Deadline::Mono(at.mono.advance(limits::CHECKPOINT_PERIOD)),
                );
            }
            SchedulerKind::Midnight => {
                if self.deferred.is_empty() {
                    self.split_foreground(at, step);
                }
                self.deadlines.register(
                    owner,
                    Deadline::Utc(
                        at.next_midnight()
                            .expect("validated UTC calendar has a next day"),
                    ),
                );
            }
            SchedulerKind::Idle => step.effects.push(CaptureEffect::CheckIdle),
            SchedulerKind::PrivacyContext => self.flush_deferred(BrowserResolution::Discard, step),
        }
    }
    fn expire_policy(
        &mut self,
        owner: TimedPolicyKind,
        at: Timestamp,
        rules: &RuleSnapshot,
        step: &mut CaptureStep,
    ) {
        step.writes.push(CaptureWrite::ClearTimedPolicy(owner));
        self.timed.remove(owner);
        if owner == TimedPolicyKind::Pause {
            self.stop(StopChange::End(StopReason::Paused), at, step);
        }
        if owner == TimedPolicyKind::Privacy {
            self.tracker.expire_privacy();
        }
        self.policy_boundary(at, PolicyObservation::Refresh, rules, step);
        step.effects.push(CaptureEffect::PolicyExpired(owner));
        step.effects.push(CaptureEffect::CheckIdle);
    }

    fn flush_deferred(&mut self, context: BrowserResolution, step: &mut CaptureStep) {
        self.deadlines.cancel(SchedulerKind::PrivacyContext);
        if self
            .persisted_deadlines
            .remove(&SchedulerKind::PrivacyContext)
        {
            step.writes.push(CaptureWrite::ClearDeadline {
                owner: SchedulerKind::PrivacyContext,
            });
        }
        while let Some(mut pending) = self.deferred.pop() {
            if let Foreground::Window { privacy, .. } = &mut pending.foreground {
                privacy.browser = match &context {
                    BrowserResolution::Context {
                        observation,
                        browser,
                    } if *observation == pending.observation
                        && matches!(browser, BrowserContext::Ready(_)) =>
                    {
                        browser.clone()
                    }
                    _ => BrowserContext::Offline,
                };
            }
            self.foreground(
                ForegroundReading {
                    generation: pending.generation,
                    source: pending.source,
                    foreground: pending.foreground,
                    at: pending.at,
                },
                EvaluationContext::fresh(&pending.rules),
                step,
            );
        }
    }
}
