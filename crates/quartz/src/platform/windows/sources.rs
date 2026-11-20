//! Main-thread system observation, session registration and one-shot scheduling.

use crate::core::{
    app::{CaptureSender, CloseCaptureError, ObserveCaptureError, ObservedEvent, ShutdownPort},
    sources::{
        ClockReader, ClockSample, InputSample, PowerNotice, ReadClockError, SessionAttempt,
        SessionInitialization, SessionNotice, SessionRegistration,
    },
};
use quartz_domain::capture::{
    AdvanceTimeError, CaptureEvent, ClockChange, LastInput, MonoInstant, SessionStatus, StopReason,
    Timer, Timestamp,
};
use snafu::Snafu;
use std::{collections::BTreeSet, sync::Arc};
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND, WPARAM},
        System::{
            Power::{self, HPOWERNOTIFY},
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, ProcessIdToSessionId, WTS_CURRENT_SESSION,
                WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNLOCK, WTSActive, WTSFreeMemory,
                WTSINFOEXW, WTSQuerySessionInformationW, WTSRegisterSessionNotification,
                WTSSessionInfoEx, WTSUnRegisterSessionNotification,
            },
            SystemInformation::{GetSystemTimePreciseAsFileTime, GetTickCount64},
            Threading::GetCurrentProcessId,
            WindowsProgramming::QueryUnbiasedInterruptTime,
        },
        UI::{
            Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
            WindowsAndMessaging::{
                DEVICE_NOTIFY_WINDOW_HANDLE, KillTimer, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND,
                SetTimer, USER_TIMER_MAXIMUM, USER_TIMER_MINIMUM, WM_POWERBROADCAST, WM_TIMECHANGE,
                WM_WTSSESSION_CHANGE, WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT,
                WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
            },
        },
    },
    core::PWSTR,
};

const TIMER_ID: usize = 1;
const WTS_INFO_LEVEL: u32 = 1;

#[derive(Debug)]
pub(super) struct SystemClock;
impl ClockReader for SystemClock {
    fn read(&self) -> Result<Timestamp, ReadClockError> {
        // SAFETY: These clock APIs access only the supplied writable scalar or return a value.
        let filetime = unsafe { GetSystemTimePreciseAsFileTime() };
        let mut interrupt_ticks = 0;
        // SAFETY: The output points to a live u64 for the duration of the call.
        if !unsafe { QueryUnbiasedInterruptTime(&mut interrupt_ticks) }.as_bool() {
            return Err(ReadClockError::Interrupt {
                source: windows::core::Error::from_thread(),
            });
        }
        ClockSample {
            filetime_ticks: (u64::from(filetime.dwHighDateTime) << u32::BITS)
                | u64::from(filetime.dwLowDateTime),
            interrupt_ticks,
        }
        .timestamp()
    }
}

#[derive(Debug, Snafu)]
pub(super) enum QuerySessionError {
    #[snafu(display("could not register WTS session notification"))]
    Register { source: windows::core::Error },
    #[snafu(display("could not query WTSInfoEx"))]
    Query { source: windows::core::Error },
    #[snafu(display("WTSInfoEx buffer exceeds addressable memory"))]
    BufferSize { source: std::num::TryFromIntError },
    #[snafu(display("WTSInfoEx returned an unsupported layout or session state"))]
    InvalidState,
}
#[derive(Debug, Snafu)]
pub(super) enum ObserveSystemError {
    #[snafu(display("could not observe foreground"))]
    Foreground {
        source: super::foreground::ObserveForegroundError,
    },
    #[snafu(display("could not create system message-only window"))]
    Window {
        source: super::tray::window::CreateCoreWindowError,
    },
    #[snafu(display("could not read system observation clock"))]
    Clock { source: ReadClockError },
    #[snafu(display("could not deliver system observation"))]
    Deliver { source: ObserveCaptureError },
    #[snafu(display("could not close capture for session shutdown"))]
    Close { source: CloseCaptureError },
    #[snafu(display("could not register suspend/resume notification"))]
    Power { source: windows::core::Error },
    #[snafu(display("could not set system deadline timer"))]
    Timer { source: windows::core::Error },
    #[snafu(display("could not calculate system deadline"))]
    Deadline { source: AdvanceTimeError },
}

