//! One current-thread runtime owns all pipe instances and client I/O tasks.

use super::super::handle::OwnedHandle;
use super::{
    CheckPeerError, CreatePipeSecurityError, PipeName, ProcessIdentity, security::PipeSecurity,
    win32_error::Win32ErrorCode,
};
pub(crate) use crate::ipc::connection::ConnectionTimeouts;
use crate::ipc::{ConnectionSession, Services, connection::Connection, registry::RegistryHandle};
use crossbeam_channel::{Receiver, bounded};
use snafu::{ResultExt, Snafu};
use std::{
    io,
    os::windows::io::AsRawHandle,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::{
    io::split,
    net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions},
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use windows::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
    System::{
        Pipes::{DisconnectNamedPipe, GetNamedPipeClientProcessId},
        Threading::GetCurrentProcess,
    },
};

/// Maximum simultaneous connections from 04 §1, including clients awaiting hello.
const PIPE_INSTANCES: usize = 8;
/// Core blocking-pool ceiling from 01 §7.2; the pipe tasks do not use that pool.
const MAX_BLOCKING_THREADS: usize = 2;
/// Bounded non-blocking main-thread control commands (01 §6).
const CONTROL_MAILBOX_CAPACITY: usize = 64;
/// Yield while a bounded shutdown flush completes (01 §6).
const SHUTDOWN_FLUSH_POLL: Duration = Duration::from_millis(10);

enum ServerMessage {
    Control(quartz_protocol::ControlOp),
    #[cfg(test)]
    Inspect {
        reply: crossbeam_channel::Sender<Vec<OutboxMetric>>,
    },
}
#[cfg(test)]
pub(crate) struct OutboxMetric {
    pub(crate) queued: usize,
    pub(crate) peak: usize,
}
/// Start retrying an unavailable pipe slot without affecting capture (04 §1).
const SLOT_RETRY_INITIAL: Duration = Duration::from_millis(25);
/// Cap exponential slot recovery delays while other slots keep serving (04 §1).
const SLOT_RETRY_MAX: Duration = Duration::from_secs(1);

pub(crate) struct PipeServerConfig {
    pub(crate) shutdown_timeout: Duration,
    pub(crate) owner: ProcessIdentity,
    pub(crate) name: PipeName,
    pub(crate) timeouts: ConnectionTimeouts,
    pub(crate) services: Services,
}

impl PipeServerConfig {
    pub(crate) fn current(
        shutdown_timeout: Duration,
        services: Services,
    ) -> Result<Self, StartPipeServerError> {
        let owner = ProcessIdentity::current().context(start_pipe_server_error::IdentitySnafu)?;
        Ok(Self {
            shutdown_timeout,
            name: owner.pipe_name(),
            owner,
            timeouts: ConnectionTimeouts::default(),
            services,
        })
    }
}

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum StartPipeServerError {
    #[snafu(display("could not identify pipe owner"))]
    Identity { source: CheckPeerError },
    #[snafu(display("could not create pipe security"))]
    Security { source: CreatePipeSecurityError },
    #[snafu(display("could not start pipe runtime"))]
    Io { source: io::Error },
    #[snafu(display("pipe name is already owned by another process"))]
    NameOccupied { source: io::Error },
    #[snafu(display("pipe startup did not complete before its deadline"))]
    Startup,
}
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum StopPipeServerError {
    #[snafu(display("pipe shutdown exceeded its deadline"))]
    Deadline,
    #[snafu(display("pipe I/O thread panicked"))]
    Panicked,
}
#[derive(Debug)]
pub(crate) struct PipeServer {
    shutdown_timeout: Duration,
    control: mpsc::Sender<ServerMessage>,
    stop: Option<oneshot::Sender<()>>,
    completed: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}
