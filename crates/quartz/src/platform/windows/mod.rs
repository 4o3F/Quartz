//! Windows core lifecycle and main-thread platform integration.

pub(crate) mod foreground;
mod handle;
mod instance;
pub(crate) mod ipc;
mod language;
mod locale;
mod ports;
mod process;
mod sources;
mod tray;

use crate::{
    core::app::{
        Autostart, CoreRuntime, MainCommand, PlatformPorts, RuntimeExit, StartCoreError,
        StopCoreError, Worker,
    },
    persistence::PersistenceState,
};
use quartz_domain::capture::{CaptureEffect, CaptureState};
use quartz_protocol::{CoreError, CoreErrorCode};
use sources::{Sources, SystemIngress};
use std::process::ExitCode;

use snafu::{ResultExt, Snafu};
use windows::Win32::{
    Foundation::{WAIT_FAILED, WAIT_OBJECT_0},
    System::Threading::INFINITE,
    UI::WindowsAndMessaging::{self, MSG, PM_REMOVE, QS_ALLINPUT, WM_QUIT},
};

use crate::{
    data_directory::{self, DataDirectory},
    logging,
    ui_state::UiState,
};
pub(crate) use instance::{AcquireInstance, AcquireInstanceError};
use tray::{CreateTrayError, Tray, TrayCommand, TrayIconState, TrayState, TrayTooltip};

pub(crate) use language::detect_ui_language;

