//! Native pipe readiness notification; no timer polling while the core starts.

use super::PipeName;
use snafu::{ResultExt, Snafu};
use std::{
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    sync::Arc,
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{HANDLE, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0},
        System::Threading::{CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects},
    },
    core::PCWSTR,
};

/// The core's pipe startup budget (01 §4.4, 04 §6.1).
pub const CORE_START_TIMEOUT: Duration = Duration::from_secs(8);

/// Failure to register, wait for, or publish pipe readiness.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum WaitCorePipeError {
    #[snafu(display("could not observe pipe readiness ({call})"))]
    Windows {
        call: &'static str,
        source: windows::core::Error,
    },
    #[snafu(display("core pipe did not become ready before its startup deadline"))]
    Deadline { source: tokio::time::error::Elapsed },
    #[snafu(display("core pipe wait worker failed"))]
    Worker { source: tokio::task::JoinError },
    #[snafu(display("core pipe wait was cancelled"))]
    Cancelled,
    #[snafu(display("unexpected core pipe wait status: {status:?}"))]
    UnexpectedStatus { status: WAIT_EVENT },
}
#[derive(Debug)]
struct Event {
    handle: OwnedHandle,
}
impl Event {
    fn create(name: Option<&PipeName>) -> Result<Self, WaitCorePipeError> {
        let name = name.map(PipeName::readiness_name);
        let text: Option<Vec<u16>> = name
            .as_ref()
            .map(|name| name.encode_wide().chain(Some(0)).collect());
        let text = text
            .as_ref()
            .map_or(PCWSTR::null(), |text| PCWSTR(text.as_ptr()));
        // SAFETY: The optional name is terminated and live; the manual-reset event is not inherited.
        let raw = unsafe { CreateEventW(None, true, false, text) }.context(
            wait_core_pipe_error::WindowsSnafu {
                call: "CreateEventW",
            },
        )?;
        // SAFETY: CreateEventW transferred ownership of a valid closable kernel handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        Ok(Self { handle })
    }
    fn raw(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle())
    }
    fn signal(&self) -> Result<(), WaitCorePipeError> {
        // SAFETY: This event owns a live event handle with modify access.
        unsafe { SetEvent(self.raw()) }
            .context(wait_core_pipe_error::WindowsSnafu { call: "SetEvent" })
    }
    fn reset(&self) -> Result<(), WaitCorePipeError> {
        // SAFETY: This event owns a live event handle with modify access.
        unsafe { ResetEvent(self.raw()) }
            .context(wait_core_pipe_error::WindowsSnafu { call: "ResetEvent" })
    }
}
/// Kept by the pipe server; signals after listeners exist and resets on shutdown.
#[derive(Debug)]
pub struct CoreReadySignal {
    event: Event,
}
impl CoreReadySignal {
    /// Claims readiness publication after the server owns its pipe listeners.
    pub fn create(name: &PipeName) -> Result<Self, WaitCorePipeError> {
        let event = Event::create(Some(name))?;
        event.reset()?;
        Ok(Self { event })
    }
    /// Publishes that the initial pipe listeners are available.
    pub fn signal(&self) -> Result<(), WaitCorePipeError> {
        self.event.signal()
    }
}
impl Drop for CoreReadySignal {
    fn drop(&mut self) {
        if let Err(error) = self.event.reset() {
            tracing::warn!(%error, "could not reset pipe readiness");
        }
    }
}

/// Registered before launching the core so a fast startup cannot lose its notification.
#[derive(Debug)]
pub struct CoreReadyWaiter {
    event: Event,
}
impl CoreReadyWaiter {
    /// Opens the endpoint's manual-reset notification without creating a pipe connection.
    pub fn new(name: &PipeName) -> Result<Self, WaitCorePipeError> {
        Ok(Self {
            event: Event::create(Some(name))?,
        })
    }
    /// Waits at most `CORE_START_TIMEOUT`; dropping the future cancels the native wait.
    pub async fn wait(self) -> Result<(), WaitCorePipeError> {
        let cancellation = Arc::new(Event::create(None)?);
        let cancel = CancelWait {
            event: Arc::clone(&cancellation),
        };
        let waiting = tokio::task::spawn_blocking(move || {
            let handles = [self.event.raw(), cancellation.raw()];
            // SAFETY: Both handles remain live until this wait returns; no handle ownership transfers.
            let outcome = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
            if outcome == WAIT_OBJECT_0 {
                return Ok(());
            }
            if outcome == WAIT_FAILED {
                return Err(WaitCorePipeError::Windows {
                    call: "WaitForMultipleObjects",
                    source: windows::core::Error::from_thread(),
                });
            }
            // Cancellation is the second event in this wait's handle array.
            const CANCELLATION_HANDLE_INDEX: u32 = 1;
            if outcome == WAIT_EVENT(WAIT_OBJECT_0.0 + CANCELLATION_HANDLE_INDEX) {
                return Err(WaitCorePipeError::Cancelled);
            }
            Err(WaitCorePipeError::UnexpectedStatus { status: outcome })
        });
        let result = tokio::time::timeout(CORE_START_TIMEOUT, waiting)
            .await
            .context(wait_core_pipe_error::DeadlineSnafu)?
            .context(wait_core_pipe_error::WorkerSnafu)?;
        drop(cancel);
        result
    }
}
struct CancelWait {
    event: Arc<Event>,
}
impl Drop for CancelWait {
    fn drop(&mut self) {
        if let Err(error) = self.event.signal() {
            tracing::warn!(%error, "could not cancel native pipe wait");
        }
    }
}
