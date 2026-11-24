//! One current-thread runtime owns all pipe instances and client I/O tasks.

use super::{
    CheckPeerError, CreatePipeSecurityError, PipeName, ProcessIdentity, security::PipeSecurity,
    win32_error::Win32ErrorCode,
};
pub(crate) use crate::ipc::connection::ConnectionTimeouts;
use crate::ipc::{ConnectionSession, Services, connection::Connection, registry::RegistryHandle};
use crossbeam_channel::{Receiver, bounded};
use snafu::{Report, ResultExt, Snafu};
use std::{
    io,
    os::windows::io::AsRawHandle,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions},
    sync::{Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
};
use windows::Win32::{Foundation::HANDLE, System::Pipes::GetNamedPipeClientProcessId};

/// Maximum simultaneous connections from 04 §1, including clients awaiting hello.
const PIPE_INSTANCES: usize = 8;
/// Core blocking-pool ceiling from 01 §7.2, shared by authentication and rule reads.
const MAX_BLOCKING_THREADS: usize = 2;
/// Reserve one blocking worker for authenticated rule reads (01 §7.2).
const RULE_READ_BLOCKING_RESERVE: usize = 1;
const AUTHENTICATION_PARALLELISM: usize = MAX_BLOCKING_THREADS - RULE_READ_BLOCKING_RESERVE;
/// Bounded non-blocking main-thread control commands (01 §6).
const CONTROL_MAILBOX_CAPACITY: usize = 64;
/// Yield while a bounded shutdown flush completes (01 §6).
const SHUTDOWN_FLUSH_POLL: Duration = Duration::from_millis(10);

enum ServerMessage {
    Control(quartz_protocol::ControlOp),
    TargetControl {
        connection: crate::ipc::registry::ConnectionId,
        op: quartz_protocol::ControlOp,
    },
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
    #[snafu(display("could not create pipe instance"))]
    CreateInstance { source: CreatePipeInstanceError },
    #[snafu(display("could not start pipe runtime"))]
    Io { source: io::Error },
    #[snafu(display("pipe name is already owned by another process"))]
    NameOccupied { source: CreatePipeInstanceError },
    #[snafu(display("pipe startup did not complete before its deadline"))]
    Startup,
    #[snafu(display("could not publish pipe readiness"))]
    Readiness {
        source: quartz_pipe::WaitCorePipeError,
    },
}
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum CreatePipeInstanceError {
    #[snafu(display("could not create pipe security"))]
    Security { source: CreatePipeSecurityError },
    #[snafu(display("CreateNamedPipeW failed because the pipe name is already owned"))]
    NameOccupied { source: io::Error },
    #[snafu(display("CreateNamedPipeW failed"))]
    Create { source: io::Error },
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
    readiness: Option<quartz_pipe::CoreReadySignal>,
    shutdown_timeout: Duration,
    control: mpsc::Sender<ServerMessage>,
    stop: Option<oneshot::Sender<()>>,
    completed: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}
