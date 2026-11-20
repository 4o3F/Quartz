//! COM lifetime confined to the capture worker that performs identity queries.

use std::{marker::PhantomData, rc::Rc};
use windows::Win32::System::Com::{self, COINIT_MULTITHREADED};

#[derive(Debug, snafu::Snafu)]
#[snafu(display("could not initialize capture worker COM apartment"))]
pub(crate) struct InitializeCaptureComError {
    source: windows::core::Error,
}

/// Balances COM initialization on the same worker thread, after all local interfaces are released.
#[derive(Debug)]
pub(crate) struct CaptureApartment {
    thread: PhantomData<Rc<()>>,
}
impl CaptureApartment {
    pub(crate) fn initialize() -> Result<Self, InitializeCaptureComError> {
        // SAFETY: The capture worker has no message loop or existing apartment. Both S_OK and
        // S_FALSE require a matching CoUninitialize on this thread.
        unsafe { Com::CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|source| InitializeCaptureComError { source })?;
        Ok(Self {
            thread: PhantomData,
        })
    }
}
impl Drop for CaptureApartment {
    fn drop(&mut self) {
        // SAFETY: This non-Send guard balances one successful initialization on its own thread.
        unsafe { Com::CoUninitialize() };
    }
}
