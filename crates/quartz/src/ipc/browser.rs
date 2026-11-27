//! Typed browser stream inputs and nonblocking delivery port to capture (05 §3.1).

use std::sync::Arc;

use quartz_domain::capture::Timestamp;
use quartz_protocol::{
    BrowserKind, BrowserStateRequest, BrowserStream, BrowserStreamId, ProtocolError, Uuid,
};

use crate::store::browser::BrowserStreamRowId;

/// Live browser stream key pairing browser family and profile UUID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BrowserStreamKey {
    pub browser: BrowserKind,
    pub profile_instance_id: Uuid,
}

impl BrowserStreamKey {
    /// Projects this stream key to its wire representation for `core.status` and `state`.
    #[must_use]
    pub(crate) fn to_protocol_stream(&self) -> BrowserStream {
        BrowserStream {
            browser_stream: BrowserStreamId(self.profile_instance_id.to_string()),
            browser: self.browser,
        }
    }
}

/// Disconnect notice enqueued into capture when a browser stream closes or is replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserStreamClosed {
    pub stream_id: BrowserStreamRowId,
    pub key: BrowserStreamKey,
    pub at: Timestamp,
}

/// Typed browser input delivered through the capture observation queue (05 §3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserInput {
    /// Validated `browser.state` snapshot stamped with core arrival time.
    State {
        stream_id: BrowserStreamRowId,
        key: BrowserStreamKey,
        at: Timestamp,
        state: BrowserStateRequest,
    },
    /// Browser stream disconnected or was replaced; capture drops its retained state.
    StreamClosed(BrowserStreamClosed),
}

impl BrowserInput {
    /// Returns the core arrival timestamp for this browser input.
    #[must_use]
    pub(crate) const fn at(&self) -> Timestamp {
        match self {
            Self::State { at, .. } => *at,
            Self::StreamClosed(closed) => closed.at,
        }
    }
}

type SendBrowserStateFn = dyn Fn(BrowserStreamRowId, BrowserStreamKey, BrowserStateRequest) -> Result<(), ProtocolError>
    + Send
    + Sync;
type SendBrowserClosedFn = dyn Fn(BrowserStreamRowId, BrowserStreamKey) + Send + Sync;

/// Nonblocking port delivering validated browser state and close notices to the capture queue.
#[derive(Clone)]
pub(crate) struct BrowserCapturePort {
    state: Arc<SendBrowserStateFn>,
    closed: Arc<SendBrowserClosedFn>,
}

impl std::fmt::Debug for BrowserCapturePort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserCapturePort")
            .finish_non_exhaustive()
    }
}

impl BrowserCapturePort {
    /// Creates a port backed by capture-queue admission closures.
    pub(crate) fn new(
        state: impl Fn(
            BrowserStreamRowId,
            BrowserStreamKey,
            BrowserStateRequest,
        ) -> Result<(), ProtocolError>
        + Send
        + Sync
        + 'static,
        closed: impl Fn(BrowserStreamRowId, BrowserStreamKey) + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Arc::new(state),
            closed: Arc::new(closed),
        }
    }

    /// Enqueues a validated `browser.state` snapshot into the capture queue.
    pub(crate) fn send_state(
        &self,
        stream_id: BrowserStreamRowId,
        key: BrowserStreamKey,
        state: BrowserStateRequest,
    ) -> Result<(), ProtocolError> {
        (self.state)(stream_id, key, state)
    }

    /// Enqueues a `BrowserStreamClosed` notice into the capture queue.
    pub(crate) fn send_closed(&self, stream_id: BrowserStreamRowId, key: BrowserStreamKey) {
        (self.closed)(stream_id, key);
    }

    /// No-op port for isolated IPC tests that do not exercise capture ingestion.
    #[cfg(test)]
    pub(crate) fn noop() -> Self {
        Self::new(|_, _, _| Ok(()), |_, _| {})
    }
}
