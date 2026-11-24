//! Connection registry actor; fan-out never awaits a client writer.

use super::{
    ProcessId, PublishedStatus, UiCommand, UiPort,
    outbox::{OutboxHandle, QueueFrameError},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use quartz_protocol::{ControlBody, ControlOp, Response, Role, Subscribe, Topic};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{mpsc, oneshot, watch};

/// Bounded registry commands on the existing I/O runtime (01 §6, 04 §7).
const REGISTRY_MAILBOX_CAPACITY: usize = 64;
/// Reconciliation failures retry after one second without blocking registry controls.
const RECONCILE_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ConnectionId(u64);
impl ConnectionId {
    #[cfg(test)]
    pub(crate) const fn for_test(value: u64) -> Self {
        Self(value)
    }
}

/// Kernel-authenticated identity and the UI phase declared in hello.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UiPeer {
    pub pid: ProcessId,
    pub state: quartz_protocol::UiState,
}

/// Authenticated role metadata; a UI registration always includes its PID and hello phase.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ConnectionPeer {
    Ui(UiPeer),
    Cli,
    Host,
}
impl ConnectionPeer {
    pub(crate) fn role(self) -> Role {
        match self {
            Self::Ui(_) => Role::Ui,
            Self::Cli => Role::Cli,
            Self::Host => Role::Host,
        }
    }
}

enum RegistryMessage {
    Register {
        id: ConnectionId,
        peer: ConnectionPeer,
        outbox: OutboxHandle,
        registered_at: std::time::Instant,
    },
    Unregister {
        id: ConnectionId,
    },
    Subscribe {
        id: ConnectionId,
        subscription: Subscribe,
        response: Response,
        reply: oneshot::Sender<Result<(), QueueFrameError>>,
    },
    Unsubscribe {
        id: ConnectionId,
        subscription: quartz_protocol::Unsubscribe,
        reply: oneshot::Sender<()>,
    },
    Control(ControlOp),
    TargetControl {
        connection: ConnectionId,
        op: ControlOp,
    },
    Outboxes {
        reply: oneshot::Sender<Vec<OutboxHandle>>,
    },
}

#[derive(Clone)]
pub(crate) struct RegistryHandle {
    mailbox: mpsc::Sender<RegistryMessage>,
    next_id: Arc<AtomicU64>,
}
#[derive(Debug, snafu::Snafu)]
#[snafu(display("connection registry is unavailable"))]
pub(crate) struct QueueRegistryError;

