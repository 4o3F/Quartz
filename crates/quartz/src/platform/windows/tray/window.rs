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
                WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY, WNDCLASSW,
            },
        },
    },
    core::{PCWSTR, w},
};

/// Private messages delivered to the core's hidden window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
#[derive(num_enum::IntoPrimitive)]
pub(in crate::platform::windows) enum AppMessage {
    TrayCallback = WM_APP + 1,
    WakeCore = WM_APP + 2,
    WakeWorkers = WM_APP + 3,
    OpenUi = WM_APP + 4,
    UiFirstFrame = WM_APP + 5,
}

impl AppMessage {
    fn from_raw(message: u32) -> Option<Self> {
        match message {
            value if value == u32::from(Self::TrayCallback) => Some(Self::TrayCallback),
            value if value == u32::from(Self::WakeCore) => Some(Self::WakeCore),
            value if value == u32::from(Self::WakeWorkers) => Some(Self::WakeWorkers),
            value if value == u32::from(Self::OpenUi) => Some(Self::OpenUi),
            value if value == u32::from(Self::UiFirstFrame) => Some(Self::UiFirstFrame),
            _ => None,
        }
    }
}

// NIN_KEYSELECT is defined by shellapi.h as NIN_SELECT | NINF_KEY (0x1).
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

/// Notification code in LOWORD(lParam) for NOTIFYICON_VERSION_4 callbacks.
struct TrayNotification(LPARAM);
impl TrayNotification {
    fn code(self) -> u32 {
        // LOWORD keeps the unsigned notification code even when LPARAM is negative.
        const LOW_WORD_MASK: isize = 0xffff;
        u32::try_from(self.0.0 & LOW_WORD_MASK).expect("a masked low word fits in u32")
    }
}

/// Main-loop actions translated from window and notification icon messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CoreEvent {
    OpenUi,
    ShowTrayMenu,
    Hover,
    RestoreTray,
    Quit,
    EndSession,
    Deadline(quartz_domain::capture::Timestamp),
}

