//! One command budget shared by the producer, retained writes and the active transaction.

use super::{
    CommittedCheckpoint, ExecuteCaptureBatchError, PersistenceOutcome, PersistenceResult,
    PersistenceState, TRANSACTION_COMMANDS,
};
use quartz_domain::capture::{
    BacklogFailure, CaptureParts, CaptureWrite, MonoInstant, Timestamp, WriteBacklog, limits,
};
use quartz_domain::capture::{BacklogOutcome, BatchId, StoredGap, WriteBatch};
use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

/// Retry ten times within the one-second confirmation budget (02 §4.3).
const PERSISTENCE_RETRY_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub(crate) struct WriteQueue(Arc<Mutex<QueueState>>);
#[derive(Debug)]
struct QueueState {
    backlog: WriteBacklog,
    state: PersistenceState,
    shutdown: Option<super::QueueShutdown>,
    retry: Option<MonoInstant>,
}
#[derive(Debug)]
pub(super) enum TransactionPlan {
    Empty,
    Batches(Vec<WriteBatch>),
    Repair {
        stored: StoredGap,
        writes: Vec<CaptureWrite>,
    },
}
#[derive(Debug)]
pub(super) struct PreparedTransaction {
    pub result: PersistenceResult,
    pub plan: TransactionPlan,
}
impl WriteQueue {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(QueueState {
            backlog: WriteBacklog::new(),
            state: PersistenceState::Ready,
            shutdown: None,
            retry: None,
        })))
    }
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.0
            .lock()
            .expect("write queue mutations contain no I/O or application callbacks")
    }
    /// Only mutates bounded memory; never waits for capacity or for SQLite.
    pub(crate) fn accept(&self, parts: CaptureParts, now: Timestamp) -> PersistenceResult {
        let mut queue = self.lock();
        let mut result = queue.result();
        result.effects = parts.effects;
        if let Some(batch) = parts.batch {
            result.append_outcome(queue.backlog.push(batch, now));
        }
        queue.update_state();
        result.outcome = queue.outcome();
        result
    }
    pub(crate) fn finish(&self, request: super::QueueShutdown) {
        self.lock().shutdown = Some(request);
    }
    pub(crate) fn shutdown(&self) -> Option<super::QueueShutdown> {
        self.lock().shutdown
    }
    pub(crate) fn state(&self) -> PersistenceState {
        let mut queue = self.lock();
        queue.update_state();
        queue.state
    }
    pub(crate) fn next_deadline(&self) -> Option<MonoInstant> {
        self.lock().next_deadline()
    }
    pub(super) fn due(&self, now: Timestamp) -> bool {
        let queue = self.lock();
        if matches!(queue.state, PersistenceState::Failed { .. }) {
            return queue
                .next_deadline()
                .is_some_and(|deadline| deadline <= now.mono);
        }
        queue.backlog.first_gap().is_some()
            || queue.backlog.command_count() >= TRANSACTION_COMMANDS
            || queue.immediate()
            || queue
                .next_deadline()
                .is_some_and(|deadline| deadline <= now.mono)
    }
    pub(super) fn result(&self) -> PersistenceResult {
        self.lock().result()
    }
    pub(super) fn prepare(&self, now: Timestamp) -> PreparedTransaction {
        let mut queue = self.lock();
        let mut result = queue.result();
        // A producer can observe a later clock while the previous SQLite transaction runs.
        if queue
            .backlog
            .oldest_queued_at()
            .is_none_or(|queued| queued.mono <= now.mono)
        {
            result.append_outcome(queue.backlog.advance(now));
        }
        let plan = queue.reserve_transaction();
        PreparedTransaction { result, plan }
    }
    pub(super) fn committed(&self, plan: &TransactionPlan) {
        let mut queue = self.lock();
        match plan {
            TransactionPlan::Empty => {}
            TransactionPlan::Batches(batches) => {
                for batch in batches {
                    assert!(
                        queue.backlog.committed(batch.id),
                        "only the pinned prefix is acknowledged"
                    );
                }
            }
            TransactionPlan::Repair { stored, .. } => {
                assert!(
                    queue.backlog.committed_gap(stored.run, stored.gap),
                    "only the pinned gap is acknowledged"
                );
            }
        }
        queue.backlog.failed(BacklogFailure::QueueFull);
        queue.retry = None;
        queue.state = PersistenceState::Ready;
        queue.update_state();
    }
    pub(super) fn retry_after(&self, now: Timestamp) -> BacklogOutcome {
        let mut queue = self.lock();
        let outcome = if queue
            .backlog
            .oldest_queued_at()
            .is_none_or(|queued| queued.mono <= now.mono)
        {
            queue.backlog.advance(now)
        } else {
            BacklogOutcome::default()
        };
        queue.update_state();
        queue.schedule_retry(now);
        outcome
    }
    #[cfg(test)]
    pub(super) fn failed(
        &self,
        error: ExecuteCaptureBatchError,
        now: Timestamp,
    ) -> PersistenceOutcome {
        let outcome = self.mark_failed(error);
        self.lock().schedule_retry(now);
        outcome
    }
    pub(super) fn mark_failed(&self, error: ExecuteCaptureBatchError) -> PersistenceOutcome {
        let mut queue = self.lock();
        queue.backlog.release_reservation();
        queue.backlog.failed(BacklogFailure::Database);
        let commands = queue.backlog.command_count();
        queue.state = PersistenceState::Failed { commands };
        queue.retry = None;
        PersistenceOutcome::Failed { commands, error }
    }
}
impl QueueState {
    fn schedule_retry(&mut self, now: Timestamp) {
        let retry = now
            .mono
            .checked_add(PERSISTENCE_RETRY_INTERVAL)
            .expect("validated clock has room for persistence retry");
        let confirmation = self
            .backlog
            .batches()
            .flat_map(|batch| &batch.writes)
            .filter_map(CaptureWrite::checkpoint)
            .map(|point| point.commit_by_mono)
            .filter(|deadline| *deadline > now.mono)
            .min();
        self.retry = Some(confirmation.map_or(retry, |deadline| retry.min(deadline)));
    }

