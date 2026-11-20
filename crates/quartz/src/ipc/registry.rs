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
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};

/// Bounded registry commands on the existing I/O runtime (01 §6, 04 §7).
const REGISTRY_MAILBOX_CAPACITY: usize = 64;
pub(crate) use super::outbox::STATE_PUSH_INTERVAL;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ConnectionId(u64);

pub(crate) struct ControlMessage {
    pub(crate) op: ControlOp,
}

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
    Control(ControlMessage),
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
    pub(crate) fn start(status: watch::Receiver<PublishedStatus>) -> Self {
        let (mailbox, receiver) = mpsc::channel(REGISTRY_MAILBOX_CAPACITY);
        let handle = Self {
            mailbox,
            next_id: Arc::new(AtomicU64::new(0)),
        };
        tokio::spawn(
            ConnectionRegistry {
                receiver,
                status,
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
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| {
                    tracing::error!("connection identity sequence exhausted");
                    QueueRegistryError
                })?,
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
    pub(crate) fn control(&self, message: ControlMessage) -> Result<(), QueueRegistryError> {
        self.send(RegistryMessage::Control(message))
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
enum StateChange {
    Clean,
    Pending,
}
#[derive(Debug)]
struct StateThrottle {
    next: Instant,
    change: StateChange,
}
impl StateThrottle {
    fn after_snapshot(now: Instant) -> Self {
        Self {
            next: now + STATE_PUSH_INTERVAL,
            change: StateChange::Clean,
        }
    }
    fn changed(&mut self) {
        self.change = StateChange::Pending;
    }
    fn deadline(&self) -> Option<Instant> {
        (self.change == StateChange::Pending).then_some(self.next)
    }
    fn take_due(&mut self, now: Instant) -> bool {
        if self.change == StateChange::Pending && now >= self.next {
            *self = Self::after_snapshot(now);
            true
        } else {
            false
        }
    }
}
struct Connection {
    role: Role,
    subscriptions: Vec<Topic>,
    state: Option<StateThrottle>,
    outbox: OutboxHandle,
}
struct ConnectionRegistry {
    receiver: mpsc::Receiver<RegistryMessage>,
    status: watch::Receiver<PublishedStatus>,
    connections: BTreeMap<ConnectionId, Connection>,
}
impl ConnectionRegistry {
    async fn run(mut self) {
        loop {
            self.connections
                .retain(|_, connection| !connection.outbox.is_closed());
            let deadline = self
                .connections
                .values()
                .filter_map(|connection| {
                    connection.state.as_ref().and_then(StateThrottle::deadline)
                })
                .min();
            tokio::select! {
                message = self.receiver.recv() => match message { Some(message) => self.receive(message), None => return },
                changed = self.status.changed() => {
                    if let Err(error) = changed { tracing::debug!(%error, "main status publisher stopped"); return; }
                    for connection in self.connections.values_mut() { if let Some(state) = &mut connection.state { state.changed(); } }
                }
                () = Self::deadline(deadline) => self.publish_due(),
            }
        }
    }
    async fn deadline(deadline: Option<Instant>) {
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    }
    fn receive(&mut self, message: RegistryMessage) {
        match message {
            RegistryMessage::Register { id, role, outbox } => {
                self.connections.insert(
                    id,
                    Connection {
                        role,
                        subscriptions: Vec::new(),
                        state: None,
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
            RegistryMessage::Control(message) => {
                match message.op {
                    ControlOp::UiShutdown => {}
                    ControlOp::UiShow | ControlOp::UiActivate => {
                        // TODO(M3): Route to the UI selected by the main-thread lifecycle.
                        tracing::debug!("UI show/activate routing awaits M3");
                        return;
                    }
                    _ => {
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
                        op: message.op,
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
        let connection = self
            .connections
            .get_mut(&id)
            .ok_or(QueueFrameError::Closed)?;
        connection.outbox.response(response)?;
        for topic in subscription.topics {
            if !connection.subscriptions.contains(&topic) {
                connection.subscriptions.push(topic);
            }
        }
        if connection.subscriptions.contains(&Topic::State) && connection.state.is_none() {
            connection.state = Some(StateThrottle::after_snapshot(Instant::now()));
            connection.outbox.state(self.status.borrow().snapshot());
        }
        Ok(())
    }
    fn publish_due(&mut self) {
        let now = Instant::now();
        // Construct no event body unless at least one subscriber is actually due.
        let mut snapshot = None;
        for connection in self.connections.values_mut() {
            if connection
                .state
                .as_mut()
                .is_some_and(|throttle| throttle.take_due(now))
            {
                let state = snapshot.get_or_insert_with(|| self.status.borrow().snapshot());
                connection.outbox.state(state.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_throttle_uses_latest_change_and_never_pushes_early() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            tokio::time::pause();
            let mut throttle = StateThrottle::after_snapshot(Instant::now());
            for _ in 0..10 {
                throttle.changed();
            }
            assert!(!throttle.take_due(Instant::now()));
            tokio::time::advance(STATE_PUSH_INTERVAL).await;
            assert!(throttle.take_due(Instant::now()));
            assert!(!throttle.take_due(Instant::now()));
            assert_eq!(throttle.deadline(), None);
        });
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
            let registry = RegistryHandle::start(publisher.subscribe());
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
