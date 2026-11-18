//! Windows core lifecycle and main-thread platform integration.

pub(crate) mod foreground;
mod handle;
mod instance;
mod language;
mod process;
mod sources;
mod tray;

use crate::{
    capture::PersistenceState,
    core::{
        app::{
            CaptureIndicator, JoinWorkersError, MainCommand, RequestShutdownError,
            StartWorkersError, Worker, Workers,
        },
        sources::ClockReader,
    },
};
use quartz_domain::capture::{CaptureEffect, CaptureState, RunEndReason};
use quartz_protocol::{CoreError, CoreErrorCode};
use sources::{Sources, SystemClock, SystemIngress};
use std::sync::Arc;
use std::{fs, path::PathBuf, process::ExitCode};

use snafu::{ResultExt, Snafu};
use windows::Win32::{
    Foundation::{WAIT_FAILED, WAIT_OBJECT_0},
    System::Threading::INFINITE,
    UI::WindowsAndMessaging::{self, MSG, PM_REMOVE, QS_ALLINPUT, WM_QUIT},
};

use crate::{
    data_directory::{self, DataDirectory},
    logging, store,
    ui_state::UiState,
};
pub(crate) use instance::{AcquireInstance, AcquireInstanceError};
use tray::{CreateTrayError, Tray, TrayCommand, TrayIconState, TrayState, TrayTooltip};

pub(crate) use language::UiLanguage;

