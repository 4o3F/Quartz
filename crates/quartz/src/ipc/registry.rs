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
        versions: watch::Receiver<crate::query::versions::VersionState>,
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
    versions: watch::Receiver<crate::query::versions::VersionState>,
    connections: BTreeMap<ConnectionId, Connection>,
}
impl ConnectionRegistry {
    async fn run(mut self) {
        let mut publisher = VersionPublisher::Active;
        loop {
            self.connections
                .retain(|_, connection| !connection.outbox.is_closed());
            tokio::select! {
                message = self.receiver.recv() => match message { Some(message) => self.receive(message), None => return },
                changed = self.status.changed() => {
                    if let Err(error) = changed { tracing::debug!(%error, "main status publisher stopped"); return; }
                    self.publish_state();
                }
                changed = self.versions.changed(), if publisher == VersionPublisher::Active => {
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
        // Release the publisher lock before resolving subscribed dates.
        let state = self.versions.borrow().clone();
        for connection in self.connections.values_mut() {
            for (date, latest) in &mut connection.days {
                match state.day(*date) {
                    Ok(changed) => {
                        if changed.versions != latest.versions {
                            connection.outbox.day(changed.clone());
                            *latest = changed;
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "could not update day subscription");
                        connection.outbox.close();
                    }
                }
            }
        }
    }
    fn receive(&mut self, message: RegistryMessage) {
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
                self.connections.remove(&id);
            }
            RegistryMessage::Subscribe {
                id,
                subscription,
                response,
                reply,
            } => {
                let result = self.subscribe(id, subscription, response);
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
    fn subscribe(
        &mut self,
        id: ConnectionId,
        subscription: Subscribe,
        response: Response,
    ) -> Result<(), QueueFrameError> {
        let mut prepared_days = BTreeMap::new();
        for topic in &subscription.topics {
            if let Topic::Day(date) = topic {
                let result = self
                    .versions
                    .borrow()
                    .day(*date)
                    .map_err(|error| error.kind());
                match result {
                    Ok(changed) => {
                        prepared_days.insert(*date, changed);
                    }
                    Err(kind) => {
                        let connection =
                            self.connections.get(&id).ok_or(QueueFrameError::Closed)?;
                        return connection.outbox.response(Response {
                            id: response.id,
                            outcome: quartz_protocol::ResponseOutcome::Err {
                                error: quartz_protocol::ProtocolError::new(kind),
                            },
                        });
                    }
                }
            }
        }
        let connection = self
            .connections
            .get_mut(&id)
            .ok_or(QueueFrameError::Closed)?;
        connection.outbox.response(response)?;
        for changed in prepared_days.into_values() {
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
    #[test]
    fn registry_registers_and_unregisters_connections() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let publisher =
                crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("registry".into()));
            let (_versions, versions) =
                watch::channel(crate::query::versions::VersionState::empty());
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
