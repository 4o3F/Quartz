//! Database actor; each continuation requires observable flush progress.

use super::{
    COMMIT_HEADROOM, DatabaseCommand, Feedback, MainCommand, MainPort, SendMainCommandError,
};
use crate::{
    capture::{
        FlushProgress, FlushStalledError, Persistence, PersistenceOutcome, PersistenceResult,
        PersistenceState, QueueShutdown,
    },
    core::sources::{ClockReader, ReadClockError},
    rules::registry::{RegistryResult, RegistryShell, RunRegistryCommandError},
    store::Store,
};
use crossbeam_channel::{Receiver, RecvError, RecvTimeoutError, SendError, Sender};
use quartz_domain::{
    capture::{AdvanceTimeError, MonoInstant, Timestamp},
    rules::registry::RegistryCommand,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum RunDatabaseLoopError {
    #[snafu(display("could not read database worker clock"))]
    Clock { source: ReadClockError },
    #[snafu(display("database command channel disconnected"))]
    Receive { source: RecvError },
    #[snafu(display("could not publish database state"))]
    Publish { source: SendMainCommandError },
    #[snafu(display("capture feedback channel disconnected"))]
    Feedback { source: SendError<()> },
    #[snafu(display("{source}"))]
    Stalled { source: FlushStalledError },
    #[snafu(display("could not shut down database worker"))]
    Shutdown { source: Box<ShutdownDatabaseError> },
}
impl From<SendMainCommandError> for RunDatabaseLoopError {
    fn from(source: SendMainCommandError) -> Self {
        Self::Publish { source }
    }
}
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum ShutdownDatabaseError {
    #[snafu(display("database shutdown exceeded its deadline"))]
    Deadline,
    #[snafu(display("could not read shutdown clock"))]
    Clock { source: ReadClockError },
    #[snafu(display("shutdown clock precedes the observed shutdown event"))]
    Time { source: AdvanceTimeError },
    #[snafu(display("could not publish shutdown state"))]
    Publish { source: SendMainCommandError },
    #[snafu(display("database final transaction failed"))]
    Flush {
        source: Box<crate::store::capture::ExecuteCaptureBatchError>,
    },
    #[snafu(display("database retained failed writes at shutdown"))]
    RetainedWrites,
    #[snafu(display("{source}"))]
    Stalled { source: FlushStalledError },
    #[snafu(display("could not acknowledge database shutdown"))]
    Feedback {
        source: crossbeam_channel::SendTimeoutError<()>,
    },
}
impl From<SendMainCommandError> for ShutdownDatabaseError {
    fn from(source: SendMainCommandError) -> Self {
        Self::Publish { source }
    }
}

pub(super) struct DatabaseLoop {
    pub registry: RegistryShell,
    pub persistence: Persistence,
    pub writes: Receiver<DatabaseCommand>,
    pub feedback: Sender<Feedback>,
    pub main: MainPort,
    pub clock: Arc<dyn ClockReader>,
}
enum DatabaseProgress {
    Continue,
    Ended,
}
enum DrainProgress {
    Drained,
    Waiting,
}
impl DatabaseLoop {
    fn now(&self) -> Result<Timestamp, ReadClockError> {
        self.clock.read()
    }
    fn publish(&self, result: PersistenceResult) -> Result<(), RunDatabaseLoopError> {
        if let Some(error) = result.failure() {
            tracing::error!(%error, "capture write failed");
        }
        self.main.send(MainCommand::Persistence(result.state()))?;
        if !result.feedback.is_empty() {
            self.feedback
                .send(Feedback::Events(result.feedback))
                .map_err(|_| RunDatabaseLoopError::Feedback {
                    source: SendError(()),
                })?;
        }
        Ok(())
    }
    pub(super) fn run(&mut self) -> Result<(), RunDatabaseLoopError> {
        loop {
            if let Some(request) = self.persistence.writer().shutdown() {
                return self.shutdown(request);
            }
            if let DrainProgress::Drained = self.drain_ready()? {
                continue;
            }
            let command = self.wait_for_command()?;
            if let DatabaseProgress::Ended = self.handle_command(command)? {
                return Ok(());
            }
        }
    }
    fn drain_ready(&mut self) -> Result<DrainProgress, RunDatabaseLoopError> {
        if !matches!(self.persistence.state(), PersistenceState::Buffered { .. }) {
            return Ok(DrainProgress::Waiting);
        }
        let now = self
            .now()
            .map_err(|source| RunDatabaseLoopError::Clock { source })?;
        if !self.persistence.is_due(now) {
            return Ok(DrainProgress::Waiting);
        }
        let result = self.persistence.flush(self.registry.store_mut(), now);
        Self::ensure_progress(&result)
            .map_err(|source| RunDatabaseLoopError::Stalled { source })?;
        self.publish(result)?;
        Ok(DrainProgress::Drained)
    }
    fn ensure_progress(result: &PersistenceResult) -> Result<(), FlushStalledError> {
        match (result.state(), result.progress) {
            (PersistenceState::Buffered { .. }, FlushProgress::None) => Err(FlushStalledError),
            _ => Ok(()),
        }
    }
    fn wait_for_command(&self) -> Result<Option<DatabaseCommand>, RunDatabaseLoopError> {
        let received = match self.persistence.next_deadline() {
            Some(deadline) => self.writes.recv_timeout(self.wait_duration(deadline)?),
            None => self
                .writes
                .recv()
                .map_err(|_| RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(command) => Ok(Some(command)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(RunDatabaseLoopError::Receive { source: RecvError })
            }
        }
    }
    fn wait_duration(&self, deadline: MonoInstant) -> Result<Duration, RunDatabaseLoopError> {
        let now = self
            .now()
            .map_err(|source| RunDatabaseLoopError::Clock { source })?;
        let remaining = Duration::from_millis(deadline.get().saturating_sub(now.mono.get()));
        Ok(match self.persistence.state() {
            PersistenceState::Failed { .. } => remaining,
            _ => remaining.saturating_sub(COMMIT_HEADROOM),
        })
    }
    fn handle_command(
        &mut self,
        command: Option<DatabaseCommand>,
    ) -> Result<DatabaseProgress, RunDatabaseLoopError> {
        if let Some(request) = self.persistence.writer().shutdown() {
            self.shutdown(request)?;
            return Ok(DatabaseProgress::Ended);
        }
        match command {
            Some(DatabaseCommand::Drain) => {
                let now = self
                    .now()
                    .map_err(|source| RunDatabaseLoopError::Clock { source })?;
                let result = self.persistence.flush_due(self.registry.store_mut(), now);
                self.publish(result)?;
            }
            Some(DatabaseCommand::Registry { command, reply }) => {
                self.registry_command(*command, reply)?
            }
            None => {
                let now = self
                    .now()
                    .map_err(|source| RunDatabaseLoopError::Clock { source })?;
                let result = self.persistence.flush(self.registry.store_mut(), now);
                Self::ensure_progress(&result)
                    .map_err(|source| RunDatabaseLoopError::Stalled { source })?;
                self.publish(result)?;
            }
        }
        Ok(DatabaseProgress::Continue)
    }
    fn registry_command(
        &mut self,
        command: RegistryCommand,
        reply: Sender<Result<RegistryResult, RunRegistryCommandError>>,
    ) -> Result<(), RunDatabaseLoopError> {
        let result = self.registry.run(command);
        if result.as_ref().is_ok_and(|result| result.event.is_some()) {
            self.feedback.send(Feedback::RulesChanged).map_err(|_| {
                RunDatabaseLoopError::Feedback {
                    source: SendError(()),
                }
            })?;
        }
        let _ = reply.send(result);
        Ok(())
    }
    fn shutdown(&mut self, request: QueueShutdown) -> Result<(), RunDatabaseLoopError> {
        self.flush_shutdown(request)
            .map_err(|source| RunDatabaseLoopError::Shutdown {
                source: Box::new(source),
            })
    }
    fn flush_shutdown(&mut self, request: QueueShutdown) -> Result<(), ShutdownDatabaseError> {
        self.flush_shutdown_with(request, Persistence::flush)
    }
    fn flush_shutdown_with(
        &mut self,
        request: QueueShutdown,
        mut flush: impl FnMut(&mut Persistence, &mut Store, Timestamp) -> PersistenceResult,
    ) -> Result<(), ShutdownDatabaseError> {
        loop {
            if Instant::now() >= request.deadline {
                return Err(ShutdownDatabaseError::Deadline);
            }
            let now = self
                .now()
                .map_err(|source| ShutdownDatabaseError::Clock { source })?;
            now.mono
                .duration_since(request.at.mono)
                .map_err(|source| ShutdownDatabaseError::Time { source })?;
            self.registry
                .store_mut()
                .limit_write_wait(request.deadline.saturating_duration_since(Instant::now()));
            let result = flush(&mut self.persistence, self.registry.store_mut(), now);
            Self::ensure_progress(&result)
                .map_err(|source| ShutdownDatabaseError::Stalled { source })?;
            if let DatabaseProgress::Ended = self.settle_shutdown(result, request.deadline)? {
                return Ok(());
            }
        }
    }
    fn settle_shutdown(
        &self,
        result: PersistenceResult,
        deadline: Instant,
    ) -> Result<DatabaseProgress, ShutdownDatabaseError> {
        self.main
            .send_before(MainCommand::Persistence(result.state()), deadline)?;
        if Instant::now() >= deadline {
            return Err(ShutdownDatabaseError::Deadline);
        }
        match result.outcome {
            PersistenceOutcome::Ready => {
                self.feedback
                    .send_deadline(Feedback::Ended, deadline)
                    .map_err(|error| ShutdownDatabaseError::Feedback {
                        source: match error {
                            crossbeam_channel::SendTimeoutError::Timeout(_) => {
                                crossbeam_channel::SendTimeoutError::Timeout(())
                            }
                            crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                                crossbeam_channel::SendTimeoutError::Disconnected(())
                            }
                        },
                    })?;
                Ok(DatabaseProgress::Ended)
            }
            PersistenceOutcome::Buffered { .. } => Ok(DatabaseProgress::Continue),
            PersistenceOutcome::Failed { error, .. } => Err(ShutdownDatabaseError::Flush {
                source: Box::new(error),
            }),
            PersistenceOutcome::RetainedFailure { .. } => {
                Err(ShutdownDatabaseError::RetainedWrites)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::app::{CONTROL_CAPACITY, SHUTDOWN_TIMEOUT},
        data_directory::DataDirectory,
        store::capture::{RunMetadata, WindowsSessionId},
    };
    use crossbeam_channel::bounded;
    use quartz_domain::capture::UtcInstant;

    struct TestClock;
    impl ClockReader for TestClock {
        fn read(&self) -> Result<Timestamp, ReadClockError> {
            Ok(Timestamp {
                utc: UtcInstant::try_from(0).expect("UTC"),
                mono: MonoInstant::from_millis(0),
            })
        }
    }
    struct Fixture {
        database: DatabaseLoop,
        _directory: tempfile::TempDir,
        _commands: Receiver<MainCommand>,
        _feedback: Receiver<Feedback>,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("data directory");
            let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
            let opened = Persistence::open(
                &mut store,
                RunMetadata {
                    device_id: "test-device".into(),
                    win_session_id: WindowsSessionId::new(7),
                    app_version: "test".into(),
                },
            )
            .expect("persistence");
            let (_, writes) = bounded(CONTROL_CAPACITY);
            let (feedback, received_feedback) = bounded(CONTROL_CAPACITY);
            let (sender, commands) = bounded(CONTROL_CAPACITY);
            Self {
                database: DatabaseLoop {
                    registry: RegistryShell::open(store).expect("registry"),
                    persistence: opened.persistence,
                    writes,
                    feedback,
                    main: MainPort {
                        sender,
                        wake: Arc::new(|| {}),
                    },
                    clock: Arc::new(TestClock),
                },
                _directory: directory,
                _commands: commands,
                _feedback: received_feedback,
            }
        }
        fn shutdown_request(&self, deadline: Instant) -> QueueShutdown {
            QueueShutdown {
                at: self.database.now().expect("clock"),
                deadline,
            }
        }
    }
    fn stalled_result() -> PersistenceResult {
        PersistenceResult {
            feedback: Vec::new(),
            effects: Vec::new(),
            outcome: PersistenceOutcome::Buffered { commands: 1 },
            progress: FlushProgress::None,
        }
    }
    #[test]
    fn inconsistent_shutdown_plan_returns_stalled_after_one_attempt() {
        let mut fixture = Fixture::new();
        let request = fixture.shutdown_request(Instant::now() + SHUTDOWN_TIMEOUT);
        let mut attempts = 0;
        let result = fixture.database.flush_shutdown_with(request, |_, _, _| {
            attempts += 1;
            stalled_result()
        });
        assert!(matches!(result, Err(ShutdownDatabaseError::Stalled { .. })));
        assert_eq!(attempts, 1);
    }
    #[test]
    fn shutdown_deadline_prevents_another_flush() {
        let mut fixture = Fixture::new();
        let request = fixture.shutdown_request(Instant::now());
        let result = fixture
            .database
            .flush_shutdown_with(request, |_, _, _| panic!("expired shutdown cannot flush"));
        assert!(matches!(result, Err(ShutdownDatabaseError::Deadline)));
    }
    #[test]
    fn stalled_drain_cannot_continue_the_event_loop() {
        assert!(DatabaseLoop::ensure_progress(&stalled_result()).is_err());
        let mut committed = stalled_result();
        committed.progress = FlushProgress::Committed { commands: 1 };
        assert!(DatabaseLoop::ensure_progress(&committed).is_ok());
    }
}
