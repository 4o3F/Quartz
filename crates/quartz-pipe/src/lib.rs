//! Authenticated Quartz pipes, readiness notification, framing, and request/response correlation.

mod client;
mod user_sid;
#[cfg(windows)]
mod windows;

pub use client::{AsyncCoreClient, CallCoreError, CoreClient, IncomingFrame, RESPONSE_TIMEOUT};
pub use user_sid::UserSid;
#[cfg(windows)]
pub use windows::{
    CONNECT_TIMEOUT, CORE_START_TIMEOUT, CheckPeerError, CoreReadySignal, CoreReadyWaiter,
    OpenCorePipeError, PeerExecutable, PipeClient, PipeName, ProcessIdentity, WaitCorePipeError,
};

#[cfg(all(windows, feature = "test-util"))]
pub mod test_util;
