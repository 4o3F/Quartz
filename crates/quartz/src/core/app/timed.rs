//! Clocked timed-policy admission and sanitized snapshot projection.

use super::{CaptureSender, ObservedEvent};
use crate::{
    core::sources::ClockReader,
    ipc::{TimedCommand, TimedDeadline, TimedPolicyPort},
    query::calendar::DayCalendar,
};
use quartz_domain::capture::{
    CaptureCore, CaptureEvent, PrivacyMode, TimedPolicy, TimedPolicyKind, TimedState, Timestamp,
    UtcInstant,
};
use quartz_protocol::{
    ErrorKind, ProtocolError, RequestedTimedState, TimedPolicies, TimedPolicyResponse,
    TimedPrivacyMode,
};
use std::sync::Arc;

impl CaptureSender {
    pub(super) fn timed_port(
        &self,
        clock: Arc<dyn ClockReader>,
        day_offset: i64,
    ) -> TimedPolicyPort {
        let sender = self.clone();
        TimedPolicyPort::new(move |command| {
            let at = clock.read().map_err(|error| {
                tracing::warn!(%error, "could not sample timed policy clock");
                ProtocolError::new(ErrorKind::Internal)
            })?;
            let event = command.event(at, day_offset)?;
            let requested = event.requested();
            sender.admit(ObservedEvent { at, event })?;
            Ok(TimedPolicyResponse {
                accepted: quartz_protocol::AcceptedBody,
                requested,
            })
        })
    }
}
impl TimedDeadline {
    fn resolve(self, at: Timestamp, day_offset: i64) -> Result<UtcInstant, ProtocolError> {
        let invalid = || ProtocolError::new(ErrorKind::BadRequest);
        let until = match self {
            Self::UtcMillis(value) => UtcInstant::try_from(value).map_err(|_| invalid())?,
            Self::After(duration) => at.utc.checked_add(duration).map_err(|_| invalid())?,
            Self::NextLogicalDay => UtcInstant::try_from(
                DayCalendar::next_start(at.utc.get(), day_offset).map_err(|error| {
                    tracing::warn!(%error, "could not resolve next logical day");
                    ProtocolError::new(ErrorKind::Internal)
                })?,
            )
            .map_err(|_| invalid())?,
        };
        if until <= at.utc {
            return Err(invalid());
        }
        Ok(until)
    }
}
impl TimedCommand {
    fn event(self, at: Timestamp, day_offset: i64) -> Result<CaptureEvent, ProtocolError> {
        let policy = match self {
            Self::Resume => return Ok(CaptureEvent::CancelTimedPolicy(TimedPolicyKind::Pause)),
            Self::DisablePrivacy => {
                return Ok(CaptureEvent::CancelTimedPolicy(TimedPolicyKind::Privacy));
            }
            Self::Pause(_) => TimedPolicy::Pause,
            Self::KeepTiming(_) => TimedPolicy::KeepTiming,
            Self::EnablePrivacy(_) => TimedPolicy::Privacy(PrivacyMode::Private),
        };
        let deadline = match self {
            Self::Pause(deadline) | Self::KeepTiming(deadline) | Self::EnablePrivacy(deadline) => {
                deadline
            }
            Self::Resume | Self::DisablePrivacy => unreachable!("cancellation returned above"),
        };
        let state = TimedState {
            policy,
            started: at.utc,
            until: deadline.resolve(at, day_offset)?,
        }
        .normalized()
        .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        Ok(CaptureEvent::SetTimedState(state))
    }
}
trait RequestedState {
    fn requested(&self) -> RequestedTimedState;
}
impl RequestedState for CaptureEvent {
    fn requested(&self) -> RequestedTimedState {
        match self {
            Self::SetTimedState(state) => match state.policy {
                TimedPolicy::Pause => RequestedTimedState::Pause {
                    until_utc_ms: Some(state.until.get().as_millisecond()),
                },
                TimedPolicy::KeepTiming => RequestedTimedState::KeepTiming {
                    until_utc_ms: state.until.get().as_millisecond(),
                },
                TimedPolicy::Privacy(PrivacyMode::Private) => RequestedTimedState::Privacy {
                    mode: Some(TimedPrivacyMode {
                        mode: quartz_protocol::PrivacyMode::Private,
                        until_utc_ms: state.until.get().as_millisecond(),
                    }),
                },
                _ => unreachable!("control port constructs only session-wide policies"),
            },
            Self::CancelTimedPolicy(TimedPolicyKind::Pause) => {
                RequestedTimedState::Pause { until_utc_ms: None }
            }
            Self::CancelTimedPolicy(TimedPolicyKind::Privacy) => {
                RequestedTimedState::Privacy { mode: None }
            }
            _ => unreachable!("control port constructs only timed events"),
        }
    }
}
pub(super) trait PublishTimedPolicies {
    fn published_timed_policies(&self) -> TimedPolicies;
}
impl PublishTimedPolicies for CaptureCore {
    fn published_timed_policies(&self) -> TimedPolicies {
        let mut published = TimedPolicies::default();
        for state in self.timed_states() {
            let until = state.until.get().as_millisecond();
            match state.policy {
                TimedPolicy::Pause => published.pause_until_utc_ms = Some(until),
                TimedPolicy::KeepTiming => published.keep_timing_until_utc_ms = Some(until),
                TimedPolicy::Privacy(mode) => {
                    published.privacy_mode = Some(TimedPrivacyMode {
                        mode: match mode {
                            PrivacyMode::Normal => quartz_protocol::PrivacyMode::Normal,
                            PrivacyMode::Private => quartz_protocol::PrivacyMode::Private,
                            PrivacyMode::NoTitles => quartz_protocol::PrivacyMode::NoTitles,
                        },
                        until_utc_ms: until,
                    })
                }
                _ => unreachable!("capture publishes only supported session-wide policies"),
            }
        }
        published
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{app::Observation, sources::ReadClockError},
        store::capture::test_support::{at, foreground},
    };
    use quartz_domain::{
        capture::*,
        rules::registry::{RegistryState, RuleRegistry},
    };
    use std::{
        sync::{Mutex, atomic::AtomicBool},
        time::Duration,
    };