    fn immediate(&self) -> bool {
        self.backlog
            .batches()
            .flat_map(|batch| &batch.writes)
            .any(|write| {
                matches!(
                    write,
                    CaptureWrite::OpenAvailability(quartz_domain::capture::OpenAvailability {
                        reason: quartz_domain::capture::AvailabilityReason::Stopped(_),
                        ..
                    }) | CaptureWrite::CloseInterval {
                        reason: quartz_domain::capture::IntervalEndReason::Locked
                            | quartz_domain::capture::IntervalEndReason::Suspended
                            | quartz_domain::capture::IntervalEndReason::Paused,
                        ..
                    }
                )
            })
    }

    fn next_deadline(&self) -> Option<MonoInstant> {
        if matches!(self.state, PersistenceState::Failed { .. }) {
            return self
                .retry
                .into_iter()
                .chain(self.backlog.next_deadline())
                .min();
        }
        let batching = self.backlog.oldest_queued_at().map(|at| {
            at.mono
                .checked_add(limits::BATCH_WAIT)
                .expect("validated capture clock has room for batching")
        });
        let confirmation = self
            .backlog
            .batches()
            .flat_map(|batch| &batch.writes)
            .filter_map(CaptureWrite::checkpoint)
            .map(|point| point.commit_by_mono)
            .min();
        batching
            .into_iter()
            .chain(confirmation)
            .chain(self.backlog.next_deadline())
            .min()
    }
    fn reserve_transaction(&mut self) -> TransactionPlan {
        if let Some(stored) = self.backlog.first_gap() {
            let writes = self.backlog.recovery_writes();
            self.backlog.reserve_gap();
            return TransactionPlan::Repair { stored, writes };
        }
        let batches = self.transaction_batches();
        if let Some(last) = batches.last() {
            self.backlog.reserve_batches(last.id);
        }
        if batches.is_empty() {
            TransactionPlan::Empty
        } else {
            TransactionPlan::Batches(batches)
        }
    }
    fn transaction_batches(&self) -> Vec<WriteBatch> {
        let mut commands = 0;
        self.backlog
            .batches()
            .take_while(|batch| {
                if commands > 0 && commands + batch.writes.len() > TRANSACTION_COMMANDS {
                    return false;
                }
                commands += batch.writes.len();
                true
            })
            .cloned()
            .collect()
    }
    fn update_state(&mut self) {
        let commands = self.backlog.command_count();
        self.state = match self.state {
            PersistenceState::Failed { .. } => PersistenceState::Failed { commands },
            _ if commands > 0 || self.backlog.first_gap().is_some() => {
                PersistenceState::Buffered { commands }
            }
            _ => PersistenceState::Ready,
        };
    }
    fn outcome(&self) -> PersistenceOutcome {
        match self.state {
            PersistenceState::Ready => PersistenceOutcome::Ready,
            PersistenceState::Buffered { commands } => PersistenceOutcome::Buffered { commands },
            PersistenceState::Failed { commands } => {
                PersistenceOutcome::RetainedFailure { commands }
            }
        }
    }
    fn result(&self) -> PersistenceResult {
        PersistenceResult {
            feedback: Vec::new(),
            effects: Vec::new(),
            outcome: self.outcome(),
            progress: super::FlushProgress::None,
            checkpoints: Vec::new(),
        }
    }
}
impl TransactionPlan {
    pub(super) fn checkpoints(&self) -> impl Iterator<Item = CommittedCheckpoint> + '_ {
        let batches = match self {
            Self::Batches(batches) => Some(batches),
            Self::Empty | Self::Repair { .. } => None,
        };
        batches.into_iter().flatten().flat_map(|batch| {
            batch
                .writes
                .iter()
                .filter_map(CaptureWrite::checkpoint)
                .map(move |point| CommittedCheckpoint {
                    run: point.run,
                    batch: batch.id,
                    at: point.at,
                    commit_by_mono: point.commit_by_mono,
                })
        })
    }
    pub(super) fn progress(&self) -> super::FlushProgress {
        let commands = match self {
            Self::Empty => 0,
            Self::Batches(batches) => batches.iter().map(|batch| batch.writes.len()).sum(),
            Self::Repair { writes, .. } => writes.len(),
        };
        if commands == 0 {
            super::FlushProgress::None
        } else {
            super::FlushProgress::Committed { commands }
        }
    }
    pub(super) fn committed_batches(&self) -> Vec<BatchId> {
        match self {
            Self::Batches(batches) => batches.iter().map(|batch| batch.id).collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_domain::{
        capture::*,
        rules::registry::{RegistryState, RuleRegistry, SnapshotCell},
    };
    use std::{thread, time::Duration};

    struct Script {
        core: CaptureCore,
        rules: SnapshotCell,
        segment: SegmentId,
    }
    impl Script {
        fn new() -> Self {
            let started = CaptureCore::start(RunId::new(1), Self::at(0)).expect("core");
            let segment = started
                .step
                .writes
                .iter()
                .find_map(|write| match write {
                    CaptureWrite::OpenClockSegment { segment, .. } => Some(*segment),
                    _ => None,
                })
                .expect("initial segment");
            Self {
                core: started.core,
                rules: RuleRegistry::from_state(RegistryState::default())
                    .expect("rules")
                    .snapshots(),
                segment,
            }
        }
        fn at(millis: u64) -> Timestamp {
            Timestamp {
                utc: UtcInstant::try_from(i64::try_from(millis).expect("test time")).expect("UTC"),
                mono: MonoInstant::from_millis(millis),
            }
        }
        fn parts(&mut self, writes: Vec<CaptureWrite>, range: TimeRange) -> CaptureParts {
            let mut parts = self
                .core
                .step(CaptureInput {
                    at: range.end,
                    rules: self.rules.load(),
                    event: CaptureEvent::RegisterDeadline {
                        owner: SchedulerKind::Midnight,
                        deadline: Deadline::Utc(Self::at(86_400_000).utc),
                    },
                })
                .expect("allocate batch")
                .into_parts(RunId::new(1));
            let batch = parts.batch.as_mut().expect("one atomic batch");
            batch.writes = writes;
            batch.coverage = vec![SegmentRange {
                segment: self.segment,
                range,
            }];
            parts.effects.clear();
            parts
        }
        fn checkpoint(&self, at: Timestamp) -> CaptureWrite {
            CaptureWrite::Checkpoint(Checkpoint {
                run: RunId::new(1),
                segment: self.segment,
                interval: None,
                availability: Vec::new(),
                at,
                commit_by_mono: Self::at(1_000).mono,
            })
        }
        fn range(start: u64, end: u64) -> TimeRange {
            TimeRange {
                start: Self::at(start),
                end: Self::at(end),
            }
        }
        fn identities(commands: usize) -> Vec<CaptureWrite> {
            vec![
                CaptureWrite::RetireWindow {
                    instance: WindowId::from_persisted(77),
                    at: Self::at(0)
                };
                commands
            ]
        }
    }

    #[test]
    fn failed_writes_retry_before_confirmation_expiry_and_after_gap_conversion() {
        let queue = WriteQueue::new();
        let mut script = Script::new();
        let parts = script.parts(vec![script.checkpoint(Script::at(0))], Script::range(0, 1));
        let _ = queue.accept(parts, Script::at(1));
        let _ = queue.prepare(Script::at(1));
        let fail = |at| {
            queue.failed(
                ExecuteCaptureBatchError::Database {
                    source: diesel::result::Error::RollbackTransaction,
                },
                Script::at(at),
            )
        };
        let _ = fail(1);
        assert_eq!(queue.next_deadline(), Some(Script::at(101).mono));
        assert!(!queue.due(Script::at(100)), "no busy retry loop");
        assert!(queue.due(Script::at(101)), "retry without new observations");
        let _ = queue.prepare(Script::at(950));
        let _ = fail(950);
        assert_eq!(
            queue.next_deadline(),
            Some(Script::at(1000).mono),
            "retry can still attempt confirmation before expiry"
        );
        let expired = queue.prepare(Script::at(1001));
        assert!(matches!(expired.plan, TransactionPlan::Repair { .. }));
        let _ = fail(1001);
        assert_eq!(
            queue.next_deadline(),
            Some(Script::at(1101).mono),
            "repair retains a retry even with no batches"
        );
        assert!(!queue.due(Script::at(1002)));
        assert!(queue.due(Script::at(1101)));
    }

    #[test]
    fn one_command_budget_coalesces_before_overflow_and_records_exact_gap() {
        let queue = WriteQueue::new();
        let mut script = Script::new();
        let large = script.parts(
            Script::identities(limits::WRITE_QUEUE_CAPACITY - 1),
            Script::range(0, 1),
        );
        assert!(queue.accept(large, Script::at(1)).feedback.is_empty());
        let first = script.parts(vec![script.checkpoint(Script::at(2))], Script::range(1, 2));
        let first_id = first.batch.as_ref().expect("checkpoint").id;
        let _ = queue.accept(first, Script::at(2));
        assert_eq!(
            queue.state(),
            PersistenceState::Buffered {
                commands: limits::WRITE_QUEUE_CAPACITY
            }
        );
        let replacement = script.parts(vec![script.checkpoint(Script::at(3))], Script::range(2, 3));
        let merged = queue.accept(replacement, Script::at(3));
        assert!(
            matches!(merged.feedback.as_slice(), [CaptureEvent::BatchDiscarded(id)] if *id == first_id)
        );
        assert_eq!(
            queue.state(),
            PersistenceState::Buffered {
                commands: limits::WRITE_QUEUE_CAPACITY
            }
        );
        let extra = script.parts(Script::identities(2), Script::range(3, 4));
        let overflow = queue.accept(extra, Script::at(4));
        assert!(overflow.feedback.iter().any(|event| matches!(event, CaptureEvent::PersistenceGap { reason: GapReason::WriteQueueOverload, lost_through } if *lost_through == Script::at(4))));
        let prepared = queue.prepare(Script::at(4));
        let TransactionPlan::Repair { stored, writes } = &prepared.plan else {
            panic!("identity-free repair");
        };
        assert_eq!(stored.gap.range, Script::range(0, 4));
        assert_eq!(stored.gap.reason, GapReason::WriteQueueOverload);
        assert!(writes.iter().all(|write| matches!(
            write,
            CaptureWrite::SettlePersistedRows { .. } | CaptureWrite::RecordGap(_)
        )));
    }

    #[test]
    fn producer_finishes_while_full_budget_is_reserved_by_database() {
        let queue = WriteQueue::new();
        let mut script = Script::new();
        let full = script.parts(
            Script::identities(limits::WRITE_QUEUE_CAPACITY),
            Script::range(0, 1),
        );
        let _ = queue.accept(full, Script::at(1));
        let pinned = queue.prepare(Script::at(1));
        assert!(matches!(pinned.plan, TransactionPlan::Batches(_)));
        let extra = script.parts(Script::identities(1), Script::range(1, 2));
        let producer = queue.clone();
        let (done, completed) = crossbeam_channel::bounded(1);
        let handle = thread::spawn(move || {
            done.send(producer.accept(extra, Script::at(2)))
                .expect("result")
        });
        let outcome = completed
            .recv_timeout(Duration::from_secs(1))
            .expect("producer does not wait for the transaction");
        handle.join().expect("producer thread");
        assert!(outcome.feedback.iter().any(|event| matches!(
            event,
            CaptureEvent::PersistenceGap {
                reason: GapReason::WriteQueueOverload,
                ..
            }
        )));
        assert_eq!(
            queue.state(),
            PersistenceState::Buffered {
                commands: limits::WRITE_QUEUE_CAPACITY
            }
        );
        queue.committed(&pinned.plan);
        let repair = queue.prepare(Script::at(2));
        assert!(
            matches!(repair.plan, TransactionPlan::Repair { stored, .. } if stored.gap.range == Script::range(1, 2))
        );
    }
}
