//! Database-thread persistence shell; all core feedback is returned as values.

#[cfg(any(windows, test))]
mod categories;
#[cfg(any(windows, test))]
pub(crate) mod diagnostics;
#[cfg(any(windows, test))]
pub(crate) mod dispatch;
mod queue;
use queue::TransactionPlan;
pub(crate) use queue::WriteQueue;

use crate::store::{
    Store,
    capture::{
        CaptureKeys, CaptureStartup, ExecuteCaptureBatchError, RecoverCaptureError, RunMetadata,
    },
};
#[cfg(test)]
use quartz_domain::capture::CaptureParts;
use quartz_domain::capture::{
    AdvanceTimeError, BacklogOutcome, BatchId, CaptureEffect, CaptureEvent, MonoInstant, RunId,
    Timestamp,
};
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub(crate) struct QueueShutdown {
    pub at: Timestamp,
    pub deadline: std::time::Instant,
}

/// Maximum commands per transaction, data model 03 §4.2; a single core batch is indivisible.
const TRANSACTION_COMMANDS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckpointCollection {
    Skip,
    Collect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersistenceState {
    Ready,
    Buffered { commands: usize },
    Failed { commands: usize },
}
#[derive(Debug)]
pub(crate) enum PersistenceOutcome {
    Ready,
    Buffered {
        commands: usize,
    },
    Failed {
        commands: usize,
        error: ExecuteCaptureBatchError,
    },
    /// The original error was returned by the failed write attempt.
    RetainedFailure {
        commands: usize,
    },
}
/// Progress made by one bounded database transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlushProgress {
    None,
    Committed { commands: usize },
}
#[derive(Debug, snafu::Snafu)]
#[snafu(display("capture flush retained buffered commands without making progress"))]
pub(crate) struct FlushWritesError;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CommittedCheckpoint {
    pub run: RunId,
    pub batch: BatchId,
    pub at: Timestamp,
    pub commit_by_mono: MonoInstant,
}
impl CommittedCheckpoint {
    pub(crate) fn latency_at(&self, committed: MonoInstant) -> Result<Duration, AdvanceTimeError> {
        committed.duration_since(self.at.mono)
    }
}