impl PipeServer {
    pub(crate) fn start(config: PipeServerConfig) -> Result<Self, StartPipeServerError> {
        let (stop, stopping) = oneshot::channel();
        let (control, controls) = mpsc::channel(CONTROL_MAILBOX_CAPACITY);
        let (ready, startup) = bounded(LIFECYCLE_CAPACITY);
        let (finished, completed) = bounded(LIFECYCLE_CAPACITY);
        let io = IoThread {
            listener: Arc::new(PipeListener {
                name: config.name,
                owner: config.owner,
            }),
            timeouts: config.timeouts,
            services: config.services,
            stopping,
            controls,
        };
        let thread = thread::Builder::new()
            .name("quartz-io".into())
            .spawn(move || {
                if let Err(error) = io.run(&ready)
                    && ready.send(Err(error)).is_err()
                {
                    tracing::warn!("pipe startup failed after caller left");
                }
                if finished.send(()).is_err() {
                    tracing::debug!("pipe shutdown caller already left");
                }
            })
            .context(start_pipe_server_error::IoSnafu)?;
        let server = Self {
            shutdown_timeout: config.shutdown_timeout,
            control,
            stop: Some(stop),
            completed,
            thread: Some(thread),
        };
        match startup.recv_timeout(config.shutdown_timeout) {
            Ok(Ok(())) => Ok(server),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(StartPipeServerError::Startup),
        }
    }
    pub(crate) fn control(&self, op: quartz_protocol::ControlOp) {
        if let Err(error) = self.control.try_send(ServerMessage::Control(op)) {
            tracing::warn!(%error, "could not queue main-thread UI control");
        }
    }
    #[cfg(test)]
    pub(crate) fn metrics(&self) -> Vec<OutboxMetric> {
        let (reply, received) = crossbeam_channel::bounded(1);
        self.control
            .try_send(ServerMessage::Inspect { reply })
            .expect("queue metric request");
        received
            .recv_timeout(self.shutdown_timeout)
            .expect("outbox metrics")
    }
    pub(crate) fn shutdown(mut self) -> Result<(), StopPipeServerError> {
        self.stop_and_join()
    }
    fn stop_and_join(&mut self) -> Result<(), StopPipeServerError> {
        if let Some(stop) = self.stop.take()
            && stop.send(()).is_err()
        {
            tracing::debug!("pipe runtime already stopped");
        }
        if self.thread.is_none() {
            return Ok(());
        }
        let deadline = Instant::now() + self.shutdown_timeout;
        match self.completed.recv_deadline(deadline) {
            Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                // The deadline bounds joining; detach rather than waiting again during Drop.
                drop(self.thread.take());
                return Err(StopPipeServerError::Deadline);
            }
        }
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| StopPipeServerError::Panicked)?;
        }
        Ok(())
    }
}
impl Drop for PipeServer {
    fn drop(&mut self) {
        if let Err(error) = self.stop_and_join() {
            tracing::error!(%error, "could not stop pipe I/O thread");
        }
    }
}

/// Startup and completion each communicate exactly once (01 §6 shutdown lifecycle).
const LIFECYCLE_CAPACITY: usize = 1;