impl RegistryHandle {
    pub(crate) fn start(
        status: watch::Receiver<PublishedStatus>,
        versions: crate::query::versions::VersionPort,
        ui: UiPort,
    ) -> Self {
        let (mailbox, receiver) = mpsc::channel(REGISTRY_MAILBOX_CAPACITY);
        let handle = Self {
            mailbox,
            next_id: Arc::new(AtomicU64::new(0)),
        };
        tokio::spawn(
            ConnectionRegistry {
                receiver,
                ui,
                status,
                versions,
                connections: BTreeMap::new(),
                pending: VecDeque::new(),
                versions_in_flight: FuturesUnordered::new(),
                reconciliation: ReconcileVersions::Current,
            }
            .run(),
        );
        handle
    }
    fn send(&self, message: RegistryMessage) -> Result<(), QueueRegistryError> {
        self.mailbox.try_send(message).map_err(|error| {
            tracing::warn!(%error, "connection registry mailbox unavailable");
            QueueRegistryError
        })
    }
    pub(crate) fn register(
        &self,
        peer: ConnectionPeer,
        outbox: OutboxHandle,
    ) -> Result<Registration, QueueRegistryError> {
        let id = ConnectionId(
            self.next_id
                .update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value
                        .checked_add(1)
                        .expect("u64 counter cannot overflow in practice")
                }),
        );
        self.send(RegistryMessage::Register {
            id,
            peer,
            outbox,
            registered_at: std::time::Instant::now(),
        })?;
        Ok(Registration {
            id,
            registry: self.clone(),
        })
    }
    pub(crate) fn submit_unsubscribe(
        &self,
        id: ConnectionId,
        subscription: quartz_protocol::Unsubscribe,
    ) -> Result<oneshot::Receiver<()>, QueueRegistryError> {
        let (reply, received) = oneshot::channel();
        self.send(RegistryMessage::Unsubscribe {
            id,
            subscription,
            reply,
        })?;
        Ok(received)
    }
    pub(crate) fn submit_subscribe(
        &self,
        id: ConnectionId,
        subscription: Subscribe,
        response: Response,
    ) -> Result<oneshot::Receiver<Result<(), QueueFrameError>>, QueueFrameError> {
        let (reply, received) = oneshot::channel();
        self.send(RegistryMessage::Subscribe {
            id,
            subscription,
            response,
            reply,
        })
        .map_err(|_| QueueFrameError::Closed)?;
        Ok(received)
    }
    #[cfg(test)]
    pub(crate) async fn subscribe(
        &self,
        id: ConnectionId,
        subscription: Subscribe,
        response: Response,
    ) -> Result<(), QueueFrameError> {
        self.submit_subscribe(id, subscription, response)?
            .await
            .map_err(|_| QueueFrameError::Closed)?
    }
    pub(crate) fn target_control(
        &self,
        connection: ConnectionId,
        op: ControlOp,
    ) -> Result<(), QueueRegistryError> {
        self.send(RegistryMessage::TargetControl { connection, op })
    }
    pub(crate) fn control(&self, op: ControlOp) -> Result<(), QueueRegistryError> {
        self.send(RegistryMessage::Control(op))
    }
    pub(crate) async fn outboxes(&self) -> Result<Vec<OutboxHandle>, QueueRegistryError> {
        let (reply, received) = oneshot::channel();
        self.send(RegistryMessage::Outboxes { reply })?;
        received.await.map_err(|_| QueueRegistryError)
    }
}

