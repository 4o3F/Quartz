//! One-shot UI pipe connections and a bounded, executor-independent channel bridge.

#[cfg(windows)]
mod worker;

mod annotation;
mod day;
mod details;
#[cfg(windows)]
mod rules;
use crate::failure::Failure;
pub(crate) use crate::model::QueryProgress;
#[cfg(windows)]
pub(crate) use annotation::AwayAction;
#[cfg(windows)]
pub(crate) use day::REFRESH_INDICATOR_DELAY;
pub use day::{
    DayData, DayGeneration, DayLoad, DayRequest, DayView, LoadDataError, LoadDataErrorCode,
    LocalFailureCode, UtcRange,
};
pub use details::{
    DetailData, DetailGeneration, DetailRequest, DetailTarget, EntityData, EntityKey,
};
use quartz_pipe::CallCoreError;
use quartz_protocol::{ControlOp, CoreVersion, DayChanged, InstanceId, StateSnapshot};
#[cfg(windows)]
use quartz_protocol::{CoreRequest, JsonValue, Op, Topic};
#[cfg(windows)]
use snafu::ResultExt;
use snafu::Snafu;
#[cfg(windows)]
use tokio::sync::{mpsc, oneshot, watch};

/// Bounded command, pending-request and event counts (01 §6–§7.2, 04 §7).
#[cfg(windows)]
const BRIDGE_CAPACITY: usize = 64;
/// Minimal blocking pool ceiling (01 §7.2).
#[cfg(windows)]
const UI_BLOCKING_THREADS: usize = 1;

/// A connection attempt stops at either an authenticated connection or a typed failure.
#[derive(Debug, Clone)]
pub enum ConnectionState {
    Connecting,
    Connected {
        instance_id: InstanceId,
        core_version: CoreVersion,
    },
    Failed(Failure),
}