#[derive(Clone)]
pub(super) struct SystemIngress {
    sender: CaptureSender,
    shutdown: ShutdownPort,
}
impl SystemIngress {
    pub(super) fn new(sender: CaptureSender, shutdown: ShutdownPort) -> Self {
        Self { sender, shutdown }
    }
    pub(super) fn end_session(&self) -> Result<(), ObserveSystemError> {
        let at = SystemClock
            .read()
            .map_err(|source| ObserveSystemError::Clock { source })?;
        self.shutdown
            .close(at)
            .map_err(|source| ObserveSystemError::Close { source })
    }
    pub(super) fn observe(&self, event: CaptureEvent) -> Result<(), ObserveSystemError> {
        let at = SystemClock
            .read()
            .map_err(|source| ObserveSystemError::Clock { source })?;
        self.observe_at(at, event)
    }
    pub(super) fn observe_at(
        &self,
        at: Timestamp,
        event: CaptureEvent,
    ) -> Result<(), ObserveSystemError> {
        self.sender
            .observe(ObservedEvent { at, event })
            .map_err(|source| ObserveSystemError::Deliver { source })
    }
    pub(super) fn message(&self, message: u32, wparam: WPARAM) -> Result<(), ObserveSystemError> {
        let at = SystemClock
            .read()
            .map_err(|source| ObserveSystemError::Clock { source })?;
        let events = SystemMessage::events(message, wparam);
        if events.is_empty() {
            return Ok(());
        }
        for event in events {
            self.observe_at(at, event)?;
        }
        super::foreground::ForegroundPort::system_notice(at)
            .map_err(|source| ObserveSystemError::Foreground { source })
    }
}

struct SessionBuffer(PWSTR);
impl SessionBuffer {
    fn query() -> Result<Self, QuerySessionError> {
        let mut pointer = PWSTR::null();
        let mut bytes = 0;
        // SAFETY: Both outputs remain writable until the returned buffer is freed.
        unsafe {
            WTSQuerySessionInformationW(
                None,
                WTS_CURRENT_SESSION,
                WTSSessionInfoEx,
                &mut pointer,
                &mut bytes,
            )
        }
        .map_err(|source| QuerySessionError::Query { source })?;
        let buffer = Self(pointer);
        let bytes =
            usize::try_from(bytes).map_err(|source| QuerySessionError::BufferSize { source })?;
        if bytes < size_of::<WTSINFOEXW>() || buffer.0.is_null() {
            return Err(QuerySessionError::InvalidState);
        }
        Ok(buffer)
    }
    fn status(&self) -> Result<SessionStatus, QuerySessionError> {
        // SAFETY: query validated the complete buffer length; unaligned reads need no alignment assumption.
        let info = unsafe { self.0.0.cast::<WTSINFOEXW>().read_unaligned() };
        if info.Level != WTS_INFO_LEVEL {
            return Err(QuerySessionError::InvalidState);
        }
        // SAFETY: The validated tag selects the level-one member of the union.
        let info = unsafe { info.Data.WTSInfoExLevel1 };
        let mut reasons = BTreeSet::new();
        match u32::try_from(info.SessionFlags) {
            Ok(WTS_SESSIONSTATE_LOCK) => {
                reasons.insert(StopReason::Locked);
            }
            Ok(WTS_SESSIONSTATE_UNLOCK) => {}
            _ => return Err(QuerySessionError::InvalidState),
        }
        if info.SessionState != WTSActive {
            reasons.insert(StopReason::Disconnected);
        }
        Ok(SessionStatus::Ready(reasons))
    }
}
impl Drop for SessionBuffer {
    fn drop(&mut self) {
        // SAFETY: WTSQuerySessionInformationW allocated this buffer, including on a malformed result.
        unsafe { WTSFreeMemory(self.0.0.cast()) };
    }
}