    struct FakeClock(Mutex<Timestamp>);
    impl ClockReader for FakeClock {
        fn read(&self) -> Result<Timestamp, ReadClockError> {
            Ok(*self.0.lock().expect("clock lock"))
        }
    }
    struct Harness {
        core: CaptureCore,
        rules: quartz_domain::rules::registry::SnapshotCell,
        clock: Arc<FakeClock>,
        port: TimedPolicyPort,
        ingress: crossbeam_channel::Receiver<Observation>,
        overloaded: Arc<AtomicBool>,
    }
    impl Harness {
        fn new() -> Self {
            let (sender, ingress) = crossbeam_channel::bounded(1);
            let overloaded = Arc::new(AtomicBool::new(false));
            let clock = Arc::new(FakeClock(Mutex::new(at(10))));
            let port = CaptureSender {
                sender,
                overloaded: overloaded.clone(),
            }
            .timed_port(clock.clone(), 0);
            let mut harness = Self {
                core: CaptureCore::start(RunId::new(1), at(0))
                    .expect("start capture")
                    .core,
                rules: RuleRegistry::from_state(RegistryState::default())
                    .expect("rules")
                    .snapshots(),
                clock,
                port,
                ingress,
                overloaded,
            };
            let _ = harness.step(
                at(1),
                CaptureEvent::Session(SessionStatus::Ready(Default::default())),
            );
            let _ = harness.step(at(2), foreground(1, "allowed title"));
            harness
        }
        fn step(&mut self, now: Timestamp, event: CaptureEvent) -> CaptureStep {
            self.core
                .step(CaptureInput {
                    at: now,
                    event,
                    rules: self.rules.load(),
                })
                .expect("capture transition")
        }
        fn apply(&mut self) -> CaptureStep {
            let Observation::Event(observed) = self.ingress.try_recv().expect("admitted control")
            else {
                panic!("expected control event")
            };
            self.step(observed.at, observed.event)
        }
        fn set_time(&self, millis: u64) {
            *self.clock.0.lock().expect("clock lock") = at(millis);
        }
    }
    #[test]
    fn admission_validates_future_deadlines_and_full_queue_returns_busy_without_overload() {
        let mut harness = Harness::new();
        for command in [
            TimedCommand::Pause(TimedDeadline::UtcMillis(9)),
            TimedCommand::Pause(TimedDeadline::UtcMillis(10)),
            TimedCommand::KeepTiming(TimedDeadline::UtcMillis(0)),
            TimedCommand::EnablePrivacy(TimedDeadline::UtcMillis(i64::MAX)),
        ] {
            assert_eq!(
                harness
                    .port
                    .call(command)
                    .expect_err("invalid deadline")
                    .kind,
                ErrorKind::BadRequest
            );
            assert!(harness.ingress.is_empty());
        }
        let response = harness
            .port
            .call(TimedCommand::Pause(TimedDeadline::UtcMillis(100)))
            .expect("admitted");
        assert_eq!(
            serde_json::to_value(response).expect("response"),
            serde_json::json!({"accepted":true,"requested":{"policy":"pause","until_utc_ms":100}})
        );
        assert!(
            matches!(harness.core.state(), CaptureState::Tracking(_)),
            "admission does not claim effective state"
        );
        assert_eq!(
            harness
                .port
                .call(TimedCommand::Resume)
                .expect_err("queue full")
                .kind,
            ErrorKind::Busy
        );
        assert!(
            !harness
                .overloaded
                .load(std::sync::atomic::Ordering::Acquire)
        );
        let paused = harness.apply();
        assert!(paused.writes.iter().any(|write| matches!(
            write,
            CaptureWrite::CloseInterval {
                reason: IntervalEndReason::Paused,
                ..
            }
        )));
        assert!(
            matches!(harness.core.state(), CaptureState::Stopped { reasons, .. } if reasons.contains(&StopReason::Paused))
        );
        harness.set_time(20);
        harness
            .port
            .call(TimedCommand::Resume)
            .expect("resume admitted");
        let _ = harness.apply();
        assert_eq!(
            harness.core.published_timed_policies().pause_until_utc_ms,
            None
        );
        let _ = harness.step(at(21), foreground(1, "resumed title"));
        assert!(matches!(harness.core.state(), CaptureState::Tracking(_)));
        assert!(harness.ingress.is_empty());
    }
    #[test]
    fn private_mode_drops_titles_and_expiry_creates_policy_boundary() {
        let mut harness = Harness::new();
        let response = harness
            .port
            .call(TimedCommand::EnablePrivacy(TimedDeadline::UtcMillis(30)))
            .expect("private admitted");
        assert_eq!(
            response.requested,
            RequestedTimedState::Privacy {
                mode: Some(TimedPrivacyMode {
                    mode: quartz_protocol::PrivacyMode::Private,
                    until_utc_ms: 30
                })
            }
        );
        let _ = harness.apply();
        let state = harness.core.published_timed_policies();
        assert_eq!(
            state.privacy_mode.expect("active").mode,
            quartz_protocol::PrivacyMode::Private
        );
        let _ = harness.step(at(11), foreground(1, "hidden title"));
        assert_eq!(
            harness
                .core
                .foreground_view()
                .expect("private foreground")
                .title,
            None
        );
        let expired = harness.step(at(30), CaptureEvent::Wake);
        assert!(expired.writes.iter().any(|write| matches!(
            write,
            CaptureWrite::CloseInterval {
                reason: IntervalEndReason::PolicyChanged,
                ..
            }
        )));
        assert_eq!(harness.core.published_timed_policies().privacy_mode, None);
        assert!(!format!("{expired:?}").contains("hidden title"));
        let _ = harness.step(at(31), foreground(1, "fresh allowed title"));
        assert_eq!(
            harness
                .core
                .foreground_view()
                .expect("fresh foreground")
                .title
                .as_deref(),
            Some("fresh allowed title")
        );
    }
    #[test]
    fn keep_timing_uses_domain_normalization_and_suppresses_idle_until_expiry() {
        let mut harness = Harness::new();
        let until = 300_000;
        let response = harness
            .port
            .call(TimedCommand::KeepTiming(TimedDeadline::UtcMillis(until)))
            .expect("keep admitted");
        let _ = harness.apply();
        assert_eq!(
            response.requested,
            RequestedTimedState::KeepTiming {
                until_utc_ms: until
            }
        );
        let sample = IdleSample {
            tick: Duration::from_millis(200_000),
            last_input: LastInput::Tick(0),
            policy: IdlePolicy::default(),
            exceptions: vec![],
        };
        let active = harness.step(at(200_000), CaptureEvent::Idle(sample.clone()));
        assert!(!active.writes.iter().any(|write| matches!(write, CaptureWrite::OpenAvailability(row) if row.reason == AvailabilityReason::Idle)));
        let _ = harness.step(at(300_000), CaptureEvent::Wake);
        let expired = harness.step(
            at(300_001),
            CaptureEvent::Idle(IdleSample {
                tick: Duration::from_millis(300_001),
                ..sample
            }),
        );
        assert!(expired.writes.iter().any(|write| matches!(write, CaptureWrite::OpenAvailability(row) if row.reason == AvailabilityReason::Idle)));
        assert_eq!(
            harness
                .core
                .published_timed_policies()
                .keep_timing_until_utc_ms,
            None
        );
    }
    #[test]
    fn duration_commands_use_the_injected_clock_and_disconnection_is_internal() {
        let harness = Harness::new();
        let duration = Duration::from_secs(15 * 60);
        let normalized = TimedState {
            policy: TimedPolicy::Pause,
            started: at(10).utc,
            until: at(10).utc.checked_add(duration).expect("deadline"),
        }
        .normalized()
        .expect("normalized");
        assert_eq!(
            harness
                .port
                .call(TimedCommand::Pause(TimedDeadline::After(duration)))
                .expect("duration admitted")
                .requested,
            RequestedTimedState::Pause {
                until_utc_ms: Some(normalized.until.get().as_millisecond())
            }
        );
        drop(harness.ingress);
        assert_eq!(
            harness
                .port
                .call(TimedCommand::Resume)
                .expect_err("disconnected")
                .kind,
            ErrorKind::Internal
        );
    }
}