#[derive(Debug, Clone, Copy)]
enum InstanceOrder {
    First,
    Additional,
}
struct PipeListener {
    name: PipeName,
    owner: ProcessIdentity,
}
impl PipeListener {
    fn create(&self, order: InstanceOrder) -> Result<NamedPipeServer, StartPipeServerError> {
        let security =
            PipeSecurity::new(self.owner.sid()).context(start_pipe_server_error::SecuritySnafu)?;
        let mut attributes = security.attributes();
        let mut options = ServerOptions::new();
        options
            .pipe_mode(PipeMode::Byte)
            .access_inbound(true)
            .access_outbound(true)
            .max_instances(PIPE_INSTANCES)
            .reject_remote_clients(true)
            .first_pipe_instance(matches!(order, InstanceOrder::First));
        // SAFETY: The attributes and owned security descriptor remain live during this synchronous call.
        let created = unsafe {
            options.create_with_security_attributes_raw(&self.name, (&raw mut attributes).cast())
        };
        created.map_err(|source| {
            use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY};
            let occupied = source.is_win32(ERROR_ACCESS_DENIED) || source.is_win32(ERROR_PIPE_BUSY);
            if matches!(order, InstanceOrder::First) && occupied {
                StartPipeServerError::NameOccupied { source }
            } else {
                StartPipeServerError::Io { source }
            }
        })
    }
}
struct IoThread {
    services: Services,
    listener: Arc<PipeListener>,
    timeouts: ConnectionTimeouts,
    stopping: oneshot::Receiver<()>,
    controls: mpsc::Receiver<ServerMessage>,
}
impl IoThread {
    fn run(
        self,
        ready: &crossbeam_channel::Sender<Result<(), StartPipeServerError>>,
    ) -> Result<(), StartPipeServerError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .max_blocking_threads(MAX_BLOCKING_THREADS)
            .build()
            .context(start_pipe_server_error::IoSnafu)?;
        let _entered = runtime.enter();
        let mut pipes = Vec::with_capacity(PIPE_INSTANCES);
        pipes.push(self.listener.create(InstanceOrder::First)?);
        for _ in 1..PIPE_INSTANCES {
            pipes.push(self.listener.create(InstanceOrder::Additional)?);
        }
        if ready.send(Ok(())).is_err() {
            tracing::warn!("pipe startup caller left before startup completed");
            return Ok(());
        }
        runtime.block_on(self.serve(pipes));
        Ok(())
    }
    fn slot(&self, registry: &RegistryHandle) -> Slot {
        Slot {
            registry: registry.clone(),
            services: self.services.clone(),
            listener: Arc::clone(&self.listener),
            timeouts: self.timeouts,
        }
    }
    async fn serve(mut self, pipes: Vec<NamedPipeServer>) {
        let registry = self.services.start_registry();
        let mut slots = JoinSet::new();
        for pipe in pipes {
            slots.spawn(self.slot(&registry).run(Some(pipe)));
        }
        loop {
            tokio::select! {
                _ = &mut self.stopping => break,
                message = self.controls.recv() => {
                    if let Some(message) = message { Self::server_message(message, &registry).await; }
                }
                failure = slots.join_next() => {
                    tracing::error!(?failure, "pipe slot task ended; restarting slot");
                    slots.spawn(self.slot(&registry).run(None));
                }
            }
        }
        while let Ok(message) = self.controls.try_recv() {
            Self::server_message(message, &registry).await;
        }
        // Barrier processes preceding controls before checking pending bytes; clients are never awaited by the registry.
        match registry.outboxes().await {
            Ok(outboxes) => {
                let deadline = tokio::time::Instant::now() + self.timeouts.close_flush;
                while outboxes.iter().any(|outbox| outbox.queued_bytes() != 0)
                    && tokio::time::Instant::now() < deadline
                {
                    tokio::time::sleep(SHUTDOWN_FLUSH_POLL).await;
                }
                // Let readers consume a successfully flushed shutdown notification before disconnect.
                tokio::time::sleep_until(deadline).await;
            }
            Err(error) => {
                tracing::warn!(%error, "registry unavailable during bounded shutdown flush")
            }
        }
        slots.abort_all();
        while let Some(result) = slots.join_next().await {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                tracing::warn!(%error, "pipe slot failed during shutdown");
            }
        }
    }
    async fn server_message(message: ServerMessage, registry: &RegistryHandle) {
        match message {
            ServerMessage::Control(message) => {
                if let Err(error) = registry.control(message) {
                    tracing::warn!(%error, "control fan-out unavailable");
                }
            }
            #[cfg(test)]
            ServerMessage::Inspect { reply } => match registry.outboxes().await {
                Ok(outboxes) => {
                    let metrics = outboxes
                        .iter()
                        .map(|outbox| OutboxMetric {
                            queued: outbox.queued_bytes(),
                            peak: outbox.peak_bytes(),
                        })
                        .collect();
                    if reply.send(metrics).is_err() {
                        tracing::debug!("test metric caller left");
                    }
                }
                Err(error) => tracing::warn!(%error, "could not inspect outboxes"),
            },
        }
    }
}

