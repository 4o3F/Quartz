//! Windows named-pipe discovery and mutual identity checks.
mod client;
mod identity;
mod pipe_name;
mod readiness;
pub use readiness::{CORE_START_TIMEOUT, CoreReadySignal, CoreReadyWaiter, WaitCorePipeError};
mod win32_error;
pub use client::{CONNECT_TIMEOUT, OpenCorePipeError, PipeClient};
pub use identity::{CheckPeerError, PeerExecutable, ProcessIdentity};
pub use pipe_name::PipeName;
