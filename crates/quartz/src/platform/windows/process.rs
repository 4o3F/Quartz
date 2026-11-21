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
use crate::process_id::ProcessId;

/// Failures while resolving the executable and creating the UI child.
#[derive(Debug, Snafu)]
pub(super) enum LaunchUiError {
    #[snafu(display("could not resolve Quartz executable path (current_exe)"))]
    ResolveExecutable { source: std::io::Error },
    #[snafu(display("could not create UI child process (CreateProcessW)"))]
    CreateProcess { source: windows::core::Error },
}

/// Failures while resolving the executable or starting the error UI process.
#[derive(Debug, Snafu)]
pub(super) enum LaunchCoreErrorUiError {
    #[snafu(display("could not resolve executable for core error UI"))]
    ErrorExecutable { source: std::io::Error },
    #[snafu(display("could not create core error UI process"))]
    ErrorProcess { source: std::io::Error },
}

/// Owns the launched UI process handle until exit is observed.
#[derive(Debug)]
pub(super) struct UiProcess {
    handle: OwnedHandle,
    pid: ProcessId,
}
impl UiProcess {
    /// Creates the UI child from the absolute path of the running executable.
    ///
    /// # Errors
    /// Returns executable-path lookup or CreateProcessW failures.
    pub(super) fn launch() -> Result<Self, LaunchUiError> {
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
            cb: const { super::structure_size::<STARTUPINFOW>() },
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
        let process = unsafe { OwnedHandle::from_raw(information.hProcess) };
        // SAFETY: CreateProcessW transferred the separate thread handle as well.
        let _thread = unsafe { OwnedHandle::from_raw(information.hThread) };
        let pid = ProcessId::new(information.dwProcessId);
        tracing::info!(%pid, "UI child started");
        Ok(Self {
            handle: process,
            pid,
        })
    }
    /// Opens the access needed for exit observation and exit-code logging.
    pub(super) fn adopt(pid: ProcessId) -> Result<Self, AdoptUiError> {
        // SAFETY: The PID was verified by the pipe transport; the handle is not inherited.
        let handle = unsafe {
            Threading::OpenProcess(
                Threading::PROCESS_SYNCHRONIZE | Threading::PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid.raw(),
            )
        }
        .context(AdoptUiSnafu)?;
        // SAFETY: OpenProcess transferred unique ownership of a valid process handle.
        let handle = unsafe { OwnedHandle::from_raw(handle) };
        Ok(Self { handle, pid })
    }
    pub(super) fn log_exit(&self) {
        let mut exit_code = 0;
        // SAFETY: The owned process handle includes query access and exit_code is writable storage.
        match unsafe { Threading::GetExitCodeProcess(self.raw(), &mut exit_code) } {
            Ok(()) => tracing::info!(pid = %self.pid, exit_code, "UI process exited"),
            Err(error) => {
                tracing::error!(pid = %self.pid, %error, "could not read UI process exit code")
            }
        }
    }
    pub(super) fn raw(&self) -> windows::Win32::Foundation::HANDLE {
        self.handle.raw()
    }
}

impl crate::ui_state::UiProcess for UiProcess {
    fn pid(&self) -> ProcessId {
        self.pid
    }
}

#[derive(Debug, Snafu)]
#[snafu(display("could not open UI process for exit observation (OpenProcess)"))]
pub(super) struct AdoptUiError {
    source: windows::core::Error,
}

/// Borrows the startup failure passed to a standalone error UI process.
#[derive(Debug)]
pub(super) struct CoreErrorUi<'error> {
    error: &'error quartz_protocol::CoreError,
}
impl<'error> CoreErrorUi<'error> {
    pub(super) fn new(error: &'error quartz_protocol::CoreError) -> Self {
        Self { error }
    }
    pub(super) fn launch(&self) -> Result<(), LaunchCoreErrorUiError> {
        let executable = std::env::current_exe().context(ErrorExecutableSnafu)?;
        let mut command = std::process::Command::new(executable);
        command
            .arg("ui")
            .arg("--core-error")
            .arg(Into::<&'static str>::into(self.error.code));
        for backup in &self.error.backup_paths {
            command.arg("--backup-path").arg(backup);
        }
        // Command quotes each native Windows argument, including Unicode paths and spaces.
        let child = command.spawn().context(ErrorProcessSnafu)?;
        tracing::info!(pid=child.id(),code=%self.error.code,backup_paths=?self.error.backup_paths,"core error UI started");
        // Dropping Child closes its process handle without terminating the UI.
        Ok(())
    }
}
