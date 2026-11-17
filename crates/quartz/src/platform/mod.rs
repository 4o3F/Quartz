//! Platform integration used by the entry point.

#[cfg(windows)]
mod windows;

/// Attaches CLI output to the parent console on Windows.
pub(super) fn attach_parent_console() {
    #[cfg(windows)]
    windows::attach_parent_console();
}
