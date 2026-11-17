//! Platform integration used by the entry point.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(super) use windows::{run_core, ui_language};

/// Attaches CLI output to the parent console on Windows.
pub(super) fn attach_parent_console() {
    #[cfg(windows)]
    windows::attach_parent_console();
}
