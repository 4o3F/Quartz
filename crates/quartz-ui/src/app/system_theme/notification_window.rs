//! A hidden top-level window receives desktop appearance broadcasts on GPUI's thread.

use std::{cell::RefCell, collections::HashMap};

use snafu::{ResultExt as _, Snafu};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, UnregisterClassW,
            WINDOW_EX_STYLE, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WNDCLASSW, WS_OVERLAPPED,
        },
    },
    core::{PCWSTR, w},
};

use super::{ThemeChange, ThemeEvents};
use crate::CallWindowsError;

/// An application-owned class, released after its last notification window is destroyed.
const WINDOW_CLASS: PCWSTR = w!("Quartz.AppearanceNotifications");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct WindowKey(*mut std::ffi::c_void);

thread_local! {
    // Callbacks clone the signal before waking the executor, without borrowing GPUI state.
    static SIGNALS: RefCell<HashMap<WindowKey, ThemeEvents>> = RefCell::default();
}

impl ThemeChange {
    fn from_message(message: u32) -> Option<Self> {
        match message {
            WM_SETTINGCHANGE => Some(Self::Settings),
            WM_SYSCOLORCHANGE => Some(Self::SystemColors),
            _ => None,
        }
    }
}

#[derive(Debug, Snafu)]
pub(super) enum CreateNotificationWindowError {
    #[snafu(display("could not read the application module handle"))]
    ReadModule {
        #[snafu(source(from(windows::core::Error, CallWindowsError::from)))]
        source: CallWindowsError,
    },
    #[snafu(display("could not register the appearance notification window class"))]
    RegisterClass {
        #[snafu(source(from(windows::core::Error, CallWindowsError::from)))]
        source: CallWindowsError,
    },
    #[snafu(display("could not create the appearance notification window"))]
    CreateWindow {
        #[snafu(source(from(windows::core::Error, CallWindowsError::from)))]
        source: CallWindowsError,
    },
}

#[derive(Debug)]
struct WindowClass {
    instance: Option<HINSTANCE>,
    #[cfg(test)]
    probe: Option<tests::ReleaseProbe>,
}

impl Drop for WindowClass {
    fn drop(&mut self) {
        if let Some(instance) = self.instance.take() {
            // SAFETY: this owner registered the class; the window is destroyed first.
            if let Err(error) = unsafe { UnregisterClassW(WINDOW_CLASS, Some(instance)) } {
                tracing::warn!(%error, "could not unregister appearance notification window class");
            }
        }
        #[cfg(test)]
        drop(self.probe.take());
    }
}

#[derive(Debug)]
struct HiddenWindow {
    handle: Option<HWND>,
    #[cfg(test)]
    probe: Option<tests::ReleaseProbe>,
}

impl Drop for HiddenWindow {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            SIGNALS.with(|signals| signals.borrow_mut().remove(&WindowKey(handle.0)));
            // SAFETY: the window was created on this thread and is owned by this value.
            if let Err(error) = unsafe { DestroyWindow(handle) } {
                tracing::warn!(%error, "could not destroy appearance notification window");
            }
        }
        #[cfg(test)]
        drop(self.probe.take());
    }
}

/// Kept in SystemTheme until its quit observer runs, before GPUI uninitializes OLE.
#[derive(Debug)]
pub(super) struct NotificationWindow {
    window: Option<HiddenWindow>,
    class: Option<WindowClass>,
}

impl NotificationWindow {
    pub(super) fn create(events: &ThemeEvents) -> Result<Self, CreateNotificationWindowError> {
        // SAFETY: a null module name requests the current process module without loading it.
        let instance = HINSTANCE::from(unsafe { GetModuleHandleW(None) }.context(ReadModuleSnafu)?);
        let class = WNDCLASSW {
            lpfnWndProc: Some(Self::window_proc),
            hInstance: instance,
            lpszClassName: WINDOW_CLASS,
            ..Default::default()
        };
        // SAFETY: class points to a valid descriptor and a static window procedure and name.
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(windows::core::Error::from_thread()).context(RegisterClassSnafu);
        }
        let class = WindowClass {
            instance: Some(instance),
            #[cfg(test)]
            probe: None,
        };
        // SAFETY: the class is registered on this foreground thread. No parent makes this a
        // top-level broadcast recipient. WS_VISIBLE is absent and the window is never shown.
        let handle = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                WINDOW_CLASS,
                w!(""),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .context(CreateWindowSnafu)?;
        SIGNALS.with(|signals| {
            signals
                .borrow_mut()
                .insert(WindowKey(handle.0), events.clone())
        });
        Ok(Self {
            window: Some(HiddenWindow {
                handle: Some(handle),
                #[cfg(test)]
                probe: None,
            }),
            class: Some(class),
        })
    }

    // GPUI's unfiltered GetMessageW/DispatchMessageW loop pumps this window. Waking ThemeEvents
    // only queues foreground work; WindowsDispatcher::dispatch_on_main_thread posts a message.
    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if let Some(change) = ThemeChange::from_message(message) {
            let signal =
                SIGNALS.with(|signals| signals.borrow().get(&WindowKey(window.0)).cloned());
            if let Some(signal) = signal {
                signal.signal(change);
            }
        }
        // SAFETY: Windows supplied the procedure arguments; unhandled messages use its default.
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }
}

impl Drop for NotificationWindow {
    fn drop(&mut self) {
        drop(self.window.take());
        drop(self.class.take());
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::rc::Rc;
    use windows::Win32::UI::WindowsAndMessaging::{WM_ACTIVATE, WM_THEMECHANGED};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ReleasedResource {
        Window,
        Class,
    }

    #[derive(Debug)]
    pub(super) struct ReleaseProbe {
        resource: ReleasedResource,
        released: Rc<RefCell<Vec<ReleasedResource>>>,
    }

    impl Drop for ReleaseProbe {
        fn drop(&mut self) {
            self.released.borrow_mut().push(self.resource);
        }
    }

    impl NotificationWindow {
        pub(crate) fn for_shutdown_test(released: Rc<RefCell<Vec<ReleasedResource>>>) -> Self {
            Self {
                window: Some(HiddenWindow {
                    handle: None,
                    probe: Some(ReleaseProbe {
                        resource: ReleasedResource::Window,
                        released: Rc::clone(&released),
                    }),
                }),
                class: Some(WindowClass {
                    instance: None,
                    probe: Some(ReleaseProbe {
                        resource: ReleasedResource::Class,
                        released,
                    }),
                }),
            }
        }
    }

    #[test]
    fn desktop_messages_map_to_typed_refresh_notifications() {
        assert_eq!(
            ThemeChange::from_message(WM_SETTINGCHANGE),
            Some(ThemeChange::Settings)
        );
        assert_eq!(
            ThemeChange::from_message(WM_SYSCOLORCHANGE),
            Some(ThemeChange::SystemColors)
        );
        assert_eq!(ThemeChange::from_message(WM_ACTIVATE), None);
        assert_eq!(ThemeChange::from_message(WM_THEMECHANGED), None);
    }
}
