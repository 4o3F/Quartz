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
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{Arc, Mutex},
};
use windows::Win32::{
    Foundation::{HANDLE, HWND, LPARAM, WPARAM},
    System::Threading::{CreateEventW, SetEvent},
    UI::WindowsAndMessaging::SendNotifyMessageW,
};

#[derive(Debug, snafu::Snafu)]
#[snafu(display("could not notify main thread"))]
pub(super) struct WakeMainThreadError {
    source: windows::core::Error,
}

#[derive(Default)]
struct WakeDelivery(Mutex<Option<WakeMainThreadError>>);
impl WakeDelivery {
    fn record(&self, result: Result<(), windows::core::Error>) {
        if let Err(source) = result {
            *self.0.lock().expect("wake error lock") = Some(WakeMainThreadError { source });
        }
    }
    fn notify(
        &self,
        notify: impl Fn() -> Result<(), windows::core::Error>,
        signal: impl FnOnce() -> Result<(), windows::core::Error>,
    ) {
        self.record(notify());
        if let Err(error) = signal() {
            self.record(Err(error));
            self.record(notify());
        }
    }
}

#[derive(Clone, Copy)]
struct NotificationWindow(usize);
impl NotificationWindow {
    fn new(window: HWND) -> Self {
        Self(window.0.expose_provenance())
    }
    fn raw(self) -> HWND {
        HWND(std::ptr::with_exposed_provenance_mut(self.0))
    }
}

pub(super) struct MainWake {
    event: Arc<OwnedHandle>,
    window: NotificationWindow,
    delivery: Arc<WakeDelivery>,
}
impl MainWake {
    pub(super) fn new(window: HWND) -> Result<Self, RunCoreError> {
        // SAFETY: Auto-reset event has no borrowed state and is uniquely owned below.
        let event = unsafe { CreateEventW(None, false, false, None) }
            .map_err(|source| RunCoreError::CreateWake { source })?;
        // SAFETY: Successful CreateEventW transfers a CloseHandle-compatible handle.
        let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
        Ok(Self {
            event: Arc::new(event),
            window: NotificationWindow::new(window),
            delivery: Arc::default(),
        })
    }
    pub(super) fn raw(&self) -> HANDLE {
        HANDLE(self.event.as_raw_handle())
    }
    pub(super) fn take_failure(&self) -> Option<WakeMainThreadError> {
        self.delivery.0.lock().expect("wake error lock").take()
    }
    fn notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        let event = Arc::clone(&self.event);
        let delivery = Arc::clone(&self.delivery);
        let window = self.window;
        Arc::new(move || {
            delivery.notify(
                || {
                    // SAFETY: Pointer-free sent notifications are dispatched by modal menu loops too.
                    unsafe {
                        SendNotifyMessageW(
                            window.raw(),
                            u32::from(tray::window::AppMessage::WakeWorkers),
                            WPARAM(0),
                            LPARAM(0),
                        )
                    }
                },
                || {
                    // SAFETY: The event stays owned by this closure and the main loop.
                    unsafe { SetEvent(HANDLE(event.as_raw_handle())) }
                },
            );
        })
    }
}

impl PlatformPorts {
    /// The tray has already created the calling main thread's message queue.
    pub(super) fn windows(
        system_language: quartz_i18n::Language,
        wake: &MainWake,
    ) -> Result<Self, RunCoreError> {
        let proposed_device_id = windows::core::GUID::new().context(CreateDeviceIdSnafu)?;
        let session_id = sources::SystemMessage::session_id().context(ReadSessionIdSnafu)?;
        let instance_id = windows::core::GUID::new().context(CreateInstanceIdSnafu)?;
        let main_wake = wake.notifier();
        let ui_wake = Arc::clone(&main_wake);
        Ok(Self {
            wake: main_wake,
            ui_wake: Arc::new(move |_command| {
                ui_wake();
                Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_native_wake_is_recorded_before_the_independent_signal() {
        let delivery = WakeDelivery::default();
        let signalled = std::cell::Cell::new(false);
        delivery.notify(
            || {
                Err(windows::core::Error::from_hresult(
                    windows::Win32::Foundation::E_FAIL,
                ))
            },
            || {
                assert!(delivery.0.lock().expect("failure lock").is_some());
                signalled.set(true);
                Ok(())
            },
        );
        assert!(signalled.get());
        assert!(delivery.0.lock().expect("failure lock").take().is_some());
    }
}