#[derive(Debug)]
struct SlotBackoff {
    next: Duration,
}
impl SlotBackoff {
    fn new() -> Self {
        Self {
            next: SLOT_RETRY_INITIAL,
        }
    }
    fn reset(&mut self) {
        self.next = SLOT_RETRY_INITIAL;
    }
    fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_add(self.next).min(SLOT_RETRY_MAX);
        delay
    }
}

struct Slot {
    services: Services,
    registry: RegistryHandle,
    listener: Arc<PipeListener>,
    timeouts: ConnectionTimeouts,
}
impl Slot {
    async fn run(self, mut initial: Option<NamedPipeServer>) {
        let mut backoff = SlotBackoff::new();
        loop {
            let pipe = match initial.take() {
                Some(pipe) => pipe,
                None => match self.listener.create(InstanceOrder::Additional) {
                    Ok(pipe) => pipe,
                    Err(error) => {
                        tracing::error!(%error, "could not recreate pipe slot; retrying");
                        tokio::time::sleep(backoff.next_delay()).await;
                        continue;
                    }
                },
            };
            let disconnect = match PipeDisconnect::new(&pipe) {
                Ok(disconnect) => disconnect,
                Err(error) => {
                    tracing::error!(%error, "could not retain pipe handle for bounded close; retrying");
                    tokio::time::sleep(backoff.next_delay()).await;
                    continue;
                }
            };
            let outcome = self.serve_connection(pipe).await;
            // Disconnect also releases the instance when the client retains its handle.
            if let Err(error) = disconnect.close() {
                use windows::Win32::Foundation::{ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED};
                if error.is_win32(ERROR_PIPE_NOT_CONNECTED) || error.is_win32(ERROR_NO_DATA) {
                    tracing::debug!(%error, "pipe client already disconnected");
                } else {
                    tracing::error!(%error, "could not disconnect pipe slot before replacement");
                }
            }
            drop(disconnect);
            match outcome {
                Ok(()) => backoff.reset(),
                Err(error) => {
                    tracing::error!(%error, "pipe slot failed; retrying");
                    tokio::time::sleep(backoff.next_delay()).await;
                }
            }
        }
    }
    async fn serve_connection(&self, pipe: NamedPipeServer) -> io::Result<()> {
        if let Err(error) = pipe.connect().await {
            use windows::Win32::Foundation::{
                ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
            };
            if [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED]
                .into_iter()
                .any(|code| error.is_win32(code))
            {
                tracing::debug!(%error, "pipe client disconnected before accept completed");
                return Ok(());
            }
            return Err(error);
        }
        let hello_deadline = tokio::time::Instant::now() + self.timeouts.hello;
        let mut pid = 0;
        // SAFETY: The pipe is connected and owned, and pid is writable output storage.
        let peer = unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
            .map_err(|source| CheckPeerError::Windows {
                call: "GetNamedPipeClientProcessId",
                source,
            })
            .and_then(|()| self.listener.owner.check(pid));
        match peer {
            Ok(allowed) => {
                let mut session = ConnectionSession::new(allowed);
                let (read, write) = split(pipe);
                if let Err(error) = (Connection {
                    services: self.services.clone(),
                    registry: self.registry.clone(),
                    timeouts: self.timeouts,
                })
                .run(read, write, &mut session, hello_deadline)
                .await
                {
                    tracing::debug!(pid, %error, "pipe connection closed");
                }
            }
            Err(error) => tracing::warn!(pid, %error, "rejected pipe client"),
        }
        Ok(())
    }
}

