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
/// A single temporary native flush thread stays within the core thread budget (08 §1).
const NATIVE_FLUSH_PARALLELISM: usize = 1;
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
            connections: ConnectedSlots::default(),
            listener: Arc::new(PipeListener {
                flush_capacity: Arc::new(Semaphore::new(NATIVE_FLUSH_PARALLELISM)),
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
    flush_capacity: Arc<Semaphore>,
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
/// Counts accepted transports, including peers still authenticating or awaiting hello.
#[derive(Clone, Default)]
struct ConnectedSlots(Arc<std::sync::atomic::AtomicUsize>);
impl ConnectedSlots {
    fn connect(&self) -> ConnectedSlot {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ConnectedSlot(self.clone())
    }
    fn is_empty(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed) == 0
    }
}
struct ConnectedSlot(ConnectedSlots);
impl Drop for ConnectedSlot {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}
struct IoThread {
    connections: ConnectedSlots,
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
            connections: self.connections.clone(),
            registry: registry.clone(),
            services: self.services.clone(),
            listener: Arc::clone(&self.listener),
            timeouts: self.timeouts,
            life: life.clone(),
        }
    }
    async fn flush_shutdown(&self, outboxes: &[crate::ipc::outbox::OutboxHandle]) {
        if self.connections.is_empty() {
            return;
        }
        let deadline = tokio::time::Instant::now() + self.timeouts.close_flush;
        while outboxes.iter().any(|outbox| outbox.queued_bytes() != 0)
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(SHUTDOWN_FLUSH_POLL).await;
        }
        // Let readers consume a successfully flushed shutdown notification before disconnect.
        tokio::time::sleep_until(deadline).await;
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
                self.flush_shutdown(&outboxes).await;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipeReuse {
    Reuse,
    Replace,
}
struct Slot {
    connections: ConnectedSlots,
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
            let mut disconnect = DisconnectPipe(Some(Arc::clone(&pipe)));
            let flush = super::flush::PipeFlush::new(Arc::clone(&self.listener.flush_capacity));
            let outcome = self
                .serve_connection(Arc::clone(&pipe), flush.clone())
                .await;
            // Pending completion packets can retain a closed handle until flush cleanup finishes.
            let Some(reuse) = self.settle_connection(disconnect.finish(), &flush).await else {
                return;
            };
            if reuse == PipeReuse::Replace {
                drop(pipe);
                tokio::time::sleep(backoff.next_delay()).await;
                continue;
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
    async fn settle_connection(
        &self,
        disconnected: io::Result<()>,
        flush: &super::flush::PipeFlush,
    ) -> Option<PipeReuse> {
        let reuse = if let Err(error) = disconnected {
            use windows::Win32::Foundation::{ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED};
            if error.is_win32(ERROR_PIPE_NOT_CONNECTED) || error.is_win32(ERROR_NO_DATA) {
                tracing::debug!(%error, "pipe client already disconnected");
                PipeReuse::Reuse
            } else {
                tracing::error!(error = %Report::from_error(&error), "could not disconnect pipe slot before replacement");
                PipeReuse::Replace
            }
        } else {
            PipeReuse::Reuse
        };
        if *self.life.borrow() == ServerLife::Stopping {
            return None;
        }
        if let Err(error) = flush.wait_reusable(self.timeouts.close_flush).await {
            tracing::warn!(%error, "pipe slot disabled until its old flush releases resources");
            let mut life = self.life.clone();
            tokio::select! {
                result = flush.wait_completion() => {
                    if let Err(error) = result { tracing::error!(%error, "pipe flush cleanup stopped"); return None; }
                }
                _ = life.changed() => return None,
            }
            tracing::info!("pipe slot recovered after old flush released resources");
        }
        Some(reuse)
    }
    async fn serve_connection(
        &self,
        pipe: Arc<NamedPipeServer>,
        flush: super::flush::PipeFlush,
    ) -> io::Result<()> {
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
        if *self.life.borrow() == ServerLife::Stopping {
            return Ok(());
        }
        let _connected = self.connections.connect();
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
                let read = PipeTransport::new(Arc::clone(&pipe), flush.clone());
                let write = PipeTransport::new(pipe, flush);
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

/// Cancels native pipe draining even when server shutdown aborts a slot task.
struct DisconnectPipe(Option<Arc<NamedPipeServer>>);
impl DisconnectPipe {
    fn finish(&mut self) -> io::Result<()> {
        self.0.take().expect("slot owns its pipe").disconnect()
    }
}
impl Drop for DisconnectPipe {
    fn drop(&mut self) {
        if let Some(pipe) = self.0.take() {
            // Task cancellation must also unblock a native FlushFileBuffers worker.
            let _ = pipe.disconnect();
        }
    }
}

/// Shares one instance between the slot and its sole reader and writer.
struct PipeTransport {
    pipe: Arc<NamedPipeServer>,
    flush: super::flush::PipeFlush,
    pending_flush: Option<super::flush::PendingFlush>,
}
impl PipeTransport {
    fn new(pipe: Arc<NamedPipeServer>, flush: super::flush::PipeFlush) -> Self {
        Self {
            pipe,
            flush,
            pending_flush: None,
        }
    }
}
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
            ready!(self.pipe.poll_read_ready(cx))?;
            match self.pipe.try_read_buf(buffer) {
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
            ready!(self.pipe.poll_write_ready(cx))?;
            match self.pipe.try_write(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                result => return Poll::Ready(result),
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.pending_flush.is_none() {
            self.pending_flush = Some(self.flush.start(Arc::clone(&self.pipe)));
        }
        self.pending_flush
            .as_mut()
            .expect("native flush started")
            .poll(cx)
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

    #[tokio::test(start_paused = true)]
    async fn idle_server_shutdown_completes_without_reader_grace_period() {
        let fixture = slot();
        let (_stop, stopping) = oneshot::channel();
        let (_control, controls) = mpsc::channel(CONTROL_MAILBOX_CAPACITY);
        let server = IoThread {
            connections: ConnectedSlots::default(),
            services: fixture.slot.services.clone(),
            listener: fixture.slot.listener.clone(),
            timeouts: Default::default(),
            stopping,
            controls,
        };
        let before = tokio::time::Instant::now();
        server.flush_shutdown(&[]).await;
        assert_eq!(
            tokio::time::Instant::now(),
            before,
            "no client needs a reader grace period"
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
                    connections: ConnectedSlots::default(),
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
                connections: ConnectedSlots::default(),
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
                    flush_capacity: Arc::new(Semaphore::new(NATIVE_FLUSH_PARALLELISM)),
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
    #[tokio::test]
    async fn unexpected_disconnect_waits_for_old_flush_before_replacing_the_instance() {
        let mut fixture = slot();
        let (_life, life) = watch::channel(ServerLife::Serving);
        fixture.slot.life = life;
        let flush = super::super::flush::PipeFlush::new(Arc::new(Semaphore::new(1)));
        let (release, waiting) = crossbeam_channel::bounded(1);
        let (entered, started) = oneshot::channel();
        let pending = flush.start_with(move || {
            let _ = entered.send(());
            waiting
                .recv_timeout(TEST_RECONNECT_TIMEOUT)
                .expect("release fake flush");
            Ok(())
        });
        started.await.expect("flush worker started");
        let cleanup = fixture.slot.settle_connection(
            Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            &flush,
        );
        tokio::pin!(cleanup);
        let early = tokio::time::timeout(TEST_CLOSE_TIMEOUT * 2, &mut cleanup).await;
        release.send(()).expect("release old flush");
        assert!(
            early.is_err(),
            "instance replacement must wait for flush resource release"
        );
        assert_eq!(
            tokio::time::timeout(TEST_RECONNECT_TIMEOUT, cleanup)
                .await
                .expect("cleanup finishes"),
            Some(PipeReuse::Replace)
        );
        assert!(flush.reusable());
        drop(pending);
    }
    #[test]
    fn disabled_slot_logs_one_recovery_after_flush_cleanup() {
        let log = tempfile::NamedTempFile::new().expect("slot log");
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(std::sync::Mutex::new(log.reopen().expect("log writer")))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            runtime().block_on(async {
                let mut fixture = slot();
                let (_life, life) = watch::channel(ServerLife::Serving);
                fixture.slot.life = life;
                let flush = super::super::flush::PipeFlush::new(Arc::new(Semaphore::new(1)));
                let (release, waiting) = crossbeam_channel::bounded(1);
                let (entered, started) = oneshot::channel();
                let pending = flush.start_with(move || {
                    let _ = entered.send(());
                    waiting
                        .recv_timeout(TEST_RECONNECT_TIMEOUT)
                        .expect("release fake flush");
                    Ok(())
                });
                started.await.expect("flush worker started");
                let cleanup = fixture.slot.settle_connection(Ok(()), &flush);
                tokio::pin!(cleanup);
                assert!(
                    tokio::time::timeout(TEST_CLOSE_TIMEOUT * 2, &mut cleanup)
                        .await
                        .is_err()
                );
                release.send(()).expect("release old flush");
                assert_eq!(
                    tokio::time::timeout(TEST_RECONNECT_TIMEOUT, cleanup)
                        .await
                        .expect("recovery"),
                    Some(PipeReuse::Reuse)
                );
                assert_eq!(
                    fixture.slot.settle_connection(Ok(()), &flush).await,
                    Some(PipeReuse::Reuse)
                );
                drop(pending);
            });
        });
        let messages = std::fs::read_to_string(log.path()).expect("captured slot messages");
        let disabled = messages
            .lines()
            .filter(|message| {
                message.contains("pipe slot disabled until its old flush releases resources")
            })
            .collect::<Vec<_>>();
        let recovered = messages
            .lines()
            .filter(|message| {
                message.contains("pipe slot recovered after old flush released resources")
            })
            .collect::<Vec<_>>();
        assert_eq!(disabled.len(), 1, "{messages}");
        assert_eq!(recovered.len(), 1, "{messages}");
        assert!(disabled[0].contains("WARN"), "{messages}");
        assert!(recovered[0].contains("INFO"), "{messages}");
    }
}