#[derive(Debug, Snafu)]
enum StartCoreError {
    #[snafu(display("could not open core database"))]
    OpenStore { source: store::OpenStoreError },
    #[snafu(display("could not start capture workers"))]
    StartWorkers { source: StartWorkersError },
    #[snafu(display("could not initialize capture system sources"))]
    ObserveSystem { source: sources::ObserveSystemError },
    #[snafu(display("could not read startup clock"))]
    ReadClock {
        source: crate::core::sources::ReadClockError,
    },
    #[snafu(display("could not resolve Windows session identity"))]
    SessionId { source: windows::core::Error },
    #[snafu(display("could not create installation identity"))]
    CreateDeviceId { source: windows::core::Error },
    #[snafu(display("could not load installation identity"))]
    LoadDeviceId { source: store::LoadDeviceIdError },
    #[snafu(display("could not stop capture workers"))]
    StopWorkers { source: JoinWorkersError },
    #[snafu(display("could not request capture shutdown"))]
    RequestShutdown { source: RequestShutdownError },
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
#[snafu(module(wait_error))]
enum WaitCoreEventsError {
    #[snafu(display("could not wait for core messages (MsgWaitForMultipleObjects)"))]
    Wait { source: windows::core::Error },
    #[snafu(display("could not observe system event"))]
    Observe { source: sources::ObserveSystemError },
    #[snafu(display("{worker} worker failed"))]
    WorkerFailed { worker: Worker },
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

#[derive(Debug, Clone, Copy)]
enum CoreExit {
    UserExit,
    Shutdown,
    SessionEnded,
}
impl CoreExit {
    fn reason(self) -> RunEndReason {
        match self {
            Self::UserExit => RunEndReason::UserExit,
            Self::Shutdown | Self::SessionEnded => RunEndReason::Shutdown,
        }
    }
}

/// Owns child-process tracking while the core runs on the main thread.
#[derive(Debug)]
pub(crate) struct CoreProcess {
    ui: UiState<process::UiProcess>,
    capture: Option<CaptureState>,
    persistence: PersistenceState,
    missed_events: u64,
}
impl CoreProcess {
    pub(crate) fn new() -> Self {
        Self {
            ui: UiState::None,
            capture: None,
            persistence: PersistenceState::Ready,
            missed_events: 0,
        }
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
        logging::install_core_panic_log();
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
        let mut store = store::Store::open(&data_directory).context(OpenStoreSnafu)?;
        let proposed = windows::core::GUID::new().context(CreateDeviceIdSnafu)?;
        let device_id = store
            .installation_id(&format!("{proposed:?}"))
            .context(LoadDeviceIdSnafu)?;
        let session_id = sources::SystemMessage::session_id().context(SessionIdSnafu)?;
        let metadata = store::capture::RunMetadata {
            device_id,
            win_session_id: store::capture::WindowsSessionId::new(session_id),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        // SAFETY: The main-thread message queue exists after Tray::create.
        let main_thread = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        let wake = Arc::new(move || {
            // SAFETY: This posts a pointer-free typed wakeup to the main thread's existing queue.
            if let Err(error) = unsafe {
                WindowsAndMessaging::PostThreadMessageW(
                    main_thread,
                    u32::from(tray::window::AppMessage::WakeWorkers),
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                )
            } {
                tracing::error!(%error, "could not wake main thread; exiting core");
                std::process::exit(1);
            }
        });
        let workers =
            Workers::start(store, metadata, sources::clock(), wake).context(StartWorkersSnafu)?;
        let ingress = SystemIngress::new(workers.sender.clone(), workers.shutdown_port());
        tray.attach_capture(ingress.clone());
        let mut sources = match Sources::start(tray.raw_window(), ingress) {
            Ok(sources) => sources,
            Err(error) => {
                let at = SystemClock.read().context(ReadClockSnafu)?;
                let _ = workers.request_shutdown(at, RunEndReason::Shutdown);
                let _ = workers.join();
                return Err(error).context(ObserveSystemSnafu);
            }
        };
        if let Err(error) = tray.update(self.tray_state(TrayTooltip::Ready)) {
            tracing::error!(?error, "could not update tray state");
        }
        tracing::info!("core message loop started");
        let result = self.message_loop(&mut tray, &workers, &mut sources);
        self.ui = UiState::None;
        let exit = result.as_ref().copied().unwrap_or(CoreExit::Shutdown);
        let reason = exit.reason();
        let now = SystemClock.read().context(ReadClockSnafu)?;
        let request = match exit {
            CoreExit::SessionEnded => Ok(()),
            _ => workers.request_shutdown(now, reason),
        };
        let joined = workers.join();
        drop(sources);
        result.context(RunMessageLoopSnafu)?;
        request.context(RequestShutdownSnafu)?;
        joined.context(StopWorkersSnafu)?;
        drop(tray);
        tracing::info!("core stopped");
        Ok(())
    }
    fn message_loop(
        &mut self,
        tray: &mut Tray,
        workers: &Workers,
        sources: &mut Sources,
    ) -> Result<CoreExit, WaitCoreEventsError> {
        loop {
            let mut message = MSG::default();
            // SAFETY: message is writable MSG storage. Drain the entire queue before waiting again,
            // including system events that can wake MsgWaitForMultipleObjects without queued input.
            while unsafe { WindowsAndMessaging::PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }
                .as_bool()
            {
                if message.message == WM_QUIT {
                    return Ok(CoreExit::Shutdown);
                }
                // SAFETY: The message was retrieved for this thread. The callback records actions
                // without borrowing the UI state or tray across this reentrant Windows call.
                unsafe {
                    let _ = WindowsAndMessaging::TranslateMessage(&message);
                    WindowsAndMessaging::DispatchMessageW(&message);
                }
            }
            while let Ok(command) = workers.commands.try_recv() {
                match command {
                    MainCommand::Persistence(state) => self.persistence = state,
                    MainCommand::Fatal(worker) => {
                        tracing::error!(?worker, "stopping core after worker failure");
                        return Err(WaitCoreEventsError::WorkerFailed { worker });
                    }
                    MainCommand::Effect(effect) => match effect {
                        CaptureEffect::StateChanged(state) => self.capture = Some(state),
                        CaptureEffect::ScheduleTimer(timer) => {
                            sources.timer(timer).context(wait_error::ObserveSnafu)?
                        }
                        CaptureEffect::QuerySession => {
                            sources.query_session().context(wait_error::ObserveSnafu)?
                        }
                        CaptureEffect::CheckIdle => {
                            sources.check_idle().context(wait_error::ObserveSnafu)?
                        }
                        CaptureEffect::MissedEvent => {
                            self.missed_events = self.missed_events.saturating_add(1);
                            tracing::info!(
                                missed_events = self.missed_events,
                                "foreground review detected a missed event"
                            );
                        }
                        CaptureEffect::IdleSampleFailed(error) => {
                            tracing::warn!(%error, "idle sample was rejected")
                        }
                        CaptureEffect::RebindHooks { generation, target } => sources
                            .foreground(foreground::ForegroundCommand::Rebind {
                                generation,
                                target,
                            })
                            .context(wait_error::ObserveSnafu)?,
                        CaptureEffect::ReviewForeground => sources
                            .foreground(foreground::ForegroundCommand::Review)
                            .context(wait_error::ObserveSnafu)?,
                        CaptureEffect::DiscardQueuedObservations => {
                            // The capture worker has already discarded the queue before publishing this effect.
                            tracing::trace!("capture worker discarded queued observations");
                        }
                        CaptureEffect::AwaitBrowserContext { .. } => {
                            // M5 supplies the browser port; CaptureCore owns the existing timeout.
                            tracing::trace!("browser context request awaits browser integration");
                        }
                        CaptureEffect::PolicyExpired(kind) => {
                            // CaptureCore has applied expiration and requested the necessary fresh observations.
                            tracing::trace!(?kind, "capture policy expired");
                        }
                    },
                }
                if let Err(error) = tray.update(self.tray_state(TrayTooltip::Ready)) {
                    tracing::error!(%error, "could not update capture tray state");
                }
            }
            loop {
                let events = tray.commands();
                if events.is_empty() {
                    break;
                }
                for event in events {
                    match event {
                        TrayCommand::Exit => return Ok(CoreExit::UserExit),
                        TrayCommand::EndSession => return Ok(CoreExit::SessionEnded),
                        TrayCommand::Deadline(at) => {
                            sources.fired(at).context(wait_error::ObserveSnafu)?
                        }
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
                return Err(windows::core::Error::from_thread()).context(wait_error::WaitSnafu);
            }
            if handles.is_some() && result == WAIT_OBJECT_0 {
                if let UiState::Starting(process) = &self.ui {
                    process.log_exit();
                }
                self.ui.exited();
                if let Err(error) = tray.update(self.tray_state(TrayTooltip::Ready)) {
                    tracing::error!(?error, "could not restore tray tooltip");
                }
            }
        }
    }
    fn tray_state(&self, tooltip: TrayTooltip) -> TrayState {
        let indicator = CaptureIndicator::from_state(self.capture.as_ref(), self.persistence);
        let icon = match indicator {
            CaptureIndicator::Normal => TrayIconState::Normal,
            _ => TrayIconState::Warning,
        };
        let tooltip = match indicator {
            CaptureIndicator::Normal => tooltip,
            CaptureIndicator::PersistenceFailed => TrayTooltip::PersistenceFailed,
            CaptureIndicator::SessionUnavailable => TrayTooltip::SessionUnavailable,
            CaptureIndicator::CaptureGap => TrayTooltip::CaptureGap,
        };
        TrayState { icon, tooltip }
    }
    fn open_ui(&mut self, tray: &mut Tray) {
        if let Err(error) = self.ui.open(process::UiProcess::launch) {
            tracing::error!(?error, "could not launch UI");
            return;
        }
        if let Err(error) = tray.update(self.tray_state(TrayTooltip::Opening)) {
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