/// Keeps a duplicate server handle alive so bounded close can disconnect split I/O halves.
struct PipeDisconnect {
    handle: std::fs::File,
}
impl PipeDisconnect {
    fn new(pipe: &NamedPipeServer) -> io::Result<Self> {
        let mut duplicate = HANDLE::default();
        // SAFETY: The live pipe handle belongs to this process; duplicate is writable output storage.
        unsafe {
            let process = GetCurrentProcess();
            DuplicateHandle(
                process,
                HANDLE(pipe.as_raw_handle()),
                process,
                &mut duplicate,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .map_err(io::Error::from)?;
        // SAFETY: DuplicateHandle transferred unique ownership of a valid kernel handle.
        let handle = unsafe { OwnedHandle::from_raw(duplicate) }.into_file();
        Ok(Self { handle })
    }
    fn close(&self) -> io::Result<()> {
        // SAFETY: The duplicate keeps this server pipe instance alive until after disconnection.
        unsafe { DisconnectNamedPipe(HANDLE(self.handle.as_raw_handle())) }.map_err(io::Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::StatusPublisher;

    /// Bounds lifecycle unit tests independently of production connection deadlines.
    const TEST_CLOSE_TIMEOUT: Duration = Duration::from_millis(20);

    struct TestSlot {
        slot: Slot,
        _queries: crossbeam_channel::Receiver<crate::query::dispatch::QueryCommand>,
        _database: crate::query::dispatch::test_support::TestDatabase,
    }
    fn slot() -> TestSlot {
        let suffix = windows::core::GUID::new().expect("test pipe suffix");
        let publisher = StatusPublisher::new(quartz_protocol::InstanceId(format!("{suffix:?}")));
        let channel = crate::query::dispatch::QueryChannel::new();
        let database = crate::query::dispatch::test_support::TestDatabase::start();
        let received = database.versions.clone();
        TestSlot {
            _queries: channel.receiver,
            _database: database,
            slot: Slot {
                registry: RegistryHandle::start(publisher.subscribe(), received.clone()),
                services: Services {
                    timed: crate::ipc::TimedPolicyPort::unavailable(),
                    queries: channel.port,
                    writes: crate::ipc::WriteChannel::new().port,
                    versions: received,
                    status: publisher.subscribe(),
                    ui: Arc::new(|_| Ok(())),
                },
                listener: Arc::new(PipeListener {
                    name: PipeName::for_test(suffix),
                    owner: ProcessIdentity::current().expect("test identity"),
                }),
                timeouts: ConnectionTimeouts {
                    hello: ConnectionTimeouts::default().hello,
                    close_flush: TEST_CLOSE_TIMEOUT,
                },
            },
        }
    }
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("test runtime")
    }

    #[test]
    fn failed_slot_creation_retries_and_recovers_after_capacity_frees() {
        runtime().block_on(async {
            let slot = slot();
            let listener = Arc::clone(&slot.slot.listener);
            let mut occupied = Vec::new();
            occupied.push(listener.create(InstanceOrder::First).expect("first instance"));
            for _ in 1..PIPE_INSTANCES { occupied.push(listener.create(InstanceOrder::Additional).expect("occupy instance")); }
            let task = tokio::spawn(slot.slot.run(None));
            tokio::time::sleep(SLOT_RETRY_INITIAL.saturating_add(SLOT_RETRY_INITIAL)).await;
            assert!(!task.is_finished(), "creation failure must keep retrying");
            drop(occupied.pop());
            tokio::time::sleep(SLOT_RETRY_MAX).await;
            let error = listener.create(InstanceOrder::Additional).expect_err("recovered slot occupies the freed capacity");
            assert!(matches!(error, StartPipeServerError::Io { source } if source.is_win32(windows::Win32::Foundation::ERROR_PIPE_BUSY)));
            assert!(!task.is_finished());
            task.abort();
            assert!(task.await.expect_err("cancel test slot").is_cancelled());
        });
    }

    #[test]
    fn slot_backoff_caps_and_resets_after_recovery() {
        let mut backoff = SlotBackoff::new();
        assert_eq!(backoff.next_delay(), SLOT_RETRY_INITIAL);
        let mut previous = SLOT_RETRY_INITIAL;
        while previous < SLOT_RETRY_MAX {
            let next = backoff.next_delay();
            assert!(next > previous && next <= SLOT_RETRY_MAX);
            previous = next;
        }
        assert_eq!(backoff.next_delay(), SLOT_RETRY_MAX);
        backoff.reset();
        assert_eq!(backoff.next_delay(), SLOT_RETRY_INITIAL);
    }
}
