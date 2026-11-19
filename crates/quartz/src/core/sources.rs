//! Checked conversion of system samples, without platform handles.

use quartz_domain::capture::{
    AdvanceTimeError, IdlePolicy, IdleSample, LastInput, MonoInstant, SessionStatus, Timestamp,
    UtcInstant,
};
use snafu::Snafu;
use std::time::Duration;

const FILETIME_UNIX_EPOCH_TICKS: i128 = 116_444_736_000_000_000;
const TICKS_PER_MILLISECOND: u64 = 10_000;

/// Session initialization retries no sooner than five seconds after failure (02 §3).
const SESSION_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionRegistration {
    Pending,
    Registered,
}

#[derive(Debug)]
pub(crate) enum SessionAttempt<E> {
    Deferred,
    Ready(SessionStatus),
    Failed(E),
}

#[derive(Debug)]
pub(crate) struct SessionInitialization {
    registration: SessionRegistration,
    retry: Option<MonoInstant>,
}
impl SessionInitialization {
    pub(crate) fn new() -> Self {
        Self {
            registration: SessionRegistration::Pending,
            retry: None,
        }
    }
    pub(crate) fn registration(&self) -> SessionRegistration {
        self.registration
    }
    pub(crate) fn retry(&self) -> Option<MonoInstant> {
        self.retry
    }
    pub(crate) fn attempt<E>(
        &mut self,
        now: MonoInstant,
        register: impl FnOnce() -> Result<(), E>,
        query: impl FnOnce() -> Result<SessionStatus, E>,
    ) -> Result<SessionAttempt<E>, AdvanceTimeError> {
        if self.retry.is_some_and(|retry| now < retry) {
            return Ok(SessionAttempt::Deferred);
        }
        let result = match self.registration {
            SessionRegistration::Pending => register().map(|()| {
                self.registration = SessionRegistration::Registered;
            }),
            SessionRegistration::Registered => Ok(()),
        }
        .and_then(|()| query());
        match result {
            Ok(status) => {
                self.retry = None;
                Ok(SessionAttempt::Ready(status))
            }
            Err(error) => {
                self.retry = Some(now.checked_add(SESSION_RETRY_DELAY)?);
                Ok(SessionAttempt::Failed(error))
            }
        }
    }
}

#[derive(Debug, Snafu)]
pub(crate) enum ReadClockError {
    #[snafu(display("system UTC reading is outside the supported calendar"))]
    Utc { source: jiff::Error },
    #[snafu(display("system UTC reading exceeds integer milliseconds"))]
    Range { source: std::num::TryFromIntError },
    #[cfg(windows)]
    #[snafu(display("could not read QueryUnbiasedInterruptTime"))]
    Interrupt { source: windows::core::Error },
}

pub(crate) trait ClockReader: Send + Sync {
    fn read(&self) -> Result<Timestamp, ReadClockError>;
}

pub(crate) struct ClockSample {
    pub filetime_ticks: u64,
    pub interrupt_ticks: u64,
}
impl ClockSample {
    pub(crate) fn timestamp(self) -> Result<Timestamp, ReadClockError> {
        let milliseconds = (i128::from(self.filetime_ticks) - FILETIME_UNIX_EPOCH_TICKS)
            .div_euclid(i128::from(TICKS_PER_MILLISECOND));
        let milliseconds =
            i64::try_from(milliseconds).map_err(|source| ReadClockError::Range { source })?;
        let utc = jiff::Timestamp::from_millisecond(milliseconds)
            .map_err(|source| ReadClockError::Utc { source })?;
        Ok(Timestamp {
            utc: UtcInstant::new(utc),
            mono: MonoInstant::from_millis(self.interrupt_ticks / TICKS_PER_MILLISECOND),
        })
    }
}

