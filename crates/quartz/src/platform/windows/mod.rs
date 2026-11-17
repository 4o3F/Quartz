//! Windows core lifecycle and main-thread platform integration.

mod handle;
mod instance;
mod language;
mod process;
mod tray;

use quartz_protocol::{CoreError, CoreErrorCode};
use std::{fs, path::PathBuf, process::ExitCode};

use snafu::{ResultExt, Snafu};
use windows::Win32::{
    Foundation::{WAIT_FAILED, WAIT_OBJECT_0},
    System::Threading::INFINITE,
    UI::WindowsAndMessaging::{self, MSG, PM_REMOVE, QS_ALLINPUT, WM_QUIT},
};

use crate::{
    data_directory::{self, DataDirectory},
    logging,
    rules::registry::{OpenRegistryShellError, RegistryShell},
    store,
    ui_state::UiState,
};
pub(crate) use instance::{AcquireInstance, AcquireInstanceError};
use tray::{CreateTrayError, Tray, TrayCommand, TrayState, TrayTooltip};

pub(crate) use language::UiLanguage;

#[derive(Debug, Snafu)]
enum StartCoreError {
    #[snafu(display("could not open core database"))]
    OpenStore { source: store::OpenStoreError },
    #[snafu(display("could not initialize rule registry"))]
    OpenRegistry { source: OpenRegistryShellError },
    #[snafu(display("could not resolve core data directory"))]
    DataDirectory {
        source: data_directory::ResolveDataDirectoryError,
    },
    #[snafu(display("{source}"))]
    InitLog { source: logging::InitFileLogError },
    #[snafu(display("could not acquire core instance"))]
    AcquireInstance { source: AcquireInstanceError },
    #[snafu(display("could not initialize shell tray"))]
    CreateTray { source: CreateTrayError },
    #[snafu(display("data directory is locked by another Windows session"))]
    OtherSession,
    #[snafu(display("could not run core message loop"))]
    RunMessageLoop { source: WaitCoreEventsError },
}

#[derive(Debug, Snafu)]
#[snafu(display("could not wait for core messages and UI process (MsgWaitForMultipleObjects)"))]
struct WaitCoreEventsError {
    source: windows::core::Error,
}