pub(super) struct Sources {
    foreground: super::foreground::ForegroundPort,
    window: HWND,
    session_window: super::tray::window::Window,
    ingress: SystemIngress,
    power: HPOWERNOTIFY,
    session: SessionInitialization,
    core_deadline: Option<MonoInstant>,
}
impl Sources {
    pub(super) fn start(window: HWND, ingress: SystemIngress) -> Result<Self, ObserveSystemError> {
        let session_window = super::tray::window::Window::create_system()
            .map_err(|source| ObserveSystemError::Window { source })?;
        session_window.attach_capture(ingress.clone());
        let foreground = super::foreground::ForegroundPort::start(ingress.sender.clone())
            .map_err(|source| ObserveSystemError::Foreground { source })?;
        // SAFETY: The message-only window is owned by Sources until registration is removed.
        let power = unsafe {
            Power::RegisterSuspendResumeNotification(
                HANDLE(session_window.raw().0),
                DEVICE_NOTIFY_WINDOW_HANDLE,
            )
        }
        .map_err(|source| ObserveSystemError::Power { source })?;
        Ok(Self {
            foreground,
            window,
            session_window,
            ingress,
            power,
            session: SessionInitialization::new(),
            core_deadline: None,
        })
    }
    pub(super) fn foreground(
        &mut self,
        command: super::foreground::ForegroundCommand,
    ) -> Result<(), ObserveSystemError> {
        self.foreground
            .dispatch(command)
            .map_err(|source| ObserveSystemError::Foreground { source })
    }
    pub(super) fn query_session(&mut self) -> Result<(), ObserveSystemError> {
        let now = SystemClock
            .read()
            .map_err(|source| ObserveSystemError::Clock { source })?;
        let window = self.session_window.raw();
        let attempt = self
            .session
            .attempt(
                now.mono,
                || {
                    // SAFETY: Notifications are delivered only to the live window on this thread.
                    unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) }
                        .map_err(|source| QuerySessionError::Register { source })
                },
                || SessionBuffer::query()?.status(),
            )
            .map_err(|source| ObserveSystemError::Deadline { source })?;
        let status = match attempt {
            SessionAttempt::Deferred => return self.arm(now),
            SessionAttempt::Ready(status) => status,
            SessionAttempt::Failed(error) => {
                tracing::error!(%error, "session initialization failed");
                SessionStatus::Unavailable
            }
        };
        self.ingress.observe(CaptureEvent::Session(status))?;
        self.arm(now)
    }
    pub(super) fn timer(&mut self, timer: Timer) -> Result<(), ObserveSystemError> {
        self.core_deadline = match timer {
            Timer::Arm(at) => Some(at),
            Timer::Cancel => None,
        };
        self.arm(
            SystemClock
                .read()
                .map_err(|source| ObserveSystemError::Clock { source })?,
        )
    }
    fn arm(&self, now: Timestamp) -> Result<(), ObserveSystemError> {
        // SAFETY: This is the sole timer ID on this live main-thread window.
        unsafe {
            // Cancellation is best-effort because no timer may currently be armed.
            let _ = KillTimer(Some(self.window), TIMER_ID);
        }
        if let Some(deadline) = self
            .core_deadline
            .into_iter()
            .chain(self.session.retry())
            .min()
        {
            let delay = deadline
                .get()
                .saturating_sub(now.mono.get())
                .max(u64::from(USER_TIMER_MINIMUM));
            // Longer deadlines are clamped to Win32's maximum timer interval.
            let delay = u32::try_from(delay)
                .unwrap_or(USER_TIMER_MAXIMUM)
                .min(USER_TIMER_MAXIMUM);
            // SAFETY: A window timer posts WM_TIMER; no callback pointer or cross-thread borrow is used.
            if unsafe { SetTimer(Some(self.window), TIMER_ID, delay, None) } == 0 {
                return Err(ObserveSystemError::Timer {
                    source: windows::core::Error::from_thread(),
                });
            }
        }
        Ok(())
    }
    pub(super) fn fired(&mut self, at: Timestamp) -> Result<(), ObserveSystemError> {
        // SAFETY: Kill before processing makes SetTimer one-shot, including reentrant dispatch.
        unsafe {
            // Cancellation is best-effort because no timer may currently be armed.
            let _ = KillTimer(Some(self.window), TIMER_ID);
        }
        if self
            .core_deadline
            .is_some_and(|deadline| deadline <= at.mono)
        {
            self.core_deadline = None;
        }
        if self.session.retry().is_some_and(|retry| retry <= at.mono) {
            self.query_session()?;
        }
        self.arm(at)
    }
    pub(super) fn check_idle(&self) -> Result<(), ObserveSystemError> {
        let mut info = LASTINPUTINFO {
            cbSize: u32::try_from(size_of::<LASTINPUTINFO>())
                .expect("Win32 structure size fits u32"),
            ..Default::default()
        };
        // SAFETY: info is initialized, writable and has the API-mandated size.
        let last_input = if unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            LastInput::Tick(info.dwTime)
        } else {
            tracing::warn!("GetLastInputInfo failed");
            LastInput::ApiFailed
        };
        // SAFETY: GetTickCount64 takes no pointers.
        let tick_ms = unsafe { GetTickCount64() };
        self.ingress.observe(CaptureEvent::Idle(
            InputSample {
                tick_ms,
                last_input,
            }
            .idle(),
        ))
    }
}
impl Drop for Sources {
    fn drop(&mut self) {
        // SAFETY: Registrations and timer belong to this live main-thread window.
        unsafe {
            // Best-effort teardown: the timer may already be cancelled.
            let _ = KillTimer(Some(self.window), TIMER_ID);
            if let SessionRegistration::Registered = self.session.registration() {
                // Best-effort teardown: Drop cannot report session unregistration failures.
                let _ = WTSUnRegisterSessionNotification(self.session_window.raw());
            }
            if let Err(error) = Power::UnregisterSuspendResumeNotification(self.power) {
                tracing::error!(%error, "could not unregister power notification");
            }
        }
    }
}

