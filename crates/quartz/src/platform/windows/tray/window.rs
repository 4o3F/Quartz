//! Hidden top-level window and callback-to-loop event delivery.

use std::{cell::RefCell, collections::VecDeque};

use snafu::{ResultExt, Snafu};
use windows::{
    Win32::{
        Foundation::{self, ERROR_SUCCESS, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader,
        UI::{
            Shell::{NIN_POPUPOPEN, NIN_SELECT},
            WindowsAndMessaging::{
                self, CREATESTRUCTW, GWLP_USERDATA, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
                WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY,
                WNDCLASSW,
            },
        },
    },
    core::w,
};

/// Private messages delivered to the core's hidden window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(super) enum AppMessage {
    TrayCallback = WM_APP + 1,
    WakeCore = WM_APP + 2,
}

impl AppMessage {
    fn from_raw(message: u32) -> Option<Self> {
        match message {
            value if value == Self::TrayCallback as u32 => Some(Self::TrayCallback),
            value if value == Self::WakeCore as u32 => Some(Self::WakeCore),
            _ => None,
        }
    }
}

// NIN_KEYSELECT is defined by shellapi.h as NIN_SELECT | NINF_KEY (0x1).
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

/// Main-loop actions translated from window and notification icon messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CoreEvent {
    OpenUi,
    ShowTrayMenu,
    RestoreTray,
    Quit,
}

impl CoreEvent {
    fn from_message(message: u32, lparam: LPARAM, taskbar_created: u32) -> Option<Self> {
        match AppMessage::from_raw(message) {
            // Version 4 encodes the notification in LOWORD(lParam).
            Some(AppMessage::TrayCallback) => match (lparam.0 as u32) & 0xffff {
                NIN_SELECT | NIN_KEYSELECT => Some(Self::OpenUi),
                WM_CONTEXTMENU => Some(Self::ShowTrayMenu),
                // TODO(M3): Start UI prewarming on hover, with TTL and cooldown.
                NIN_POPUPOPEN | WM_MOUSEMOVE => None,
                _ => None,
            },
            Some(AppMessage::WakeCore) => None,
            None if message == taskbar_created => Some(Self::RestoreTray),
            None if matches!(message, WM_CLOSE | WM_DESTROY) => Some(Self::Quit),
            None => None,
        }
    }
}

#[derive(Debug)]
struct WindowState {
    taskbar_created: u32,
    events: RefCell<VecDeque<CoreEvent>>,
}

impl WindowState {
    fn new(taskbar_created: u32) -> Self {
        Self {
            taskbar_created,
            events: RefCell::new(VecDeque::new()),
        }
    }

    fn enqueue(&self, event: CoreEvent) {
        let mut events = self.events.borrow_mut();
        // Deduplication bounds the queue to the four CoreEvent variants.
        if !events.contains(&event) {
            if event == CoreEvent::Quit {
                events.push_front(event);
            } else {
                events.push_back(event);
            }
        }
    }

    fn drain_events(&self) -> Vec<CoreEvent> {
        // Return owned events so no RefCell borrow survives a nested Windows message loop.
        self.events.borrow_mut().drain(..).collect()
    }
}

/// Failures while registering and creating the core's hidden top-level window.
#[derive(Debug, Snafu)]
pub(in crate::platform::windows) enum CreateCoreWindowError {
    #[snafu(display("could not find executable module (GetModuleHandleW)"))]
    GetModuleHandle { source: windows::core::Error },
    #[snafu(display("could not register TaskbarCreated message (RegisterWindowMessageW)"))]
    RegisterTaskbarMessage { source: windows::core::Error },
    #[snafu(display("could not register core window class (RegisterClassW)"))]
    RegisterClass { source: windows::core::Error },
    #[snafu(display("could not create core window (CreateWindowExW)"))]
    CreateWindow { source: windows::core::Error },
}

#[derive(Debug)]
pub(super) struct Window {
    handle: HWND,
    instance: HINSTANCE,
    // Drop destroys the window and clears its borrowed pointer before releasing this Box.
    state: Box<WindowState>,
}

