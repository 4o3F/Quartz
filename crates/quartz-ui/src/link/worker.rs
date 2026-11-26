//! One-attempt connection lifecycle and request handling on the pipe thread.

use super::{
    BRIDGE_CAPACITY, Command, ConnectionState, InitialConnection, LinkEvent, Reply,
    RequestCoreError, SendUiEventError, StartCoreError, TopicChange,
};
use crate::failure::Failure;
use quartz_pipe::{AsyncCoreClient, CallCoreError, IncomingFrame, PipeClient, RESPONSE_TIMEOUT};
use quartz_protocol::{
    ControlBody, CoreRequest, HelloRole, JsonValue, Op, RequestId, Response, ResponseOutcome,
    Subscribe, Topic, UiFirstFrameRequest, UiHello, UiHiddenRequest, UiState, Unsubscribe,
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
    Request {
        op: Op,
        reply: Reply,
    },
    Topics {
        change: TopicChange,
        topics: Vec<Topic>,
        reply: Reply,
    },
    DaySubscribed {
        generation: super::DayGeneration,
    },
    ReconcileDays,
    FirstFrame,
    Hidden,
}
impl Completion {
    fn fail(self, error: RequestCoreError) {
        match self {
            Self::Request { reply, .. } | Self::Topics { reply, .. } => {
                if reply.send(Err(error)).is_err() {
                    tracing::debug!("UI request cancelled before failure delivery");
                }
            }
            Self::DaySubscribed { .. } | Self::ReconcileDays => {
                tracing::warn!(%error, "could not reconcile day subscriptions")
            }
            Self::Hidden => tracing::warn!(%error, "could not notify core that UI is hidden"),
            Self::FirstFrame => tracing::warn!(%error, "could not acknowledge the UI frame"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameStatus {
    Awaiting,
    Drawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HiddenNotice {
    Idle,
    Pending,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryVisibility {
    Visible,
    Hidden,
}
#[derive(Clone)]
struct DayIntent {
    request: super::DayRequest,
    generation: super::DayGeneration,
    mode: super::DayLoad,
    link: super::Link,
}
pub(super) struct Worker {
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<LinkEvent>,
    initial: InitialConnection,
    hello_state: UiState,
    topics: Vec<Topic>,
    frame: FrameStatus,
    hidden_notice: HiddenNotice,
    query_visibility: QueryVisibility,
    pending: BTreeMap<RequestId, Pending>,
    day: Option<super::day::DayLoader>,
    day_intent: Option<DayIntent>,
    day_subscription: Option<super::DayGeneration>,
    details: Option<super::details::DetailLoader>,
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
            hello_state: UiState::Starting,
            topics: vec![Topic::State],
            frame: FrameStatus::Awaiting,
            hidden_notice: HiddenNotice::Idle,
            query_visibility: QueryVisibility::Visible,
            pending: BTreeMap::new(),
            day: None,
            day_intent: None,
            day_subscription: None,
            details: None,
            #[cfg(test)]
            name: None,
            #[cfg(test)]
            launcher: None,
            #[cfg(test)]
            initial_failure: None,
        }
    }
    pub(super) fn with_hello_state(mut self, state: UiState) -> Self {
        self.hello_state = state;
        self.query_visibility = if state == UiState::Prewarming {
            QueryVisibility::Hidden
        } else {
            QueryVisibility::Visible
        };
        self
    }
    pub(super) async fn run(mut self) {
        let closed = self.events.clone();
        tokio::select! {
            () = closed.closed() => {},
            result = self.run_inner() => { if let Err(SendUiEventError) = result { tracing::debug!("UI event receiver closed"); } },
        }
        self.fail_pending();
    }
    async fn run_inner(&mut self) -> Result<(), SendUiEventError> {
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
                    state: self.hello_state,
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
    async fn handle_idle_command(
        &mut self,
        command: Command,
    ) -> Result<Option<Client>, SendUiEventError> {
        match command {
            Command::SelectDetails { reply, .. } => {
                if reply.send(Err(RequestCoreError::Disconnected)).is_err() {
                    tracing::debug!("detail selection cancelled");
                }
                Ok(None)
            }
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
                Self::reply_request(reply, Err(RequestCoreError::Disconnected));
                Ok(None)
            }
            Command::Hidden => {
                self.query_visibility = QueryVisibility::Hidden;
                self.frame = FrameStatus::Awaiting;
                tracing::warn!("discarded hidden notification while UI is disconnected");
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
            if self.hidden_notice == HiddenNotice::Pending && self.pending.len() < BRIDGE_CAPACITY {
                let body = (UiHiddenRequest {})
                    .body()
                    .map_err(|source| CallCoreError::EncodeBody { source })?;
                self.hidden_notice = HiddenNotice::Idle;
                self.submit(client, Op::UiHidden, body, Completion::Hidden)
                    .await?;
            }
            let deadline = self.pending.values().map(|pending| pending.deadline).min();
            tokio::select! {
                command = self.commands.recv(), if self.pending.len() < BRIDGE_CAPACITY => {
                    let Some(command) = command else { return Ok(()); };
                    self.handle_command(client, command).await?;
                }
                incoming = client.receive() => self.handle_frame(incoming).await?,
                () = Self::wait_deadline(deadline) => self.expire_requests(client).await?,
            }
        }
    }
    async fn handle_command(
        &mut self,
        client: &mut Client,
        command: Command,
    ) -> Result<(), CallCoreError> {
        match command {
            Command::SelectDetails {
                request,
                generation,
                link,
                reply,
            } => {
                let loader = self.details.get_or_insert_with(|| {
                    super::details::DetailLoader::start(link, self.events.clone())
                });
                let result = loader.select(super::details::DetailIntent {
                    request,
                    generation,
                });
                if reply.send(result).is_err() {
                    tracing::debug!("detail selection cancelled");
                }
            }
            Command::LoadDay {
                request,
                generation,
                mode,
                link,
            } => {
                if self.query_visibility == QueryVisibility::Hidden {
                    return Ok(());
                }
                if self
                    .day_intent
                    .as_ref()
                    .is_some_and(|intent| intent.generation > generation)
                {
                    return Ok(());
                }
                if self
                    .day_intent
                    .as_ref()
                    .is_some_and(|intent| intent.request.date != request.date)
                {
                    self.day = None;
                }
                self.day_intent = Some(DayIntent {
                    request,
                    generation,
                    mode,
                    link,
                });
                let previous: Vec<_> = self
                    .topics
                    .iter()
                    .filter(|topic| matches!(topic, Topic::Day(date) if *date != request.date))
                    .cloned()
                    .collect();
                self.remove_day_topics(client, previous).await?;
                if self.topics.contains(&Topic::Day(request.date)) {
                    if self.day_subscription.is_none() {
                        self.send_day(self.day_intent.clone().expect("load intent"))
                            .await;
                    }
                } else {
                    let body = Subscribe {
                        topics: vec![Topic::Day(request.date)],
                    }
                    .body()
                    .map_err(|source| CallCoreError::EncodeBody { source })?;
                    // Remember the intent before its response, so hide also removes in-flight subscriptions.
                    self.topics.push(Topic::Day(request.date));
                    self.day_subscription = Some(generation);
                    self.submit(
                        client,
                        Op::Subscribe,
                        body,
                        Completion::DaySubscribed { generation },
                    )
                    .await?;
                }
            }
            Command::Retry { reply } => Self::reply_retry(reply, Ok(())),
            Command::StartCore { reply } => Self::reply_start(reply, Ok(())),
            Command::Request { op, body, reply } => {
                self.submit(client, op, body, Completion::Request { op, reply })
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
                        Self::reply_request(
                            reply,
                            Err(RequestCoreError::Call {
                                source: CallCoreError::EncodeBody { source },
                            }),
                        );
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
            Command::Hidden => {
                self.query_visibility = QueryVisibility::Hidden;
                self.frame = FrameStatus::Awaiting;
                self.day = None;
                self.day_intent = None;
                self.day_subscription = None;
                self.details = None;
                let mut days: Vec<_> = self
                    .topics
                    .iter()
                    .filter(|topic| matches!(topic, Topic::Day(_)))
                    .cloned()
                    .collect();
                for pending in self.pending.values_mut() {
                    if let Completion::Topics {
                        change: TopicChange::Subscribe,
                        topics,
                        ..
                    } = &mut pending.completion
                    {
                        for topic in topics.iter().filter(|topic| matches!(topic, Topic::Day(_))) {
                            if !days.contains(topic) {
                                days.push(topic.clone());
                            }
                        }
                        // A late acknowledgement must not restore the removed dates locally.
                        topics.retain(|topic| !matches!(topic, Topic::Day(_)));
                    }
                }
                self.remove_day_topics(client, days).await?;
                self.hidden_notice = HiddenNotice::Pending;
            }
            Command::FirstFrame => {
                self.frame = FrameStatus::Drawn;
                self.first_frame(client).await?;
            }
        }
        Ok(())
    }
    async fn send_day(&mut self, intent: DayIntent) {
        let DayIntent {
            request,
            generation,
            mode,
            link,
        } = intent;
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
    async fn remove_day_topics(
        &mut self,
        client: &mut Client,
        topics: Vec<Topic>,
    ) -> Result<(), CallCoreError> {
        if topics.is_empty() {
            return Ok(());
        }
        let body = Unsubscribe {
            topics: topics.clone(),
        }
        .body()
        .map_err(|source| CallCoreError::EncodeBody { source })?;
        self.update_topics(TopicChange::Unsubscribe, topics);
        self.submit(client, Op::Unsubscribe, body, Completion::ReconcileDays)
            .await
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
            IncomingFrame::Response(response) => self.handle_response(response).await?,
            IncomingFrame::Event(event) => {
                let event = match event.topic {
                    Topic::State => serde_json::from_value(event.body).map(LinkEvent::State),
                    Topic::Day(_) => serde_json::from_value(event.body).map(LinkEvent::Day),
                    Topic::Control => serde_json::from_value::<ControlBody>(event.body)
                        .map(|body| LinkEvent::Control(body.op)),
                };
                match event {
                    Ok(event) => {
                        if matches!(
                            &event,
                            LinkEvent::Control(
                                quartz_protocol::ControlOp::UiShow
                                    | quartz_protocol::ControlOp::UiActivate
                            )
                        ) {
                            self.query_visibility = QueryVisibility::Visible;
                        }
                        if let LinkEvent::Day(changed) = &event
                            && let Some(loader) = &self.day
                        {
                            loader.changed(changed.date);
                        }
                        if let LinkEvent::Day(changed) = &event
                            && let Some(loader) = &self.details
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
    async fn handle_response(&mut self, response: Response) -> Result<(), CallCoreError> {
        let Some(id) = response.id else {
            tracing::warn!("discarded response without request identity");
            return Ok(());
        };
        let Some(pending) = self.pending.remove(&id) else {
            tracing::debug!(?id, "discarded response for retired UI request");
            return Ok(());
        };
        let result = match response.outcome {
            ResponseOutcome::Ok { body } => Ok(body),
            ResponseOutcome::Err { error } => Err(RequestCoreError::Call {
                source: CallCoreError::Protocol { kind: error.kind },
            }),
        };
        self.complete(pending.completion, result).await
    }
    async fn complete(
        &mut self,
        completion: Completion,
        result: Result<JsonValue, RequestCoreError>,
    ) -> Result<(), CallCoreError> {
        match completion {
            Completion::Request { op, reply } => {
                if result.is_ok()
                    && matches!(
                        op,
                        Op::AnnotationsCreate
                            | Op::AnnotationsUpdate
                            | Op::AnnotationsDelete
                            | Op::TrackingResume
                            | Op::RuleSetsSetEnabled
                            | Op::RuleSetsDelete
                    )
                    && let Some(loader) = &self.day
                {
                    loader.invalidate();
                }
                Self::reply_request(reply, result);
            }
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
            Completion::DaySubscribed { generation } => {
                if self.day_subscription == Some(generation) {
                    self.day_subscription = None;
                    if let Some(intent) = self.day_intent.clone() {
                        match result {
                            Ok(_) => self.send_day(intent).await,
                            Err(source) => {
                                self.topics
                                    .retain(|topic| *topic != Topic::Day(intent.request.date));
                                let _ = self
                                    .emit(LinkEvent::DayLoaded {
                                        generation: intent.generation,
                                        result: Err(source.into()),
                                    })
                                    .await;
                            }
                        }
                    }
                }
            }
            Completion::ReconcileDays => {
                if let Err(error) = result {
                    tracing::warn!(%error, "could not remove day subscriptions; closing the pipe to release them");
                    return Err(CallCoreError::DiscardConnection);
                }
            }
            Completion::Hidden => {
                if let Err(error) = result {
                    tracing::warn!(%error, "core rejected UI hidden notification");
                }
            }
            Completion::FirstFrame => {
                if let Err(error) = result {
                    tracing::warn!(%error, "core rejected UI first-frame acknowledgement");
                }
            }
        }
        Ok(())
    }
    async fn expire_requests(&mut self, client: &mut Client) -> Result<(), CallCoreError> {
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
            self.complete(pending.completion, Err(RequestCoreError::Timeout))
                .await?;
        }
        Ok(())
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
            self.complete(completion, Err(RequestCoreError::Busy))
                .await?;
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
                self.complete(completion, Err(RequestCoreError::Call { source: error }))
                    .await?;
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
    async fn report_failure(&self, failure: Failure) -> Result<(), SendUiEventError> {
        self.emit(LinkEvent::Connection(ConnectionState::Failed(failure)))
            .await
    }
    async fn emit(&self, event: LinkEvent) -> Result<(), SendUiEventError> {
        self.events.send(event).await.map_err(|_| SendUiEventError)
    }
}
