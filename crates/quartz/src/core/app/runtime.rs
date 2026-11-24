//! Composition root and one teardown path for partial startup and normal exit.

use super::{
    CaptureSender, JoinWorkersError, RequestShutdownError, SHUTDOWN_TIMEOUT, ShutdownPort,
    StartWorkersError, Workers,
};
use crate::{
    core::{
        foreground::CreateForeground,
        sources::{ClockReader, ReadClockError},
    },
    data_directory::DataDirectory,
    ipc::{QueueUiError, Services, StatusPublisher, UiCommand, UiPort},
    store::{
        self, Store,
        capture::{RunMetadata, WindowsSessionId},
    },
};
use quartz_domain::capture::RunEndReason;
use quartz_protocol::{CoreError, CoreErrorCode, InstanceId, LocaleSettings, UiLanguage};
use std::{error::Error, sync::Arc, time::Duration};

pub(crate) type CallPlatformPortError = Box<dyn Error + Send + Sync>;

/// Transport startup receives the shutdown budget and application services.
pub(crate) struct PipePorts {
    pub shutdown_timeout: Duration,
    pub services: Services,
}

pub(crate) trait CorePipe: Send {
    fn control(&self, op: quartz_protocol::ControlOp);
    fn target_control(
        &self,
        connection: crate::ipc::registry::ConnectionId,
        op: quartz_protocol::ControlOp,
    );
    /// Flushes pending control messages within the transport's shutdown budget and joins I/O.
    fn shutdown(self: Box<Self>) -> Result<(), CallPlatformPortError>;
}

type StartPipePort =
    Box<dyn FnOnce(PipePorts) -> Result<Box<dyn CorePipe>, CallPlatformPortError> + Send>;