pub(crate) struct Registration {
    pub(crate) id: ConnectionId,
    registry: RegistryHandle,
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Err(error) = self
            .registry
            .send(RegistryMessage::Unregister { id: self.id })
        {
            // Closed outboxes are also removed on every registry turn.
            tracing::debug!(%error, "connection unregister deferred until registry cleanup");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateSubscription {
    Absent,
    Subscribed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionPublisher {
    Active,
    Stopped,
}
struct Connection {
    role: Role,
    state: StateSubscription,
    days: BTreeMap<quartz_protocol::LocalDate, quartz_protocol::DayChanged>,
    outbox: OutboxHandle,
}
impl Connection {
    fn deliver_control(&self, op: ControlOp) {
        let body = ControlBody {
            op,
            fields: serde_json::Map::new(),
        };
        if let Err(error) = self.outbox.control(body) {
            tracing::debug!(%error, "control delivery closed slow connection");
            self.outbox.close();
        }
    }
}
struct ConnectionRegistry {
    ui: UiPort,
    receiver: mpsc::Receiver<RegistryMessage>,
    status: watch::Receiver<PublishedStatus>,
    versions: crate::query::versions::VersionPort,
    connections: BTreeMap<ConnectionId, Connection>,
    pending: VecDeque<SubscriptionChange>,
    versions_in_flight: FuturesUnordered<VersionFlight>,
    reconciliation: ReconcileVersions,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReconcileVersions {
    Current,
    Required,
    RetryAt(tokio::time::Instant),
}
struct PendingSubscription {
    id: ConnectionId,
    subscription: Subscribe,
    response: Response,
    reply: oneshot::Sender<Result<(), QueueFrameError>>,
}
enum SubscriptionChange {
    Subscribe(PendingSubscription),
    Unsubscribe {
        id: ConnectionId,
        subscription: quartz_protocol::Unsubscribe,
        reply: oneshot::Sender<()>,
    },
}
enum VersionAction {
    Subscribe(PendingSubscription),
    Reconcile,
}
struct VersionCompletion {
    action: VersionAction,
    result: Result<crate::query::versions::PublishedVersions, quartz_protocol::ProtocolError>,
}
type VersionFlight = std::pin::Pin<Box<dyn std::future::Future<Output = VersionCompletion> + Send>>;

impl ConnectionRegistry {
    async fn run(mut self) {
        let mut publisher = VersionPublisher::Active;
        loop {
            let mut removed_days = std::collections::BTreeSet::new();
            self.connections.retain(|id, connection| {
                if connection.outbox.is_closed() {
                    if connection.role == Role::Ui
                        && let Err(error) = (self.ui)(UiCommand::Unregistered { connection: *id })
                    {
                        tracing::warn!(%error, "could not report UI disconnection");
                    }
                    removed_days.extend(connection.days.keys().copied());
                    false
                } else {
                    true
                }
            });
            self.prune_days(removed_days);
            self.advance_subscriptions();
            let reconciliation = self.reconciliation;
            tokio::select! {
                () = Self::wait_reconciliation(reconciliation) => { self.reconciliation = ReconcileVersions::Required; }
                completed = self.versions_in_flight.next(), if !self.versions_in_flight.is_empty() => {
                    let completed = completed.expect("version request exists");
                    self.complete_versions(completed);
                }
                message = self.receiver.recv() => match message { Some(message) => self.receive(message), None => return },
                changed = self.status.changed() => {
                    if let Err(error) = changed { tracing::debug!(%error, "main status publisher stopped"); return; }
                    self.publish_state();
                }
                changed = self.versions.updates.changed(), if publisher == VersionPublisher::Active => {
                    match changed {
                        Ok(()) => self.update_days(),
                        Err(error) => {
                            tracing::debug!(%error, "day version publisher stopped");
                            publisher = VersionPublisher::Stopped;
                        }
                    }
                }
            }
        }
    }
    fn update_days(&mut self) {
        // Copy only the subscribed-date digests before delivering to connection outboxes.
        let state = self.versions.updates.borrow().clone();
        for connection in self.connections.values_mut() {
            for (date, latest) in &mut connection.days {
                match state.days.get(date) {
                    Some(changed) => {
                        if changed.versions != latest.versions {
                            connection.outbox.day(changed.clone());
                            *latest = changed.clone();
                        }
                    }
                    None => {
                        tracing::error!(%date, "subscribed day missing from version publication");
                        connection.outbox.close();
                    }
                }
            }
        }
    }
    fn receive(&mut self, message: RegistryMessage) {
        match message {
            RegistryMessage::Register {
                id,
                peer,
                outbox,
                registered_at,
            } => {
                if let ConnectionPeer::Ui(ui) = peer
                    && let Err(error) = (self.ui)(UiCommand::Registered {
                        connection: id,
                        pid: ui.pid,
                        state: ui.state,
                        registered_at,
                    })
                {
                    tracing::warn!(%error, "could not report UI registration");
                    outbox.close();
                    return;
                }
                self.connections.insert(
                    id,
                    Connection {
                        role: peer.role(),
                        state: StateSubscription::Absent,
                        days: BTreeMap::new(),
                        outbox,
                    },
                );
            }
            RegistryMessage::Unregister { id } => {
                if let Some(connection) = self.connections.remove(&id) {
                    if connection.role == Role::Ui
                        && let Err(error) = (self.ui)(UiCommand::Unregistered { connection: id })
                    {
                        tracing::warn!(%error, "could not report UI disconnection");
                    }
                    self.prune_days(connection.days.into_keys().collect());
                }
            }
            RegistryMessage::Subscribe {
                id,
                subscription,
                response,
                reply,
            } => {
                self.queue_change(SubscriptionChange::Subscribe(PendingSubscription {
                    id,
                    subscription,
                    response,
                    reply,
                }));
            }
            RegistryMessage::Unsubscribe {
                id,
                subscription,
                reply,
            } => {
                self.queue_change(SubscriptionChange::Unsubscribe {
                    id,
                    subscription,
                    reply,
                });
            }
            RegistryMessage::TargetControl { connection, op } => {
                if !matches!(
                    op,
                    ControlOp::UiShow | ControlOp::UiActivate | ControlOp::UiShutdown
                ) {
                    tracing::warn!(?op, "rejected unsupported targeted control");
                    return;
                }
                match self
                    .connections
                    .get(&connection)
                    .filter(|peer| peer.role == Role::Ui)
                {
                    Some(peer) => peer.deliver_control(op),
                    None => tracing::warn!(
                        ?connection,
                        "UI control destination is no longer registered"
                    ),
                }
            }
            RegistryMessage::Control(op) => {
                if op != ControlOp::UiShutdown {
                    tracing::warn!(?op, "rejected unsupported broadcast control");
                    return;
                }
                for connection in self
                    .connections
                    .values()
                    .filter(|peer| peer.role == Role::Ui)
                {
                    connection.deliver_control(op);
                }
            }
            RegistryMessage::Outboxes { reply } => {
                if reply
                    .send(
                        self.connections
                            .values()
                            .map(|connection| connection.outbox.clone())
                            .collect(),
                    )
                    .is_err()
                {
                    tracing::debug!("registry barrier caller left");
                }
            }
        }
        self.advance_subscriptions();
    }
    fn queue_change(&mut self, change: SubscriptionChange) {
        // Waiting for a digest must not turn the bounded mailbox into an unbounded backlog.
        if self.pending.len() < REGISTRY_MAILBOX_CAPACITY {
            self.pending.push_back(change);
            return;
        }
        match change {
            SubscriptionChange::Subscribe(pending) => {
                let result = self
                    .connections
                    .get(&pending.id)
                    .ok_or(QueueFrameError::Closed)
                    .and_then(|connection| {
                        connection.outbox.response(Response {
                            id: pending.response.id,
                            outcome: quartz_protocol::ResponseOutcome::Err {
                                error: quartz_protocol::ProtocolError::new(
                                    quartz_protocol::ErrorKind::Busy,
                                ),
                            },
                        })
                    });
                let _ = pending.reply.send(result);
            }
            SubscriptionChange::Unsubscribe { id, .. } => {
                if let Some(connection) = self.connections.get(&id) {
                    connection.outbox.close();
                }
            }
        }
    }
    fn desired_dates(&self) -> BTreeSet<quartz_protocol::LocalDate> {
        self.connections
            .values()
            .flat_map(|connection| connection.days.keys().copied())
            .collect()
    }
    fn request_versions(
        &mut self,
        dates: BTreeSet<quartz_protocol::LocalDate>,
        action: VersionAction,
    ) {
        let submitted = self.versions.submit_days(dates);
        self.versions_in_flight.push(Box::pin(async move {
            let result = match submitted {
                Ok(received) => received.await.unwrap_or_else(|_| {
                    Err(quartz_protocol::ProtocolError::new(
                        quartz_protocol::ErrorKind::Internal,
                    ))
                }),
                Err(error) => Err(error),
            };
            VersionCompletion { result, action }
        }));
    }

    async fn wait_reconciliation(state: ReconcileVersions) {
        match state {
            ReconcileVersions::RetryAt(at) => tokio::time::sleep_until(at).await,
            ReconcileVersions::Current | ReconcileVersions::Required => {
                std::future::pending().await
            }
        }
    }
    fn advance_subscriptions(&mut self) {
        while self.versions_in_flight.is_empty() {
            if self.reconciliation == ReconcileVersions::Required {
                self.reconciliation = ReconcileVersions::Current;
                self.request_versions(self.desired_dates(), VersionAction::Reconcile);
                return;
            }
            let Some(change) = self.pending.pop_front() else {
                return;
            };
            match change {
                SubscriptionChange::Subscribe(subscription) => {
                    let Some(connection) = self.connections.get(&subscription.id) else {
                        let _ = subscription.reply.send(Err(QueueFrameError::Closed));
                        continue;
                    };
                    if connection.outbox.is_closed() {
                        let _ = subscription.reply.send(Err(QueueFrameError::Closed));
                        continue;
                    }
                    let mut dates = self.desired_dates();
                    let needs_days =
                        subscription
                            .subscription
                            .topics
                            .iter()
                            .any(|topic| match topic {
                                Topic::Day(date) => !dates.contains(date),
                                _ => false,
                            });
                    if needs_days {
                        dates.extend(subscription.subscription.topics.iter().filter_map(|topic| {
                            match topic {
                                Topic::Day(date) => Some(*date),
                                _ => None,
                            }
                        }));
                        self.request_versions(dates, VersionAction::Subscribe(subscription));
                        return;
                    }
                    let prepared = self.versions.updates.borrow().clone();
                    self.complete_subscription(subscription, Ok(prepared));
                }
                SubscriptionChange::Unsubscribe {
                    id,
                    subscription,
                    reply,
                } => {
                    if let Some(connection) = self.connections.get_mut(&id) {
                        for topic in subscription.topics {
                            match topic {
                                Topic::State => connection.state = StateSubscription::Absent,
                                Topic::Day(date) => {
                                    if connection.days.remove(&date).is_some() {
                                        self.reconciliation = ReconcileVersions::Required;
                                    }
                                }
                                Topic::Control => {}
                            }
                            connection.outbox.unsubscribe(topic);
                        }
                    }
                    // Shared dates do not need another database turn.
                    if self.reconciliation == ReconcileVersions::Required {
                        let published = self.versions.updates.borrow();
                        if self.desired_dates().iter().eq(published.days.keys()) {
                            self.reconciliation = ReconcileVersions::Current;
                        }
                    }
                    let _ = reply.send(());
                }
            }
        }
    }
    fn complete_versions(&mut self, completed: VersionCompletion) {
        match completed.action {
            VersionAction::Subscribe(subscription) => {
                self.complete_subscription(subscription, completed.result)
            }
            VersionAction::Reconcile => {
                if let Err(error) = completed.result {
                    tracing::warn!(?error, "could not reconcile day version subscriptions");
                    self.reconciliation =
                        ReconcileVersions::RetryAt(tokio::time::Instant::now() + RECONCILE_RETRY);
                }
            }
        }
    }
    fn complete_subscription(
        &mut self,
        pending: PendingSubscription,
        prepared: Result<crate::query::versions::PublishedVersions, quartz_protocol::ProtocolError>,
    ) {
        let result = self.install_subscription(&pending, prepared);
        if result.is_err() {
            // The publisher may have installed candidate dates before response admission failed.
            self.reconciliation = ReconcileVersions::Required;
        }
        if pending.reply.send(result).is_err() {
            tracing::debug!("subscriber left before registry acknowledgement");
        }
    }
    fn install_subscription(
        &mut self,
        pending: &PendingSubscription,
        prepared: Result<crate::query::versions::PublishedVersions, quartz_protocol::ProtocolError>,
    ) -> Result<(), QueueFrameError> {
        let connection = self
            .connections
            .get_mut(&pending.id)
            .ok_or(QueueFrameError::Closed)?;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                return connection.outbox.response(Response {
                    id: pending.response.id,
                    outcome: quartz_protocol::ResponseOutcome::Err { error },
                });
            }
        };
        let days = pending
            .subscription
            .topics
            .iter()
            .filter_map(|topic| match topic {
                Topic::Day(date) => Some(*date),
                _ => None,
            })
            .map(|date| {
                prepared
                    .days
                    .get(&date)
                    .cloned()
                    .ok_or(QueueFrameError::Closed)
            })
            .collect::<Result<Vec<_>, _>>()?;
        connection.outbox.response(pending.response.clone())?;
        for changed in days {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                connection.days.entry(changed.date)
            {
                connection.outbox.day(changed.clone());
                entry.insert(changed);
            }
        }
        if pending.subscription.topics.contains(&Topic::State)
            && connection.state == StateSubscription::Absent
        {
            connection.state = StateSubscription::Subscribed;
            connection.outbox.state(self.status.borrow().snapshot());
        }
        Ok(())
    }
    fn prune_days(&mut self, removed_days: BTreeSet<quartz_protocol::LocalDate>) {
        if !removed_days.is_subset(&self.desired_dates()) {
            self.reconciliation = ReconcileVersions::Required;
        }
    }
    fn publish_state(&self) {
        let mut snapshot = None;
        for connection in self
            .connections
            .values()
            .filter(|connection| connection.state == StateSubscription::Subscribed)
        {
            let state = snapshot.get_or_insert_with(|| self.status.borrow().snapshot());
            connection.outbox.state(state.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Bounds the barrier when a regression waits for an unserviced database command.
    const TEST_BARRIER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

    #[tokio::test]
    async fn registry_controls_remain_available_while_subscription_digest_waits() {
        let status =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("waiting-digest".into()));
        let mut fixture = crate::store::query::test_support::QueryFixture::new(0);
        let snapshots =
            quartz_domain::rules::registry::RuleRegistry::from_state(Default::default())
                .expect("rules")
                .snapshots();
        let channel = crate::query::versions::VersionPublisher::open(&mut fixture.store, snapshots)
            .expect("versions");
        let registry =
            RegistryHandle::start(status.subscribe(), channel.port, Arc::new(|_| Ok(())));
        let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
        let registration = registry
            .register(ConnectionPeer::Cli, writer.handle.clone())
            .expect("register");
        let _reply = registry
            .submit_subscribe(
                registration.id,
                Subscribe {
                    topics: vec![Topic::Day("2025-01-02".parse().expect("date"))],
                },
                Response {
                    id: Some(quartz_protocol::RequestId(1)),
                    outcome: quartz_protocol::ResponseOutcome::Ok {
                        body: serde_json::json!({}),
                    },
                },
            )
            .expect("submit");
        tokio::task::yield_now().await;
        assert!(
            !channel.publisher.requests.is_empty(),
            "database request is waiting"
        );
        assert_eq!(
            tokio::time::timeout(TEST_BARRIER_TIMEOUT, registry.outboxes())
                .await
                .expect("registry barrier must not wait for database")
                .expect("barrier")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn rejected_ui_registration_does_not_emit_disconnection() {
        let status =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("rejected-ui".into()));
        let database = crate::query::dispatch::test_support::TestDatabase::start();
        let disconnected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = disconnected.clone();
        let registry = RegistryHandle::start(
            status.subscribe(),
            database.versions.clone(),
            Arc::new(move |command| match command {
                UiCommand::Registered { .. } => Err(super::super::QueueUiError),
                UiCommand::Unregistered { .. } => {
                    observed.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }
                _ => Ok(()),
            }),
        );
        let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
        let registration = registry
            .register(
                ConnectionPeer::Ui(UiPeer {
                    pid: ProcessId::new(std::process::id()),
                    state: quartz_protocol::UiState::Starting,
                }),
                writer.handle.clone(),
            )
            .expect("register");
        registry.outboxes().await.expect("barrier");
        drop(registration);
        registry.outboxes().await.expect("cleanup barrier");
        assert_eq!(disconnected.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cli_unregister_and_closed_outbox_cleanup_do_not_request_day_versions() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let status =
                crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("cli-prune".into()));
            let mut fixture = crate::store::query::test_support::QueryFixture::new(1);
            let snapshots =
                quartz_domain::rules::registry::RuleRegistry::from_state(Default::default())
                    .expect("rules")
                    .snapshots();
            let channel =
                crate::query::versions::VersionPublisher::open(&mut fixture.store, snapshots)
                    .expect("versions");
            let registry =
                RegistryHandle::start(status.subscribe(), channel.port, Arc::new(|_| Ok(())));
            let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
            let cli = registry
                .register(ConnectionPeer::Cli, writer.handle.clone())
                .expect("register cli");
            assert_eq!(
                registry.outboxes().await.expect("register barrier").len(),
                1
            );
            drop(cli);
            let outboxes = tokio::time::timeout(TEST_BARRIER_TIMEOUT, registry.outboxes())
                .await
                .expect("unregister must not wait for the database")
                .expect("unregister barrier");
            assert!(outboxes.is_empty());
            assert!(
                channel.publisher.requests.is_empty(),
                "CLI unregister must not call set_days"
            );

            let _cli = registry
                .register(ConnectionPeer::Cli, writer.handle.clone())
                .expect("register closed cli");
            registry.outboxes().await.expect("register barrier");
            writer.handle.close();
            registry.outboxes().await.expect("wake cleanup");
            let outboxes = tokio::time::timeout(TEST_BARRIER_TIMEOUT, registry.outboxes())
                .await
                .expect("cleanup must not wait for the database")
                .expect("cleanup barrier");
            assert!(outboxes.is_empty());
            assert!(
                channel.publisher.requests.is_empty(),
                "closed CLI cleanup must not call set_days"
            );
        });
    }

    #[test]
    fn removing_a_shared_day_only_prunes_after_its_last_subscriber_leaves() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId(
                "shared-prune".into(),
            ));
            let mut fixture = crate::store::query::test_support::QueryFixture::new(1);
            let snapshots =
                quartz_domain::rules::registry::RuleRegistry::from_state(Default::default())
                    .expect("rules")
                    .snapshots();
            let mut channel =
                crate::query::versions::VersionPublisher::open(&mut fixture.store, snapshots)
                    .expect("versions");
            let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
            let date = "2026-11-20".parse().expect("date");
            let day = quartz_protocol::DayChanged {
                date,
                versions: quartz_protocol::QueryVersions("test".into()),
            };
            let (_sender, receiver) = mpsc::channel(REGISTRY_MAILBOX_CAPACITY);
            let mut registry = ConnectionRegistry {
                receiver,
                ui: Arc::new(|_| Ok(())),
                status: status.subscribe(),
                versions: channel.port,
                connections: BTreeMap::new(),
                pending: VecDeque::new(),
                versions_in_flight: FuturesUnordered::new(),
                reconciliation: ReconcileVersions::Current,
            };
            for id in [ConnectionId::for_test(1), ConnectionId::for_test(2)] {
                registry.connections.insert(
                    id,
                    Connection {
                        role: Role::Ui,
                        state: StateSubscription::Absent,
                        days: BTreeMap::from([(date, day.clone())]),
                        outbox: writer.handle.clone(),
                    },
                );
            }
            registry.receive(RegistryMessage::Unregister {
                id: ConnectionId::for_test(1),
            });
            assert!(channel.publisher.requests.is_empty());
            registry.receive(RegistryMessage::Unregister {
                id: ConnectionId::for_test(2),
            });
            let command = channel
                .publisher
                .requests
                .try_recv()
                .expect("last subscriber prunes its date");
            channel.publisher.receive(command);
            assert!(channel.publisher.requests.is_empty());
        });
    }

    #[tokio::test]
    async fn subscribing_to_trays_today_survives_the_tray_advancing_to_tomorrow() {
        let status =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("tray-rollover".into()));
        let database = crate::query::dispatch::test_support::TestDatabase::start();
        let today: quartz_protocol::LocalDate = "2025-01-02".parse().expect("today");
        let tomorrow = today.tomorrow().expect("tomorrow");
        database
            .versions
            .set_summary_day(today)
            .await
            .expect("tray day");
        let registry = RegistryHandle::start(
            status.subscribe(),
            database.versions.clone(),
            Arc::new(|_| Ok(())),
        );
        let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
        let registration = registry
            .register(
                ConnectionPeer::Ui(UiPeer {
                    pid: ProcessId::new(std::process::id()),
                    state: quartz_protocol::UiState::Starting,
                }),
                writer.handle.clone(),
            )
            .expect("register");
        registry
            .subscribe(
                registration.id,
                Subscribe {
                    topics: vec![Topic::Day(today)],
                },
                Response {
                    id: Some(quartz_protocol::RequestId(1)),
                    outcome: quartz_protocol::ResponseOutcome::Ok {
                        body: serde_json::json!({}),
                    },
                },
            )
            .await
            .expect("subscribe UI day already cached by tray");
        let published = database
            .versions
            .set_summary_day(tomorrow)
            .await
            .expect("advance tray day");
        assert!(
            published.days.contains_key(&today),
            "UI retains yesterday after tray rollover"
        );
        assert!(published.days.contains_key(&tomorrow));
        assert!(!writer.handle.is_closed());
    }

    #[test]
    fn registry_registers_and_unregisters_connections() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let publisher =
                crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("registry".into()));
            let database = crate::query::dispatch::test_support::TestDatabase::start();
            let versions = database.versions.clone();
            let registry =
                RegistryHandle::start(publisher.subscribe(), versions, Arc::new(|_| Ok(())));
            let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
            let registration = registry
                .register(
                    ConnectionPeer::Ui(UiPeer {
                        pid: ProcessId::new(std::process::id()),
                        state: quartz_protocol::UiState::Starting,
                    }),
                    writer.handle.clone(),
                )
                .expect("register");
            assert_eq!(registry.outboxes().await.expect("barrier").len(), 1);
            drop(registration);
            assert!(registry.outboxes().await.expect("barrier").is_empty());
        });
    }
    #[tokio::test]
    async fn failed_subscription_response_reconciles_publisher_dates() {
        let status =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("orphan-day".into()));
        let mut fixture = crate::store::query::test_support::QueryFixture::new(0);
        let snapshots =
            quartz_domain::rules::registry::RuleRegistry::from_state(Default::default())
                .expect("rules")
                .snapshots();
        let mut channel =
            crate::query::versions::VersionPublisher::open(&mut fixture.store, snapshots)
                .expect("versions");
        let updates = channel.port.updates.clone();
        let registry =
            RegistryHandle::start(status.subscribe(), channel.port, Arc::new(|_| Ok(())));
        let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
        let registration = registry
            .register(ConnectionPeer::Cli, writer.handle.clone())
            .expect("register");
        let reply = registry
            .submit_subscribe(
                registration.id,
                Subscribe {
                    topics: vec![Topic::Day("2025-01-02".parse().expect("date"))],
                },
                Response {
                    id: Some(quartz_protocol::RequestId(1)),
                    outcome: quartz_protocol::ResponseOutcome::Ok {
                        body: serde_json::json!({}),
                    },
                },
            )
            .expect("subscribe");
        let request = SelfContainedVersions::next(&channel.publisher.requests).await;
        writer.handle.close();
        channel.publisher.receive(request);
        assert!(reply.await.expect("subscription reply").is_err());
        let reconciliation = SelfContainedVersions::next(&channel.publisher.requests).await;
        drop(reconciliation); // Simulate a failed reconciliation round trip.
        assert!(!updates.borrow().days.is_empty());
        let retry = SelfContainedVersions::next(&channel.publisher.requests).await;
        channel.publisher.receive(retry);
        assert!(
            updates.borrow().days.is_empty(),
            "no subscriber owns the abandoned date"
        );
    }
    struct SelfContainedVersions;
    impl SelfContainedVersions {
        async fn next(
            requests: &crossbeam_channel::Receiver<crate::query::versions::VersionCommand>,
        ) -> crate::query::versions::VersionCommand {
            tokio::time::timeout(RECONCILE_RETRY + TEST_BARRIER_TIMEOUT, async {
                loop {
                    match requests.try_recv() {
                        Ok(command) => return command,
                        Err(crossbeam_channel::TryRecvError::Empty) => {
                            tokio::task::yield_now().await
                        }
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            panic!("version publisher disconnected")
                        }
                    }
                }
            })
            .await
            .expect("registry submits version request without another subscriber")
        }
    }
}
