//! UI child creation without a command shell or additional waiting thread.

use std::{ffi::OsString, os::windows::ffi::OsStrExt};

use snafu::{ResultExt, Snafu};
use windows::{
    Win32::System::Threading::{
        self, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTF_FORCEONFEEDBACK, STARTUPINFOW,
    },
    core::{PCWSTR, PWSTR},
};

use super::handle::OwnedHandle;

/// Failures while resolving the executable and creating the UI child.
#[derive(Debug, Snafu)]
pub(super) enum LaunchUiError {
    #[snafu(display("could not resolve Quartz executable path (current_exe)"))]
    ResolveExecutable { source: std::io::Error },
    #[snafu(display("could not create UI child process (CreateProcessW)"))]
    CreateProcess { source: windows::core::Error },
}

/// Creates the UI child from the absolute path of the running executable.
///
/// # Errors
/// Returns executable-path lookup or CreateProcessW failures.
pub(super) fn launch_ui() -> Result<OwnedHandle, LaunchUiError> {
    let executable = std::env::current_exe().context(ResolveExecutableSnafu)?;
    let application: Vec<_> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut command = OsString::from("\"");
    command.push(executable.as_os_str());
    command.push("\" ui");
    let mut command: Vec<_> = command.encode_wide().chain(Some(0)).collect();
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_FORCEONFEEDBACK,
        ..Default::default()
    };
    let mut information = PROCESS_INFORMATION::default();
    // SAFETY: Both UTF-16 strings are terminated and remain live; the command buffer is writable.
    // current_exe is absolute, application is explicit, and no handles are inherited by the child.
    unsafe {
        Threading::CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS::default(),
            None,
            None,
            &startup,
            &mut information,
        )
    }
    .context(CreateProcessSnafu)?;
    // SAFETY: CreateProcessW succeeded and transferred ownership of both returned kernel handles.
    let (process, _thread) = unsafe {
        (
            OwnedHandle::from_raw(information.hProcess),
            OwnedHandle::from_raw(information.hThread),
        )
    };
    tracing::info!(pid = information.dwProcessId, "UI child started");
    Ok(process)
}

pub(super) fn log_exit(process: &OwnedHandle) {
    let mut code = 0;
    // SAFETY: The owned process handle is live; code points to writable DWORD storage.
    match unsafe { Threading::GetExitCodeProcess(process.raw(), &mut code) } {
        Ok(()) => tracing::info!(exit_code = code, "UI child exited"),
        Err(error) => tracing::error!(%error, "could not read UI child exit code"),
    }
}

/// Failures while resolving the executable or starting the error UI process.
#[derive(Debug, Snafu)]
pub(super) enum LaunchCoreErrorUiError {
    #[snafu(display("could not resolve executable for core error UI"))]
    ErrorExecutable { source: std::io::Error },
    #[snafu(display("could not create core error UI process"))]
    ErrorProcess { source: std::io::Error },
}

pub(super) fn launch_core_error(
    error: &quartz_protocol::CoreError,
) -> Result<(), LaunchCoreErrorUiError> {
    let executable = std::env::current_exe().context(ErrorExecutableSnafu)?;
    let mut command = std::process::Command::new(executable);
    command
        .arg("ui")
        .arg("--core-error")
        .arg(error.code.as_str());
    for backup in &error.backup_paths {
        command.arg("--backup-path").arg(backup);
    }
    // Command quotes each native Windows argument, including Unicode paths and spaces.
    let child = command.spawn().context(ErrorProcessSnafu)?;
    tracing::info!(pid=child.id(),code=%error.code,backup_paths=?error.backup_paths,"core error UI started");
    // Dropping Child closes its process handle without terminating the UI.
    Ok(())
}
