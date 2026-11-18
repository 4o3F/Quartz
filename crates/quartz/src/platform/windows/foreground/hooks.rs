//! Out-of-context hooks owned and dispatched exclusively by the main thread.

use super::Window;
use crate::core::{
    app::{CaptureSender, ObserveCaptureError},
    foreground::{
        Desktop, EventDecision, EventKind, ForegroundTarget, NativeEvent, RebindDecision,
        SELF_CHILD, Snapshot, TITLE_BUFFER_UNITS, TitleRead, WINDOW_OBJECT, title_snapshot,
    },
    sources::{ClockReader, ReadClockError},
};
use crate::platform::windows::sources::SystemClock;
use quartz_domain::capture::{ForegroundGeneration, HookTarget, ObservationSource};
use std::{cell::RefCell, marker::PhantomData, rc::Rc};
use windows::Win32::{
    Foundation::{self, ERROR_SUCCESS, HWND},
    UI::{
        Accessibility::{self, HWINEVENTHOOK},
        WindowsAndMessaging::{
            self, EVENT_OBJECT_DESTROY, EVENT_OBJECT_NAMECHANGE, EVENT_SYSTEM_DESKTOPSWITCH,
            EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
            WINEVENT_OUTOFCONTEXT,
        },
    },
};

/// One process hook for title changes and one for destroys, capture 02 §1.1.
const PROCESS_EVENT_COUNT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, num_enum::TryFromPrimitive)]
#[repr(u32)]
enum WinEvent {
    Foreground = EVENT_SYSTEM_FOREGROUND,
    Title = EVENT_OBJECT_NAMECHANGE,
    Destroy = EVENT_OBJECT_DESTROY,
    MinimizeStart = EVENT_SYSTEM_MINIMIZESTART,
    MinimizeEnd = EVENT_SYSTEM_MINIMIZEEND,
    DesktopSwitch = EVENT_SYSTEM_DESKTOPSWITCH,
}
impl WinEvent {
    fn kind(self) -> EventKind {
        match self {
            Self::Foreground => EventKind::Foreground,
            Self::Title => EventKind::Title,
            Self::Destroy => EventKind::Destroy,
            Self::MinimizeStart | Self::MinimizeEnd | Self::DesktopSwitch => EventKind::Review,
        }
    }
}
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(in crate::platform::windows) enum ObserveForegroundError {
    #[snafu(display("could not install foreground event hook"))]
    Hook { source: windows::core::Error },
    #[snafu(display("could not stamp foreground snapshot"))]
    Clock { source: ReadClockError },
    #[snafu(display("could not enqueue foreground snapshot"))]
    Deliver { source: ObserveCaptureError },
}
struct Hook(HWINEVENTHOOK);
impl Hook {
    fn install(event: u32, process: u32) -> Result<Self, ObserveForegroundError> {
        // SAFETY: OUTOFCONTEXT delivers callbacks on the installing main thread's message loop.
        let hook = unsafe {
            Accessibility::SetWinEventHook(
                event,
                event,
                None,
                Some(callback),
                process,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        if hook.is_invalid() {
            Err(ObserveForegroundError::Hook {
                source: windows::core::Error::from_thread(),
            })
        } else {
            Ok(Self(hook))
        }
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        // SAFETY: This hook is uniquely owned and dropped on its installing thread.
        let _ = unsafe { Accessibility::UnhookWinEvent(self.0) };
    }
}
#[derive(Debug, Clone, Copy)]
pub(in crate::platform::windows) enum ForegroundCommand {
    Review,
    Rebind {
        generation: ForegroundGeneration,
        target: HookTarget,
    },
}
enum ProcessHooks {
    Unbound,
    Bound {
        generation: ForegroundGeneration,
        hooks: [Hook; PROCESS_EVENT_COUNT],
    },
}
impl ProcessHooks {
    fn install(
        generation: ForegroundGeneration,
        target: HookTarget,
    ) -> Result<Self, ObserveForegroundError> {
        match target {
            HookTarget::Unbind => Ok(Self::Unbound),
            HookTarget::Process(process) => Ok(Self::Bound {
                generation,
                hooks: [
                    Hook::install(EVENT_OBJECT_NAMECHANGE, process.get())?,
                    Hook::install(EVENT_OBJECT_DESTROY, process.get())?,
                ],
            }),
        }
    }
    fn accepts(&self, hook: HWINEVENTHOOK, current: ForegroundGeneration) -> EventDecision {
        match self {
            Self::Unbound => EventDecision::Ignore,
            Self::Bound { generation, hooks } => {
                if *generation == current && hooks.iter().any(|owned| owned.0 == hook) {
                    EventDecision::Accept
                } else {
                    EventDecision::Ignore
                }
            }
        }
    }
}
enum RebindOutcome {
    Applied(ProcessHooks),
    Stale(Result<ProcessHooks, ObserveForegroundError>),
}
struct CallbackState {
    sender: CaptureSender,
    foreground: ForegroundTarget,
    process_hooks: ProcessHooks,
    desktop: Desktop,
}
thread_local! {
    // SAFETY: Only ForegroundPort on the main thread installs this state. OUTOFCONTEXT
    // callbacks run on that same thread. No borrow survives any reentrant Win32 call.
    static CALLBACK: RefCell<Option<CallbackState>> = const { RefCell::new(None) };
}

pub(in crate::platform::windows) struct ForegroundPort {
    global: Vec<Hook>,
    // Prevent hook ownership from moving to another thread.
    main_thread: PhantomData<Rc<()>>,
}
impl ForegroundPort {
    pub(in crate::platform::windows) fn start(
        sender: CaptureSender,
    ) -> Result<Self, ObserveForegroundError> {
        let mut port = Self {
            global: Vec::new(),
            main_thread: PhantomData,
        };
        CALLBACK.with(|state| {
            assert!(
                state.borrow().is_none(),
                "one foreground port per main thread"
            );
            *state.borrow_mut() = Some(CallbackState {
                sender,
                foreground: ForegroundTarget::new(),
                process_hooks: ProcessHooks::Unbound,
                desktop: Desktop::Observable,
            });
        });
        for event in [
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_MINIMIZESTART,
            EVENT_SYSTEM_MINIMIZEEND,
            EVENT_SYSTEM_DESKTOPSWITCH,
        ] {
            port.global.push(Hook::install(event, 0)?);
        }
        Ok(port)
    }
    pub(in crate::platform::windows) fn dispatch(
        &mut self,
        command: ForegroundCommand,
    ) -> Result<(), ObserveForegroundError> {
        match command {
            ForegroundCommand::Review => self.review(),
            ForegroundCommand::Rebind { generation, target } => {
                // Registration can dispatch native callbacks; publish the prepared set only after
                // checking the generation again. Old and rejected sets are unhooked outside the borrow.
                let prepared = ProcessHooks::install(generation, target);
                let outcome = CALLBACK.with(|slot| {
                    let mut slot = slot.borrow_mut();
                    let state = slot
                        .as_mut()
                        .expect("live ForegroundPort owns callback state");
                    match state.foreground.accepts_rebind(generation) {
                        RebindDecision::Stale => Ok(RebindOutcome::Stale(prepared)),
                        RebindDecision::Accept => prepared.map(|hooks| {
                            RebindOutcome::Applied(std::mem::replace(
                                &mut state.process_hooks,
                                hooks,
                            ))
                        }),
                    }
                })?;
                match outcome {
                    RebindOutcome::Stale(prepared) => {
                        drop(prepared);
                        Ok(())
                    }
                    RebindOutcome::Applied(retired) => {
                        drop(retired);
                        self.review()
                    }
                }
            }
        }
    }
    fn review(&self) -> Result<(), ObserveForegroundError> {
        let desktop = Desktop::observe();
        // SAFETY: Returns an opaque foreground handle without dereferencing it.
        let window = unsafe { WindowsAndMessaging::GetForegroundWindow() };
        let state = CALLBACK.with(|state| {
            let mut state = state.borrow_mut();
            let state = state.as_mut()?;
            state.foreground.observe(NativeEvent {
                kind: EventKind::Review,
                window: Window::from_native(window).handle(),
                object: WINDOW_OBJECT,
                child: SELF_CHILD,
            });
            state.desktop = desktop;
            Some(state.submission())
        });
        if let Some(state) = state {
            state.submit(
                window,
                EventKind::Foreground,
                ObservationSource::Review,
                0,
                desktop,
            )?;
        }
        Ok(())
    }
}
impl Drop for ForegroundPort {
    fn drop(&mut self) {
        let state = CALLBACK.with(|state| state.borrow_mut().take());
        drop(state);
        self.global.clear();
    }
}
struct CallbackSubmission {
    submission: Submission,
    kind: EventKind,
}
struct Submission {
    sender: CaptureSender,
    generation: ForegroundGeneration,
    desktop: Desktop,
}
impl CallbackState {
    fn submission(&self) -> Submission {
        Submission {
            sender: self.sender.clone(),
            generation: self.foreground.generation(),
            desktop: self.desktop,
        }
    }
}
impl Submission {
    fn submit(
        &self,
        window: HWND,
        kind: EventKind,
        source: ObservationSource,
        event_time_ms: u32,
        desktop: Desktop,
    ) -> Result<(), ObserveForegroundError> {
        let at = SystemClock
            .read()
            .map_err(|source| ObserveForegroundError::Clock { source })?;
        let mut buffer = [0u16; TITLE_BUFFER_UNITS];
        // SAFETY: The buffer is writable and sized to 4,097 UTF-16 units (02 §1.1).
        // Clearing last error distinguishes an empty title from an unsuccessful read.
        let read = unsafe {
            Foundation::SetLastError(ERROR_SUCCESS);
            let copied = WindowsAndMessaging::GetWindowTextW(window, &mut buffer);
            if copied == 0 && Foundation::GetLastError() != ERROR_SUCCESS {
                TitleRead::Failed
            } else {
                match usize::try_from(copied) {
                    Ok(count) => TitleRead::Copied(count),
                    Err(_) => TitleRead::Failed,
                }
            }
        };
        let read_at = SystemClock
            .read()
            .map_err(|source| ObserveForegroundError::Clock { source })?;
        self.sender
            .foreground(Snapshot {
                at,
                read_at,
                event_time_ms,
                generation: self.generation,
                source,
                kind,
                window: Window::from_native(window).handle(),
                title: title_snapshot(&buffer, read),
                desktop,
            })
            .map_err(|source| ObserveForegroundError::Deliver { source })
    }
}
unsafe extern "system" fn callback(
    hook: HWINEVENTHOOK,
    event: u32,
    window: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    event_time_ms: u32,
) {
    let Ok(event) = WinEvent::try_from(event) else {
        return;
    };
    let kind = event.kind();
    let current = if kind == EventKind::Title {
        // SAFETY: The API returns an opaque handle and does not access Rust memory.
        let foreground = unsafe { WindowsAndMessaging::GetForegroundWindow() };
        Some(Window::from_native(foreground).handle())
    } else {
        None
    };
    let state = CALLBACK.with(|state| {
        let mut state = state.borrow_mut();
        let state = state.as_mut()?;
        match kind {
            EventKind::Title | EventKind::Destroy => match state
                .process_hooks
                .accepts(hook, state.foreground.generation())
            {
                EventDecision::Ignore => return None,
                EventDecision::Accept => {}
            },
            EventKind::Foreground | EventKind::Review => {}
        }
        let foreground = match current {
            Some(window) => window,
            None => state.foreground.window(),
        };
        match (NativeEvent {
            kind,
            window: Window::from_native(window).handle(),
            object,
            child,
        })
        .accepts(foreground)
        {
            EventDecision::Ignore => return None,
            EventDecision::Accept => {}
        }
        if event == WinEvent::DesktopSwitch {
            state.desktop = Desktop::Unknown;
            state.foreground.observe(NativeEvent {
                kind: EventKind::Foreground,
                window: state.foreground.window(),
                object: WINDOW_OBJECT,
                child: SELF_CHILD,
            });
        }
        if kind == EventKind::Foreground {
            state.foreground.observe(NativeEvent {
                kind,
                window: Window::from_native(window).handle(),
                object,
                child,
            });
        }
        let kind = if kind == EventKind::Title && foreground != state.foreground.window() {
            EventKind::Review
        } else {
            kind
        };
        Some(CallbackSubmission {
            submission: state.submission(),
            kind,
        })
    });
    if let Some(callback) = state {
        let state = callback.submission;
        // Callback failures must not unwind across the system ABI or perform logging I/O.
        if state
            .submit(
                window,
                callback.kind,
                ObservationSource::Hook,
                event_time_ms,
                state.desktop,
            )
            .is_err()
        {
            state.sender.mark_overloaded();
        }
    }
}
