//! Core IPC decisions, status snapshots, and the blocking command client.

pub(crate) mod client;
mod session;
mod status;

pub(crate) use session::{AllowedRoles, ConnectionSession, UiCommand};
pub(crate) use status::StatusPublisher;

use std::sync::Arc;

/// Bounded low-frequency UI command queue between I/O and main threads (01 §6).
pub(crate) const MAIN_COMMAND_CAPACITY: usize = 64;

/// Non-blocking delivery to the main thread; failure closes or rejects the request.
pub(crate) type UiPort = Arc<dyn Fn(UiCommand) -> Result<(), QueueUiError> + Send + Sync>;

#[derive(Debug, snafu::Snafu)]
#[snafu(display("main-thread UI queue is unavailable"))]
pub(crate) struct QueueUiError;
