//! Core IPC sessions, registry and outbox actors, snapshots, and command clients.

pub(crate) mod client;
pub(crate) mod connection;
pub(crate) mod outbox;
pub(crate) mod registry;
mod session;
mod status;

#[cfg(test)]
pub(crate) use crate::persistence::dispatch::WriteChannel;

pub(crate) use session::{AllowedRoles, ConnectionSession, UiCommand};
pub(crate) use status::{PublishedStatus, StatusPublisher};

use std::sync::Arc;

/// Application services shared by transport, registry, and connection tasks.
#[derive(Clone)]
pub(crate) struct Services {
    pub writes: crate::persistence::dispatch::WritePort,
    pub queries: crate::query::dispatch::QueryPort,
    pub versions: crate::query::versions::VersionPort,
    pub status: tokio::sync::watch::Receiver<PublishedStatus>,
    pub ui: UiPort,
}
impl Services {
    pub(crate) fn start_registry(&self) -> registry::RegistryHandle {
        registry::RegistryHandle::start(self.status.clone(), self.versions.clone())
    }
}

/// Bounded low-frequency UI command queue between I/O and main threads (01 §6).
pub(crate) const MAIN_COMMAND_CAPACITY: usize = 64;

/// Non-blocking delivery to the main thread; failure closes or rejects the request.
pub(crate) type UiPort = Arc<dyn Fn(UiCommand) -> Result<(), QueueUiError> + Send + Sync>;

#[derive(Debug, snafu::Snafu)]
#[snafu(display("main-thread UI queue is unavailable"))]
pub(crate) struct QueueUiError;