pub(crate) struct PlatformPorts {
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub ui_wake: Arc<dyn Fn(UiCommand) -> Result<(), QueueUiError> + Send + Sync>,
    pub clock: Arc<dyn ClockReader>,
    pub session_id: WindowsSessionId,
    pub proposed_device_id: String,
    pub instance_id: InstanceId,
    pub foreground: CreateForeground,
    pub pipe: StartPipePort,
    pub language: quartz_i18n::Language,
    pub locale: Option<LocaleSettings>,
}

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum StartCoreError {
    #[snafu(display("could not read logical-day startup clock"))]
    ReadClock { source: ReadClockError },
    #[snafu(display("could not resolve logical day"))]
    ResolveLogicalDay { source: crate::query::RunQueryError },
    #[snafu(display("could not open core database"))]
    OpenStore {
        source: store::OpenStoreError,
        directory: DataDirectory,
    },
    #[snafu(display("could not load installation identity"))]
    LoadDeviceId { source: store::LoadDeviceIdError },
    #[snafu(display("could not load UI settings"))]
    LoadSettings {
        source: store::settings::AccessSettingError,
    },
    #[snafu(display("could not start capture workers"))]
    StartWorkers { source: StartWorkersError },
    #[snafu(display("could not start core pipe server"))]
    StartPipe { source: CallPlatformPortError },
}
impl StartCoreError {
    pub(crate) fn report(&self) -> CoreError {
        if let Self::OpenStore { source, directory } = self {
            match source.startup_failure() {
                store::StartupStoreFailure::NewerDatabase => {
                    return CoreError::new(
                        CoreErrorCode::NewerDatabase,
                        directory.available_backups(),
                    );
                }
                store::StartupStoreFailure::MigrationFailed { backup } => {
                    return CoreError::new(
                        CoreErrorCode::MigrationFailed,
                        vec![backup.to_path_buf()],
                    );
                }
                store::StartupStoreFailure::Other => {}
            }
        }
        CoreError::new(CoreErrorCode::CoreStartFailed, vec![])
    }
}

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum StopCoreError {
    #[snafu(display("could not stop core pipe server"))]
    StopPipe { source: CallPlatformPortError },
    #[snafu(display("could not read shutdown clock"))]
    ReadClock { source: ReadClockError },
    #[snafu(display("could not request capture shutdown"))]
    RequestShutdown { source: RequestShutdownError },
    #[snafu(display("could not stop capture workers"))]
    StopWorkers { source: JoinWorkersError },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeExit {
    UserExit,
    Shutdown,
    /// WM_ENDSESSION has already settled both workers through ShutdownPort.
    SessionEnded,
}
impl RuntimeExit {
    fn reason(self) -> RunEndReason {
        match self {
            Self::UserExit => RunEndReason::UserExit,
            Self::Shutdown | Self::SessionEnded => RunEndReason::Shutdown,
        }
    }
}

pub(crate) struct CoreRuntime {
    workers: Option<Workers>,
    pipe: Option<Box<dyn CorePipe>>,
    clock: Arc<dyn ClockReader>,
    pub status: StatusPublisher,
    _ui_port: UiPort,
    pub summary: crate::ipc::summary::SummaryPort,
    pub timed: crate::ipc::TimedPolicyPort,
    pub ui_commands: crate::ipc::UiCommands,
}
impl DataDirectory {
    /// Installs core logging before platform instance acquisition and tray creation.
    pub(crate) fn initialize_core_logging(
        &self,
    ) -> Result<crate::logging::FileLogGuard, crate::logging::InitFileLogError> {
        let guard = self.init_file_log(crate::logging::LogTarget::Core)?;
        crate::logging::install_core_panic_log(guard.panic_log());
        Ok(guard)
    }
}
impl CoreRuntime {
    pub(crate) fn target_ui(
        &self,
        connection: crate::ipc::registry::ConnectionId,
        op: quartz_protocol::ControlOp,
    ) {
        if let Some(pipe) = &self.pipe {
            pipe.target_control(connection, op);
        } else {
            tracing::warn!("UI control requested after pipe shutdown");
        }
    }

    pub(crate) fn start(
        directory: &DataDirectory,
        ports: PlatformPorts,
    ) -> Result<Self, StartCoreError> {
        let mut store = Store::open(directory).map_err(|source| StartCoreError::OpenStore {
            source,
            directory: directory.clone(),
        })?;
        let day_offset = store
            .day_start_offset()
            .map_err(|source| StartCoreError::LoadSettings { source })?;
        let device_id = store
            .installation_id(&ports.proposed_device_id)
            .map_err(|source| StartCoreError::LoadDeviceId { source })?;
        let metadata = RunMetadata {
            device_id,
            win_session_id: ports.session_id,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let language = match ports.language {
            quartz_i18n::Language::SimplifiedChinese => UiLanguage::SimplifiedChinese,
            quartz_i18n::Language::English => UiLanguage::English,
            #[cfg(debug_assertions)]
            quartz_i18n::Language::PseudoEnglish => {
                tracing::debug!("publishing English catalog identity for debug pseudo language");
                UiLanguage::English
            }
        };
        let system = quartz_protocol::EffectiveUiSettings {
            resident: Default::default(),
            prewarm: Default::default(),
            ui_language: language,
            locale: ports.locale,
        };
        let ui = store
            .recover_ui_settings()
            .map_err(|source| StartCoreError::LoadSettings { source })?;
        let effective = ui.resolve(system);
        let now = ports
            .clock
            .read()
            .map_err(|source| StartCoreError::ReadClock { source })?;
        let logical_day = crate::ipc::LogicalDay::new(now.utc.get(), day_offset)
            .map_err(|source| StartCoreError::ResolveLogicalDay { source })?;
        let status = StatusPublisher::with_locale(ports.instance_id, effective, logical_day);
        let crate::ipc::UiCommandChannel {
            port: ui_port,
            commands: ui_commands,
        } = crate::ipc::UiCommands::channel(ports.ui_wake);
        let mut workers = Workers::start(
            store,
            metadata,
            Arc::clone(&ports.clock),
            Arc::clone(&ports.wake),
            ports.foreground,
        )
        .map_err(|source| StartCoreError::StartWorkers { source })?;
        workers.writes = workers
            .writes
            .clone()
            .with_settings(system, Arc::clone(&ui_port))
            .with_completion_wake(Arc::clone(&ports.wake));
        let timed = workers
            .sender
            .timed_port(Arc::clone(&ports.clock), day_offset);
        let summary = crate::ipc::summary::SummaryTask::channel(day_offset, ports.wake);
        let pipe_ports = PipePorts {
            shutdown_timeout: SHUTDOWN_TIMEOUT,
            services: Services {
                summary: Some(summary.task),
                queries: workers.queries.clone(),
                writes: workers.writes.clone(),
                versions: workers.versions.clone(),
                status: status.subscribe(),
                ui: Arc::clone(&ui_port),
                timed: timed.clone(),
            },
        };
        let mut runtime = Self {
            workers: Some(workers),
            pipe: None,
            clock: ports.clock,
            status,
            summary: summary.port,
            ui_commands,
            _ui_port: ui_port,
            timed,
        };
        // The runtime owns workers before the next fallible step; Drop uses the same shutdown path.
        runtime.pipe =
            Some((ports.pipe)(pipe_ports).map_err(|source| StartCoreError::StartPipe { source })?);
        Ok(runtime)
    }
    pub(crate) fn writes(&self) -> crate::persistence::dispatch::WritePort {
        self.workers().writes.clone()
    }
    pub(crate) fn sender(&self) -> CaptureSender {
        self.workers().sender.clone()
    }
    pub(crate) fn shutdown_port(&self) -> ShutdownPort {
        self.workers().shutdown_port()
    }
    pub(crate) fn commands(&self) -> &super::MainReceiver {
        &self.workers().commands
    }
    fn workers(&self) -> &Workers {
        self.workers
            .as_ref()
            .expect("runtime owns workers until shutdown")
    }
    pub(crate) fn shutdown(mut self, exit: RuntimeExit) -> Result<(), StopCoreError> {
        self.stop(exit)
    }
    fn stop(&mut self, exit: RuntimeExit) -> Result<(), StopCoreError> {
        let Some(workers) = self.workers.take() else {
            return Ok(());
        };
        let pipe_stopped = if let Some(pipe) = self.pipe.take() {
            if matches!(exit, RuntimeExit::UserExit) {
                pipe.control(quartz_protocol::ControlOp::UiShutdown);
            }
            pipe.shutdown()
                .map_err(|source| StopCoreError::StopPipe { source })
        } else {
            Ok(())
        };
        let mut clock_result = Ok(());
        let request = if matches!(exit, RuntimeExit::SessionEnded) {
            Ok(())
        } else {
            match self.clock.read() {
                Ok(at) => workers.request_shutdown(at, exit.reason()),
                Err(source) => {
                    tracing::error!(%source, "shutdown clock failed; settling at the last capture observation");
                    clock_result = Err(StopCoreError::ReadClock { source });
                    workers.request_shutdown_after_clock_failure(exit.reason())
                }
            }
        }.map_err(|source| StopCoreError::RequestShutdown { source });
        let joined = workers
            .join()
            .map_err(|source| StopCoreError::StopWorkers { source });
        // Evaluate every cleanup stage before propagating the first failure.
        let mut outcome = Ok(());
        for result in [pipe_stopped, clock_result, request, joined] {
            if let Err(error) = result {
                tracing::error!(%error, "core shutdown stage failed");
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
        }
        outcome
    }
}
impl Drop for CoreRuntime {
    fn drop(&mut self) {
        if let Err(error) = self.stop(RuntimeExit::Shutdown) {
            tracing::error!(%error, "could not shut down core runtime");
        }
    }
}