impl CoreEvent {
    fn from_message(message: u32, lparam: LPARAM, taskbar_created: u32) -> Option<Self> {
        match AppMessage::from_raw(message) {
            // Version 4 encodes the notification in LOWORD(lParam).
            Some(AppMessage::TrayCallback) => match TrayNotification(lparam).code() {
                NIN_SELECT | NIN_KEYSELECT => Some(Self::OpenUi),
                WM_CONTEXTMENU => Some(Self::ShowTrayMenu),
                NIN_POPUPOPEN => Some(Self::Hover),
                // TODO(M3): Start UI prewarming on hover, with TTL and cooldown.
                _ => None,
            },
            Some(
                AppMessage::WakeCore
                | AppMessage::WakeWorkers
                | AppMessage::OpenUi
                | AppMessage::UiFirstFrame,
            ) => None,
            None if message == taskbar_created => Some(Self::RestoreTray),
            None if matches!(message, WM_CLOSE | WM_DESTROY) => Some(Self::Quit),
            None => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum WindowRole {
    Tray,
    System,
}
impl WindowRole {
    fn class_name(self) -> PCWSTR {
        match self {
            Self::Tray => w!("Quartz.Core"),
            Self::System => w!("Quartz.SystemSources"),
        }
    }
    fn parent(self) -> Option<HWND> {
        match self {
            Self::Tray => None,
            Self::System => Some(WindowsAndMessaging::HWND_MESSAGE),
        }
    }
    fn observes(self, message: u32) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{
            WM_POWERBROADCAST, WM_TIMECHANGE, WM_WTSSESSION_CHANGE,
        };
        match self {
            Self::Tray => message == WM_TIMECHANGE,
            Self::System => matches!(message, WM_POWERBROADCAST | WM_WTSSESSION_CHANGE),
        }
    }
}

struct WindowState {
    role: WindowRole,
    taskbar_created: u32,
    events: RefCell<VecDeque<CoreEvent>>,
    ingress: RefCell<Option<super::super::sources::SystemIngress>>,
}

impl WindowState {
    #[cfg(test)]
    fn new(taskbar_created: u32) -> Self {
        Self::with_role(taskbar_created, WindowRole::Tray)
    }
    fn with_role(taskbar_created: u32, role: WindowRole) -> Self {
        Self {
            role,
            taskbar_created,
            events: RefCell::new(VecDeque::new()),
            ingress: RefCell::new(None),
        }
    }

    fn enqueue(&self, event: CoreEvent) {
        let mut events = self.events.borrow_mut();
        // Semantic actions are deduplicated; the one-shot timer has at most one pending deadline.
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

pub(in crate::platform::windows) struct Window {
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
        Self::create_with(WindowRole::Tray)
    }
    pub(in crate::platform::windows) fn create_system() -> Result<Self, CreateCoreWindowError> {
        Self::create_with(WindowRole::System)
    }
    fn create_with(role: WindowRole) -> Result<Self, CreateCoreWindowError> {
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
            lpszClassName: role.class_name(),
            ..Default::default()
        };
        // SAFETY: Class storage is initialized; the callback and class name live for the process lifetime.
        if unsafe { WindowsAndMessaging::RegisterClassW(&class) } == 0 {
            return Err(windows::core::Error::from_thread()).context(RegisterClassSnafu);
        }
        let mut window = Self {
            handle: HWND::default(),
            instance,
            state: Box::new(WindowState::with_role(taskbar_created, role)),
        };
        // SAFETY: The registered class uses a hidden top-level tray window or HWND_MESSAGE
        // for explicitly registered system sources. Neither role sets WS_VISIBLE.
        // The boxed state has a stable address and outlives every callback for this window.
        window.handle = unsafe {
            WindowsAndMessaging::CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                role.class_name(),
                w!("Quartz"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                role.parent(),
                None,
                Some(instance),
                Some(std::ptr::from_ref(window.state.as_ref()).cast()),
            )
        }
        .context(CreateWindowSnafu)?;
        Ok(window)
    }

    pub(in crate::platform::windows) fn attach_capture(
        &self,
        ingress: super::super::sources::SystemIngress,
    ) {
        *self.state.ingress.borrow_mut() = Some(ingress);
    }

    pub(in crate::platform::windows) fn raw(&self) -> HWND {
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
                // Best-effort teardown: the window may already have been destroyed.
                let _ = WindowsAndMessaging::DestroyWindow(self.handle);
            }
            // Best-effort teardown: Drop cannot report class unregistration failures.
            let _ = WindowsAndMessaging::UnregisterClassW(
                self.state.role.class_name(),
                Some(self.instance),
            );
        }
    }
}

