//! Startup ownership transfer and bounded worker completion.

use super::{
    CONTROL_CAPACITY, CaptureSender, DatabaseCommand, Feedback, MainCommand, MainPort, Observation,
    SHUTDOWN_TIMEOUT, Shutdown, ShutdownPort, WORKER_COUNT, Worker, WorkerFailure, WorkerGuard,
};
use super::{capture_loop::CaptureLoop, database_loop::DatabaseLoop};
use crate::{capture::OpenedPersistence, store::capture::CaptureStartup};
use crate::{
    capture::Persistence,
    core::sources::{ClockReader, ReadClockError},
    rules::registry::{OpenRegistryShellError, RegistryShell},
    store::{
        Store,
        capture::{RecoverCaptureError, RunMetadata},
    },
};
use crossbeam_channel::{Receiver, Sender, bounded, select_biased};
use quartz_domain::capture::{
    CaptureCore, CaptureStep, RunEndReason, StartCaptureError, StartedCapture, Timestamp, limits,
};
use std::{
    sync::{Arc, atomic::AtomicBool},
    thread::{self, JoinHandle},
    time::Instant,
};

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum StartWorkersError {
    #[snafu(display("could not recover capture"))]
    Recover { source: RecoverCaptureError },
    #[snafu(display("could not open registry"))]
    Registry { source: OpenRegistryShellError },
    #[snafu(display("could not initialize query versions"))]
    Query { source: crate::query::RunQueryError },
    #[snafu(display("could not read capture startup clock"))]
    Clock { source: ReadClockError },
    #[snafu(display("could not start capture core"))]
    Capture { source: StartCaptureError },
    #[snafu(display("could not spawn worker thread"))]
    Spawn { source: std::io::Error },
}
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum RequestShutdownError {
    #[snafu(display("could not queue capture shutdown"))]
    Send {
        source: crossbeam_channel::TrySendError<()>,
    },
}
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum JoinWorkersError {
    #[snafu(display("worker completion channel disconnected"))]
    Receive {
        source: crossbeam_channel::RecvError,
    },
    #[snafu(display("worker shutdown exceeded its deadline"))]
    Deadline,
    #[snafu(display("worker panicked"))]
    Panicked,
    #[snafu(display("{source}"))]
    Failed { source: Box<WorkerFailure> },
}

