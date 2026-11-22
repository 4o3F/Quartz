//! One-attempt connection lifecycle and request handling on the pipe thread.

use super::{
    BRIDGE_CAPACITY, Command, ConnectionState, InitialConnection, LinkEvent, Reply,
    RequestCoreError, StartCoreError, TopicChange, UiClosed,
};
use crate::failure::Failure;
use quartz_pipe::{AsyncCoreClient, CallCoreError, IncomingFrame, PipeClient, RESPONSE_TIMEOUT};
use quartz_protocol::{
    ControlBody, CoreRequest, HelloRole, JsonValue, Op, RequestId, Response, ResponseOutcome,
    Subscribe, Topic, UiFirstFrameRequest, UiHello, UiState, Unsubscribe,
};
use std::collections::BTreeMap;
use tokio::{
    net::windows::named_pipe::NamedPipeClient,
    sync::{mpsc, oneshot},
    time::Instant,
};

type Client = AsyncCoreClient<NamedPipeClient>;
struct Pending {
    deadline: Instant,
    completion: Completion,
}
enum Completion {
    Request(Reply),
    Topics {
        change: TopicChange,
        topics: Vec<Topic>,
        reply: Reply,
    },
    FirstFrame,
}
impl Completion {
    fn fail(self, error: RequestCoreError) {
        match self {
            Self::Request(reply) | Self::Topics { reply, .. } => {
                if reply.send(Err(error)).is_err() {
                    tracing::debug!("UI request cancelled before failure delivery");
                }
            }
            Self::FirstFrame => tracing::warn!(%error, "could not acknowledge the UI frame"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameStatus {
    Awaiting,
    Drawn,
}

pub(super) struct Worker {
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<LinkEvent>,
    initial: InitialConnection,
    topics: Vec<Topic>,
    frame: FrameStatus,
    pending: BTreeMap<RequestId, Pending>,
    day: Option<super::day::DayLoader>,
    #[cfg(test)]
    pub(super) name: Option<quartz_pipe::PipeName>,
    #[cfg(test)]
    pub(super) launcher: Option<oneshot::Sender<()>>,
    #[cfg(test)]
    pub(super) initial_failure: Option<CallCoreError>,
}
impl Worker {
    pub(super) fn new(
        commands: mpsc::Receiver<Command>,
        events: mpsc::Sender<LinkEvent>,
        initial: InitialConnection,
    ) -> Self {
        Self {
            commands,
            events,
            initial,
            topics: vec![Topic::State],
            frame: FrameStatus::Awaiting,
            pending: BTreeMap::new(),
            day: None,
            #[cfg(test)]
            name: None,
            #[cfg(test)]
            launcher: None,
            #[cfg(test)]
            initial_failure: None,
        }
    }
    pub(super) async fn run(mut self) {
        let closed = self.events.clone();
        tokio::select! {
            () = closed.closed() => {},
            result = self.run_inner() => { if let Err(UiClosed) = result { tracing::debug!("UI event receiver closed"); } },
        }
        self.fail_pending();
    }
    async fn run_inner(&mut self) -> Result<(), UiClosed> {
        let mut client = match self.initial {
            InitialConnection::Connect => {
                self.emit(LinkEvent::Connection(ConnectionState::Connecting))
                    .await?;
                match self.connect().await {
                    Ok(client) => Some(client),
                    Err(error) => {
                        self.report_failure(Failure::from_connection(&error))
                            .await?;
                        None
                    }
                }
            }
            InitialConnection::AwaitAction => None,
        };
        loop {
            if let Some(mut connection) = client.take() {
                if let Err(error) = self.connected(&mut connection).await {
                    tracing::debug!(%error, "UI pipe transport ended");
                    self.fail_pending();
                    self.emit(LinkEvent::Disconnected).await?;
                }
                return Ok(());
            }
            let Some(command) = self.commands.recv().await else {
                return Ok(());
            };
            client = self.handle_idle_command(command).await?;
        }
    }
    async fn connect(&mut self) -> Result<Client, CallCoreError> {
        #[cfg(test)]
        if let Some(error) = self.initial_failure.take() {
            return Err(error);
        }
        #[cfg(test)]
        let pipe = PipeClient::open_named(
            self.name
                .as_ref()
                .expect("link tests require an isolated endpoint"),
        )
        .await;
        #[cfg(not(test))]
        let pipe = PipeClient::connect().await;
        let mut client = Client::new(
            pipe.map_err(|source| CallCoreError::Open { source })?
                .into_inner(),
        );
        let hello = client
            .handshake(HelloRole::Ui {
                ui: UiHello {
                    state: UiState::Starting,
                },
            })
            .await?;
        client
            .call(&Subscribe {
                topics: self.topics.clone(),
            })
            .await?;
        if self.frame == FrameStatus::Drawn {
            self.first_frame(&mut client).await?;
        }
        if self
            .emit(LinkEvent::Connection(ConnectionState::Connected {
                instance_id: hello.instance_id,
                core_version: hello.core_version,
            }))
            .await
            .is_err()
        {
            self.commands.close();
        }
        Ok(client)
    }
    async fn handle_idle_command(&mut self, command: Command) -> Result<Option<Client>, UiClosed> {
        match command {
            Command::LoadDay { generation, .. } => {
                self.emit(LinkEvent::DayLoaded {
                    generation,
                    result: Err(RequestCoreError::Disconnected.into()),
                })
                .await?;
                Ok(None)
            }
            Command::Retry { reply } => {
                self.emit(LinkEvent::Connection(ConnectionState::Connecting))
                    .await?;
                match self.connect().await {
                    Ok(client) => {
                        Self::reply_retry(reply, Ok(()));
                        Ok(Some(client))
                    }
                    Err(error) => {
                        self.report_failure(Failure::from_connection(&error))
                            .await?;
                        Self::reply_retry(reply, Err(RequestCoreError::Call { source: error }));
                        Ok(None)
                    }
                }
            }
            Command::StartCore { reply } => {
                self.emit(LinkEvent::Connection(ConnectionState::Connecting))
                    .await?;
                match self.start_core().await {
                    Ok(client) => {
                        Self::reply_start(reply, Ok(()));
                        Ok(Some(client))
                    }
                    Err(error) => {
                        self.report_failure(Failure::from_start(&error)).await?;
                        Self::reply_start(reply, Err(error));
                        Ok(None)
                    }
                }
            }
            Command::Request { reply, .. } => {
                Completion::Request(reply).fail(RequestCoreError::Disconnected);
                Ok(None)
            }
            Command::FirstFrame => {
                self.frame = FrameStatus::Drawn;
                Ok(None)
            }
            Command::Topics {
                change,
                topics,
                reply,
            } => {
                self.update_topics(change, topics);
                Self::reply_request(reply, Ok(JsonValue::Object(Default::default())));
                Ok(None)
            }
        }
    }
    async fn start_core(&mut self) -> Result<Client, StartCoreError> {
        let waiter = self
            .prepare_wait()
            .map_err(|source| StartCoreError::Connect { source })?;
        #[cfg(test)]
        match self.launcher.take() {
            Some(launched) => {
                launched.send(()).map_err(|_| StartCoreError::Closed)?;
            }
            None => return Err(StartCoreError::Closed),
        }
        #[cfg(not(test))]
        StartCoreError::launch()?;
        waiter
            .wait()
            .await
            .map_err(|source| StartCoreError::Connect {
                source: CallCoreError::Wait { source },
            })?;
        self.connect()
            .await
            .map_err(|source| StartCoreError::Connect { source })
    }
    fn prepare_wait(&self) -> Result<quartz_pipe::CoreReadyWaiter, CallCoreError> {
        #[cfg(test)]
        let name = self
            .name
            .as_ref()
            .expect("link tests require an isolated endpoint")
            .clone();
        #[cfg(not(test))]
        let name = quartz_pipe::ProcessIdentity::current()
            .map_err(|source| CallCoreError::Open {
                source: quartz_pipe::OpenCorePipeError::ServerIdentity { source },
            })?
            .pipe_name();
        quartz_pipe::CoreReadyWaiter::new(&name).map_err(|source| CallCoreError::Wait { source })
    }
    async fn connected(&mut self, client: &mut Client) -> Result<(), CallCoreError> {
        loop {
            let deadline = self.pending.values().map(|pending| pending.deadline).min();
            tokio::select! {
                command = self.commands.recv(), if self.pending.len() < BRIDGE_CAPACITY => {
                    let Some(command) = command else { return Ok(()); };
                    self.handle_command(client, command).await?;
                }
                incoming = client.receive() => self.handle_frame(incoming).await?,
                () = Self::wait_deadline(deadline) => self.expire_requests(client),
            }
        }
    }
    async fn handle_command(
        &mut self,
        client: &mut Client,
        command: Command,
    ) -> Result<(), CallCoreError> {
        match command {
            Command::LoadDay {
                request,
                generation,
                mode,
                link,
            } => {
                let loader = self
                    .day
                    .get_or_insert_with(|| super::day::DayLoader::start(link, self.events.clone()));
                if let Err(source) = loader.send(super::day::DayCommand::Load {
                    request,
                    generation,
                    mode,
                }) {
                    let _ = self
                        .emit(LinkEvent::DayLoaded {
                            generation,
                            result: Err(source.into()),
                        })
                        .await;
                }
            }
            Command::Retry { reply } => Self::reply_retry(reply, Ok(())),
            Command::StartCore { reply } => Self::reply_start(reply, Ok(())),
            Command::Request { op, body, reply } => {
                self.submit(client, op, body, Completion::Request(reply))
                    .await?
            }
            Command::Topics {
                change,
                topics,
                reply,
            } => {
                let body = match change {
                    TopicChange::Subscribe => Subscribe {
                        topics: topics.clone(),
                    }
                    .body(),
                    TopicChange::Unsubscribe => Unsubscribe {
                        topics: topics.clone(),
                    }
                    .body(),
                };
                let body = match body {
                    Ok(body) => body,
                    Err(source) => {
                        Completion::Request(reply).fail(RequestCoreError::Call {
                            source: CallCoreError::EncodeBody { source },
                        });
                        return Ok(());
                    }
                };
                let op = match change {
                    TopicChange::Subscribe => Op::Subscribe,
                    TopicChange::Unsubscribe => Op::Unsubscribe,
                };
                self.submit(
                    client,
                    op,
                    body,
                    Completion::Topics {
                        change,
                        topics,
                        reply,
                    },
                )
                .await?;
            }
            Command::FirstFrame => {
                self.frame = FrameStatus::Drawn;
                self.first_frame(client).await?;
            }
        }
        Ok(())
    }
    async fn first_frame(&mut self, client: &mut Client) -> Result<(), CallCoreError> {
        let body = match (UiFirstFrameRequest {}).body() {
            Ok(body) => body,
            Err(source) => {
                Completion::FirstFrame.fail(RequestCoreError::Call {
                    source: CallCoreError::EncodeBody { source },
                });
                return Ok(());
            }
        };
        self.submit(client, Op::UiFirstFrame, body, Completion::FirstFrame)
            .await
    }
    async fn handle_frame(
        &mut self,
        incoming: Result<IncomingFrame, CallCoreError>,
    ) -> Result<(), CallCoreError> {
        let incoming = match incoming {
            Ok(frame) => frame,
            Err(error) if error.ends_connection() => return Err(error),
            Err(error) => {
                tracing::warn!(%error, "discarded invalid core frame");
                return Ok(());
            }
        };
        match incoming {
            IncomingFrame::Response(response) => self.handle_response(response),
            IncomingFrame::Event(event) => {
                let event = match event.topic {
                    Topic::State => serde_json::from_value(event.body).map(LinkEvent::State),
                    Topic::Day(_) => serde_json::from_value(event.body).map(LinkEvent::Day),
                    Topic::Control => serde_json::from_value::<ControlBody>(event.body)
                        .map(|body| LinkEvent::Control(body.op)),
                };
                match event {
                    Ok(event) => {
                        if let LinkEvent::Day(changed) = &event
                            && let Some(loader) = &self.day
                        {
                            loader.changed(changed.date);
                        }
                        if self.emit(event).await.is_err() {
                            self.commands.close();
                        }
                    }
                    Err(_) => tracing::warn!("discarded invalid core event body"),
                }
            }
        }
        Ok(())
    }
    fn handle_response(&mut self, response: Response) {
        let Some(id) = response.id else {
            tracing::warn!("discarded response without request identity");
            return;
        };
        let Some(pending) = self.pending.remove(&id) else {
            tracing::debug!(?id, "discarded response for retired UI request");
            return;
        };
        let result = match response.outcome {
            ResponseOutcome::Ok { body } => Ok(body),
            ResponseOutcome::Err { error } => Err(RequestCoreError::Call {
                source: CallCoreError::Protocol { kind: error.kind },
            }),
        };
        match pending.completion {
            Completion::Request(reply) => Self::reply_request(reply, result),
            Completion::Topics {
                change,
                topics,
                reply,
            } => {
                if result.is_ok() {
                    self.update_topics(change, topics);
                }
                Self::reply_request(reply, result);
            }
            Completion::FirstFrame => {
                if let Err(error) = result {
                    tracing::warn!(%error, "core rejected UI first-frame acknowledgement");
                }
            }
        }
    }
    fn expire_requests(&mut self, client: &mut Client) {
        let now = Instant::now();
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|entry| entry.1.deadline <= now)
            .map(|entry| *entry.0)
            .collect();
        for id in expired {
            let pending = self
                .pending
                .remove(&id)
                .expect("expired request is pending");
            client.retire_request(id);
            pending.completion.fail(RequestCoreError::Timeout);
        }
    }
    async fn wait_deadline(deadline: Option<Instant>) {
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    }
    async fn submit(
        &mut self,
        client: &mut Client,
        op: Op,
        body: JsonValue,
        completion: Completion,
    ) -> Result<(), CallCoreError> {
        if self.pending.len() == BRIDGE_CAPACITY {
            completion.fail(RequestCoreError::Busy);
            return Ok(());
        }
        match client.send_request(op, body).await {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending {
                        deadline: Instant::now() + RESPONSE_TIMEOUT,
                        completion,
                    },
                );
                Ok(())
            }
            Err(error) if error.ends_connection() => {
                completion.fail(RequestCoreError::Disconnected);
                Err(error)
            }
            Err(error) => {
                completion.fail(RequestCoreError::Call { source: error });
                Ok(())
            }
        }
    }
    fn reply_request(reply: Reply, result: Result<JsonValue, RequestCoreError>) {
        if reply.send(result).is_err() {
            tracing::debug!("UI request cancelled");
        }
    }
    fn reply_retry(
        reply: oneshot::Sender<Result<(), RequestCoreError>>,
        result: Result<(), RequestCoreError>,
    ) {
        if reply.send(result).is_err() {
            tracing::debug!("UI closed before retry result");
        }
    }
    fn reply_start(
        reply: oneshot::Sender<Result<(), StartCoreError>>,
        result: Result<(), StartCoreError>,
    ) {
        if reply.send(result).is_err() {
            tracing::debug!("UI closed before core startup result");
        }
    }
    fn fail_pending(&mut self) {
        for pending in std::mem::take(&mut self.pending).into_values() {
            pending.completion.fail(RequestCoreError::Disconnected);
        }
    }
    fn update_topics(&mut self, change: TopicChange, topics: Vec<Topic>) {
        for topic in topics {
            match change {
                TopicChange::Subscribe if !self.topics.contains(&topic) => self.topics.push(topic),
                TopicChange::Unsubscribe => self.topics.retain(|active| *active != topic),
                TopicChange::Subscribe => {}
            }
        }
    }
    async fn report_failure(&self, failure: Failure) -> Result<(), UiClosed> {
        self.emit(LinkEvent::Connection(ConnectionState::Failed(failure)))
            .await
    }
    async fn emit(&self, event: LinkEvent) -> Result<(), UiClosed> {
        self.events.send(event).await.map_err(|_| UiClosed)
    }
}
