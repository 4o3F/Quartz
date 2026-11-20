//! Platform integration used by the entry point.

#[cfg(windows)]
pub(crate) mod windows;

#[cfg(windows)]
pub(super) use windows::{AcquireInstance, AcquireInstanceError, CoreProcess, detect_ui_language};

/// Attaches CLI output to the parent console on Windows.
pub(super) fn attach_parent_console() {
    #[cfg(windows)]
    windows::attach_parent_console();
}

#[cfg(windows)]
pub(crate) use windows::foreground::{
    CaptureApartment, InitializeCaptureComError, Resolution as ForegroundResolution,
    Resolver as ForegroundResolver,
};
