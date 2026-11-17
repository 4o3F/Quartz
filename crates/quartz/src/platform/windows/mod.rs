//! Windows core lifecycle and main-thread platform integration.

mod handle;
mod instance;
mod process;
mod tray;
mod window;

use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    process::ExitCode,
};

use snafu::{OptionExt, ResultExt, Snafu};
use windows::{
    Win32::{
        Foundation::{WAIT_FAILED, WAIT_OBJECT_0},
        Globalization::{self, MUI_LANGUAGE_NAME},
        System::Threading::INFINITE,
        UI::WindowsAndMessaging::{
            self, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MESSAGEBOX_STYLE, MSG, PM_REMOVE,
            QS_ALLINPUT, WM_QUIT,
        },
    },
    core::{PCWSTR, PWSTR},
};

use crate::{logging, ui_state::UiState};
use instance::{AcquireInstance, AcquireInstanceError};
use tray::{AddTrayIconError, TrayIcon, TrayMenuItem};
use window::{CoreEvent, CreateCoreWindowError, Window};

#[derive(Debug, Snafu)]
enum StartCoreError {
    #[snafu(display("LOCALAPPDATA must name an absolute data directory"))]
    ResolveDataDirectory,
    #[snafu(display("could not create core log directory {} (create_dir_all)", path.display()))]
    CreateLogDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not open core log file {} (OpenOptions::open)", path.display()))]
    OpenLogFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not initialize core tracing"))]
    InitializeTracing { source: logging::InitTracingError },
    #[snafu(display("could not acquire core instance"))]
    AcquireInstance { source: AcquireInstanceError },
    #[snafu(display("could not create core message window"))]
    CreateWindow { source: CreateCoreWindowError },
    #[snafu(display("could not add core tray icon"))]
    AddTrayIcon { source: AddTrayIconError },
    #[snafu(display("could not run core message loop"))]
    RunMessageLoop { source: WaitCoreEventsError },
}

#[derive(Debug, Snafu)]
#[snafu(display("could not read preferred Windows UI languages (GetUserPreferredUILanguages)"))]
struct ReadUiLanguagesError {
    source: windows::core::Error,
}

#[derive(Debug, Snafu)]
#[snafu(display("could not wait for core messages and UI process (MsgWaitForMultipleObjects)"))]
struct WaitCoreEventsError {
    source: windows::core::Error,
}

/// Runs the Windows core on the calling main thread and reports startup failures.
pub(crate) fn run_core() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = ?error, "core failed");
            show_message(&quartz_i18n::translate("error.start_core"), MB_ICONERROR);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), StartCoreError> {
    let data_directory = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .context(ResolveDataDirectorySnafu)?
        .join("Quartz");
    let logs = data_directory.join("logs");
    fs::create_dir_all(&logs).context(CreateLogDirectorySnafu { path: &logs })?;
    let log_path = logs.join("core.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context(OpenLogFileSnafu { path: &log_path })?;
    logging::init_file(file).context(InitializeTracingSnafu)?;
    let language = ui_language().unwrap_or_else(|error| {
        tracing::warn!(%error, "using English UI language");
        "en"
    });
    quartz_i18n::set_locale(language);
    let _instance = match instance::acquire(&data_directory).context(AcquireInstanceSnafu)? {
        AcquireInstance::SameSession => {
            // TODO(M2): Forward the open request through the core pipe before exiting.
            tracing::info!("core already running in this Windows session");
            return Ok(());
        }
        AcquireInstance::OtherSession => {
            tracing::info!("core already running in another Windows session");
            show_message(
                &quartz_i18n::translate("error.other_session"),
                MB_ICONINFORMATION,
            );
            return Ok(());
        }
        AcquireInstance::Acquired(instance) => instance,
    };
    let window = Window::create().context(CreateWindowSnafu)?;
    let mut tray = TrayIcon::add(&window).context(AddTrayIconSnafu)?;
    tracing::info!(language, "core message loop started");
    message_loop(&window, &mut tray).context(RunMessageLoopSnafu)?;
    // The tray borrows the window, ensuring icon removal precedes window destruction.
    drop(tray);
    drop(window);
    tracing::info!("core stopped");
    Ok(())
}

fn ui_language() -> Result<&'static str, ReadUiLanguagesError> {
    let mut count = 0;
    let mut length = 0;
    // SAFETY: A null output buffer queries the size of the preferred-language MULTI_SZ.
    unsafe {
        Globalization::GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut length)
    }
    .context(ReadUiLanguagesSnafu)?;
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: The buffer has the queried length; Windows writes at most that many UTF-16 units.
    unsafe {
        Globalization::GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut length,
        )
    }
    .context(ReadUiLanguagesSnafu)?;
    let languages: Vec<_> = buffer
        .split(|unit| *unit == 0)
        .filter(|name| !name.is_empty())
        .map(String::from_utf16_lossy)
        .collect();
    let preferred: Vec<_> = languages.iter().map(String::as_str).collect();
    Ok(quartz_i18n::negotiate_language(&preferred))
}

fn message_loop(window: &Window, tray: &mut TrayIcon<'_>) -> Result<(), WaitCoreEventsError> {
    let mut ui: UiState<handle::OwnedHandle> = UiState::None;
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
            let events = window.drain_events();
            if events.is_empty() {
                break;
            }
            for event in events {
                match event {
                    CoreEvent::Quit => return Ok(()),
                    CoreEvent::RestoreTray => match tray.readd() {
                        Ok(()) => tracing::info!("tray icon restored after TaskbarCreated"),
                        Err(error) => tracing::error!(?error, "could not restore tray icon"),
                    },
                    CoreEvent::OpenUi => open_ui(&mut ui, tray),
                    CoreEvent::ShowTrayMenu => match tray.menu() {
                        Ok(Some(TrayMenuItem::Open)) => open_ui(&mut ui, tray),
                        Ok(Some(TrayMenuItem::Exit)) => return Ok(()),
                        Ok(None) => {}
                        Err(error) => tracing::error!(?error, "could not show tray menu"),
                    },
                }
            }
            // TrackPopupMenu may have queued more events in its nested message loop.
        }
        let handles = match &ui {
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
            if let UiState::Starting(process) = &ui {
                process::log_exit(process);
            }
            ui.exited();
            if let Err(error) = tray.tooltip("app.name") {
                tracing::error!(?error, "could not restore tray tooltip");
            }
        }
    }
}

fn open_ui(ui: &mut UiState<handle::OwnedHandle>, tray: &mut TrayIcon<'_>) {
    if let Err(error) = ui.open(process::launch_ui) {
        tracing::error!(?error, "could not launch UI");
        return;
    }
    if let Err(error) = tray.tooltip("tray.opening") {
        tracing::error!(?error, "could not set opening tooltip");
    }
}

fn show_message(text: &str, style: MESSAGEBOX_STYLE) {
    let text = wide(text);
    let title = wide(&quartz_i18n::translate("app.name"));
    // SAFETY: Both strings are terminated and remain live for the duration of this modal call.
    let _ = unsafe {
        WindowsAndMessaging::MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | style,
        )
    };
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
    fn startup_error_preserves_io_cause_and_path() {
        let path = PathBuf::from(r"C:\Quartz\logs");
        let error = Err::<(), _>(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .context(CreateLogDirectorySnafu { path: &path })
            .expect_err("log directory failure must reach the startup error");
        assert!(
            matches!(&error, StartCoreError::CreateLogDirectory { path: failed_path, .. } if failed_path == &path)
        );
        let source = error
            .source()
            .and_then(|source| source.downcast_ref::<std::io::Error>())
            .expect("original I/O error source");
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