pub(crate) struct InputSample {
    pub tick_ms: u64,
    pub last_input: LastInput,
}
impl InputSample {
    /// CaptureCore adds active persisted grants after selecting the current foreground scope.
    pub(crate) fn idle(self) -> IdleSample {
        IdleSample {
            tick: Duration::from_millis(self.tick_ms),
            last_input: self.last_input,
            policy: IdlePolicy::default(),
            exceptions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SessionNotice {
    Lock,
    Unlock,
    Connect,
    Disconnect,
}
impl SessionNotice {
    pub(crate) fn change(self) -> quartz_domain::capture::StopChange {
        use quartz_domain::capture::{StopChange, StopReason};
        match self {
            Self::Lock => StopChange::Begin(StopReason::Locked),
            Self::Unlock => StopChange::End(StopReason::Locked),
            Self::Connect => StopChange::End(StopReason::Disconnected),
            Self::Disconnect => StopChange::Begin(StopReason::Disconnected),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum PowerNotice {
    Suspend,
    Resume,
}
impl PowerNotice {
    pub(crate) fn events(self) -> Vec<quartz_domain::capture::CaptureEvent> {
        use quartz_domain::capture::{CaptureEvent, ClockChange, StopChange, StopReason};
        match self {
            Self::Suspend => vec![CaptureEvent::Stop(StopChange::Begin(StopReason::Suspended))],
            Self::Resume => vec![
                CaptureEvent::ClockChanged(ClockChange::Resume),
                CaptureEvent::Stop(StopChange::End(StopReason::Suspended)),
            ],
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use quartz_domain::capture::IdleStatus;

    #[test]
    fn session_registration_failure_retries_at_deadline_and_queries_before_ready() {
        let mut session = SessionInitialization::new();
        let start = MonoInstant::from_millis(0);
        let deadline = start.checked_add(SESSION_RETRY_DELAY).expect("retry time");
        let first = session
            .attempt(
                start,
                || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
                || panic!("failed registration must not query session state"),
            )
            .expect("retry deadline");
        assert!(
            matches!(first, SessionAttempt::Failed(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
        );
        assert_eq!(session.registration(), SessionRegistration::Pending);
        assert_eq!(session.retry(), Some(deadline));
        let early = session
            .attempt::<std::io::Error>(
                deadline
                    .checked_sub(Duration::from_millis(1))
                    .expect("before retry"),
                || panic!("registration cannot retry before five seconds"),
                || panic!("query cannot bypass the retry deadline"),
            )
            .expect("deferred attempt");
        assert!(matches!(early, SessionAttempt::Deferred));
        let failed_query = session
            .attempt(
                deadline,
                || Ok(()),
                || Err(std::io::Error::from(std::io::ErrorKind::NotConnected)),
            )
            .expect("query retry deadline");
        assert!(
            matches!(failed_query, SessionAttempt::Failed(error) if error.kind() == std::io::ErrorKind::NotConnected)
        );
        assert_eq!(session.registration(), SessionRegistration::Registered);
        let next = deadline
            .checked_add(SESSION_RETRY_DELAY)
            .expect("next retry");
        assert_eq!(session.retry(), Some(next));
        let ready = session
            .attempt::<std::io::Error>(
                next,
                || panic!("successful registration must not be repeated"),
                || {
                    Ok(SessionStatus::Ready(std::collections::BTreeSet::from([
                        quartz_domain::capture::StopReason::Locked,
                    ])))
                },
            )
            .expect("successful retry");
        assert!(
            matches!(ready, SessionAttempt::Ready(SessionStatus::Ready(reasons)) if reasons == std::collections::BTreeSet::from([quartz_domain::capture::StopReason::Locked]))
        );
        assert_eq!(session.retry(), None);
    }

    #[test]
    fn converts_filetime_epoch_and_floors_submillisecond_readings() {
        let sample = ClockSample {
            filetime_ticks: 116_444_736_000_019_999,
            interrupt_ticks: 29_999,
        };
        let at = sample.timestamp().expect("valid clock");
        assert_eq!(at.utc.get().as_millisecond(), 1);
        assert_eq!(at.mono.get(), 2);
        let before = ClockSample {
            filetime_ticks: 116_444_735_999_999_999,
            interrupt_ticks: 0,
        }
        .timestamp()
        .expect("before Unix epoch");
        assert_eq!(before.utc.get().as_millisecond(), -1);
        assert!(
            ClockSample {
                filetime_ticks: u64::MAX,
                interrupt_ticks: 0
            }
            .timestamp()
            .is_err()
        );
    }

    #[test]
    fn idle_sample_preserves_wrap_and_api_failure() {
        let at = ClockSample {
            filetime_ticks: 116_444_736_000_000_000,
            interrupt_ticks: 1_000_000,
        }
        .timestamp()
        .expect("clock");
        let idle = InputSample {
            tick_ms: u64::from(u32::MAX) + 11,
            last_input: LastInput::Tick(u32::MAX - 10),
        }
        .idle();
        assert_eq!(idle.decide(at).status, IdleStatus::Active);
        assert!(
            InputSample {
                tick_ms: 0,
                last_input: LastInput::ApiFailed
            }
            .idle()
            .decide(at)
            .error()
            .is_some()
        );
    }

    #[test]
    fn system_notices_change_only_their_own_reason() {
        use quartz_domain::capture::{CaptureEvent, ClockChange, StopChange, StopReason};
        assert_eq!(
            SessionNotice::Lock.change(),
            StopChange::Begin(StopReason::Locked)
        );
        assert_eq!(
            SessionNotice::Unlock.change(),
            StopChange::End(StopReason::Locked)
        );
        assert_eq!(
            SessionNotice::Connect.change(),
            StopChange::End(StopReason::Disconnected)
        );
        assert_eq!(
            SessionNotice::Disconnect.change(),
            StopChange::Begin(StopReason::Disconnected)
        );
        assert!(matches!(
            PowerNotice::Suspend.events().as_slice(),
            [CaptureEvent::Stop(StopChange::Begin(StopReason::Suspended))]
        ));
        assert!(matches!(
            PowerNotice::Resume.events().as_slice(),
            [
                CaptureEvent::ClockChanged(ClockChange::Resume),
                CaptureEvent::Stop(StopChange::End(StopReason::Suspended))
            ]
        ));
    }
}
