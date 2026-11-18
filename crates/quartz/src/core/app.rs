//! Worker loops. Only the main-thread notification closure is platform-specific.

mod capture_loop;
mod database_loop;
mod workers;
use crate::capture::PersistenceState;
use crate::rules::registry::{RegistryResult, RunRegistryCommandError};
use crossbeam_channel::{Receiver, Sender, TrySendError, select_biased};
use quartz_domain::{
    capture::{
        CaptureEffect, CaptureEvent, CaptureState, RunEndReason, SessionReadiness, Timestamp,
    },
    rules::registry::RegistryCommand,
};
use snafu::Snafu;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
pub(crate) use workers::Workers;
#[cfg(windows)]
pub(crate) use workers::{JoinWorkersError, RequestShutdownError, StartWorkersError};

/// Bounds final database settlement and worker joining, including SQLite's busy timeout.
const WORKER_COUNT: usize = 2;
const CONTROL_CAPACITY: usize = 1;
pub(crate) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(8);
/// Begin a pending transaction before the one-second commit limit (03 §4.2).
pub(super) const COMMIT_HEADROOM: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) struct ObservedEvent {
    pub at: Timestamp,
    pub event: CaptureEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureIndicator {
    Normal,
    PersistenceFailed,
    SessionUnavailable,
    CaptureGap,
}
impl CaptureIndicator {
    pub(crate) fn from_state(
        capture: Option<&CaptureState>,
        persistence: PersistenceState,
    ) -> Self {
        match (capture, persistence) {
            (_, PersistenceState::Failed { .. }) => Self::PersistenceFailed,
            (
                Some(CaptureState::Stopped {
                    session: SessionReadiness::AwaitingSession,
                    ..
                }),
                _,
            ) => Self::SessionUnavailable,
            (Some(CaptureState::Gap(_)), _) => Self::CaptureGap,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug)]
pub(crate) enum MainCommand {
    Effect(CaptureEffect),
    Persistence(PersistenceState),
    Fatal(Worker),
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum Worker {
    Capture,
    Database,
}
#[derive(Debug)]
enum DatabaseCommand {
    Drain,
    #[expect(dead_code, reason = "the registry port is connected to IPC in M2")]
    Registry {
        command: Box<RegistryCommand>,
        reply: Sender<Result<RegistryResult, RunRegistryCommandError>>,
    },
}
#[derive(Debug)]
enum Feedback {
    Events(Vec<CaptureEvent>),
    RulesChanged,
    Ended,
}
#[derive(Debug)]
struct Shutdown {
    at: Timestamp,
    reason: RunEndReason,
    deadline: Instant,
}
impl Shutdown {
    fn new(at: Timestamp, reason: RunEndReason) -> Self {
        Self {
            at,
            reason,
            deadline: Instant::now() + SHUTDOWN_TIMEOUT,
        }
    }
}
impl std::fmt::Display for Worker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Capture => "capture",
            Self::Database => "database",
        })
    }
}

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum SendMainCommandError {
    #[snafu(display("main command delivery exceeded its deadline"))]
    Deadline {
        source: crossbeam_channel::SendTimeoutError<()>,
    },
    #[snafu(display("main command receiver disconnected"))]
    Disconnected {
        source: crossbeam_channel::SendError<()>,
    },
}
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum ObserveCaptureError {
    #[snafu(display("capture event receiver disconnected"))]
    Disconnected {
        source: crossbeam_channel::TrySendError<()>,
    },
}
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum CloseCaptureError {
    #[snafu(display("could not request capture shutdown"))]
    Send {
        source: crossbeam_channel::SendTimeoutError<()>,
    },
    #[snafu(display("worker completion channel disconnected"))]
    Receive {
        source: crossbeam_channel::RecvError,
    },
    #[snafu(display("capture shutdown exceeded its deadline"))]
    Deadline,
}
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum WorkerFailure {
    #[snafu(display("capture worker failed: {source}"))]
    Capture {
        source: Box<capture_loop::RunCaptureLoopError>,
    },
    #[snafu(display("database worker failed: {source}"))]
    Database {
        source: Box<database_loop::RunDatabaseLoopError>,
    },
}