/// Wire events and link lifecycle notices delivered in receive order.
#[derive(Debug)]
pub enum LinkEvent {
    DetailLoading {
        generation: DetailGeneration,
    },
    DetailProgress {
        generation: DetailGeneration,
        progress: QueryProgress,
    },
    DetailLoaded {
        generation: DetailGeneration,
        result: Result<DetailData, LoadDataError>,
    },
    State(StateSnapshot),
    Day(DayChanged),
    Control(ControlOp),
    Connection(ConnectionState),
    Disconnected,
    DayLoading {
        generation: DayGeneration,
    },
    DayLoaded {
        generation: DayGeneration,
        result: Result<DayData, LoadDataError>,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowAction {
    Observe,
    Activate,
    Quit,
}
impl LinkEvent {
    pub(crate) fn window_action(&self) -> WindowAction {
        match self {
            Self::Disconnected | Self::Control(ControlOp::UiShutdown) => WindowAction::Quit,
            Self::Control(ControlOp::UiShow | ControlOp::UiActivate) => WindowAction::Activate,
            Self::DetailLoading { .. }
            | Self::DetailProgress { .. }
            | Self::DetailLoaded { .. }
            | Self::DayLoading { .. }
            | Self::DayLoaded { .. }
            | Self::State(_)
            | Self::Day(_)
            | Self::Connection(_) => WindowAction::Observe,
        }
    }
    /// The application closes its windows for transport loss or an explicit core shutdown.
    #[cfg(test)]
    pub(crate) fn exits_ui(&self) -> bool {
        self.window_action() == WindowAction::Quit
    }
}

/// A typed failure from a view request.
#[derive(Debug, Snafu)]
pub enum RequestCoreError {
    #[snafu(display("UI pipe thread is unavailable"))]
    Closed,
    #[snafu(display("Quartz core is disconnected"))]
    Disconnected,
    #[snafu(display("UI request deadline expired"))]
    Timeout,
    #[snafu(display("UI request queue is full"))]
    Busy,
    #[snafu(display("could not call Quartz core"))]
    Call { source: CallCoreError },
}

/// Failure to create the dedicated pipe thread.
#[derive(Debug, Snafu)]
#[snafu(display("could not start the UI pipe thread"))]
pub struct StartLinkError {
    source: std::io::Error,
}

/// The foreground receiver was dropped, so the worker must close its pipe.
#[derive(Debug, Snafu)]
#[snafu(display("UI event receiver is closed"))]
#[cfg(windows)]
pub(super) struct SendUiEventError;

/// Failures while launching the core, waiting for readiness, or making its single connection attempt.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum StartCoreError {
    #[snafu(display("UI pipe thread is unavailable"))]
    Closed,
    #[snafu(display("could not launch Quartz core"))]
    Launch { source: std::io::Error },
    #[snafu(display("could not connect to the started Quartz core"))]
    Connect { source: CallCoreError },
}
impl StartCoreError {
    #[cfg(all(windows, not(test)))]
    fn launch() -> Result<(), Self> {
        let executable = std::env::current_exe().context(start_core_error::LaunchSnafu)?;
        std::process::Command::new(executable)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context(start_core_error::LaunchSnafu)?;
        Ok(())
    }
}

#[cfg(windows)]
type Reply = oneshot::Sender<Result<JsonValue, RequestCoreError>>;
#[cfg(windows)]
type ProgressObserver = watch::Sender<Option<QueryProgress>>;
#[cfg(windows)]
enum Command {
    SelectDetails {
        request: Option<DetailRequest>,
        generation: DetailGeneration,
        link: Link,
        reply: oneshot::Sender<Result<(), RequestCoreError>>,
    },
    LoadDay {
        request: DayRequest,
        generation: DayGeneration,
        mode: DayLoad,
        link: Link,
    },
    Request {
        op: Op,
        body: JsonValue,
        reply: Reply,
        progress: Option<ProgressObserver>,
    },
    Topics {
        change: TopicChange,
        topics: Vec<Topic>,
        reply: Reply,
    },
    Hidden,
    FirstFrame,
    Retry {
        reply: oneshot::Sender<Result<(), RequestCoreError>>,
    },
    StartCore {
        reply: oneshot::Sender<Result<(), StartCoreError>>,
    },
}
#[derive(Debug, Clone, Copy)]
#[cfg(windows)]
enum TopicChange {
    Subscribe,
    Unsubscribe,
}
#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
enum InitialConnection {
    Connect,
    AwaitAction,
}

/// A view-side handle; its futures only wait on bounded channels.
#[derive(Debug, Clone)]
#[cfg(windows)]
pub struct Link {
    commands: mpsc::Sender<Command>,
}
/// Unique event receiver; dropping it stops the worker and closes the pipe.
#[derive(Debug)]
#[cfg(windows)]
pub struct LinkEvents {
    receiver: mpsc::Receiver<LinkEvent>,
}
#[cfg(windows)]
impl LinkEvents {
    pub async fn recv(&mut self) -> Option<LinkEvent> {
        self.receiver.recv().await
    }
}
/// The handle and event receiver owned by the UI process.
#[derive(Debug)]
#[cfg(windows)]
pub struct LinkBridge {
    pub link: Link,
    pub events: LinkEvents,
}
#[cfg(windows)]
impl LinkBridge {
    /// Starts one connection attempt on a dedicated thread.
    pub fn start() -> Result<Self, StartLinkError> {
        Self::spawn(
            InitialConnection::Connect,
            quartz_protocol::UiState::Starting,
        )
    }
    /// Creates an idle bridge for a recoverable startup failure's actions.
    pub(crate) fn for_failure() -> Result<Self, StartLinkError> {
        Self::spawn(
            InitialConnection::AwaitAction,
            quartz_protocol::UiState::Starting,
        )
    }
    pub(crate) fn prewarm() -> Result<Self, StartLinkError> {
        Self::spawn(
            InitialConnection::Connect,
            quartz_protocol::UiState::Prewarming,
        )
    }
    fn spawn(
        initial: InitialConnection,
        state: quartz_protocol::UiState,
    ) -> Result<Self, StartLinkError> {
        let (commands, receive) = mpsc::channel(BRIDGE_CAPACITY);
        let (send, events) = mpsc::channel(BRIDGE_CAPACITY);
        std::thread::Builder::new()
            .name("quartz-ui-pipe".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .max_blocking_threads(UI_BLOCKING_THREADS)
                    .build()
                {
                    Ok(runtime) => runtime.block_on(
                        worker::Worker::new(receive, send, initial)
                            .with_hello_state(state)
                            .run(),
                    ),
                    Err(source) => {
                        let failure = Failure::from_connection(&CallCoreError::Runtime { source });
                        if send
                            .blocking_send(LinkEvent::Connection(ConnectionState::Failed(failure)))
                            .is_err()
                        {
                            tracing::debug!("UI closed before pipe runtime startup failure");
                        }
                    }
                }
            })
            .context(StartLinkSnafu)?;
        Ok(Self {
            link: Link { commands },
            events: LinkEvents { receiver: events },
        })
    }
}
#[cfg(windows)]
impl Link {
    #[cfg(test)]
    pub(crate) fn closed_for_test() -> Self {
        let (commands, receiver) = mpsc::channel(BRIDGE_CAPACITY);
        drop(receiver);
        Self { commands }
    }