impl StartCoreError {
    fn report(&self) -> CoreError {
        match self {
            Self::OpenStore {
                source: store::OpenStoreError::NewerDatabase { .. },
            } => CoreError::new(
                CoreErrorCode::NewerDatabase,
                DataDirectory::resolve()
                    .map(|directory| directory.available_backups())
                    .unwrap_or_default(),
            ),
            Self::OpenStore {
                source: store::OpenStoreError::Migrate { backup, .. },
            } => CoreError::new(CoreErrorCode::MigrationFailed, vec![backup.clone()]),
            Self::OtherSession => CoreError::new(CoreErrorCode::OtherSession, vec![]),
            _ => CoreError::new(CoreErrorCode::CoreStartFailed, vec![]),
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Attaches the parent console when one is available.
pub(super) fn attach_parent_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

    // SAFETY: This attaches the current process to its parent's console and takes no pointers.
    // No parent console and an already attached console are both normal launch conditions.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

impl DataDirectory {
    fn available_backups(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(self.backups()) else {
            return Vec::new();
        };
        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
            .collect();
        files.sort();
        files
    }
}

/// Owns child-process tracking while the core runs on the main thread.
#[derive(Debug)]
pub(crate) struct CoreProcess {
    ui: UiState<process::UiProcess>,
}
impl CoreProcess {
    pub(crate) fn new() -> Self {
        Self { ui: UiState::None }
    }
    /// Runs the Windows core on the calling main thread and reports startup failures.
    pub(crate) fn run(mut self) -> ExitCode {
        match self.start() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let report = error.report();
                tracing::error!(?error, code = %report.code, "core failed");
                if let Err(error) = process::CoreErrorUi::new(&report).launch() {
                    tracing::error!(?error, "could not launch core error UI");
                }
                ExitCode::FAILURE
            }
        }
    }
    fn start(&mut self) -> Result<(), StartCoreError> {
        let data_directory = DataDirectory::resolve().context(DataDirectorySnafu)?;
        data_directory
            .init_file_log(logging::LogTarget::Core)
            .context(InitLogSnafu)?;
        let _instance = match data_directory
            .acquire_instance()
            .context(AcquireInstanceSnafu)?
        {
            AcquireInstance::SameSession => {
                // TODO(M2): Forward the open request through the core pipe before exiting.
                tracing::info!("core already running in this Windows session");
                return Ok(());
            }
            AcquireInstance::OtherSession => return OtherSessionSnafu.fail(),
            AcquireInstance::Acquired(instance) => instance,
        };
        let mut tray = Tray::create(TrayState {
            tooltip: TrayTooltip::Upgrading,
            ..TrayState::default()
        })
        .context(CreateTraySnafu)?;
        let store = store::Store::open(&data_directory).context(OpenStoreSnafu)?;
        let _registry = RegistryShell::open(store).context(OpenRegistrySnafu)?;
        if let Err(error) = tray.update(TrayState::default()) {
            tracing::error!(?error, "could not update tray state");
        }
        tracing::info!("core message loop started");
        // TODO(M2): Push runtime failures as structured state over the pipe and select the warning tray icon.
        let result = self.message_loop(&mut tray);
        self.ui = UiState::None;
        result.context(RunMessageLoopSnafu)?;
        drop(tray);
        tracing::info!("core stopped");
        Ok(())
    }
    fn message_loop(&mut self, tray: &mut Tray) -> Result<(), WaitCoreEventsError> {
        loop {
            let mut message = MSG::default();
            // SAFETY: message is writable MSG storage. Drain the entire queue before waiting again,
            // including system events that can wake MsgWaitForMultipleObjects without queued input.
            while unsafe { WindowsAndMessaging::PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }
                .as_bool()
            {
                if message.message == WM_QUIT {
                    return Ok(());
                }
                // SAFETY: The message was retrieved for this thread. The callback records actions
                // without borrowing the UI state or tray across this reentrant Windows call.
                unsafe {
                    let _ = WindowsAndMessaging::TranslateMessage(&message);
                    WindowsAndMessaging::DispatchMessageW(&message);
                }
            }
            loop {
                let events = tray.commands();
                if events.is_empty() {
                    break;
                }
                for event in events {
                    match event {
                        TrayCommand::Exit => return Ok(()),
                        TrayCommand::OpenUi => self.open_ui(tray),
                    }
                }
                // Native dispatch can enqueue another batch of semantic commands.
            }
            let handles = match &self.ui {
                UiState::None => None,
                UiState::Starting(process) => Some([process.raw()]),
            };
            // SAFETY: The optional process handle stays owned throughout the wait. Waiting for any
            // message or process exit on this thread needs neither polling nor a worker thread.
            let result = unsafe {
                WindowsAndMessaging::MsgWaitForMultipleObjects(
                    handles.as_ref().map(|handles| handles.as_slice()),
                    false,
                    INFINITE,
                    QS_ALLINPUT,
                )
            };
            if result == WAIT_FAILED {
                return Err(windows::core::Error::from_thread()).context(WaitCoreEventsSnafu);
            }
            if handles.is_some() && result == WAIT_OBJECT_0 {
                if let UiState::Starting(process) = &self.ui {
                    process.log_exit();
                }
                self.ui.exited();
                if let Err(error) = tray.update(TrayState::default()) {
                    tracing::error!(?error, "could not restore tray tooltip");
                }
            }
        }
    }
    fn open_ui(&mut self, tray: &mut Tray) {
        if let Err(error) = self.ui.open(process::UiProcess::launch) {
            tracing::error!(?error, "could not launch UI");
            return;
        }
        if let Err(error) = tray.update(TrayState {
            tooltip: TrayTooltip::Opening,
            ..TrayState::default()
        }) {
            tracing::error!(?error, "could not set opening tooltip");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use windows::{Win32::Foundation::ERROR_ACCESS_DENIED, core::HRESULT};

    #[test]
    fn startup_error_preserves_win32_cause_and_call_name() {
        let code = HRESULT::from_win32(ERROR_ACCESS_DENIED.0);
        let instance_error = AcquireInstanceError::OpenProcessToken {
            source: windows::core::Error::from_hresult(code),
        };
        let error = Err::<(), _>(instance_error)
            .context(AcquireInstanceSnafu)
            .expect_err("instance acquisition failure must reach the startup error");
        assert!(matches!(error, StartCoreError::AcquireInstance { .. }));
        let instance_source = error.source().expect("instance acquisition source");
        assert!(
            instance_source.to_string().contains("OpenProcessToken"),
            "Win32 call context"
        );
        let source = instance_source
            .source()
            .and_then(|source| source.downcast_ref::<windows::core::Error>())
            .expect("original Windows error source");
        assert_eq!(source.code(), code, "original Windows error code");
    }

    #[test]
    fn startup_error_wraps_file_log_error_without_redefining_it() {
        let path = std::path::PathBuf::from(r"C:\Quartz\logs");
        let log_error = logging::InitFileLogError::CreateDirectory {
            target: logging::LogTarget::Core,
            path: path.clone(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let error = Err::<(), _>(log_error)
            .context(InitLogSnafu)
            .expect_err("log failure reaches core startup");
        let source = error
            .source()
            .and_then(|source| source.downcast_ref::<logging::InitFileLogError>())
            .expect("original file log error");
        assert!(
            matches!(source, logging::InitFileLogError::CreateDirectory { path: failed_path, .. } if failed_path == &path)
        );
        assert_eq!(
            source
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .expect("original I/O cause")
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}
