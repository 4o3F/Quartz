//! Core assembly, worker loops, and ordered shutdown through injected platform ports.

pub(crate) mod annotation;
mod capture_loop;
mod database_loop;
mod main_queue;
pub(crate) use main_queue::MainReceiver;
use main_queue::{MainChannel, MainSender};
mod runtime;
#[cfg(test)]
mod tests;
mod timed;
mod workers;
use crate::persistence::PersistenceState;
use crate::rules::registry::{RegistryResult, RunRegistryCommandError};
use crossbeam_channel::{Receiver, Sender, TrySendError};
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
    ClockChanged,
    DayOffsetChanged {
        offset: crate::query::calendar::DayStartOffset,
    },
    IdleAvailability(annotation::IdleAvailability),
    PolicyChanged,
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
    #[snafu(display("main command queue is full"))]
    Full,
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
    sender: MainSender,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl MainPort {
    fn send(&self, command: MainCommand) -> Result<(), SendMainCommandError> {
        self.sender.send(command)?;
        (self.wake)();
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct CaptureLife(Arc<AtomicBool>);
impl CaptureLife {
    pub(crate) fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub(crate) fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Clone)]
pub(crate) struct CaptureSender {
    life: CaptureLife,
    sender: Sender<Observation>,
    overloaded: Arc<AtomicBool>,
}
impl CaptureSender {
    pub(crate) fn stop(&self) {
        self.life.stop();
    }
    pub(crate) fn is_stopped(&self) -> bool {
        self.life.is_stopped()
    }

    pub(crate) fn observe(&self, observation: ObservedEvent) -> Result<(), ObserveCaptureError> {
        self.send(Observation::Event(observation))
    }
    fn admit(&self, observation: ObservedEvent) -> Result<(), quartz_protocol::ProtocolError> {
        if self.is_stopped() {
            return Err(quartz_protocol::ProtocolError::new(
                quartz_protocol::ErrorKind::Busy,
            ));
        }
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
        if self.is_stopped() {
            return Ok(());
        }
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
    life: CaptureLife,
    sender: Sender<Shutdown>,
    finished: Receiver<()>,
}
impl ShutdownPort {
    /// WM_ENDSESSION must wait for final persistence before its callback returns.
    pub(crate) fn close(&self, at: Timestamp) -> Result<(), CloseCaptureError> {
        self.life.stop();
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
        for _ in 0..WORKER_COUNT {
            self.finished
                .recv_deadline(deadline)
                .map_err(|error| match error {
                    crossbeam_channel::RecvTimeoutError::Timeout => CloseCaptureError::Deadline,
                    crossbeam_channel::RecvTimeoutError::Disconnected => {
                        CloseCaptureError::Receive {
                            source: crossbeam_channel::RecvError,
                        }
                    }
                })?;
        }
        Ok(())
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
            tracing::debug!(%error, "could not deliver main notification during worker shutdown");
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
            self.main
                .send_during_shutdown(MainCommand::Fatal(self.worker));
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
