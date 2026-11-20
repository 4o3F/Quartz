//! One current-thread runtime owns all pipe instances and client I/O tasks.

use super::super::handle::OwnedHandle;
use super::{
    CheckPeerError, CreatePipeSecurityError, PipeName, ProcessIdentity, security::PipeSecurity,
    win32_error::Win32ErrorCode,
};
use crate::{
    core::app::SHUTDOWN_TIMEOUT,
    ipc::{ConnectionSession, UiPort},
};
use crossbeam_channel::{Receiver, bounded};
use futures_util::{SinkExt, StreamExt};
use quartz_protocol::{
    CoreStatusResponse, Envelope, ErrorKind, ProtocolError, ResponseOutcome, decode_json,
    encode_json, frame_codec,
};
use snafu::{ResultExt, Snafu};
use std::{
    io,
    os::windows::io::AsRawHandle,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncWrite, split},
    net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions},
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodecError};
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
/// Bounded response count for B1 (04 §7); B2 adds byte accounting and coalescing.
const RESPONSE_CAPACITY: usize = 16;
/// Bound the final error response and close grace period (04 §2 error-and-close).
const CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_millis(500);
/// Require a complete first hello frame within five seconds (04 §4).
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Start retrying an unavailable pipe slot without affecting capture (04 §1).
const SLOT_RETRY_INITIAL: Duration = Duration::from_millis(25);
/// Cap exponential slot recovery delays while other slots keep serving (04 §1).
const SLOT_RETRY_MAX: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy)]
pub(super) struct ConnectionTimeouts {
    pub(super) hello: Duration,
    pub(super) close_flush: Duration,
}
impl Default for ConnectionTimeouts {
    fn default() -> Self {
        Self {
            hello: HELLO_TIMEOUT,
            close_flush: CLOSE_FLUSH_TIMEOUT,
        }
    }
}
pub(super) struct PipeServerConfig {
    pub(super) name: PipeName,
    pub(super) timeouts: ConnectionTimeouts,
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
    stop: Option<oneshot::Sender<()>>,
    completed: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}
