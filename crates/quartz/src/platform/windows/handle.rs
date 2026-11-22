//! Ownership of kernel handles returned by Windows APIs.

use windows::Win32::Foundation::{self, HANDLE};

/// Closes a uniquely owned, valid kernel handle on drop.
#[derive(Debug)]
pub(super) struct OwnedHandle(HANDLE);

impl OwnedHandle {
    /// Takes ownership of a handle returned successfully by a kernel API.
    ///
    /// # Safety
    /// The handle must be valid, uniquely owned, and closable with CloseHandle.
    pub(super) unsafe fn from_raw(handle: HANDLE) -> Self {
        Self(handle)
    }

    pub(super) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: Construction transfers unique ownership of a valid kernel handle.
        // Best-effort teardown: Drop cannot report handle closure failures.
        let _ = unsafe { Foundation::CloseHandle(self.0) };
    }
}