impl PipeServer {
    pub(crate) fn start(config: PipeServerConfig) -> Result<Self, StartPipeServerError> {
        let readiness_name = config.name.clone();
        let (stop, stopping) = oneshot::channel();
        let (control, controls) = mpsc::channel(CONTROL_MAILBOX_CAPACITY);
        let (ready, startup) = bounded(LIFECYCLE_CAPACITY);
        let (finished, completed) = bounded(LIFECYCLE_CAPACITY);
        let io = IoThread {
            listener: Arc::new(PipeListener {
                name: config.name,
                owner: config.owner,
                authentication: AuthenticationPool::new(),
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
        let mut server = Self {
            readiness: None,
            shutdown_timeout: config.shutdown_timeout,
            control,
            stop: Some(stop),
            completed,
            thread: Some(thread),
        };
        match startup.recv_timeout(config.shutdown_timeout) {
            Ok(Ok(())) => {
                let readiness = quartz_pipe::CoreReadySignal::create(&readiness_name)
                    .context(start_pipe_server_error::ReadinessSnafu)?;
                readiness
                    .signal()
                    .context(start_pipe_server_error::ReadinessSnafu)?;
                server.readiness = Some(readiness);
                Ok(server)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(StartPipeServerError::Startup),
        }
    }
    pub(crate) fn target_control(
        &self,
        connection: crate::ipc::registry::ConnectionId,
        op: quartz_protocol::ControlOp,
    ) {
        if let Err(error) = self
            .control
            .try_send(ServerMessage::TargetControl { connection, op })
        {
            tracing::warn!(%error, "could not queue targeted UI control");
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
    authentication: AuthenticationPool,
}
impl PipeListener {
    fn create(&self, order: InstanceOrder) -> Result<NamedPipeServer, CreatePipeInstanceError> {
        let security = PipeSecurity::new(self.owner.sid())
            .context(create_pipe_instance_error::SecuritySnafu)?;
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
                CreatePipeInstanceError::NameOccupied { source }
            } else {
                CreatePipeInstanceError::Create { source }
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
        pipes.push(
            self.listener
                .create(InstanceOrder::First)
                .map_err(|source| {
                    if matches!(source, CreatePipeInstanceError::NameOccupied { .. }) {
                        StartPipeServerError::NameOccupied { source }
                    } else {
                        StartPipeServerError::CreateInstance { source }
                    }
                })?,
        );
        for _ in 1..PIPE_INSTANCES {
            pipes.push(
                self.listener
                    .create(InstanceOrder::Additional)
                    .context(start_pipe_server_error::CreateInstanceSnafu)?,
            );
        }
        if ready.send(Ok(())).is_err() {
            tracing::warn!("pipe startup caller left before startup completed");
            return Ok(());
        }
        drop(_entered);
        runtime.block_on(self.serve(pipes));
        // Running filesystem calls cannot be cancelled; their bounded permits remain owned until completion.
        runtime.shutdown_background();
        Ok(())
    }
    fn slot(&self, registry: &RegistryHandle, life: &watch::Receiver<ServerLife>) -> Slot {
        Slot {
            registry: registry.clone(),
            services: self.services.clone(),
            listener: Arc::clone(&self.listener),
            timeouts: self.timeouts,
            life: life.clone(),
        }
    }
    async fn serve(mut self, pipes: Vec<NamedPipeServer>) {
        let registry = self.services.start_registry();
        let (life, state) = watch::channel(ServerLife::Serving);
        let mut slots = JoinSet::new();
        for pipe in pipes {
            slots.spawn(self.slot(&registry, &state).run(Some(pipe)));
        }
        loop {
            tokio::select! {
                biased;
                _ = &mut self.stopping => break,
                message = self.controls.recv() => {
                    if let Some(message) = message { Self::server_message(message, &registry).await; }
                }
                failure = slots.join_next() => {
                    tracing::error!(?failure, "pipe slot task ended; restarting slot");
                    slots.spawn(self.slot(&registry, &state).run(None));
                }
            }
        }
        life.send_replace(ServerLife::Stopping);
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
            ServerMessage::TargetControl { connection, op } => {
                if let Err(error) = registry.target_control(connection, op) {
                    tracing::warn!(%error, "targeted UI control unavailable");
                }
            }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ServerLife {
    Serving,
    Stopping,
}

struct AuthenticationPool(Arc<Semaphore>);
#[derive(Debug, Snafu)]
#[snafu(module)]
enum AuthenticatePeerError {
    #[snafu(display("pipe peer authentication exceeded the hello deadline"))]
    Deadline,
    #[snafu(display("pipe authentication pool is closed"))]
    Closed { source: tokio::sync::AcquireError },
    #[snafu(display("pipe authentication task failed"))]
    Join { source: tokio::task::JoinError },
    #[snafu(display("pipe peer identity check failed"))]
    Check { source: CheckPeerError },
}
impl AuthenticationPool {
    fn new() -> Self {
        Self(Arc::new(Semaphore::new(AUTHENTICATION_PARALLELISM)))
    }
    async fn check<T: Send + 'static>(
        &self,
        deadline: tokio::time::Instant,
        authenticate: impl FnOnce() -> Result<T, CheckPeerError> + Send + 'static,
    ) -> Result<T, AuthenticatePeerError> {
        tokio::time::timeout_at(deadline, async {
            let permit = Arc::clone(&self.0)
                .acquire_owned()
                .await
                .map_err(|source| AuthenticatePeerError::Closed { source })?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                authenticate()
            })
            .await
            .map_err(|source| AuthenticatePeerError::Join { source })?
            .map_err(|source| AuthenticatePeerError::Check { source })
        })
        .await
        .map_err(|_| AuthenticatePeerError::Deadline)?
    }
}

struct Slot {
    services: Services,
    registry: RegistryHandle,
    listener: Arc<PipeListener>,
    timeouts: ConnectionTimeouts,
    life: watch::Receiver<ServerLife>,
}
impl Slot {
    async fn run(self, initial: Option<NamedPipeServer>) {
        let mut initial = initial.map(Arc::new);
        let mut backoff = SlotBackoff::new();
        while *self.life.borrow() == ServerLife::Serving {
            let pipe = match initial.take() {
                Some(pipe) => pipe,
                None => match self.listener.create(InstanceOrder::Additional) {
                    Ok(pipe) => Arc::new(pipe),
                    Err(error) => {
                        tracing::error!(error = %Report::from_error(&error), "could not recreate pipe slot; retrying");
                        tokio::time::sleep(backoff.next_delay()).await;
                        continue;
                    }
                },
            };
            let outcome = self.serve_connection(Arc::clone(&pipe)).await;
            // Reuse the slot's instance; pending completion packets can retain a closed handle.
            if let Err(error) = pipe.disconnect() {
                use windows::Win32::Foundation::{ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED};
                if error.is_win32(ERROR_PIPE_NOT_CONNECTED) || error.is_win32(ERROR_NO_DATA) {
                    tracing::debug!(%error, "pipe client already disconnected");
                } else {
                    tracing::error!(error = %Report::from_error(&error), "could not disconnect pipe slot before replacement");
                    tokio::time::sleep(backoff.next_delay()).await;
                    continue;
                }
            }
            if *self.life.borrow() == ServerLife::Stopping {
                return;
            }
            initial = Some(pipe);
            match outcome {
                Ok(()) => backoff.reset(),
                Err(error) => {
                    tracing::error!(%error, "pipe slot failed; retrying");
                    tokio::time::sleep(backoff.next_delay()).await;
                }
            }
        }
    }
    async fn serve_connection(&self, pipe: Arc<NamedPipeServer>) -> io::Result<()> {
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
            .map_err(|source| AuthenticatePeerError::Check { source });
        let peer = match peer {
            Ok(()) => {
                let listener = Arc::clone(&self.listener);
                self.listener
                    .authentication
                    .check(hello_deadline, move || listener.owner.check(pid))
                    .await
            }
            Err(error) => Err(error),
        }
        .map(|peer| match peer {
            quartz_pipe::PeerExecutable::Quartz => crate::ipc::AllowedRoles::Quartz,
            quartz_pipe::PeerExecutable::Host => crate::ipc::AllowedRoles::Host,
        });
        match peer {
            Ok(allowed) => {
                let mut session = ConnectionSession::new(allowed);
                let read = PipeTransport(Arc::clone(&pipe));
                let write = PipeTransport(pipe);
                if let Err(error) = (Connection {
                    services: self.services.clone(),
                    registry: self.registry.clone(),
                    timeouts: self.timeouts,
                    peer_pid: crate::ipc::ProcessId::new(pid),
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

/// Shares one instance between the slot and its sole reader and writer.
struct PipeTransport(Arc<NamedPipeServer>);
impl AsyncRead for PipeTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            ready!(self.0.poll_read_ready(cx))?;
            match self.0.try_read_buf(buffer) {
                Ok(_) => return Poll::Ready(Ok(())),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => return Poll::Ready(Err(error)),
            }
        }
    }
}
impl AsyncWrite for PipeTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            ready!(self.0.poll_write_ready(cx))?;
            match self.0.try_write(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                result => return Poll::Ready(result),
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::StatusPublisher;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use tokio_util::codec::Framed;

    /// Bounds lifecycle unit tests independently of production connection deadlines.
    const TEST_CLOSE_TIMEOUT: Duration = Duration::from_millis(20);
    /// Bounds instance replacement independently of the production hello deadline.
    const TEST_RECONNECT_TIMEOUT: Duration = Duration::from_secs(2);

    #[test]
    fn hung_authentication_leaves_rule_read_capacity_and_bounds_new_client_waits() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(MAX_BLOCKING_THREADS)
            .build()
            .expect("production-size blocking pool");
        runtime.block_on(async {
            let database = crate::query::dispatch::test_support::TestDatabase::start();
            let pool = Arc::new(AuthenticationPool::new());
            let mut releases = Vec::new();
            let mut tasks = Vec::new();
            for _ in 0..AUTHENTICATION_PARALLELISM {
                let (release, blocked) = crossbeam_channel::bounded(1);
                let (entered, started) = oneshot::channel();
                releases.push(release);
                let pool = Arc::clone(&pool);
                tasks.push(tokio::spawn(async move {
                    pool.check(
                        tokio::time::Instant::now() + TEST_CLOSE_TIMEOUT,
                        move || {
                            let _ = entered.send(());
                            let _ = blocked.recv_timeout(TEST_RECONNECT_TIMEOUT * 3);
                            Ok(())
                        },
                    )
                    .await
                }));
                started
                    .await
                    .expect("fake authentication occupies its worker");
            }
            let result = tokio::time::timeout(
                TEST_RECONNECT_TIMEOUT,
                database
                    .port
                    .read_rules(crate::rules::read::ReadRequest::Tree(
                        quartz_protocol::RuleSetsTreeRequest {
                            kind: quartz_protocol::RuleKind::Privacy,
                            cursor: None,
                        },
                    )),
            )
            .await;
            let waiting = pool
                .check(
                    tokio::time::Instant::now() + TEST_CLOSE_TIMEOUT,
                    || -> Result<(), CheckPeerError> {
                        panic!("hung authentication retains its permit");
                    },
                )
                .await;
            for release in releases {
                let _ = release.send(());
            }
            for task in tasks {
                let _ = task.await.expect("authentication timeout");
            }
            result
                .expect("authenticated rule read keeps its own blocking capacity")
                .expect("rule read");
            assert!(
                matches!(waiting, Err(AuthenticatePeerError::Deadline)),
                "new clients leave at their authentication deadline"
            );
        });
    }

    #[tokio::test]
    async fn authentication_capacity_wait_obeys_deadline_without_starting_native_work() {
        let pool = AuthenticationPool::new();
        let permits = Arc::clone(&pool.0)
            .acquire_many_owned(
                u32::try_from(AUTHENTICATION_PARALLELISM).expect("authentication limit"),
            )
            .await
            .expect("reserve pool");
        let result = pool
            .check(
                tokio::time::Instant::now() + TEST_CLOSE_TIMEOUT,
                || -> Result<(), CheckPeerError> {
                    panic!("native authentication cannot start without capacity");
                },
            )
            .await;
        assert!(matches!(result, Err(AuthenticatePeerError::Deadline)));
        drop(permits);
        assert_eq!(pool.0.available_permits(), AUTHENTICATION_PARALLELISM);
    }

    #[tokio::test]
    async fn delayed_authentication_keeps_an_active_connection_responsive_and_times_out() {
        let fixture = slot();
        let connection = Connection {
            services: fixture.slot.services.clone(),
            registry: fixture.slot.registry.clone(),
            timeouts: ConnectionTimeouts::default(),
            peer_pid: crate::ipc::ProcessId::new(1),
        };
        let mut client = crate::ipc::connection::tests::MemoryClient::connect(
            connection,
            quartz_protocol::HelloRole::Cli,
        )
        .await;
        let pool = Arc::new(AuthenticationPool::new());
        let (release, waiting) = crossbeam_channel::bounded(1);
        let (entered, started) = tokio::sync::oneshot::channel();
        let checking = Arc::clone(&pool);
        let delayed = tokio::spawn(async move {
            checking
                .check(
                    tokio::time::Instant::now() + TEST_RECONNECT_TIMEOUT,
                    move || {
                        let _ = entered.send(Instant::now());
                        waiting
                            .recv_timeout(TEST_RECONNECT_TIMEOUT + TEST_RECONNECT_TIMEOUT)
                            .expect("release fake authenticator");
                        Ok(())
                    },
                )
                .await
        });
        let started = started.await.expect("blocking authenticator started");
        assert!(
            started.elapsed() < TEST_RECONNECT_TIMEOUT,
            "native authentication must not suspend the I/O scheduler"
        );
        tokio::time::timeout(
            TEST_CLOSE_TIMEOUT,
            client.request(2, quartz_protocol::Op::CoreStatus, serde_json::json!({})),
        )
        .await
        .expect("second active connection remains responsive");
        assert!(matches!(
            delayed.await.expect("authentication task"),
            Err(AuthenticatePeerError::Deadline)
        ));
        assert_eq!(
            pool.0.available_permits(),
            AUTHENTICATION_PARALLELISM - 1,
            "timed-out native work retains its concurrency permit"
        );
        release
            .send(())
            .expect("release timed-out native authentication");
        client.close().await;
    }

    async fn connect(name: &PipeName) -> NamedPipeClient {
        tokio::time::timeout(TEST_RECONNECT_TIMEOUT, async {
            loop {
                match ClientOptions::new().open(name) {
                    Ok(client) => return client,
                    Err(error) if error.is_win32(windows::Win32::Foundation::ERROR_PIPE_BUSY) => {
                        tokio::time::sleep(TEST_CLOSE_TIMEOUT).await;
                    }
                    Err(error) => panic!("open test client: {error}"),
                }
            }
        })
        .await
        .expect("slot becomes available")
    }

    async fn handshake(
        client: NamedPipeClient,
    ) -> Framed<NamedPipeClient, tokio_util::codec::LengthDelimitedCodec> {
        let mut framed = Framed::new(client, quartz_protocol::frame_codec());
        let hello = quartz_protocol::encode_json(&serde_json::json!({
            "t": "req", "id": 1, "op": "hello", "body": {
                "proto": {"min": 1, "max": 1},
                "client_version": "test", "role": "cli"
            }
        }))
        .expect("hello frame");
        framed.send(hello.as_slice()).await.expect("send hello");
        framed
            .next()
            .await
            .expect("hello response")
            .expect("read hello");
        framed
    }

    #[test]
    fn client_disconnect_recreates_slot_without_error_or_backoff() {
        let log = tempfile::NamedTempFile::new().expect("slot log");
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::ERROR)
            .with_writer(std::sync::Mutex::new(log.reopen().expect("log writer")))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            runtime().block_on(async {
                let fixture = slot();
                let listener = Arc::clone(&fixture.slot.listener);
                let pipe = listener
                    .create(InstanceOrder::First)
                    .expect("first instance");
                let mut client = connect(&listener.name).await;
                let mut pipes = Vec::new();
                let mut occupied = Vec::new();
                for _ in 1..PIPE_INSTANCES {
                    pipes.push(
                        listener
                            .create(InstanceOrder::Additional)
                            .expect("instance"),
                    );
                    occupied.push(connect(&listener.name).await);
                }
                let task = tokio::spawn(fixture.slot.run(Some(pipe)));
                for _ in 0..PIPE_INSTANCES {
                    let framed = handshake(client).await;
                    drop(framed);
                    client = connect(&listener.name).await;
                }
                drop(client);
                task.abort();
                assert!(task.await.expect_err("cancel slot").is_cancelled());
            });
        });
        let errors = std::fs::read_to_string(log.path()).expect("captured slot errors");
        assert!(
            errors.is_empty(),
            "slot must recycle without errors/backoff: {errors}"
        );
    }

    #[test]
    fn shutdown_with_disconnecting_client_does_not_recreate_or_log_errors() {
        let log = tempfile::NamedTempFile::new().expect("shutdown log");
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::ERROR)
            .with_writer(std::sync::Mutex::new(log.reopen().expect("log writer")))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            runtime().block_on(async {
                let fixture = slot();
                let listener = Arc::clone(&fixture.slot.listener);
                let mut pipes = Vec::with_capacity(PIPE_INSTANCES);
                pipes.push(
                    listener
                        .create(InstanceOrder::First)
                        .expect("first instance"),
                );
                for _ in 1..PIPE_INSTANCES {
                    pipes.push(
                        listener
                            .create(InstanceOrder::Additional)
                            .expect("instance"),
                    );
                }
                let (stop, stopping) = oneshot::channel();
                let (_control, controls) = mpsc::channel(CONTROL_MAILBOX_CAPACITY);
                let server = IoThread {
                    services: fixture.slot.services.clone(),
                    listener: Arc::clone(&listener),
                    timeouts: ConnectionTimeouts {
                        hello: fixture.slot.timeouts.hello,
                        close_flush: TEST_RECONNECT_TIMEOUT,
                    },
                    stopping,
                    controls,
                };
                let task = tokio::spawn(server.serve(pipes));
                let client = handshake(connect(&listener.name).await).await;
                stop.send(()).expect("request shutdown");
                tokio::time::sleep(TEST_CLOSE_TIMEOUT).await;
                drop(client);
                tokio::time::sleep(TEST_CLOSE_TIMEOUT).await;
                let replacement = listener
                    .create(InstanceOrder::Additional)
                    .expect("stopping slot releases its instance without accepting again");
                assert!(matches!(listener.create(InstanceOrder::Additional),
                    Err(CreatePipeInstanceError::Create { source })
                    if source.is_win32(windows::Win32::Foundation::ERROR_PIPE_BUSY)));
                drop(replacement);
                task.await.expect("server shutdown");
                let pipe = listener
                    .create(InstanceOrder::First)
                    .expect("shutdown releases all instances");
                drop(pipe);
            });
        });
        let errors = std::fs::read_to_string(log.path()).expect("captured shutdown errors");
        assert!(
            errors.is_empty(),
            "shutdown must stop slots without errors: {errors}"
        );
    }

    struct TestSlot {
        slot: Slot,
        _queries: crossbeam_channel::Receiver<crate::query::dispatch::QueryCommand>,
        _database: crate::query::dispatch::test_support::TestDatabase,
        _status: StatusPublisher,
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
                registry: RegistryHandle::start(
                    publisher.subscribe(),
                    received.clone(),
                    Arc::new(|_| Ok(())),
                ),
                services: Services {
                    summary: None,
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
                    authentication: AuthenticationPool::new(),
                }),
                timeouts: ConnectionTimeouts {
                    hello: ConnectionTimeouts::default().hello,
                    close_flush: TEST_CLOSE_TIMEOUT,
                },
                life: watch::channel(ServerLife::Serving).1,
            },
            _status: publisher,
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
            assert!(matches!(error, CreatePipeInstanceError::Create { source } if source.is_win32(windows::Win32::Foundation::ERROR_PIPE_BUSY)));
            assert!(!task.is_finished());
            task.abort();
            assert!(task.await.expect_err("cancel test slot").is_cancelled());
        });
    }

    #[test]
    fn instance_creation_reports_create_named_pipe_and_win32_busy_code() {
        runtime().block_on(async {
            let fixture = slot();
            let listener = &fixture.slot.listener;
            let mut pipes = Vec::with_capacity(PIPE_INSTANCES);
            pipes.push(
                listener
                    .create(InstanceOrder::First)
                    .expect("first instance"),
            );
            for _ in 1..PIPE_INSTANCES {
                pipes.push(
                    listener
                        .create(InstanceOrder::Additional)
                        .expect("instance"),
                );
            }
            let error = listener
                .create(InstanceOrder::Additional)
                .expect_err("all instances occupied");
            let CreatePipeInstanceError::Create { source } = &error else {
                panic!("expected CreateNamedPipeW failure: {error:?}");
            };
            assert!(source.is_win32(windows::Win32::Foundation::ERROR_PIPE_BUSY));
            let report = Report::from_error(&error).to_string();
            assert!(report.contains("CreateNamedPipeW"), "{report}");
            assert!(
                report.contains(&windows::Win32::Foundation::ERROR_PIPE_BUSY.0.to_string()),
                "{report}"
            );
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
