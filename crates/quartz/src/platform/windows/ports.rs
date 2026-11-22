//! Native callbacks and transport construction supplied to the core runtime.

use super::{
    CreateDeviceIdSnafu, CreateInstanceIdSnafu, ReadSessionIdSnafu, RunCoreError, foreground, ipc,
    locale, sources, tray,
};
use crate::{
    core::app::{CallPlatformPortError, CorePipe, PipePorts, PlatformPorts},
    store::capture::WindowsSessionId,
};
use snafu::ResultExt;
use std::sync::Arc;
use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::WindowsAndMessaging::PostThreadMessageW,
};

impl PlatformPorts {
    /// The tray has already created the calling main thread's message queue.
    pub(super) fn windows(system_language: quartz_i18n::Language) -> Result<Self, RunCoreError> {
        let proposed_device_id = windows::core::GUID::new().context(CreateDeviceIdSnafu)?;
        let session_id = sources::SystemMessage::session_id().context(ReadSessionIdSnafu)?;
        let instance_id = windows::core::GUID::new().context(CreateInstanceIdSnafu)?;
        // SAFETY: This reads the current thread ID after Tray::create established its queue.
        let main_thread = unsafe { GetCurrentThreadId() };
        Ok(Self {
            wake: Arc::new(move || {
                // SAFETY: The main thread owns an existing queue; the typed message has no pointers.
                if let Err(error) = unsafe {
                    PostThreadMessageW(
                        main_thread,
                        u32::from(tray::window::AppMessage::WakeWorkers),
                        WPARAM(0),
                        LPARAM(0),
                    )
                } {
                    tracing::error!(%error, "could not wake main thread; exiting core");
                    std::process::exit(1);
                }
            }),
            ui_wake: Arc::new(move |command| {
                let message = match command {
                    crate::ipc::UiCommand::Open => tray::window::AppMessage::OpenUi,
                    crate::ipc::UiCommand::Registered { .. }
                    | crate::ipc::UiCommand::Hidden { .. }
                    | crate::ipc::UiCommand::Unregistered { .. }
                    | crate::ipc::UiCommand::SettingsChanged(_) => {
                        tray::window::AppMessage::WakeWorkers
                    }
                    crate::ipc::UiCommand::FirstFrame { .. } => {
                        tray::window::AppMessage::UiFirstFrame
                    }
                };
                // SAFETY: The main thread owns an existing queue; the typed message has no pointers.
                unsafe { PostThreadMessageW(main_thread, u32::from(message), WPARAM(0), LPARAM(0)) }
                    .map_err(|error| {
                        tracing::warn!(%error, "could not post IPC command to main thread");
                        crate::ipc::QueueUiError
                    })
            }),
            clock: sources::clock(),
            session_id: WindowsSessionId::new(session_id),
            proposed_device_id: format!("{proposed_device_id:?}"),
            instance_id: quartz_protocol::InstanceId(format!("{instance_id:?}")),
            foreground: Box::new(foreground::create_resolver),
            pipe: Box::new(|ports: PipePorts| {
                let config =
                    ipc::PipeServerConfig::current(ports.shutdown_timeout, ports.services)?;
                Ok(Box::new(ipc::PipeServer::start(config)?))
            }),
            language: system_language,
            locale: match locale::UserLocale::read() {
                Ok(locale) => Some(locale),
                Err(error) => {
                    tracing::warn!(%error, "regional settings unavailable in state snapshot");
                    None
                }
            },
        })
    }
}
impl CorePipe for ipc::PipeServer {
    fn target_control(
        &self,
        connection: crate::ipc::registry::ConnectionId,
        op: quartz_protocol::ControlOp,
    ) {
        self.target_control(connection, op);
    }

    fn control(&self, op: quartz_protocol::ControlOp) {
        self.control(op);
    }
    fn shutdown(self: Box<Self>) -> Result<(), CallPlatformPortError> {
        Ok((*self).shutdown()?)
    }
}
