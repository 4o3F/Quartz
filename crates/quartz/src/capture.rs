//! Database-thread persistence shell; all core feedback is returned as values.

use crate::store::{
    Store,
    capture::{
        CaptureKeys, CaptureStartup, ExecuteCaptureBatchError, RecoverCaptureError, RunMetadata,
    },
};
use quartz_domain::capture::{
    BacklogFailure, BacklogOutcome, CaptureEffect, CaptureEvent, CaptureParts, CaptureWrite,
    MonoInstant, Timestamp, WriteBacklog, limits,
};

/// Maximum commands per transaction, data model 03 §4.2; a single core batch is indivisible.
const TRANSACTION_COMMANDS: usize = 64;

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
#[derive(Debug)]
pub(crate) struct PersistenceResult {
    pub feedback: Vec<CaptureEvent>,
    pub effects: Vec<CaptureEffect>,
    pub outcome: PersistenceOutcome,
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
        for gap in outcome.gaps {
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
    backlog: WriteBacklog,
    keys: CaptureKeys,
    metadata: RunMetadata,
    state: PersistenceState,
}
impl Persistence {
    /// Settle previous runs atomically before returning a writer for the new run.
    pub(crate) fn open(
        store: &mut Store,
        metadata: RunMetadata,
    ) -> Result<OpenedPersistence, RecoverCaptureError> {
        let mut keys = CaptureKeys::default();
        let startup = store.recover_capture(&mut keys, &metadata)?;
        Ok(OpenedPersistence {
            persistence: Self {
                backlog: WriteBacklog::new(),
                keys,
                metadata,
                state: PersistenceState::Ready,
            },
            startup,
        })
    }
    pub(crate) fn accept(&mut self, parts: CaptureParts, now: Timestamp) -> PersistenceResult {
        let mut result = self.result();
        result.effects = parts.effects;
        if let Some(batch) = parts.batch {
            result.append_outcome(self.backlog.push(batch, now));
        }
        self.update_state();
        result.outcome = self.outcome();
        result
    }
    /// Caller arms one timer at this deadline; there is no clock polling inside the shell.
    pub(crate) fn next_deadline(&self) -> Option<MonoInstant> {
        let batching = self.backlog.oldest_queued_at().map(|at| {
            at.mono
                .checked_add(limits::BATCH_WAIT)
                .expect("validated capture clock has room for batching")
        });
        let confirmation = self
            .backlog
            .batches()
            .flat_map(|batch| &batch.writes)
            .filter_map(|write| match write {
                CaptureWrite::Checkpoint(point) => Some(point.commit_by_mono),
                _ => None,
            })
            .min();
        batching
            .into_iter()
            .chain(confirmation)
            .chain(self.backlog.next_deadline())
            .min()
    }
    pub(crate) fn flush_due(&mut self, store: &mut Store, now: Timestamp) -> PersistenceResult {
        if self.backlog.first_gap().is_some()
            || self.backlog.command_count() >= TRANSACTION_COMMANDS
            || self
                .next_deadline()
                .is_some_and(|deadline| deadline <= now.mono)
        {
            self.flush(store, now)
        } else {
            self.result()
        }
    }
    /// Flushes a command group now, including on shutdown; loss repair always precedes retained batches.
    pub(crate) fn flush(&mut self, store: &mut Store, now: Timestamp) -> PersistenceResult {
        let mut result = self.result();
        result.append_outcome(self.backlog.advance(now));
        let committed = match self
            .flush_gaps(store)
            .and_then(|()| self.flush_batches(store))
        {
            Ok(committed) => committed,
            Err(error) => return self.failed(result, error),
        };
        result
            .feedback
            .extend(committed.into_iter().map(CaptureEvent::Committed));
        self.backlog.failed(BacklogFailure::QueueFull);
        self.state = PersistenceState::Ready;
        self.update_state();
        result.outcome = self.outcome();
        result
    }
    fn flush_gaps(&mut self, store: &mut Store) -> Result<(), ExecuteCaptureBatchError> {
        while let Some(stored) = self.backlog.first_gap() {
            let writes = self.backlog.recovery_writes();
            store.capture_transaction(&mut self.keys, &self.metadata, |capture| {
                capture.execute_writes(stored.run, &writes)
            })?;
            assert!(
                self.backlog.committed_gap(stored.run, stored.gap),
                "only the persisted front gap is acknowledged"
            );
        }
        Ok(())
    }
    fn flush_batches(
        &mut self,
        store: &mut Store,
    ) -> Result<Vec<quartz_domain::capture::BatchId>, ExecuteCaptureBatchError> {
        let mut commands = 0;
        let batches: Vec<_> = self
            .backlog
            .batches()
            .take_while(|batch| {
                if commands > 0 && commands + batch.writes.len() > TRANSACTION_COMMANDS {
                    return false;
                }
                commands += batch.writes.len();
                true
            })
            .collect();
        let committed: Vec<_> = batches.iter().map(|batch| batch.id).collect();
        if !batches.is_empty() {
            let transaction =
                store.capture_transaction(&mut self.keys, &self.metadata, |capture| {
                    for batch in &batches {
                        capture.execute(batch)?;
                    }
                    Ok(())
                });
            transaction?;
        }
        for batch in &committed {
            assert!(
                self.backlog.committed(*batch),
                "only committed prefix batches are removed"
            );
        }
        Ok(committed)
    }
    fn failed(
        &mut self,
        mut result: PersistenceResult,
        error: ExecuteCaptureBatchError,
    ) -> PersistenceResult {
        self.backlog.failed(BacklogFailure::Database);
        self.state = PersistenceState::Failed {
            commands: self.backlog.command_count(),
        };
        result.outcome = PersistenceOutcome::Failed {
            commands: self.backlog.command_count(),
            error,
        };
        result
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
        }
    }
}