impl Window {
    /// Registers a hidden top-level window that receives Explorer broadcasts.
    ///
    /// # Errors
    /// Returns executable-module lookup, message or class registration, or window creation failures.
    pub(super) fn create() -> Result<Self, CreateCoreWindowError> {
        // SAFETY: A null module name requests the current executable; it remains loaded until exit.
        let instance = unsafe { LibraryLoader::GetModuleHandleW(None) }
            .context(GetModuleHandleSnafu)?
            .into();
        // SAFETY: The message name is a static terminated UTF-16 string.
        let taskbar_created =
            unsafe { WindowsAndMessaging::RegisterWindowMessageW(w!("TaskbarCreated")) };
        if taskbar_created == 0 {
            return Err(windows::core::Error::from_thread()).context(RegisterTaskbarMessageSnafu);
        }
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_procedure),
            hInstance: instance,
            lpszClassName: w!("Quartz.Core"),
            ..Default::default()
        };
        // SAFETY: Class storage is initialized; the callback and class name live for the process lifetime.
        if unsafe { WindowsAndMessaging::RegisterClassW(&class) } == 0 {
            return Err(windows::core::Error::from_thread()).context(RegisterClassSnafu);
        }
        let mut window = Self {
            handle: HWND::default(),
            instance,
            state: Box::new(WindowState::new(taskbar_created)),
        };
        // SAFETY: The class was registered above; no parent means a top-level window. Omitting
        // WS_VISIBLE keeps it hidden while allowing Explorer's TaskbarCreated broadcasts.
        // The boxed state has a stable address and outlives every callback for this window.
        window.handle = unsafe {
            WindowsAndMessaging::CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("Quartz.Core"),
                w!("Quartz"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                Some(std::ptr::from_ref(window.state.as_ref()).cast()),
            )
        }
        .context(CreateWindowSnafu)?;
        Ok(window)
    }

    pub(super) fn raw(&self) -> HWND {
        self.handle
    }

    /// Takes pending actions, with Quit first, without holding a queue borrow.
    pub(super) fn drain_events(&self) -> Vec<CoreEvent> {
        self.state.drain_events()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: This object owns the window on the creating thread and its registered class.
        // The null handle only occurs when creation failed after class registration.
        // Clear GWLP_USERDATA before destruction so callbacks cannot outlive the boxed state;
        // struct fields, including state, are released only after this Drop method returns.
        unsafe {
            if !self.handle.is_invalid() {
                WindowsAndMessaging::SetWindowLongPtrW(self.handle, GWLP_USERDATA, 0);
                let _ = WindowsAndMessaging::DestroyWindow(self.handle);
            }
            let _ = WindowsAndMessaging::UnregisterClassW(w!("Quartz.Core"), Some(self.instance));
        }
    }
}