#[derive(Debug, Snafu)]
enum RunCoreError {
    #[snafu(display("could not resolve Windows session identity"))]
    ReadSessionId { source: windows::core::Error },
    #[snafu(display("could not create installation identity"))]
    CreateDeviceId { source: windows::core::Error },
    #[snafu(display("could not create core instance identity"))]
    CreateInstanceId { source: windows::core::Error },
    #[snafu(display("could not start core runtime"))]
    StartRuntime { source: StartCoreError },
    #[snafu(display("could not stop core runtime"))]
    StopRuntime { source: StopCoreError },
    #[snafu(display("could not initialize capture system sources"))]
    ObserveSystem { source: sources::ObserveSystemError },
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

impl RunCoreError {
    fn report(&self) -> CoreError {
        match self {
            Self::StartRuntime { source } => source.report(),
            Self::OtherSession => CoreError::new(CoreErrorCode::OtherSession, vec![]),
            _ => CoreError::new(CoreErrorCode::CoreStartFailed, vec![]),
        }
    }
}

/// Encodes a compile-time Win32 structure size into its DWORD size field.
const fn structure_size<T>() -> u32 {
    let bytes = size_of::<T>().to_le_bytes();
    let mut index = size_of::<u32>();
    while index < bytes.len() {
        assert!(bytes[index] == 0, "Win32 structure size exceeds a DWORD");
        index += 1;
    }
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Owned NUL-terminated UTF-16 text for synchronous Win32 calls.
#[derive(Debug)]
struct WideString(Vec<u16>);
impl WideString {
    fn new(text: &str) -> Self {
        Self(text.encode_utf16().chain(Some(0)).collect())
    }
    fn as_ptr(&self) -> *const u16 {
        self.0.as_ptr()
    }
}

/// Attaches the parent console when one is available.
pub(super) fn attach_parent_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

    // SAFETY: This attaches the current process to its parent's console and takes no pointers.
    // No parent console and an already attached console are both normal launch conditions.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[derive(Debug, Clone, Copy)]
enum CoreExit {
    UserExit,
    Shutdown,
    SessionEnded,
    Failed,
}
impl CoreExit {
    fn from_loop(result: &Result<Self, WaitCoreEventsError>) -> Self {
        match result {
            Ok(exit) => *exit,
            Err(_) => Self::Failed,
        }
    }
    fn runtime_exit(self) -> RuntimeExit {
        match self {
            Self::UserExit => RuntimeExit::UserExit,
            Self::SessionEnded => RuntimeExit::SessionEnded,
            Self::Shutdown | Self::Failed => RuntimeExit::Shutdown,
        }
    }
}

/// Owns child-process tracking while the core runs on the main thread.
#[derive(Debug)]
pub(crate) struct CoreProcess {
    ui: UiState<process::UiProcess>,
    capture: Option<CaptureState>,
    persistence: PersistenceState,
    untriggered_changes: u64,
}
impl CoreProcess {
    pub(crate) fn new() -> Self {
        Self {
            ui: UiState::default(),
            capture: None,
            persistence: PersistenceState::Ready,
            untriggered_changes: 0,
        }
    }
    /// Runs the Windows core on the calling main thread and reports startup failures.
    pub(crate) fn run(mut self, autostart: Autostart) -> ExitCode {
        match self.start(autostart) {
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
    fn start(&mut self, autostart: Autostart) -> Result<(), RunCoreError> {
        let data_directory = DataDirectory::resolve().context(DataDirectorySnafu)?;
        data_directory
            .initialize_core_logging()
            .context(InitLogSnafu)?;
        let _instance = match data_directory
            .acquire_instance()
            .context(AcquireInstanceSnafu)?
        {
            AcquireInstance::SameSession => {
                if matches!(autostart, Autostart::Disabled)
                    && let Err(error) =
                        ipc::CoreClient::connect().and_then(|mut client| client.open_ui())
                {
                    tracing::warn!(%error, "could not forward open request to existing core within timeout");
                }
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
        let ports = PlatformPorts::windows()?;
        let runtime = CoreRuntime::start(&data_directory, ports).context(StartRuntimeSnafu)?;
        let ingress = SystemIngress::new(runtime.sender(), runtime.shutdown_port());
        tray.attach_capture(ingress.clone());
        let mut sources = Sources::start(tray.raw_window(), ingress).context(ObserveSystemSnafu)?;
        if let Err(error) = tray.update(TrayState::from_status(
            runtime.status.stop_reason(),
            TrayTooltip::Ready,
        )) {
            tracing::error!(?error, "could not update tray state");
        }
        tracing::info!(?autostart, "core message loop started");
        let result = self.message_loop(&mut tray, &runtime, &mut sources);
        self.ui = UiState::default();
        let exit = CoreExit::from_loop(&result).runtime_exit();
        let stopped = runtime.shutdown(exit);
        drop(sources);
        // Preserve transport failures before message-loop failures and worker cleanup errors.
        if matches!(stopped, Err(StopCoreError::StopPipe { .. })) {
            stopped.context(StopRuntimeSnafu)?;
        } else {
            result.context(RunMessageLoopSnafu)?;
            stopped.context(StopRuntimeSnafu)?;
        }
        drop(tray);
        tracing::info!("core stopped");
        Ok(())
    }
    fn message_loop(
        &mut self,
        tray: &mut Tray,
        runtime: &CoreRuntime,
        sources: &mut Sources,
    ) -> Result<CoreExit, WaitCoreEventsError> {
        let status = &runtime.status;
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
                    // The return value only says whether a character message was posted.
                    let _ = WindowsAndMessaging::TranslateMessage(&message);
                    WindowsAndMessaging::DispatchMessageW(&message);
                }
            }
            while let Ok(command) = runtime.ui_commands.try_recv() {
                match command {
                    crate::ipc::UiCommand::Open => self.open_ui(tray, status),
                    crate::ipc::UiCommand::FirstFrame => {
                        self.ui.first_frame();
                        if let Err(error) = tray.update(TrayState::from_status(
                            status.stop_reason(),
                            TrayTooltip::Ready,
                        )) {
                            tracing::warn!(%error, "could not restore tray after UI first frame");
                        }
                    }
                }
            }
            while let Ok(command) = runtime.commands().try_recv() {
                match command {
                    MainCommand::CaptureSnapshot { state, interval } => {
                        self.capture = Some(state);
                        status.publish_snapshot(self.capture.as_ref(), self.persistence, interval);
                    }
                    MainCommand::Persistence(state) => {
                        self.persistence = state;
                        status.publish(self.capture.as_ref(), self.persistence);
                    }
                    MainCommand::Fatal(worker) => {
                        tracing::error!(?worker, "stopping core after worker failure");
                        return Err(WaitCoreEventsError::WorkerFailed { worker });
                    }
                    MainCommand::Effect(effect) => match effect {
                        CaptureEffect::StateChanged(_) => {
                            unreachable!("state arrives through CaptureSnapshot")
                        }
                        CaptureEffect::ScheduleTimer(timer) => {
                            sources.timer(timer).context(wait_error::ObserveSnafu)?
                        }
                        CaptureEffect::QuerySession => {
                            sources.query_session().context(wait_error::ObserveSnafu)?
                        }
                        CaptureEffect::CheckIdle => {
                            sources.check_idle().context(wait_error::ObserveSnafu)?
                        }
                        CaptureEffect::UntriggeredChange {
                            previous_window,
                            reviewed_window,
                        } => {
                            tracing::debug!(
                                previous_hwnd = ?previous_window.map(|window| window.get()),
                                reviewed_hwnd = ?reviewed_window.map(|window| window.get()),
                                "foreground review window comparison"
                            );
                            self.untriggered_changes = self.untriggered_changes.saturating_add(1);
                            tracing::info!(
                                untriggered_changes = self.untriggered_changes,
                                "foreground fallback review detected an untriggered change"
                            );
                        }
                        CaptureEffect::IdleSampleFailed(error) => {
                            tracing::warn!(%error, "idle sample was rejected")
                        }
                        CaptureEffect::RebindHooks { target } => sources
                            .foreground(foreground::ForegroundCommand::Rebind { target })
                            .context(wait_error::ObserveSnafu)?,
                        CaptureEffect::ReviewForeground => sources
                            .foreground(foreground::ForegroundCommand::Review)
                            .context(wait_error::ObserveSnafu)?,
                        CaptureEffect::FallbackReviewForeground => sources
                            .foreground(foreground::ForegroundCommand::FallbackReview)
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
                if let Err(error) = tray.update(TrayState::from_status(
                    status.stop_reason(),
                    TrayTooltip::Ready,
                )) {
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
                        TrayCommand::OpenUi => self.open_ui(tray, status),
                    }
                }
                // Native dispatch can enqueue another batch of semantic commands.
            }
            let handles = self.ui.process().map(|process| [process.raw()]);
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
                if let Some(process) = self.ui.process() {
                    process.log_exit();
                }
                self.ui.exited();
                if let Err(error) = tray.update(TrayState::from_status(
                    status.stop_reason(),
                    TrayTooltip::Ready,
                )) {
                    tracing::error!(?error, "could not restore tray tooltip");
                }
            }
        }
    }
    fn open_ui(&mut self, tray: &mut Tray, status: &crate::ipc::StatusPublisher) {
        if let Err(error) = self.ui.open(process::UiProcess::launch) {
            tracing::error!(?error, "could not launch UI");
            return;
        }
        if let Err(error) = tray.update(TrayState::from_status(
            status.stop_reason(),
            TrayTooltip::Opening,
        )) {
            tracing::error!(?error, "could not set opening tooltip");
        }
    }
}

impl TrayState {
    fn from_status(reason: Option<quartz_protocol::StopReason>, normal: TrayTooltip) -> Self {
        use quartz_protocol::StopReason;
        let tooltip = match reason {
            Some(StopReason::PersistenceFailed) => TrayTooltip::PersistenceFailed,
            Some(StopReason::SessionUnavailable) => TrayTooltip::SessionUnavailable,
            Some(StopReason::CaptureGap) => TrayTooltip::CaptureGap,
            None
            | Some(
                StopReason::Starting
                | StopReason::Locked
                | StopReason::Disconnected
                | StopReason::Suspended
                | StopReason::Paused
                | StopReason::AwaitingBrowserContext
                | StopReason::NoForeground
                | StopReason::Ended,
            ) => {
                return Self {
                    icon: TrayIconState::Normal,
                    tooltip: normal,
                };
            }
        };
        Self {
            icon: TrayIconState::Warning,
            tooltip,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn published_stop_reasons_preserve_tray_appearance() {
        use quartz_protocol::StopReason;
        for reason in [
            None,
            Some(StopReason::Starting),
            Some(StopReason::Locked),
            Some(StopReason::Disconnected),
            Some(StopReason::Suspended),
            Some(StopReason::Paused),
            Some(StopReason::AwaitingBrowserContext),
            Some(StopReason::NoForeground),
            Some(StopReason::Ended),
        ] {
            for tooltip in [TrayTooltip::Ready, TrayTooltip::Opening] {
                let state = TrayState::from_status(reason, tooltip);
                assert_eq!(state.icon, TrayIconState::Normal);
                assert_eq!(state.tooltip, tooltip);
            }
        }
        for reason in [
            StopReason::PersistenceFailed,
            StopReason::SessionUnavailable,
            StopReason::CaptureGap,
        ] {
            let state = TrayState::from_status(Some(reason), TrayTooltip::Opening);
            assert_eq!(state.icon, TrayIconState::Warning);
            let expected = match reason {
                StopReason::PersistenceFailed => TrayTooltip::PersistenceFailed,
                StopReason::SessionUnavailable => TrayTooltip::SessionUnavailable,
                StopReason::CaptureGap => TrayTooltip::CaptureGap,
                _ => unreachable!("warning fixtures"),
            };
            assert_eq!(state.tooltip, expected);
        }
    }

    use windows::{Win32::Foundation::ERROR_ACCESS_DENIED, core::HRESULT};

    #[test]
    fn message_loop_failure_settles_at_current_time_and_preserves_error() {
        let result = Err::<CoreExit, _>(WaitCoreEventsError::WorkerFailed {
            worker: Worker::Capture,
        });
        let exit = CoreExit::from_loop(&result);
        assert_eq!(exit.runtime_exit(), RuntimeExit::Shutdown);
        assert!(result.is_err(), "the message-loop error still propagates");
        assert_eq!(CoreExit::Shutdown.runtime_exit(), RuntimeExit::Shutdown);
        assert_eq!(CoreExit::UserExit.runtime_exit(), RuntimeExit::UserExit);
    }

    #[test]
    fn startup_error_preserves_win32_cause_and_call_name() {
        let code = HRESULT::from_win32(ERROR_ACCESS_DENIED.0);
        let instance_error = AcquireInstanceError::OpenProcessToken {
            source: windows::core::Error::from_hresult(code),
        };
        let error = Err::<(), _>(instance_error)
            .context(AcquireInstanceSnafu)
            .expect_err("instance acquisition failure must reach the startup error");
        assert!(matches!(error, RunCoreError::AcquireInstance { .. }));
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
