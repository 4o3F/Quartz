//! Per-user UI mutex ownership within the current Windows session.

use super::{CheckPeerError, ProcessIdentity};
use snafu::{ResultExt, Snafu};
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use windows::{
    Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
        System::Threading::CreateMutexW,
    },
    core::PCWSTR,
};

/// Failure to identify the user or create the UI mutex.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum AcquireUiInstanceError {
    #[snafu(display("could not identify UI mutex owner"))]
    Identify { source: CheckPeerError },
    #[snafu(display("could not create UI instance mutex (CreateMutexW)"))]
    CreateMutex { source: windows::core::Error },
}
/// Keeps this session's UI mutex alive until the UI process finishes.
#[derive(Debug)]
pub struct UiInstance {
    _handle: OwnedHandle,
}
/// A duplicate UI forwards its open request to the core and exits.
#[derive(Debug)]
pub enum AcquireUiInstance {
    Acquired(UiInstance),
    AlreadyRunning,
}
impl UiInstance {
    /// Creates or opens the UI mutex using the current token's canonical SID.
    ///
    /// # Errors
    /// Returns identity lookup or mutex creation failures.
    pub fn acquire() -> Result<AcquireUiInstance, AcquireUiInstanceError> {
        let identity =
            ProcessIdentity::current().context(acquire_ui_instance_error::IdentifySnafu)?;
        let name: Vec<_> = identity
            .sid()
            .ui_mutex_name()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: The name is terminated and live; the mutex is non-inherited with no initial owner.
        let raw = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .context(acquire_ui_instance_error::CreateMutexSnafu)?;
        // SAFETY: Read the successful CreateMutexW status before any other Windows call.
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        // SAFETY: CreateMutexW transferred ownership of a valid closable handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        if exists {
            Ok(AcquireUiInstance::AlreadyRunning)
        } else {
            Ok(AcquireUiInstance::Acquired(Self { _handle: handle }))
        }
    }
}