/// Borrowed handle valid only while Windows is dispatching the current callback.
struct CallbackWindow(HWND);
impl CallbackWindow {
    /// Dispatches a message using the owning window's borrowed callback state.
    ///
    /// # Safety
    /// The handle and message parameters must be valid for the active Windows callback.
    unsafe fn dispatch(&self, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let window = self.0;

        if message == WM_NCCREATE {
            // SAFETY: WM_NCCREATE supplies a live CREATESTRUCTW. create() passed the stable address
            // of its owned WindowState as lpCreateParams, and retains that Box through destruction.
            let creation = unsafe {
                &*std::ptr::with_exposed_provenance::<CREATESTRUCTW>(lparam.0.cast_unsigned())
            };
            let state = creation.lpCreateParams.cast::<WindowState>();
            if !state.is_null() {
                // SAFETY: This callback owns the window on its creating thread; GWLP_USERDATA stores
                // the borrowed state pointer. Clearing last-error distinguishes a zero prior value
                // from SetWindowLongPtrW failure, in which case window creation is rejected.
                let installed = unsafe {
                    Foundation::SetLastError(ERROR_SUCCESS);
                    let previous = WindowsAndMessaging::SetWindowLongPtrW(
                        window,
                        GWLP_USERDATA,
                        state.expose_provenance().cast_signed(),
                    );
                    previous != 0 || Foundation::GetLastError() == ERROR_SUCCESS
                };
                if !installed {
                    return LRESULT(0);
                }
            }
        }
        // SAFETY: The window is live for this callback; its user-data slot is zero until WM_NCCREATE
        // stores the WindowState pointer and is cleared before the owned Box can be dropped.
        let address = unsafe { WindowsAndMessaging::GetWindowLongPtrW(window, GWLP_USERDATA) };
        let state = std::ptr::with_exposed_provenance::<WindowState>(address.cast_unsigned());
        if !state.is_null() {
            if message == WM_NCDESTROY {
                // SAFETY: The window is being destroyed on its owning thread. Detach its borrowed
                // state pointer before the owner can release the allocation after DestroyWindow.
                unsafe { WindowsAndMessaging::SetWindowLongPtrW(window, GWLP_USERDATA, 0) };
            } else {
                // SAFETY: Only create() installs this pointer, and Window keeps the Box alive until
                // after destruction. RefCell accesses stay on this same window-owning thread.
                let state = unsafe { &*state };
                use windows::Win32::UI::WindowsAndMessaging::{
                    WM_ENDSESSION, WM_QUERYENDSESSION, WM_TIMER,
                };
                if state.role.observes(message)
                    && let Some(ingress) = state.ingress.borrow().as_ref()
                    && let Err(error) = ingress.message(message, wparam)
                {
                    tracing::error!(%error, "could not observe system message; exiting core");
                    std::process::exit(1);
                }
                if message == WM_QUERYENDSESSION {
                    return LRESULT(1);
                }
                if message == WM_ENDSESSION && wparam.0 != 0 {
                    if let Some(ingress) = state.ingress.borrow().as_ref()
                        && let Err(error) = ingress.end_session()
                    {
                        tracing::error!(%error, "could not settle capture before session end; exiting core");
                        std::process::exit(1);
                    }
                    state.enqueue(CoreEvent::EndSession);
                    return LRESULT(0);
                }
                if message == WM_TIMER {
                    // SAFETY: Cancel the window timer before returning to any nested message loop.
                    unsafe {
                        // Cancellation is best-effort; a nested dispatch may already have cancelled it.
                        let _ = WindowsAndMessaging::KillTimer(Some(window), wparam.0);
                    }
                    use crate::core::sources::ClockReader;
                    match super::super::sources::SystemClock.read() {
                        Ok(at) => {
                            if let Some(ingress) = state.ingress.borrow().as_ref()
                                && let Err(error) = ingress
                                    .observe_at(at, quartz_domain::capture::CaptureEvent::Wake)
                            {
                                tracing::error!(%error, "could not deliver timer observation; exiting core");
                                std::process::exit(1);
                            }
                            state.enqueue(CoreEvent::Deadline(at));
                        }
                        Err(error) => {
                            tracing::error!(%error, "could not timestamp timer");
                            state.enqueue(CoreEvent::Quit);
                        }
                    }
                    return LRESULT(0);
                }
                if let Some(event) = CoreEvent::from_message(message, lparam, state.taskbar_created)
                {
                    state.enqueue(event);
                    // SAFETY: The callback's live HWND receives a pointer-free wake message after
                    // enqueue() has released its RefCell borrow, allowing nested message dispatch.
                    if let Err(error) = unsafe {
                        WindowsAndMessaging::PostMessageW(
                            Some(window),
                            u32::from(AppMessage::WakeCore),
                            WPARAM(0),
                            LPARAM(0),
                        )
                    } {
                        tracing::warn!(%error, "could not post queued core event wakeup");
                    }
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
}

/// Windows ABI adapter for the native callback window.
///
/// # Safety
/// Windows must supply a live window and valid window-procedure parameters.
unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: Windows owns this callback's live HWND and supplies its message parameters.
    unsafe { CallbackWindow(window).dispatch(message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::{Shell::NIN_POPUPOPEN, WindowsAndMessaging::WM_MOUSEMOVE};

    #[test]
    fn custom_message_ids_round_trip() {
        for message in [AppMessage::TrayCallback, AppMessage::WakeCore] {
            assert_eq!(AppMessage::from_raw(u32::from(message)), Some(message));
        }
        assert_eq!(AppMessage::from_raw(WM_CLOSE), None);
    }

    #[test]
    fn window_messages_translate_to_core_events() {
        let taskbar_created = 0xc000;
        let tray_callback = u32::from(AppMessage::TrayCallback);
        for (message, parameter, expected) in [
            (tray_callback, NIN_SELECT as isize, Some(CoreEvent::OpenUi)),
            (
                tray_callback,
                isize::MIN | NIN_SELECT as isize,
                Some(CoreEvent::OpenUi),
            ),
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
            (
                tray_callback,
                isize::try_from(NIN_POPUPOPEN).expect("notification fits LPARAM"),
                Some(CoreEvent::Hover),
            ),
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
            (u32::from(AppMessage::WakeCore), 0, None),
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