impl PipeServer {
    pub(crate) fn start(
        status: watch::Receiver<CoreStatusResponse>,
        ui: UiPort,
    ) -> Result<Self, StartPipeServerError> {
        let owner = ProcessIdentity::current().context(start_pipe_server_error::IdentitySnafu)?;
        let name = owner.pipe_name();
        Self::start_named(name, owner, status, ui)
    }
    pub(crate) fn start_named(
        name: PipeName,
        owner: ProcessIdentity,
        status: watch::Receiver<CoreStatusResponse>,
        ui: UiPort,
    ) -> Result<Self, StartPipeServerError> {
        Self::start_configured(
            PipeServerConfig {
                name,
                timeouts: ConnectionTimeouts::default(),
            },
            owner,
            status,
            ui,
        )
    }
    pub(super) fn start_configured(
        config: PipeServerConfig,
        owner: ProcessIdentity,
        status: watch::Receiver<CoreStatusResponse>,
        ui: UiPort,
    ) -> Result<Self, StartPipeServerError> {
        let (stop, stopping) = oneshot::channel();
        let (ready, startup) = bounded(LIFECYCLE_CAPACITY);
        let (finished, completed) = bounded(LIFECYCLE_CAPACITY);
        let io = IoThread {
            listener: Arc::new(PipeListener {
                name: config.name,
                owner,
            }),
            timeouts: config.timeouts,
            status,
            ui,
            stopping,
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
            stop: Some(stop),
            completed,
            thread: Some(thread),
        };
        match startup.recv_timeout(SHUTDOWN_TIMEOUT) {
            Ok(Ok(())) => Ok(server),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(StartPipeServerError::Startup),
        }
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
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
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
    listener: Arc<PipeListener>,
    timeouts: ConnectionTimeouts,
    status: watch::Receiver<CoreStatusResponse>,
    ui: UiPort,
    stopping: oneshot::Receiver<()>,
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
    fn slot(&self) -> Slot {
        Slot {
            listener: Arc::clone(&self.listener),
            timeouts: self.timeouts,
            status: self.status.clone(),
            ui: Arc::clone(&self.ui),
        }
    }
    async fn serve(mut self, pipes: Vec<NamedPipeServer>) {
        let mut slots = JoinSet::new();
        for pipe in pipes {
            slots.spawn(self.slot().run(Some(pipe)));
        }
        loop {
            tokio::select! {
                _ = &mut self.stopping => break,
                failure = slots.join_next() => {
                    tracing::error!(?failure, "pipe slot task ended; restarting slot");
                    slots.spawn(self.slot().run(None));
                }
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
    listener: Arc<PipeListener>,
    timeouts: ConnectionTimeouts,
    status: watch::Receiver<CoreStatusResponse>,
    ui: UiPort,
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
                if let Err(error) = self.connection(pipe, &mut session, hello_deadline).await {
                    tracing::debug!(pid, %error, "pipe connection closed");
                }
            }
            Err(error) => tracing::warn!(pid, %error, "rejected pipe client"),
        }
        Ok(())
    }
    async fn connection(
        &self,
        pipe: NamedPipeServer,
        session: &mut ConnectionSession,
        hello_deadline: tokio::time::Instant,
    ) -> io::Result<()> {
        let (read, write) = split(pipe);
        let mut reader = FramedRead::new(read, frame_codec());
        let (responses, queued) = mpsc::channel(RESPONSE_CAPACITY);
        let writer = WriterTask::spawn(write, queued);
        loop {
            let received = if session.hello_pending() {
                match tokio::time::timeout_at(hello_deadline, reader.next()).await {
                    Ok(received) => received,
                    Err(error) => {
                        tracing::debug!(%error, "pipe hello deadline expired; closing connection");
                        return Ok(());
                    }
                }
            } else {
                reader.next().await
            };
            let mut decision = match received {
                None => break,
                Some(Ok(payload)) => match decode_json::<Envelope>(&payload) {
                    Ok(envelope) => session.receive(envelope, &self.status.borrow()),
                    // Parse diagnostics can contain request body values; only the stable code is sent.
                    Err(_) => session.fail(ErrorKind::BadRequest),
                },
                Some(Err(error))
                    if error
                        .get_ref()
                        .and_then(|source| source.downcast_ref::<LengthDelimitedCodecError>())
                        .is_some() =>
                {
                    session.fail(ErrorKind::FrameTooLarge)
                }
                Some(Err(error))
                    if error.kind() == io::ErrorKind::Other && !reader.read_buffer().is_empty() =>
                {
                    // tokio-util 0.7.20 codec/decoder.rs::decode_eof uses Other for a buffered EOF tail.
                    tracing::debug!(%error, "dropped half frame at pipe EOF");
                    break;
                }
                Some(Err(error)) => return Err(error),
            };
            if let Some(command) = decision.command
                && let Err(error) = (self.ui)(command)
            {
                tracing::warn!(%error, "could not queue IPC UI command");
                decision.response.outcome = ResponseOutcome::Err {
                    error: ProtocolError::new(ErrorKind::Internal),
                };
            }
            let payload =
                encode_json(&Envelope::Response(decision.response)).map_err(io::Error::other)?;
            // TODO(M2-B2): Account for queued bytes and coalesce replaceable state/day events.
            if responses.try_send(payload).is_err() {
                return Err(io::Error::other(
                    "pipe response queue full or writer closed",
                ));
            }
            if session.closing() {
                drop(responses);
                return self.finish_closing(writer).await;
            }
        }
        drop(responses);
        drop(writer);
        Ok(())
    }
    async fn finish_closing(&self, writer: WriterTask) -> io::Result<()> {
        let deadline = tokio::time::Instant::now() + self.timeouts.close_flush;
        let outcome = tokio::time::timeout(self.timeouts.close_flush, writer.finish()).await;
        match outcome {
            Ok(result) => {
                // Allow the final response to be read before DisconnectNamedPipe discards unread bytes.
                tokio::time::sleep_until(deadline).await;
                result
            }
            Err(error) => {
                tracing::debug!(%error, "pipe final response deadline expired; closing connection");
                Ok(())
            }
        }
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

/// Aborts the sole writer on every early return and when its slot is cancelled.
struct WriterTask {
    task: tokio::task::JoinHandle<io::Result<()>>,
}
impl WriterTask {
    fn spawn(
        write: impl AsyncWrite + Unpin + Send + 'static,
        mut queued: mpsc::Receiver<Vec<u8>>,
    ) -> Self {
        Self {
            task: tokio::spawn(async move {
                let mut writer = FramedWrite::new(write, frame_codec());
                while let Some(payload) = queued.recv().await {
                    writer.send(payload.as_slice()).await?;
                }
                Ok(())
            }),
        }
    }
    async fn finish(mut self) -> io::Result<()> {
        (&mut self.task).await.map_err(io::Error::other)?
    }
}
impl Drop for WriterTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::StatusPublisher;

    /// Bounds lifecycle unit tests independently of production connection deadlines.
    const TEST_CLOSE_TIMEOUT: Duration = Duration::from_millis(20);

    fn slot() -> Slot {
        let suffix = windows::core::GUID::new().expect("test pipe suffix");
        let publisher = StatusPublisher::new(quartz_protocol::InstanceId(format!("{suffix:?}")));
        Slot {
            listener: Arc::new(PipeListener {
                name: PipeName::for_test(suffix),
                owner: ProcessIdentity::current().expect("test identity"),
            }),
            timeouts: ConnectionTimeouts {
                hello: HELLO_TIMEOUT,
                close_flush: TEST_CLOSE_TIMEOUT,
            },
            status: publisher.subscribe(),
            ui: Arc::new(|_| Ok(())),
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
    fn final_response_timeout_aborts_a_stalled_writer() {
        runtime().block_on(async {
            let writer = WriterTask {
                task: tokio::spawn(std::future::pending()),
            };
            let abort = writer.task.abort_handle();
            slot().finish_closing(writer).await.expect("bounded close");
            tokio::task::yield_now().await;
            assert!(
                abort.is_finished(),
                "timed-out writer must release its pipe reference"
            );
        });
    }

    #[test]
    fn failed_slot_creation_retries_and_recovers_after_capacity_frees() {
        runtime().block_on(async {
            let slot = slot();
            let listener = Arc::clone(&slot.listener);
            let mut occupied = Vec::new();
            occupied.push(listener.create(InstanceOrder::First).expect("first instance"));
            for _ in 1..PIPE_INSTANCES { occupied.push(listener.create(InstanceOrder::Additional).expect("occupy instance")); }
            let task = tokio::spawn(slot.run(None));
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