pub(super) struct SystemMessage;
impl SystemMessage {
    pub(super) fn events(message: u32, parameter: WPARAM) -> Vec<CaptureEvent> {
        let Ok(parameter) = u32::try_from(parameter.0) else {
            return Vec::new();
        };
        match message {
            WM_WTSSESSION_CHANGE => {
                let notice = match parameter {
                    WTS_SESSION_LOCK => SessionNotice::Lock,
                    WTS_SESSION_UNLOCK => SessionNotice::Unlock,
                    WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => SessionNotice::Connect,
                    WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => SessionNotice::Disconnect,
                    _ => return Vec::new(),
                };
                vec![CaptureEvent::Stop(notice.change())]
            }
            WM_POWERBROADCAST => match parameter {
                PBT_APMSUSPEND => PowerNotice::Suspend.events(),
                PBT_APMRESUMEAUTOMATIC => PowerNotice::Resume.events(),
                _ => Vec::new(),
            },
            WM_TIMECHANGE => vec![CaptureEvent::ClockChanged(ClockChange::TimeChange)],
            _ => Vec::new(),
        }
    }
    pub(super) fn session_id() -> Result<u32, windows::core::Error> {
        let mut id = 0;
        // SAFETY: Current process ID is valid; id is writable for this call.
        unsafe {
            ProcessIdToSessionId(GetCurrentProcessId(), &mut id)?;
        }
        Ok(id)
    }
}

pub(super) fn clock() -> Arc<dyn ClockReader> {
    Arc::new(SystemClock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_domain::capture::StopChange;
    #[test]
    fn wts_messages_preserve_independent_stop_reasons() {
        for notice in [WTS_CONSOLE_CONNECT, WTS_REMOTE_CONNECT] {
            assert!(matches!(
                SystemMessage::events(
                    WM_WTSSESSION_CHANGE,
                    WPARAM(usize::try_from(notice).expect("message"))
                )
                .as_slice(),
                [CaptureEvent::Stop(StopChange::End(
                    StopReason::Disconnected
                ))]
            ));
        }
        for notice in [WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT] {
            assert!(matches!(
                SystemMessage::events(
                    WM_WTSSESSION_CHANGE,
                    WPARAM(usize::try_from(notice).expect("message"))
                )
                .as_slice(),
                [CaptureEvent::Stop(StopChange::Begin(
                    StopReason::Disconnected
                ))]
            ));
        }
        assert!(matches!(
            SystemMessage::events(
                WM_WTSSESSION_CHANGE,
                WPARAM(usize::try_from(WTS_SESSION_LOCK).expect("message"))
            )
            .as_slice(),
            [CaptureEvent::Stop(StopChange::Begin(StopReason::Locked))]
        ));
        assert!(matches!(
            SystemMessage::events(
                WM_WTSSESSION_CHANGE,
                WPARAM(usize::try_from(WTS_SESSION_UNLOCK).expect("message"))
            )
            .as_slice(),
            [CaptureEvent::Stop(StopChange::End(StopReason::Locked))]
        ));
    }
    #[test]
    fn power_and_time_messages_have_explicit_clock_boundaries() {
        assert!(matches!(
            SystemMessage::events(
                WM_POWERBROADCAST,
                WPARAM(usize::try_from(PBT_APMSUSPEND).expect("message"))
            )
            .as_slice(),
            [CaptureEvent::Stop(StopChange::Begin(StopReason::Suspended))]
        ));
        assert!(matches!(
            SystemMessage::events(
                WM_POWERBROADCAST,
                WPARAM(usize::try_from(PBT_APMRESUMEAUTOMATIC).expect("message"))
            )
            .as_slice(),
            [
                CaptureEvent::ClockChanged(ClockChange::Resume),
                CaptureEvent::Stop(StopChange::End(StopReason::Suspended))
            ]
        ));
        assert!(matches!(
            SystemMessage::events(WM_TIMECHANGE, WPARAM(0)).as_slice(),
            [CaptureEvent::ClockChanged(ClockChange::TimeChange)]
        ));
    }
}