#[derive(Debug)]
pub(crate) struct PersistenceResult {
    pub feedback: Vec<CaptureEvent>,
    pub effects: Vec<CaptureEffect>,
    pub outcome: PersistenceOutcome,
    pub progress: FlushProgress,
    pub checkpoints: Vec<CommittedCheckpoint>,
}
impl PersistenceResult {
    pub(crate) fn state(&self) -> PersistenceState {
        match self.outcome {
            PersistenceOutcome::Ready => PersistenceState::Ready,
            PersistenceOutcome::Buffered { commands } => PersistenceState::Buffered { commands },
            PersistenceOutcome::Failed { commands, .. }
            | PersistenceOutcome::RetainedFailure { commands } => {
                PersistenceState::Failed { commands }
            }
        }
    }
    pub(crate) fn failure(&self) -> Option<&ExecuteCaptureBatchError> {
        match &self.outcome {
            PersistenceOutcome::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    fn append_outcome(&mut self, outcome: BacklogOutcome) {
        self.feedback.extend(
            outcome
                .coalesced
                .into_iter()
                .chain(outcome.discarded)
                .map(CaptureEvent::BatchDiscarded),
        );
        // Storage retains every segment's loss; the core only needs its latest untrusted frontier.
        if let Some(gap) = outcome
            .gaps
            .into_iter()
            .max_by_key(|gap| gap.range.end.mono)
        {
            self.feedback.push(CaptureEvent::PersistenceGap {
                reason: gap.reason,
                lost_through: gap.range.end,
            });
        }
    }
}

#[derive(Debug)]
pub(crate) struct OpenedPersistence {
    pub persistence: Persistence,
    pub startup: CaptureStartup,
}

/// Owns retained writes and transaction-local key mappings, without calling the core.
#[derive(Debug)]
pub(crate) struct Persistence {
    queue: WriteQueue,
    keys: CaptureKeys,
    metadata: RunMetadata,
}
impl Persistence {
    #[cfg(test)]
    pub(crate) fn window_count(&self) -> usize {
        self.keys.window_count()
    }

    #[cfg(test)]
    pub(crate) fn segment_count(&self) -> usize {
        self.keys.segment_count()
    }

    pub(crate) fn state(&self) -> PersistenceState {
        self.queue.state()
    }
    pub(crate) fn writer(&self) -> WriteQueue {
        self.queue.clone()
    }
    pub(crate) fn open(
        store: &mut Store,
        metadata: RunMetadata,
    ) -> Result<OpenedPersistence, RecoverCaptureError> {
        let mut keys = CaptureKeys::default();
        let startup = store.recover_capture(&mut keys, &metadata)?;
        Ok(OpenedPersistence {
            persistence: Self {
                queue: WriteQueue::new(),
                keys,
                metadata,
            },
            startup,
        })
    }
    #[cfg(test)]
    pub(crate) fn accept(&mut self, parts: CaptureParts, now: Timestamp) -> PersistenceResult {
        self.queue.accept(parts, now)
    }
    pub(crate) fn next_deadline(&self) -> Option<MonoInstant> {
        self.queue.next_deadline()
    }
    pub(crate) fn is_due(&self, now: Timestamp) -> bool {
        self.queue.due(now)
    }
    /// Failure retry time is sampled after SQLite has returned, including lock waits.
    pub(crate) fn flush_with_clock<E>(
        &mut self,
        store: &mut Store,
        now: Timestamp,
        checkpoints: CheckpointCollection,
        read: impl FnOnce() -> Result<Timestamp, E>,
    ) -> Result<PersistenceResult, E> {
        let mut result = self.flush_transaction(store, now, checkpoints);
        if result.failure().is_some() {
            let failed_at = read()?;
            result.append_outcome(self.queue.retry_after(failed_at));
            if let PersistenceOutcome::Failed { commands, .. } = &mut result.outcome {
                *commands = match self.queue.state() {
                    PersistenceState::Failed { commands } => commands,
                    _ => unreachable!("failed transaction retains failed queue state"),
                };
            }
        }
        Ok(result)
    }
    #[cfg(test)]
    pub(crate) fn flush_due(
        &mut self,
        store: &mut Store,
        now: Timestamp,
        checkpoints: CheckpointCollection,
    ) -> PersistenceResult {
        if self.queue.due(now) {
            self.flush(store, now, checkpoints)
        } else {
            self.queue.result()
        }
    }
    /// Pins a bounded prefix under the queue lock, then executes SQLite with that lock released.
    #[cfg(test)]
    pub(crate) fn flush(
        &mut self,
        store: &mut Store,
        now: Timestamp,
        checkpoints: CheckpointCollection,
    ) -> PersistenceResult {
        self.flush_with_clock(store, now, checkpoints, || {
            Ok::<_, std::convert::Infallible>(now)
        })
        .expect("infallible fixture clock")
    }
    fn flush_transaction(
        &mut self,
        store: &mut Store,
        now: Timestamp,
        checkpoints: CheckpointCollection,
    ) -> PersistenceResult {
        let prepared = self.queue.prepare(now);
        let mut result = prepared.result;
        if let Err(error) = self.execute_plan(store, &prepared.plan) {
            result.outcome = self.queue.mark_failed(error);
            return result;
        }
        self.queue.committed(&prepared.plan);
        self.queue.retire_segments(&mut self.keys);
        if checkpoints == CheckpointCollection::Collect {
            result.checkpoints = prepared.plan.checkpoints().collect();
        }
        result.feedback.extend(
            prepared
                .plan
                .committed_batches()
                .into_iter()
                .map(CaptureEvent::Committed),
        );
        result.progress = prepared.plan.progress();
        result.outcome = self.queue.result().outcome;
        result
    }
    fn execute_plan(
        &mut self,
        store: &mut Store,
        plan: &TransactionPlan,
    ) -> Result<(), ExecuteCaptureBatchError> {
        match plan {
            TransactionPlan::Empty => Ok(()),
            TransactionPlan::Batches(batches) => {
                store.capture_transaction(&mut self.keys, &self.metadata, |capture| {
                    for batch in batches {
                        capture.execute(batch)?;
                    }
                    Ok(())
                })
            }
            TransactionPlan::Repair { stored, writes } => {
                store.capture_transaction(&mut self.keys, &self.metadata, |capture| {
                    capture.execute_writes(stored.run, writes)
                })
            }
        }
    }
}