    /// Queues the close notification before a subsequent show can acknowledge its first frame.
    pub(crate) fn hidden(&self) -> Result<(), RequestCoreError> {
        self.commands
            .try_send(Command::Hidden)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RequestCoreError::Busy,
                mpsc::error::TrySendError::Closed(_) => RequestCoreError::Closed,
            })
    }
    pub async fn select_details(
        &self,
        request: Option<DetailRequest>,
        generation: DetailGeneration,
    ) -> Result<(), RequestCoreError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::SelectDetails {
                request,
                generation,
                link: self.clone(),
                reply,
            })
            .await
            .map_err(|_| RequestCoreError::Closed)?;
        receive.await.map_err(|_| RequestCoreError::Closed)?
    }
    pub async fn load_day(
        &self,
        request: DayRequest,
        generation: DayGeneration,
        mode: DayLoad,
    ) -> Result<(), RequestCoreError> {
        self.commands
            .send(Command::LoadDay {
                request,
                generation,
                mode,
                link: self.clone(),
            })
            .await
            .map_err(|_| RequestCoreError::Closed)
    }
    /// Requests exactly one new connection attempt; there is no automatic retry.
    pub async fn retry(&self) -> Result<(), RequestCoreError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Retry { reply })
            .await
            .map_err(|_| RequestCoreError::Closed)?;
        receive.await.map_err(|_| RequestCoreError::Closed)?
    }
    /// Starts the core without a shell, waits for readiness, and makes one connection attempt.
    pub async fn start_core(&self) -> Result<(), StartCoreError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::StartCore { reply })
            .await
            .map_err(|_| StartCoreError::Closed)?;
        receive.await.map_err(|_| StartCoreError::Closed)?
    }
    /// Executes an operation with its statically associated response schema.
    pub async fn request<R: CoreRequest>(
        &self,
        request: R,
    ) -> Result<R::Response, RequestCoreError> {
        self.send_request(request, None).await
    }
    /// Executes an operation while forwarding core progress updates to `progress`.
    pub async fn request_with_progress<R: CoreRequest>(
        &self,
        request: R,
        progress: watch::Sender<Option<QueryProgress>>,
    ) -> Result<R::Response, RequestCoreError> {
        self.send_request(request, Some(progress)).await
    }
    async fn send_request<R: CoreRequest>(
        &self,
        request: R,
        progress: Option<ProgressObserver>,
    ) -> Result<R::Response, RequestCoreError> {
        let body = request
            .body()
            .map_err(|source| CallCoreError::EncodeBody { source })
            .context(CallSnafu)?;
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Request {
                op: R::OP,
                body,
                reply,
                progress,
            })
            .await
            .map_err(|_| RequestCoreError::Closed)?;
        let body = receive.await.map_err(|_| RequestCoreError::Closed)??;
        R::response(body)
            .map_err(|source| CallCoreError::DecodeBody { source })
            .context(CallSnafu)
    }
    /// Registers desired topics; changes while disconnected apply to the next explicit attempt.
    pub async fn subscribe(&self, topics: Vec<Topic>) -> Result<(), RequestCoreError> {
        self.topics(TopicChange::Subscribe, topics).await
    }
    /// Removes desired topics; changes while disconnected apply to the next explicit attempt.
    pub async fn unsubscribe(&self, topics: Vec<Topic>) -> Result<(), RequestCoreError> {
        self.topics(TopicChange::Unsubscribe, topics).await
    }
    async fn topics(
        &self,
        change: TopicChange,
        topics: Vec<Topic>,
    ) -> Result<(), RequestCoreError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Topics {
                change,
                topics,
                reply,
            })
            .await
            .map_err(|_| RequestCoreError::Closed)?;
        receive.await.map_err(|_| RequestCoreError::Closed)??;
        Ok(())
    }
    /// Queues the acknowledgement after GPUI's next-frame hook has fired.
    pub async fn first_frame(&self) -> Result<(), RequestCoreError> {
        self.commands
            .send(Command::FirstFrame)
            .await
            .map_err(|_| RequestCoreError::Closed)
    }
}

#[cfg(test)]
mod tests;
