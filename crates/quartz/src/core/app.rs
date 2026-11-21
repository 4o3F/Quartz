//! Core assembly, worker loops, and ordered shutdown through injected platform ports.

mod capture_loop;
mod database_loop;
mod runtime;
#[cfg(test)]
mod tests;
mod timed;
mod workers;
use crate::persistence::PersistenceState;
use crate::rules::registry::{RegistryResult, RunRegistryCommandError};
use crossbeam_channel::{Receiver, Sender, TrySendError, select_biased};
use quartz_domain::{
    capture::{CaptureEffect, CaptureEvent, CaptureState, RunEndReason, Timestamp},
    rules::registry::RegistryCommand,
};
pub(crate) use runtime::{
    CallPlatformPortError, CorePipe, CoreRuntime, PipePorts, PlatformPorts, RuntimeExit,
    StartCoreError, StopCoreError,
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
pub(crate) use workers::{JoinWorkersError, RequestShutdownError, StartWorkersError};

/// Bounds final database settlement and worker joining, including SQLite's busy timeout.
const WORKER_COUNT: usize = 2;
const CONTROL_CAPACITY: usize = 1;
/// Bounds database and capture notifications waiting for the main thread.
const MAIN_NOTIFICATION_CAPACITY: usize = 1024;
pub(crate) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(8);
/// Begin a pending transaction before the one-second commit limit (03 §4.2).
pub(super) const COMMIT_HEADROOM: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) struct ObservedEvent {
    pub at: Timestamp,
    pub event: CaptureEvent,
}

#[derive(Debug)]
pub(crate) enum Observation {
    Event(ObservedEvent),
    Foreground(crate::core::foreground::Snapshot),
}
impl From<ObservedEvent> for Observation {
    fn from(event: ObservedEvent) -> Self {
        Self::Event(event)
    }
}

#[derive(Debug)]
pub(crate) enum MainCommand {
    CaptureSnapshot {
        state: CaptureState,
        timed: quartz_protocol::TimedPolicies,
        foreground: Option<quartz_domain::capture::ForegroundView>,
    },
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
    #[expect(dead_code, reason = "M4 rule editing over IPC")]
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
    at: ShutdownTime,
    reason: RunEndReason,
    deadline: Instant,
}
#[derive(Debug)]
enum ShutdownTime {
    Sampled(Timestamp),
    LastObservation,
}
impl Shutdown {
    fn new(at: Timestamp, reason: RunEndReason) -> Self {
        Self {
            at: ShutdownTime::Sampled(at),
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
    Deadline,
    #[snafu(display("main command receiver disconnected"))]
    Disconnected,
}
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum ObserveCaptureError {
    #[snafu(display("capture event receiver disconnected"))]
    Disconnected,
}
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum CloseCaptureError {
    #[snafu(display("could not request capture shutdown"))]
    Disconnected,
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
                crossbeam_channel::SendTimeoutError::Timeout(_) => SendMainCommandError::Deadline,
                crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                    SendMainCommandError::Disconnected
                }
            })?;
        (self.wake)();
        Ok(())
    }
    fn send(&self, command: MainCommand) -> Result<(), SendMainCommandError> {
        self.sender
            .send(command)
            .map_err(|_| SendMainCommandError::Disconnected)?;
        (self.wake)();
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct CaptureSender {
    sender: Sender<Observation>,
    overloaded: Arc<AtomicBool>,
}
impl CaptureSender {
    pub(crate) fn observe(&self, observation: ObservedEvent) -> Result<(), ObserveCaptureError> {
        self.send(Observation::Event(observation))
    }
    fn admit(&self, observation: ObservedEvent) -> Result<(), quartz_protocol::ProtocolError> {
        self.sender
            .try_send(Observation::Event(observation))
            .map_err(|error| {
                quartz_protocol::ProtocolError::new(match error {
                    TrySendError::Full(_) => quartz_protocol::ErrorKind::Busy,
                    TrySendError::Disconnected(_) => quartz_protocol::ErrorKind::Internal,
                })
            })
    }
    pub(crate) fn mark_overloaded(&self) {
        self.overloaded.store(true, Ordering::Release);
    }
    pub(crate) fn foreground(
        &self,
        snapshot: crate::core::foreground::Snapshot,
    ) -> Result<(), ObserveCaptureError> {
        self.send(Observation::Foreground(snapshot))
    }
    fn send(&self, observation: Observation) -> Result<(), ObserveCaptureError> {
        match self.sender.try_send(observation) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.overloaded.store(true, Ordering::Release);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(ObserveCaptureError::Disconnected),
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
            .map_err(|error| match error {
                crossbeam_channel::SendTimeoutError::Timeout(_) => CloseCaptureError::Deadline,
                crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                    CloseCaptureError::Disconnected
                }
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

/// Completion and reply receivers may be dropped while shutdown joins the workers.
trait SendDuringShutdown<T> {
    fn send_during_shutdown(&self, value: T);
}
impl<T> SendDuringShutdown<T> for Sender<T> {
    fn send_during_shutdown(&self, value: T) {
        if self.send(value).is_err() {
            tracing::debug!("receiver disconnected during worker shutdown");
        }
    }
}
impl SendDuringShutdown<MainCommand> for MainPort {
    fn send_during_shutdown(&self, command: MainCommand) {
        if let Err(error) = self.send(command) {
            tracing::debug!(%error, "main receiver disconnected during worker shutdown");
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
            guard.main.send_during_shutdown(MainCommand::Fatal(worker));
        }
        tracing::info!(?worker, "worker stopped");
        completed.send_during_shutdown(result);
        finished.send_during_shutdown(());
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

/// Controls whether duplicate core launches forward an open-UI request.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Autostart {
    Enabled,
    Disabled,
}
impl From<bool> for Autostart {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}
