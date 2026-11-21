//! Connection registry actor; fan-out never awaits a client writer.

use super::{
    PublishedStatus,
    outbox::{OutboxHandle, QueueFrameError},
};
use quartz_protocol::{ControlBody, ControlOp, Response, Role, Subscribe, Topic};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{mpsc, oneshot, watch};

/// Bounded registry commands on the existing I/O runtime (01 §6, 04 §7).
const REGISTRY_MAILBOX_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ConnectionId(u64);

enum RegistryMessage {
    Register {
        id: ConnectionId,
        role: Role,
        outbox: OutboxHandle,
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
    Control(ControlOp),
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
    ) -> Self {
        let (mailbox, receiver) = mpsc::channel(REGISTRY_MAILBOX_CAPACITY);
        let handle = Self {
            mailbox,
            next_id: Arc::new(AtomicU64::new(0)),
        };
        tokio::spawn(
            ConnectionRegistry {
                receiver,
                status,
                versions,
                connections: BTreeMap::new(),
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
        role: Role,
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
        self.send(RegistryMessage::Register { id, role, outbox })?;
        Ok(Registration {
            id,
            registry: self.clone(),
        })
    }
    pub(crate) async fn subscribe(
        &self,
        id: ConnectionId,
        subscription: Subscribe,
        response: Response,
    ) -> Result<(), QueueFrameError> {
        let (reply, received) = oneshot::channel();
        self.send(RegistryMessage::Subscribe {
            id,
            subscription,
            response,
            reply,
        })
        .map_err(|_| QueueFrameError::Closed)?;
        received.await.map_err(|_| QueueFrameError::Closed)?
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
struct ConnectionRegistry {
    receiver: mpsc::Receiver<RegistryMessage>,
    status: watch::Receiver<PublishedStatus>,
    versions: crate::query::versions::VersionPort,
    connections: BTreeMap<ConnectionId, Connection>,
}
impl ConnectionRegistry {
    async fn run(mut self) {
        let mut publisher = VersionPublisher::Active;
        loop {
            let mut removed_days = std::collections::BTreeSet::new();
            self.connections.retain(|_, connection| {
                if connection.outbox.is_closed() {
                    removed_days.extend(connection.days.keys().copied());
                    false
                } else {
                    true
                }
            });
            self.prune_days(removed_days).await;
            tokio::select! {
                message = self.receiver.recv() => match message { Some(message) => self.receive(message).await, None => return },
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
    async fn receive(&mut self, message: RegistryMessage) {
        match message {
            RegistryMessage::Register { id, role, outbox } => {
                self.connections.insert(
                    id,
                    Connection {
                        role,
                        state: StateSubscription::Absent,
                        days: BTreeMap::new(),
                        outbox,
                    },
                );
            }
            RegistryMessage::Unregister { id } => {
                if let Some(connection) = self.connections.remove(&id) {
                    self.prune_days(connection.days.into_keys().collect()).await;
                }
            }
            RegistryMessage::Subscribe {
                id,
                subscription,
                response,
                reply,
            } => {
                let result = self.subscribe(id, subscription, response).await;
                if reply.send(result).is_err() {
                    tracing::debug!("subscriber left before registry acknowledgement");
                }
            }
            RegistryMessage::Control(op) => {
                match op {
                    ControlOp::UiShutdown => {}
                    ControlOp::UiShow | ControlOp::UiActivate => {
                        // TODO(M3): Route to the UI selected by the main-thread lifecycle.
                        tracing::debug!("UI show/activate routing awaits M3");
                        return;
                    }
                    ControlOp::BrowserAck
                    | ControlOp::BrowserPolicy
                    | ControlOp::CoreOffline
                    | ControlOp::BrowserPurge => {
                        // TODO(M5): Route browser control by its authenticated stream.
                        tracing::warn!("browser control fan-out awaits M5 routing");
                        return;
                    }
                }
                // UI lifecycle control is implicit for every UI hello (04 §6.1).
                for connection in self
                    .connections
                    .values()
                    .filter(|connection| connection.role == Role::Ui)
                {
                    let body = ControlBody {
                        op,
                        fields: serde_json::Map::new(),
                    };
                    if let Err(error) = connection.outbox.control(body) {
                        tracing::debug!(%error, "control delivery closed slow connection");
                        connection.outbox.close();
                    }
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
    }
    async fn subscribe(
        &mut self,
        id: ConnectionId,
        subscription: Subscribe,
        response: Response,
    ) -> Result<(), QueueFrameError> {
        let mut dates: std::collections::BTreeSet<_> = self
            .connections
            .values()
            .flat_map(|connection| connection.days.keys().copied())
            .collect();
        dates.extend(subscription.topics.iter().filter_map(|topic| match topic {
            Topic::Day(date) => Some(*date),
            _ => None,
        }));
        // A date already published for the tray still needs its own registry subscription.
        let needs_days = dates.iter().any(|date| {
            !self
                .connections
                .values()
                .any(|connection| connection.days.contains_key(date))
        });
        let prepared = if needs_days {
            match self.versions.set_days(dates).await {
                Ok(prepared) => prepared,
                Err(error) => {
                    let connection = self.connections.get(&id).ok_or(QueueFrameError::Closed)?;
                    return connection.outbox.response(Response {
                        id: response.id,
                        outcome: quartz_protocol::ResponseOutcome::Err { error },
                    });
                }
            }
        } else {
            self.versions.updates.borrow().clone()
        };
        let prepared_days = subscription
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
        let connection = self
            .connections
            .get_mut(&id)
            .ok_or(QueueFrameError::Closed)?;
        connection.outbox.response(response)?;
        for changed in prepared_days {
            let date = changed.date;
            if let std::collections::btree_map::Entry::Vacant(entry) = connection.days.entry(date) {
                connection.outbox.day(changed.clone());
                entry.insert(changed);
            }
        }
        if subscription.topics.contains(&Topic::State)
            && connection.state == StateSubscription::Absent
        {
            connection.state = StateSubscription::Subscribed;
            connection.outbox.state(self.status.borrow().snapshot());
        }
        Ok(())
    }
    async fn prune_days(
        &self,
        removed_days: std::collections::BTreeSet<quartz_protocol::LocalDate>,
    ) {
        if removed_days.is_empty() {
            return;
        }
        let dates: std::collections::BTreeSet<_> = self
            .connections
            .values()
            .flat_map(|connection| connection.days.keys().copied())
            .collect();
        if removed_days.is_subset(&dates) {
            return;
        }
        if let Err(error) = self.versions.set_days(dates).await {
            tracing::warn!(?error, "could not prune day version subscriptions");
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
            let registry = RegistryHandle::start(status.subscribe(), channel.port);
            let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
            let cli = registry
                .register(Role::Cli, writer.handle.clone())
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
                .register(Role::Cli, writer.handle.clone())
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
                status: status.subscribe(),
                versions: channel.port,
                connections: BTreeMap::new(),
            };
            for id in [ConnectionId(1), ConnectionId(2)] {
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
            tokio::time::timeout(
                TEST_BARRIER_TIMEOUT,
                registry.receive(RegistryMessage::Unregister {
                    id: ConnectionId(1),
                }),
            )
            .await
            .expect("shared day needs no database request");
            assert!(channel.publisher.requests.is_empty());
            let removal = tokio::spawn(async move {
                registry
                    .receive(RegistryMessage::Unregister {
                        id: ConnectionId(2),
                    })
                    .await;
            });
            tokio::task::yield_now().await;
            let command = channel
                .publisher
                .requests
                .try_recv()
                .expect("last subscriber prunes its date");
            channel.publisher.receive(command);
            tokio::time::timeout(TEST_BARRIER_TIMEOUT, removal)
                .await
                .expect("prune completes")
                .expect("removal task");
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
        let registry = RegistryHandle::start(status.subscribe(), database.versions.clone());
        let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
        let registration = registry
            .register(Role::Ui, writer.handle.clone())
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
            let registry = RegistryHandle::start(publisher.subscribe(), versions);
            let writer = super::super::outbox::OutboxTask::spawn(tokio::io::sink());
            let registration = registry
                .register(Role::Ui, writer.handle.clone())
                .expect("register");
            assert_eq!(registry.outboxes().await.expect("barrier").len(), 1);
            drop(registration);
            assert!(registry.outboxes().await.expect("barrier").is_empty());
        });
    }
}