pub(crate) struct Workers {
    pub sender: CaptureSender,
    pub commands: Receiver<MainCommand>,
    pub queries: crate::query::dispatch::QueryPort,
    pub versions: tokio::sync::watch::Receiver<crate::query::versions::VersionState>,
    shutdown: Sender<Shutdown>,
    finished: Receiver<()>,
    completed: Receiver<Result<(), WorkerFailure>>,
    handles: Vec<JoinHandle<()>>,
}
struct Channel<T> {
    sender: Sender<T>,
    receiver: Receiver<T>,
}
impl<T> Channel<T> {
    fn bounded(capacity: usize) -> Self {
        let (sender, receiver) = bounded(capacity);
        Self { sender, receiver }
    }
}
struct WorkerChannels {
    observations: Channel<Observation>,
    database: Channel<DatabaseCommand>,
    feedback: Channel<Feedback>,
    main: Channel<MainCommand>,
    shutdown: Channel<Shutdown>,
    completed: Channel<Result<(), WorkerFailure>>,
    finished: Channel<()>,
    overloaded: Arc<AtomicBool>,
}
impl WorkerChannels {
    fn new() -> Self {
        Self {
            observations: Channel::bounded(limits::MAIN_QUEUE_CAPACITY),
            database: Channel::bounded(CONTROL_CAPACITY),
            feedback: Channel::bounded(limits::WRITE_QUEUE_CAPACITY),
            main: Channel::bounded(limits::MAIN_QUEUE_CAPACITY),
            shutdown: Channel::bounded(CONTROL_CAPACITY),
            completed: Channel::bounded(WORKER_COUNT),
            finished: Channel::bounded(WORKER_COUNT),
            overloaded: Arc::new(AtomicBool::new(false)),
        }
    }
    fn spawn_database(
        &self,
        mut database: DatabaseLoop,
        main: MainPort,
    ) -> Result<JoinHandle<()>, StartWorkersError> {
        self.spawn(Worker::Database, main, move || {
            database.run().map_err(|source| WorkerFailure::Database {
                source: Box::new(source),
            })
        })
    }
    fn spawn_capture(
        &self,
        mut capture: CaptureLoop,
        step: CaptureStep,
        startup: CaptureStartup,
        main: MainPort,
    ) -> Result<JoinHandle<()>, StartWorkersError> {
        self.spawn(Worker::Capture, main, move || {
            capture
                .initialize(step, startup)
                .and_then(|()| capture.run())
                .map_err(|source| WorkerFailure::Capture {
                    source: Box::new(source),
                })
        })
    }
    fn spawn(
        &self,
        worker: Worker,
        main: MainPort,
        run: impl FnOnce() -> Result<(), WorkerFailure> + Send + 'static,
    ) -> Result<JoinHandle<()>, StartWorkersError> {
        let completed = self.completed.sender.clone();
        let finished = self.finished.sender.clone();
        let name = match worker {
            Worker::Capture => "quartz-capture",
            Worker::Database => "quartz-db",
        };
        thread::Builder::new()
            .name(name.into())
            .spawn(move || WorkerGuard::run(worker, main, completed, finished, run))
            .map_err(|source| StartWorkersError::Spawn { source })
    }
}
struct Startup {
    persistence: Persistence,
    startup: CaptureStartup,
    registry: RegistryShell,
    started: StartedCapture,
    at: Timestamp,
    clock: Arc<dyn ClockReader>,
}
impl Startup {
    fn open(
        mut store: Store,
        metadata: RunMetadata,
        clock: Arc<dyn ClockReader>,
    ) -> Result<Self, StartWorkersError> {
        let OpenedPersistence {
            persistence,
            startup,
        } = Persistence::open(&mut store, metadata)
            .map_err(|source| StartWorkersError::Recover { source })?;
        let registry =
            RegistryShell::open(store).map_err(|source| StartWorkersError::Registry { source })?;
        tracing::debug!(previous_segment = ?startup.last_segment, "capture recovery completed");
        let at = clock
            .read()
            .map_err(|source| StartWorkersError::Clock { source })?;
        let started = CaptureCore::start(startup.next_run, at)
            .map_err(|source| StartWorkersError::Capture { source })?;
        Ok(Self {
            persistence,
            startup,
            registry,
            started,
            at,
            clock,
        })
    }
}
impl Workers {
    pub(crate) fn start(
        store: Store,
        metadata: RunMetadata,
        clock: Arc<dyn ClockReader>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, StartWorkersError> {
        let startup = Startup::open(store, metadata, clock)?;
        Self::launch(startup, wake)
    }
    fn launch(
        startup: Startup,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, StartWorkersError> {
        let channels = WorkerChannels::new();
        let main = MainPort {
            sender: channels.main.sender.clone(),
            wake,
        };
        let Startup {
            persistence,
            startup,
            mut registry,
            started,
            at,
            clock,
        } = startup;
        let queries = crate::query::dispatch::QueryChannel::new();
        let hash = registry
            .snapshots()
            .load()
            .hashes()
            .classification
            .to_string();
        let version_state = registry
            .store_mut()
            .version_state(&hash)
            .map_err(|source| StartWorkersError::Query { source })?;
        let (versions, version_updates) = tokio::sync::watch::channel(version_state);
        let capture = CaptureLoop {
            #[cfg(windows)]
            foreground: crate::platform::ForegroundResolver::new(),
            core: started.core,
            run: startup.next_run,
            at,
            snapshots: registry.snapshots(),
            observed: channels.observations.receiver.clone(),
            replies: channels.feedback.receiver.clone(),
            stop: channels.shutdown.receiver.clone(),
            database: channels.database.sender.clone(),
            writer: persistence.writer(),
            main: main.clone(),
            overloaded: Arc::clone(&channels.overloaded),
            clock: Arc::clone(&clock),
        };
        let database = DatabaseLoop {
            persistence,
            registry,
            queries: queries.receiver,
            versions,
            writes: channels.database.receiver.clone(),
            feedback: channels.feedback.sender.clone(),
            main: main.clone(),
            clock,
        };
        let db = channels.spawn_database(database, main.clone())?;
        let capture = channels.spawn_capture(capture, started.step, startup, main)?;
        Ok(Self {
            sender: CaptureSender {
                sender: channels.observations.sender,
                overloaded: channels.overloaded,
            },
            commands: channels.main.receiver,
            queries: queries.port,
            versions: version_updates,
            shutdown: channels.shutdown.sender,
            finished: channels.finished.receiver,
            completed: channels.completed.receiver,
            handles: vec![capture, db],
        })
    }
    pub(crate) fn shutdown_port(&self) -> ShutdownPort {
        ShutdownPort {
            sender: self.shutdown.clone(),
            commands: self.commands.clone(),
            finished: self.finished.clone(),
        }
    }
    pub(crate) fn request_shutdown(
        &self,
        at: Timestamp,
        reason: RunEndReason,
    ) -> Result<(), RequestShutdownError> {
        self.shutdown
            .try_send(Shutdown::new(at, reason))
            .map_err(|error| RequestShutdownError::Send {
                source: match error {
                    crossbeam_channel::TrySendError::Full(_) => {
                        crossbeam_channel::TrySendError::Full(())
                    }
                    crossbeam_channel::TrySendError::Disconnected(_) => {
                        crossbeam_channel::TrySendError::Disconnected(())
                    }
                },
            })
    }
    pub(crate) fn join(mut self) -> Result<(), JoinWorkersError> {
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        let mut commands = self.commands.clone();
        let mut outcome = Ok(());
        for _ in &self.handles {
            if let Err(error) = self.wait_completion(&mut commands, deadline)? {
                outcome = Err(JoinWorkersError::Failed {
                    source: Box::new(error),
                });
            }
        }
        for handle in self.handles.drain(..) {
            handle.join().map_err(|_| JoinWorkersError::Panicked)?;
        }
        outcome
    }
    fn wait_completion(
        &self,
        commands: &mut Receiver<MainCommand>,
        deadline: Instant,
    ) -> Result<Result<(), WorkerFailure>, JoinWorkersError> {
        loop {
            if Instant::now() >= deadline {
                return Err(JoinWorkersError::Deadline);
            }
            select_biased! {
                recv(self.completed) -> result => return result.map_err(|source| JoinWorkersError::Receive { source }),
                recv(commands) -> result => { if result.is_err() { *commands = crossbeam_channel::never(); } },
                default(deadline.saturating_duration_since(Instant::now())) => return Err(JoinWorkersError::Deadline),
            }
        }
    }
}