#[derive(Clone)]
pub(crate) struct MainPort {
    sender: Sender<MainCommand>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl MainPort {
    fn send_before(
        &self,
        command: MainCommand,
        deadline: Instant,
    ) -> Result<(), SendMainCommandError> {
        self.sender
            .send_deadline(command, deadline)
            .map_err(|error| match error {
                crossbeam_channel::SendTimeoutError::Timeout(_) => SendMainCommandError::Deadline {
                    source: crossbeam_channel::SendTimeoutError::Timeout(()),
                },
                crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                    SendMainCommandError::Disconnected {
                        source: crossbeam_channel::SendError(()),
                    }
                }
            })?;
        (self.wake)();
        Ok(())
    }
    fn send(&self, command: MainCommand) -> Result<(), SendMainCommandError> {
        self.sender
            .send(command)
            .map_err(|_| SendMainCommandError::Disconnected {
                source: crossbeam_channel::SendError(()),
            })?;
        (self.wake)();
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct CaptureSender {
    sender: Sender<ObservedEvent>,
    overloaded: Arc<AtomicBool>,
}
impl CaptureSender {
    pub(crate) fn observe(&self, observation: ObservedEvent) -> Result<(), ObserveCaptureError> {
        match self.sender.try_send(observation) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.overloaded.store(true, Ordering::Release);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(ObserveCaptureError::Disconnected {
                source: crossbeam_channel::TrySendError::Disconnected(()),
            }),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ShutdownPort {
    sender: Sender<Shutdown>,
    commands: Receiver<MainCommand>,
    finished: Receiver<()>,
}
impl ShutdownPort {
    /// WM_ENDSESSION must wait for final persistence before its callback returns.
    pub(crate) fn close(&self, at: Timestamp) -> Result<(), CloseCaptureError> {
        let stop = Shutdown::new(at, RunEndReason::Shutdown);
        let deadline = stop.deadline;
        self.sender
            .send_deadline(stop, deadline)
            .map_err(|error| CloseCaptureError::Send {
                source: match error {
                    crossbeam_channel::SendTimeoutError::Timeout(_) => {
                        crossbeam_channel::SendTimeoutError::Timeout(())
                    }
                    crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                        crossbeam_channel::SendTimeoutError::Disconnected(())
                    }
                },
            })?;
        let mut commands = self.commands.clone();
        for _ in 0..WORKER_COUNT {
            self.wait_finished(&mut commands, deadline)?;
        }
        Ok(())
    }
    fn wait_finished(
        &self,
        commands: &mut Receiver<MainCommand>,
        deadline: Instant,
    ) -> Result<(), CloseCaptureError> {
        loop {
            if Instant::now() >= deadline {
                return Err(CloseCaptureError::Deadline);
            }
            select_biased! {
                recv(self.finished) -> result => return result.map_err(|source| CloseCaptureError::Receive { source }),
                recv(commands) -> result => { if result.is_err() { *commands = crossbeam_channel::never(); } },
                default(deadline.saturating_duration_since(Instant::now())) => return Err(CloseCaptureError::Deadline),
            }
        }
    }
}

struct WorkerGuard {
    worker: Worker,
    main: MainPort,
}
impl WorkerGuard {
    fn run(
        worker: Worker,
        main: MainPort,
        completed: Sender<Result<(), WorkerFailure>>,
        finished: Sender<()>,
        run: impl FnOnce() -> Result<(), WorkerFailure>,
    ) {
        let guard = Self { worker, main };
        tracing::info!(?worker, "worker started");
        let result = run();
        if let Err(error) = &result {
            tracing::error!(?worker, %error, "worker failed");
            let _ = guard.main.send(MainCommand::Fatal(worker));
        }
        tracing::info!(?worker, "worker stopped");
        let _ = completed.send(result);
        let _ = finished.send(());
    }
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if thread::panicking() {
            tracing::error!(worker = ?self.worker, "worker panicked; stopping core");
            // A full main queue cannot conceal a fatal worker failure.
            if self
                .main
                .sender
                .try_send(MainCommand::Fatal(self.worker))
                .is_err()
            {
                std::process::exit(1);
            }
            (self.main.wake)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{PersistenceState, WriteQueue};
    use capture_loop::{CaptureLoop, Settlement};
    use crossbeam_channel::bounded;
    use quartz_domain::capture::*;
    use quartz_domain::rules::registry::{RegistryState, RuleRegistry};

    #[test]
    fn persistence_failure_selects_warning_indicator() {
        assert_eq!(
            CaptureIndicator::from_state(None, PersistenceState::Failed { commands: 1 }),
            CaptureIndicator::PersistenceFailed
        );
        assert_eq!(
            CaptureIndicator::from_state(None, PersistenceState::Ready),
            CaptureIndicator::Normal
        );
    }
    #[test]
    fn session_and_gap_warnings_keep_distinct_indicators() {
        let unavailable = CaptureState::Stopped {
            reasons: Default::default(),
            session: SessionReadiness::AwaitingSession,
        };
        assert_eq!(
            CaptureIndicator::from_state(Some(&unavailable), PersistenceState::Ready),
            CaptureIndicator::SessionUnavailable
        );
        let gap = CaptureState::Gap(GapReason::EventOverload);
        assert_eq!(
            CaptureIndicator::from_state(Some(&gap), PersistenceState::Ready),
            CaptureIndicator::CaptureGap
        );
        assert_eq!(
            CaptureIndicator::from_state(Some(&gap), PersistenceState::Failed { commands: 1 }),
            CaptureIndicator::PersistenceFailed
        );
    }
    #[test]
    fn resume_notifications_create_only_one_clock_segment() {
        let at = Timestamp {
            utc: UtcInstant::try_from(0).expect("UTC"),
            mono: MonoInstant::from_millis(0),
        };
        let started = CaptureCore::start(RunId::new(1), at).expect("core");
        let (_sender, observed) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let (_feedback, replies) = bounded(limits::WRITE_QUEUE_CAPACITY);
        let (_shutdown, stop) = bounded(CONTROL_CAPACITY);
        let (database, _writes) = bounded(CONTROL_CAPACITY);
        let (main, _commands) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let snapshots = RuleRegistry::from_state(RegistryState::default())
            .expect("rules")
            .snapshots();
        let mut capture = CaptureLoop {
            core: started.core,
            run: RunId::new(1),
            at,
            snapshots: snapshots.clone(),
            observed,
            replies,
            stop,
            database,
            writer: WriteQueue::new(),
            main: MainPort {
                sender: main,
                wake: Arc::new(|| {}),
            },
            overloaded: Arc::new(AtomicBool::new(false)),
        };
        for event in [
            CaptureEvent::Session(SessionStatus::Ready(Default::default())),
            CaptureEvent::Stop(StopChange::Begin(StopReason::Suspended)),
            CaptureEvent::ClockChanged(ClockChange::Resume),
            CaptureEvent::Stop(StopChange::End(StopReason::Suspended)),
        ] {
            capture
                .step(ObservedEvent { at, event }, Settlement::Deferred)
                .expect("power observation");
        }
        let ended = capture
            .core
            .step(CaptureInput {
                at,
                rules: snapshots.load(),
                event: CaptureEvent::Shutdown,
            })
            .expect("final segment");
        assert!(ended.writes.iter().any(|write| matches!(write, CaptureWrite::CloseClockSegment { segment, .. } if segment.get() == 2)));
    }
}