/// Records actions without borrowing application state across reentrant Windows calls.
///
/// # Safety
/// Windows must invoke this callback with the window-procedure ABI and valid message parameters.
unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: WM_NCCREATE supplies a live CREATESTRUCTW. create() passed the stable address
        // of its owned WindowState as lpCreateParams, and retains that Box through destruction.
        let creation = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        let state = creation.lpCreateParams.cast::<WindowState>();
        if !state.is_null() {
            // SAFETY: This callback owns the window on its creating thread; GWLP_USERDATA stores
            // the borrowed state pointer. Clearing last-error distinguishes a zero prior value
            // from SetWindowLongPtrW failure, in which case window creation is rejected.
            let installed = unsafe {
                Foundation::SetLastError(ERROR_SUCCESS);
                let previous =
                    WindowsAndMessaging::SetWindowLongPtrW(window, GWLP_USERDATA, state as isize);
                previous != 0 || Foundation::GetLastError() == ERROR_SUCCESS
            };
            if !installed {
                return LRESULT(0);
            }
        }
    }
    // SAFETY: The window is live for this callback; its user-data slot is zero until WM_NCCREATE
    // stores the WindowState pointer and is cleared before the owned Box can be dropped.
    let state = unsafe { WindowsAndMessaging::GetWindowLongPtrW(window, GWLP_USERDATA) }
        as *const WindowState;
    if !state.is_null() {
        if message == WM_NCDESTROY {
            // SAFETY: The window is being destroyed on its owning thread. Detach its borrowed
            // state pointer before the owner can release the allocation after DestroyWindow.
            unsafe { WindowsAndMessaging::SetWindowLongPtrW(window, GWLP_USERDATA, 0) };
        } else {
            // SAFETY: Only create() installs this pointer, and Window keeps the Box alive until
            // after destruction. RefCell accesses stay on this same window-owning thread.
            let state = unsafe { &*state };
            if let Some(event) = CoreEvent::from_message(message, lparam, state.taskbar_created) {
                state.enqueue(event);
                // SAFETY: The callback's live HWND receives a pointer-free wake message after
                // enqueue() has released its RefCell borrow, allowing nested message dispatch.
                let _ = unsafe {
                    WindowsAndMessaging::PostMessageW(
                        Some(window),
                        AppMessage::WakeCore as u32,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
                return LRESULT(0);
            }
            if AppMessage::from_raw(message).is_some() {
                return LRESULT(0);
            }
        }
    }
    // SAFETY: Unhandled callback parameters are passed unchanged to the system default procedure.
    unsafe { WindowsAndMessaging::DefWindowProcW(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_message_ids_round_trip() {
        for message in [AppMessage::TrayCallback, AppMessage::WakeCore] {
            assert_eq!(AppMessage::from_raw(message as u32), Some(message));
        }
        assert_eq!(AppMessage::from_raw(WM_CLOSE), None);
    }

    #[test]
    fn window_messages_translate_to_core_events() {
        let taskbar_created = 0xc000;
        let tray_callback = AppMessage::TrayCallback as u32;
        for (message, parameter, expected) in [
            (tray_callback, NIN_SELECT as isize, Some(CoreEvent::OpenUi)),
            (
                tray_callback,
                NIN_KEYSELECT as isize,
                Some(CoreEvent::OpenUi),
            ),
            (
                tray_callback,
                WM_CONTEXTMENU as isize,
                Some(CoreEvent::ShowTrayMenu),
            ),
            (tray_callback, NIN_POPUPOPEN as isize, None),
            (tray_callback, WM_MOUSEMOVE as isize, None),
            (
                tray_callback,
                0x1234_0000 | NIN_SELECT as isize,
                Some(CoreEvent::OpenUi),
            ),
            (
                tray_callback,
                0x1234_0000 | NIN_KEYSELECT as isize,
                Some(CoreEvent::OpenUi),
            ),
            (
                tray_callback,
                0x1234_0000 | WM_CONTEXTMENU as isize,
                Some(CoreEvent::ShowTrayMenu),
            ),
            (tray_callback, 0xffff, None),
            (taskbar_created, 0, Some(CoreEvent::RestoreTray)),
            (WM_CLOSE, 0, Some(CoreEvent::Quit)),
            (WM_DESTROY, 0, Some(CoreEvent::Quit)),
            (AppMessage::WakeCore as u32, 0, None),
            (WM_APP + 3, 0, None),
        ] {
            assert_eq!(
                CoreEvent::from_message(message, LPARAM(parameter), taskbar_created),
                expected,
                "message {message:#x}, lparam {parameter:#x}"
            );
        }
    }

    #[test]
    fn queue_deduplicates_and_prioritizes_quit() {
        let state = WindowState::new(0xc000);
        for _ in 0..20 {
            for event in [
                CoreEvent::OpenUi,
                CoreEvent::ShowTrayMenu,
                CoreEvent::RestoreTray,
                CoreEvent::Quit,
            ] {
                state.enqueue(event);
            }
        }
        assert_eq!(state.events.borrow().len(), 4, "one entry per event");
        let events = state.drain_events();
        assert_eq!(
            events,
            [
                CoreEvent::Quit,
                CoreEvent::OpenUi,
                CoreEvent::ShowTrayMenu,
                CoreEvent::RestoreTray
            ]
        );
        assert!(
            state.drain_events().is_empty(),
            "draining consumes pending events"
        );
        state.enqueue(CoreEvent::OpenUi);
        assert_eq!(
            state.drain_events(),
            [CoreEvent::OpenUi],
            "owned batches release their RefCell borrow"
        );
    }

    #[test]
    fn window_states_keep_independent_queues() {
        let first = WindowState::new(0xc000);
        let second = WindowState::new(0xc001);
        first.enqueue(CoreEvent::RestoreTray);
        assert!(second.drain_events().is_empty());
        assert_eq!(first.drain_events(), [CoreEvent::RestoreTray]);
    }
}
