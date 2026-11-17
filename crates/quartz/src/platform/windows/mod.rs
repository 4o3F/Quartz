//! Windows console integration.

/// Attaches the parent console when one is available.
pub(super) fn attach_parent_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

    // SAFETY: This attaches the current process to its parent's console and takes no pointers.
    // No parent console and an already attached console are both normal launch conditions.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}
