//! Out-of-context hooks owned and dispatched exclusively by the main thread.

use super::Window;
use crate::core::{
    app::{CaptureSender, ObserveCaptureError, ObservedEvent},
    foreground::{
        CoverageChange, CurrentForeground, Desktop, DisplayAffinity, EventDecision, EventKind,
        ForegroundReads, NativeEvent, ReadCause, ReadDecision, ReadSample, RebindDecision,
        Snapshot, TITLE_BUFFER_UNITS, TitleRead,
    },
    sources::{ClockReader, ReadClockError},
};
use crate::platform::windows::sources::SystemClock;
use quartz_domain::capture::MonoInstant;
use quartz_domain::capture::{CaptureEvent, HookTarget, ObservationSource, Timestamp, Title};
use std::{cell::RefCell, marker::PhantomData, rc::Rc, time::Duration};
use windows::Win32::{
    Foundation::{self, ERROR_SUCCESS, HWND},
    System::SystemInformation::GetTickCount,
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

/// Foreground hook registration retry interval from capture 02 §1.1.
const HOOK_RETRY_DELAY: Duration = Duration::from_secs(5);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::platform::windows) enum HookHealth {
    #[default]
    Healthy,
    Degraded,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookInstallation {
    Installed,
    Deferred,
}
impl HookInstallation {
    fn review<E>(self, read: impl FnOnce() -> Result<(), E>) -> Result<(), E> {
        match self {
            Self::Installed => read(),
            Self::Deferred => Ok(()),
        }
    }
}
struct HookRegistration<T> {
    installed: Option<T>,
    retry: Option<MonoInstant>,
}
impl<T> HookRegistration<T> {
    fn new() -> Self {
        Self {
            installed: None,
            retry: None,
        }
    }
    fn install<E>(
        &mut self,
        now: MonoInstant,
        install: impl FnOnce() -> Result<T, E>,
    ) -> Result<HookInstallation, E> {
        if self.retry.is_some_and(|retry| now < retry) {
            return Ok(HookInstallation::Deferred);
        }
        match install() {
            Ok(hooks) => {
                self.installed = Some(hooks);
                self.retry = None;
                Ok(HookInstallation::Installed)
            }
            Err(error) => {
                self.installed = None;
                self.retry = Some(
                    now.checked_add(HOOK_RETRY_DELAY)
                        .expect("capture clock has room for hook retry"),
                );
                Err(error)
            }
        }
    }
    fn unbind(&mut self) -> HookInstallation {
        self.installed = None;
        self.retry = None;
        HookInstallation::Installed
    }
    fn health(&self) -> HookHealth {
        if self.retry.is_some() {
            HookHealth::Degraded
        } else {
            HookHealth::Healthy
        }
    }
}

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
        // Best-effort teardown: Drop cannot report hook removal failures.
        let _ = unsafe { Accessibility::UnhookWinEvent(self.0) };
    }
}
#[derive(Debug, Clone, Copy)]
pub(in crate::platform::windows) enum ForegroundCommand {
    /// Internal requests rebuild the baseline and never count notification gaps.
    Review,
    FallbackReview,
    Rebind {
        target: HookTarget,
    },
}
enum ProcessHooks {
    Unbound,
    Bound { _hooks: [Hook; PROCESS_EVENT_COUNT] },
}
impl ProcessHooks {
    fn target(target: HookTarget) -> HookTarget {
        match target {
            HookTarget::Process { process, .. } if process.get() == 0 => HookTarget::Unbind,
            target => target,
        }
    }
    fn install(target: HookTarget) -> Result<Self, ObserveForegroundError> {
        match Self::target(target) {
            HookTarget::Unbind => Ok(Self::Unbound),
            HookTarget::Process { process, .. } => Ok(Self::Bound {
                _hooks: [
                    Hook::install(EVENT_OBJECT_NAMECHANGE, process.get())?,
                    Hook::install(EVENT_OBJECT_DESTROY, process.get())?,
                ],
            }),
        }
    }
}
struct CallbackState {
    sender: CaptureSender,
    reads: ForegroundReads,
    process_hooks: HookRegistration<ProcessHooks>,
}
thread_local! {
    // Only the installing main thread accesses this state; no borrow spans a native read.
    static CALLBACK: RefCell<Option<CallbackState>> = const { RefCell::new(None) };
}
#[derive(Debug, Clone, Copy)]
enum ReadTrigger {
    Native {
        event: NativeEvent,
        event_time_ms: u32,
    },
    Requested(ReadCause),
    Rebind(HookTarget),
    RetryRebind,
}
impl ReadTrigger {
    fn cause(self) -> ReadCause {
        match self {
            Self::Requested(cause) => cause,
            Self::Native { .. } | Self::Rebind(_) | Self::RetryRebind => ReadCause::Trigger,
        }
    }
    fn native(self) -> Option<NativeEvent> {
        match self {
            Self::Native { event, .. } => Some(event),
            Self::Requested(_) | Self::Rebind(_) | Self::RetryRebind => None,
        }
    }
    fn event_time_ms(self) -> Option<u32> {
        match self {
            Self::Native { event_time_ms, .. } => Some(event_time_ms),
            Self::Requested(_) | Self::Rebind(_) | Self::RetryRebind => None,
        }
    }
    fn source(self) -> ObservationSource {
        match self {
            Self::Native { .. } => ObservationSource::Hook,
            Self::Requested(_) | Self::Rebind(_) | Self::RetryRebind => ObservationSource::Review,
        }
    }
    fn kind(self) -> EventKind {
        match self {
            Self::Native { event, .. } => event.kind,
            Self::Requested(_) | Self::Rebind(_) | Self::RetryRebind => EventKind::Review,
        }
    }
    fn binding(
        self,
        current: &CurrentForeground,
        process: quartz_domain::capture::ProcessId,
    ) -> Option<HookTarget> {
        match self {
            Self::RetryRebind => Some(
                if current.desktop == Desktop::Observable
                    && current.window.get() != 0
                    && process.get() != 0
                {
                    HookTarget::Process {
                        window: current.window,
                        process,
                    }
                } else {
                    HookTarget::Unbind
                },
            ),
            _ => self.rebind(),
        }
    }
    fn rebind(self) -> Option<HookTarget> {
        match self {
            Self::Rebind(target) => Some(ProcessHooks::target(target)),
            Self::Native { .. } | Self::Requested(_) | Self::RetryRebind => None,
        }
    }
}
struct ReadIngress {
    sender: CaptureSender,
    untriggered: Option<CoverageChange>,
}
struct ReadRequest {
    at: Timestamp,
    trigger: ReadTrigger,
}
impl ReadRequest {
    fn requested(at: Timestamp, cause: ReadCause) -> Self {
        Self {
            at,
            trigger: ReadTrigger::Requested(cause),
        }
    }
}
pub(in crate::platform::windows) struct ForegroundPort {
    global: HookRegistration<Vec<Hook>>,
    main_thread: PhantomData<Rc<()>>,
}
impl ForegroundPort {
    pub(in crate::platform::windows) fn start(
        sender: CaptureSender,
    ) -> Result<Self, ObserveForegroundError> {
        let mut port = Self {
            global: HookRegistration::new(),
            main_thread: PhantomData,
        };
        CALLBACK.with(|slot| {
            assert!(
                slot.borrow().is_none(),
                "one foreground port per main thread"
            );
            *slot.borrow_mut() = Some(CallbackState {
                sender,
                reads: ForegroundReads::default(),
                process_hooks: HookRegistration::new(),
            });
        });
        let now = Self::clock()?.mono;
        port.install_global(now);
        Ok(port)
    }
    fn install_global(&mut self, now: MonoInstant) -> HookInstallation {
        match self.global.install(now, || {
            [
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_MINIMIZESTART,
                EVENT_SYSTEM_MINIMIZEEND,
                EVENT_SYSTEM_DESKTOPSWITCH,
            ]
            .into_iter()
            .map(|event| Hook::install(event, 0))
            .collect()
        }) {
            Ok(installed) => installed,
            Err(error) => {
                tracing::warn!(%error, "foreground hooks unavailable; retry scheduled");
                HookInstallation::Deferred
            }
        }
    }
    pub(in crate::platform::windows) fn retry_deadline(&self) -> Option<MonoInstant> {
        self.global
            .retry
            .into_iter()
            .chain(CALLBACK.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .and_then(|state| state.process_hooks.retry)
            }))
            .min()
    }
    pub(in crate::platform::windows) fn health(&self) -> HookHealth {
        if self.global.health() == HookHealth::Degraded
            || CALLBACK.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .is_some_and(|state| state.process_hooks.health() == HookHealth::Degraded)
            })
        {
            HookHealth::Degraded
        } else {
            HookHealth::Healthy
        }
    }
    pub(in crate::platform::windows) fn retry(
        &mut self,
        now: MonoInstant,
    ) -> Result<(), ObserveForegroundError> {
        if self.global.retry.is_some_and(|retry| retry <= now) {
            self.install_global(now).review(|| {
                Self::invalidate();
                Self::read_current(ReadRequest::requested(Self::clock()?, ReadCause::Trigger))
            })?;
        }
        let process_due = CALLBACK.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|state| state.process_hooks.retry)
                .is_some_and(|retry| retry <= now)
        });
        if process_due {
            Self::invalidate();
            Self::read_current(ReadRequest {
                at: Self::clock()?,
                trigger: ReadTrigger::RetryRebind,
            })?;
        }
        Ok(())
    }
    pub(in crate::platform::windows) fn dispatch(
        &mut self,
        command: ForegroundCommand,
    ) -> Result<(), ObserveForegroundError> {
        let at = Self::clock()?;
        let trigger = match command {
            ForegroundCommand::Review => {
                Self::invalidate();
                ReadTrigger::Requested(ReadCause::Trigger)
            }
            ForegroundCommand::FallbackReview => ReadTrigger::Requested(ReadCause::Fallback),
            ForegroundCommand::Rebind { target } => ReadTrigger::Rebind(target),
        };
        let request = ReadRequest { at, trigger };
        Self::read_current(request)
    }
    pub(in crate::platform::windows) fn system_notice(
        at: Timestamp,
    ) -> Result<(), ObserveForegroundError> {
        Self::invalidate();
        Self::read_current(ReadRequest::requested(at, ReadCause::Trigger))
    }
    fn clock() -> Result<Timestamp, ObserveForegroundError> {
        SystemClock
            .read()
            .map_err(|source| ObserveForegroundError::Clock { source })
    }
    fn invalidate() {
        CALLBACK.with(|slot| {
            if let Some(state) = slot.borrow_mut().as_mut() {
                state.reads.invalidate();
            }
        });
    }
    fn deliver(result: Result<(), ObserveCaptureError>) -> Result<(), ObserveForegroundError> {
        CALLBACK
            .with(|slot| match slot.borrow_mut().as_mut() {
                Some(state) => state.reads.accept_delivery(result),
                None => result,
            })
            .map_err(|source| ObserveForegroundError::Deliver { source })
    }
    fn report_untriggered(
        sender: &CaptureSender,
        at: Timestamp,
        change: Option<CoverageChange>,
    ) -> Result<(), ObserveForegroundError> {
        if let Some(change) = change {
            Self::deliver(sender.observe(ObservedEvent {
                at,
                event: CaptureEvent::UntriggeredChange {
                    previous_window: change.previous_window,
                    reviewed_window: change.reviewed_window,
                },
            }))?;
        }
        Ok(())
    }
    fn read_current(request: ReadRequest) -> Result<(), ObserveForegroundError> {
        let cause = request.trigger.cause();
        let ingress = CALLBACK.with(|slot| {
            let mut slot = slot.borrow_mut();
            let state = slot.as_mut()?;
            // Settle notification evidence before filtering a title event or reading native state.
            let untriggered = request
                .trigger
                .event_time_ms()
                .and_then(|event_time_ms| state.reads.native_event(event_time_ms));
            Some(ReadIngress {
                sender: state.sender.clone(),
                untriggered,
            })
        });
        let Some(ingress) = ingress else {
            return Ok(());
        };
        let sender = ingress.sender;
        if sender.is_stopped() {
            return Ok(());
        }
        Self::report_untriggered(&sender, request.at, ingress.untriggered)?;
        // SAFETY: This only returns an opaque handle; event HWNDs never select the sampled window.
        let window = Window::from_native(unsafe { WindowsAndMessaging::GetForegroundWindow() });
        if let Some(native) = request.trigger.native()
            && native.accepts(window.handle()) == EventDecision::Ignore
        {
            return Ok(());
        }
        let desktop = Desktop::observe();
        let current = CurrentForeground {
            desktop,
            window: window.handle(),
            title: if desktop == Desktop::Observable && window.handle().get() != 0 {
                window.title()
            } else {
                Title::unreadable()
            },
            affinity: if desktop == Desktop::Observable && window.handle().get() != 0 {
                window.display_affinity()
            } else {
                DisplayAffinity::Unavailable
            },
        };
        let read_at = Self::clock().inspect_err(|_| {
            CALLBACK.with(|slot| {
                if let Some(state) = slot.borrow_mut().as_mut() {
                    state.reads.failed_read(cause);
                }
            });
        })?;
        let sample = match cause {
            ReadCause::Trigger => ReadSample::Trigger,
            ReadCause::Fallback => ReadSample::Fallback {
                // SAFETY: GetTickCount has no inputs and shares dwmsEventTime's wrapping time base.
                read_tick: unsafe { GetTickCount() },
            },
        };
        if let Some(native) = request.trigger.native()
            && native.kind == EventKind::Destroy
        {
            CALLBACK.with(|slot| {
                if let Some(state) = slot.borrow_mut().as_mut() {
                    state.reads.retire(native.window);
                }
            });
            Self::deliver(sender.observe(ObservedEvent {
                at: request.at,
                event: CaptureEvent::DestroyWindow {
                    handle: native.window,
                },
            }))?;
        }
        let outcome = CALLBACK.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .expect("live foreground port")
                .reads
                .observe(sample, &current)
        });
        let event_decision = match outcome.decision {
            ReadDecision::Unchanged => EventDecision::Ignore,
            ReadDecision::Changed => EventDecision::Accept,
        };
        tracing::debug!(sampled_hwnd = current.window.get(), ?desktop,
            ?cause, decision = ?event_decision, "foreground review sampled");
        // A retry derives its target from this fresh read, never from the failed installation.
        let process = window.process();
        let target = request.trigger.binding(&current, process);
        let rebind = target.map(|target| current.accepts_rebind(target, process));
        Self::report_untriggered(&sender, request.at, outcome.untriggered)?;
        if outcome.decision == ReadDecision::Changed {
            Self::deliver(sender.foreground(Snapshot {
                at: request.at,
                read_at,
                event_time_ms: request.trigger.event_time_ms(),
                source: request.trigger.source(),
                kind: request.trigger.kind(),
                window: current.window,
                title: current.title,
                desktop,
            }))?;
        }
        if let Some(target) = target {
            match rebind.expect("rebind request checked against current state") {
                RebindDecision::Accept => {
                    let mut registration = CALLBACK.with(|slot| {
                        std::mem::replace(
                            &mut slot
                                .borrow_mut()
                                .as_mut()
                                .expect("live foreground port")
                                .process_hooks,
                            HookRegistration::new(),
                        )
                    });
                    // Native installation and retirement may dispatch callbacks; release the RefCell borrow first.
                    let installed = match target {
                        HookTarget::Unbind => Ok(registration.unbind()),
                        HookTarget::Process { .. } => {
                            registration.install(read_at.mono, || ProcessHooks::install(target))
                        }
                    };
                    let retired = CALLBACK.with(|slot| {
                        std::mem::replace(
                            &mut slot
                                .borrow_mut()
                                .as_mut()
                                .expect("live foreground port")
                                .process_hooks,
                            registration,
                        )
                    });
                    drop(retired);
                    match installed {
                        Ok(HookInstallation::Installed) => {}
                        Ok(HookInstallation::Deferred) => return Ok(()),
                        Err(error) => {
                            tracing::warn!(%error, "foreground process hooks unavailable; retry scheduled");
                            return Ok(());
                        }
                    }
                    // Close the registration gap through the same comparison path.
                    Self::read_current(ReadRequest::requested(Self::clock()?, ReadCause::Trigger))?;
                }
                RebindDecision::ReadCurrent => {
                    tracing::debug!("foreground rebind target changed; current state sampled");
                }
            }
        }
        Ok(())
    }
}
impl Window {
    fn title(self) -> Title {
        let mut buffer = [0u16; TITLE_BUFFER_UNITS];
        // SAFETY: The buffer is writable for the full cap; last error distinguishes empty from failed.
        let read = unsafe {
            Foundation::SetLastError(ERROR_SUCCESS);
            let copied = WindowsAndMessaging::GetWindowTextW(self.raw(), &mut buffer);
            if copied == 0 && Foundation::GetLastError() != ERROR_SUCCESS {
                TitleRead::Failed
            } else {
                match usize::try_from(copied) {
                    Ok(count) => TitleRead::Copied(count),
                    Err(_) => TitleRead::Failed,
                }
            }
        };
        read.snapshot(&buffer)
    }
}
impl Drop for ForegroundPort {
    fn drop(&mut self) {
        let state = CALLBACK.with(|slot| slot.borrow_mut().take());
        drop(state);
        self.global.installed = None;
    }
}
unsafe extern "system" fn callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    window: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    event_time_ms: u32,
) {
    let at = match ForegroundPort::clock() {
        Ok(at) => at,
        Err(_) => {
            CALLBACK.with(|slot| {
                if let Some(state) = slot.borrow().as_ref() {
                    state.sender.mark_overloaded();
                }
            });
            return;
        }
    };
    let Ok(event) = WinEvent::try_from(event) else {
        return;
    };
    let kind = event.kind();
    let request = ReadRequest {
        at,
        trigger: ReadTrigger::Native {
            event: NativeEvent {
                kind,
                window: Window::from_native(window).handle(),
                object,
                child,
            },
            event_time_ms,
        },
    };
    if ForegroundPort::read_current(request).is_err() {
        CALLBACK.with(|slot| {
            if let Some(state) = slot.borrow().as_ref() {
                state.sender.mark_overloaded();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::foreground::{SELF_CHILD, WINDOW_OBJECT};
    use quartz_domain::capture::WindowHandle;

    #[test]
    fn deferred_and_failed_installations_preserve_the_foreground_baseline() {
        let reads = std::cell::Cell::new(0);
        let mut registration = HookRegistration::<()>::new();
        let now = MonoInstant::from_millis(0);
        let attempted = registration.install(now, || Err::<(), _>("fake global failure"));
        attempted
            .unwrap_or(HookInstallation::Deferred)
            .review(|| {
                reads.set(reads.get() + 1);
                Ok::<_, ()>(())
            })
            .expect("failed installation has no follow-up read");
        let deferred = registration
            .install(now, || -> Result<(), &str> { panic!("backoff") })
            .expect("deferred");
        deferred
            .review(|| {
                reads.set(reads.get() + 1);
                Ok::<_, ()>(())
            })
            .expect("deferred installation has no follow-up read");
        assert_eq!(reads.get(), 0);
        HookInstallation::Installed
            .review(|| {
                reads.set(reads.get() + 1);
                Ok::<_, ()>(())
            })
            .expect("successful follow-up read");
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn zero_process_retry_unbinds_and_backoff_does_not_suppress_unbind() {
        use quartz_domain::capture::{ProcessId, WindowHandle};
        let current = CurrentForeground {
            desktop: Desktop::Observable,
            window: WindowHandle::new(1),
            title: Title::unreadable(),
            affinity: DisplayAffinity::Unavailable,
        };
        let invalid = HookTarget::Process {
            window: current.window,
            process: ProcessId::new(0),
        };
        assert_eq!(
            ReadTrigger::RetryRebind.binding(&current, ProcessId::new(0)),
            Some(HookTarget::Unbind)
        );
        assert_eq!(ProcessHooks::target(invalid), HookTarget::Unbind);
        assert_eq!(
            current.accepts_rebind(invalid, ProcessId::new(0)),
            RebindDecision::ReadCurrent
        );
        let mut registration = HookRegistration::<()>::new();
        let now = MonoInstant::from_millis(0);
        assert!(
            registration
                .install(now, || Err::<(), _>("fake failure"))
                .is_err()
        );
        assert_eq!(
            registration
                .install(now, || Ok::<_, &str>(()))
                .expect("defer"),
            HookInstallation::Deferred
        );
        assert_eq!(registration.unbind(), HookInstallation::Installed);
        assert_eq!(registration.health(), HookHealth::Healthy);
        assert!(registration.retry.is_none());
    }

    #[test]
    fn hook_failure_degrades_and_retries_with_fresh_foreground_validation() {
        use quartz_domain::capture::{ProcessId, WindowHandle};
        let mut registration = HookRegistration::new();
        let start = MonoInstant::from_millis(0);
        assert!(
            registration
                .install(start, || Err::<HookTarget, _>("injected installer failure"))
                .is_err()
        );
        assert_eq!(registration.health(), HookHealth::Degraded);
        let retry = registration.retry.expect("bounded retry deadline");
        registration
            .install(start, || -> Result<_, &str> {
                panic!("must wait for retry deadline")
            })
            .expect("retry deferred");
        let current = CurrentForeground {
            desktop: Desktop::Observable,
            window: WindowHandle::new(2),
            title: Title::unreadable(),
            affinity: DisplayAffinity::Unavailable,
        };
        let target = ReadTrigger::RetryRebind
            .binding(&current, ProcessId::new(22))
            .expect("fresh target");
        assert_eq!(
            current.accepts_rebind(target, ProcessId::new(22)),
            RebindDecision::Accept
        );
        registration
            .install(retry, || Ok::<_, &str>(target))
            .expect("recover");
        assert_eq!(registration.health(), HookHealth::Healthy);
        assert!(registration.retry.is_none());
        assert_eq!(
            registration.installed,
            Some(HookTarget::Process {
                window: WindowHandle::new(2),
                process: ProcessId::new(22)
            })
        );
        let stale = HookTarget::Process {
            window: WindowHandle::new(1),
            process: ProcessId::new(11),
        };
        assert_eq!(
            current.accepts_rebind(stale, ProcessId::new(22)),
            RebindDecision::ReadCurrent
        );
    }

    #[test]
    fn read_trigger_derives_origin_kind_and_optional_native_event_time() {
        let event = NativeEvent {
            kind: EventKind::Title,
            window: WindowHandle::new(1),
            object: WINDOW_OBJECT,
            child: SELF_CHILD,
        };
        // A real wrapping native tick can be zero; it remains present.
        let native = ReadTrigger::Native {
            event,
            event_time_ms: 0,
        };
        assert_eq!(native.source(), ObservationSource::Hook);
        assert_eq!(native.kind(), EventKind::Title);
        assert_eq!(native.cause(), ReadCause::Trigger);
        assert_eq!(native.event_time_ms(), Some(0));
        assert!(native.rebind().is_none());
        for cause in [ReadCause::Trigger, ReadCause::Fallback] {
            let requested = ReadTrigger::Requested(cause);
            assert_eq!(requested.source(), ObservationSource::Review);
            assert_eq!(requested.kind(), EventKind::Review);
            assert_eq!(requested.cause(), cause);
            assert_eq!(requested.event_time_ms(), None);
            assert!(requested.native().is_none());
            assert!(requested.rebind().is_none());
        }
        let target = HookTarget::Unbind;
        let rebind = ReadTrigger::Rebind(target);
        assert_eq!(rebind.source(), ObservationSource::Review);
        assert_eq!(rebind.kind(), EventKind::Review);
        assert_eq!(rebind.cause(), ReadCause::Trigger);
        assert_eq!(rebind.event_time_ms(), None);
        assert!(rebind.native().is_none());
        assert_eq!(rebind.rebind(), Some(target));
    }
}
