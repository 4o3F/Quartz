//! Main-thread publication of status independently of SQLite and client writes.

use super::{LogicalDay, RefreshLogicalDay, logical_day::RefreshLogicalDayError};
use crate::{core::sources::ClockReader, query::calendar::DayStartOffset};
use quartz_domain::capture::{CaptureState, SessionReadiness, StopReason as CaptureStop};
use quartz_protocol::{CoreStatusResponse, CoreVersion, DatabaseState, InstanceId, StopReason};
use std::{
    cell::{Cell, RefCell},
    time::Duration,
};
use tokio::sync::watch;

use crate::persistence::PersistenceState;
use quartz_domain::capture::{AppIdentity, ForegroundView};
use quartz_protocol::{ForegroundState, LocaleSettings, StateSnapshot, UiLanguage};

/// Main-thread facts; the registry constructs wire state only when subscribed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishedStatus {
    logical_date: quartz_protocol::LocalDate,
    pub(crate) status: CoreStatusResponse,
    foreground: Option<ForegroundView>,
    language: UiLanguage,
    locale: Option<LocaleSettings>,
    resident: quartz_protocol::ResidentSettings,
    prewarm: quartz_protocol::PrewarmSettings,
    timed: quartz_protocol::TimedPolicies,
}
impl PublishedStatus {
    pub(crate) fn snapshot(&self) -> StateSnapshot {
        let foreground = self
            .foreground
            .as_ref()
            .filter(|_| self.status.recording)
            .map(|view| ForegroundState {
                app: view.app.as_ref().map(|app| match app {
                    AppIdentity::Exe(value) => quartz_protocol::AppIdentity::Exe(value.clone()),
                    AppIdentity::Aumid(value) => quartz_protocol::AppIdentity::Aumid(value.clone()),
                    AppIdentity::Unresolved(value) => {
                        quartz_protocol::AppIdentity::Unresolved(value.clone())
                    }
                }),
                title: view.title.clone(),
                interval_start_utc_ms: view.start.utc.get().as_millisecond(),
            });
        StateSnapshot {
            ui_resident: self.resident.enabled,
            logical_date: self.logical_date,
            timed_policies: self.timed,
            foreground,
            recording: self.status.recording,
            stop_reason: self.status.stop_reason,
            browser_streams: self.status.browser_streams.clone(),
            ui_language: self.language,
            locale: self.locale,
        }
    }
}

#[derive(Debug)]
pub(crate) struct StatusPublisher {
    sender: watch::Sender<PublishedStatus>,
    logical_day: RefCell<LogicalDay>,
    pending_offset: Cell<Option<DayStartOffset>>,
}
impl StatusPublisher {
    #[cfg(test)]
    pub(crate) fn new(instance_id: InstanceId) -> Self {
        Self::with_locale(
            instance_id,
            quartz_protocol::EffectiveUiSettings {
                resident: Default::default(),
                prewarm: Default::default(),
                ui_language: UiLanguage::English,
                locale: Some(LocaleSettings {
                    hour_cycle: quartz_protocol::HourCycle::H24,
                    first_day_of_week: quartz_protocol::Weekday::Monday,
                }),
            },
            LogicalDay::new(jiff::Timestamp::UNIX_EPOCH, DayStartOffset::MIDNIGHT)
                .expect("test logical day"),
        )
    }
    pub(crate) fn with_locale(
        instance_id: InstanceId,
        settings: quartz_protocol::EffectiveUiSettings,
        logical_day: LogicalDay,
    ) -> Self {
        Self {
            sender: watch::Sender::new(PublishedStatus {
                logical_date: logical_day.date(),
                foreground: None,
                timed: quartz_protocol::TimedPolicies::default(),
                language: settings.ui_language,
                locale: settings.locale,
                resident: settings.resident,
                prewarm: settings.prewarm,
                status: CoreStatusResponse {
                    recording: false,
                    stop_reason: Some(StopReason::Starting),
                    database: DatabaseState::Ok,
                    // TODO(M5): Publish connected browser streams.
                    browser_streams: Vec::new(),
                    instance_id,
                    core_version: CoreVersion(env!("CARGO_PKG_VERSION").into()),
                },
            }),
            logical_day: RefCell::new(logical_day),
            pending_offset: Cell::new(None),
        }
    }
    /// Retries failed reads on main-thread wakes, keeping the last successful date and boundary.
    pub(crate) fn update_logical_day(
        &self,
        change: RefreshLogicalDay,
        clock: &dyn ClockReader,
        zone: impl FnOnce() -> Result<jiff::tz::TimeZone, jiff::Error>,
    ) -> Option<Duration> {
        self.request_logical_day(change);
        self.read_logical_day(clock, zone)
    }
    pub(crate) fn request_logical_day(&self, change: RefreshLogicalDay) {
        match change {
            RefreshLogicalDay::CheckDeadline => {}
            RefreshLogicalDay::ResetZone => {
                self.pending_offset.set(Some(self.desired_day_offset()));
            }
            RefreshLogicalDay::SetOffset(offset) => self.pending_offset.set(Some(offset)),
        }
    }
    fn read_logical_day(
        &self,
        clock: &dyn ClockReader,
        zone: impl FnOnce() -> Result<jiff::tz::TimeZone, jiff::Error>,
    ) -> Option<Duration> {
        let now = match clock.read() {
            Ok(at) => at.utc.get(),
            Err(source) => {
                self.retain_logical_day(RefreshLogicalDayError::ReadClock { source });
                return None;
            }
        };
        let next = match self.pending_offset.get() {
            Some(offset) => zone()
                .map_err(crate::query::RunQueryError::from)
                .and_then(|zone| LogicalDay::in_zone(now, offset, zone))
                .map(Some),
            None if self.logical_day.borrow().is_due(now) => {
                self.logical_day.borrow().advance(now).map(Some)
            }
            None => Ok(None),
        };
        match next {
            Ok(day) => {
                if let Some(day) = day {
                    self.publish_logical_day(day);
                }
                self.pending_offset.set(None);
                Some(self.logical_day.borrow().wait(now))
            }
            Err(source) => {
                self.retain_logical_day(RefreshLogicalDayError::ResolveCalendar { source });
                let previous = self.logical_day.borrow();
                // An expired boundary stays stored, but cannot arm a zero-delay retry loop.
                if previous.is_due(now) {
                    None
                } else {
                    Some(previous.wait(now))
                }
            }
        }
    }
    fn desired_day_offset(&self) -> DayStartOffset {
        self.pending_offset
            .get()
            .unwrap_or_else(|| self.logical_day.borrow().offset())
    }
    fn retain_logical_day(&self, error: RefreshLogicalDayError) {
        self.pending_offset.set(Some(self.desired_day_offset()));
        tracing::warn!(
            ?error,
            "retaining logical day until the next main-thread wake"
        );
    }
    fn publish_logical_day(&self, day: LogicalDay) {
        let date = day.date();
        self.logical_day.replace(day);
        self.sender.send_if_modified(|published| {
            if published.logical_date == date {
                return false;
            }
            published.logical_date = date;
            true
        });
    }
    pub(crate) fn apply_ui_settings(
        &self,
        settings: quartz_protocol::EffectiveUiSettings,
        update_tray: impl FnOnce(UiLanguage),
    ) {
        update_tray(settings.ui_language);
        self.set_ui_settings(settings);
    }
    pub(crate) fn set_ui_settings(&self, settings: quartz_protocol::EffectiveUiSettings) {
        self.sender.send_if_modified(|published| {
            if published.language == settings.ui_language
                && published.locale == settings.locale
                && published.resident == settings.resident
                && published.prewarm == settings.prewarm
            {
                return false;
            }
            published.language = settings.ui_language;
            published.locale = settings.locale;
            published.resident = settings.resident;
            published.prewarm = settings.prewarm;
            true
        });
    }
    pub(crate) fn ui_settings(&self) -> quartz_protocol::EffectiveUiSettings {
        let published = self.sender.borrow();
        quartz_protocol::EffectiveUiSettings {
            resident: published.resident,
            prewarm: published.prewarm,
            ui_language: published.language,
            locale: published.locale,
        }
    }
    pub(crate) fn timed_policies(&self) -> quartz_protocol::TimedPolicies {
        self.sender.borrow().timed
    }
    pub(crate) fn stop_reason(&self) -> Option<StopReason> {
        self.sender.borrow().status.stop_reason
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<PublishedStatus> {
        self.sender.subscribe()
    }
    #[cfg(test)]
    pub(crate) fn publish_foreground(&self, foreground: Option<ForegroundView>) {
        self.sender.send_if_modified(|published| {
            if published.foreground == foreground {
                return false;
            }
            published.foreground = foreground;
            true
        });
    }
    pub(crate) fn publish(&self, capture: Option<&CaptureState>, persistence: PersistenceState) {
        let foreground = self.sender.borrow().foreground.clone();
        self.publish_snapshot(capture, persistence, foreground);
    }
    pub(crate) fn publish_snapshot(
        &self,
        capture: Option<&CaptureState>,
        persistence: PersistenceState,
        foreground: Option<ForegroundView>,
    ) {
        self.publish_capture(capture, persistence, foreground, self.timed_policies());
    }
    pub(crate) fn publish_capture(
        &self,
        capture: Option<&CaptureState>,
        persistence: PersistenceState,
        foreground: Option<ForegroundView>,
        timed: quartz_protocol::TimedPolicies,
    ) {
        let database = match persistence {
            PersistenceState::Ready | PersistenceState::Buffered { .. } => DatabaseState::Ok,
            PersistenceState::Failed { .. } => DatabaseState::PersistenceFailed,
        };
        let stop_reason = match capture {
            _ if database == DatabaseState::PersistenceFailed => {
                Some(StopReason::PersistenceFailed)
            }
            None => Some(StopReason::Starting),
            Some(CaptureState::Stopped {
                session: SessionReadiness::AwaitingSession,
                ..
            }) => Some(StopReason::SessionUnavailable),
            Some(CaptureState::Gap(_)) => Some(StopReason::CaptureGap),
            Some(CaptureState::Stopped { reasons, .. }) => [
                CaptureStop::Suspended,
                CaptureStop::Disconnected,
                CaptureStop::Locked,
                CaptureStop::Paused,
            ]
            .into_iter()
            .find(|reason| reasons.contains(reason))
            .map(|reason| match reason {
                CaptureStop::Suspended => StopReason::Suspended,
                CaptureStop::Disconnected => StopReason::Disconnected,
                CaptureStop::Locked => StopReason::Locked,
                CaptureStop::Paused => StopReason::Paused,
            })
            // A ready session without a stop mask still needs a fresh foreground fact.
            .or(Some(StopReason::NoForeground)),
            Some(CaptureState::Tracking(_)) => None,
            Some(CaptureState::NoForeground(_)) => Some(StopReason::NoForeground),
            Some(CaptureState::Ended) => Some(StopReason::Ended),
        };
        self.sender.send_if_modified(|published| {
            let status = &mut published.status;
            if status.database == database
                && status.stop_reason == stop_reason
                && published.foreground == foreground
                && published.timed == timed
            {
                return false;
            }
            published.foreground = foreground;
            published.timed = timed;
            status.database = database;
            status.stop_reason = stop_reason;
            status.recording = stop_reason.is_none();
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_domain::capture::{GapReason, IntervalFact, NoForegroundReason};

    enum TestClock {
        At(jiff::Timestamp),
        Failed,
    }
    impl ClockReader for TestClock {
        fn read(
            &self,
        ) -> Result<quartz_domain::capture::Timestamp, crate::core::sources::ReadClockError>
        {
            match self {
                Self::At(now) => Ok(quartz_domain::capture::Timestamp {
                    utc: quartz_domain::capture::UtcInstant::new(*now),
                    mono: quartz_domain::capture::MonoInstant::from_millis(0),
                }),
                Self::Failed => Err(crate::core::sources::ReadClockError::Interrupt {
                    source: Box::new(std::io::Error::other("injected clock failure")),
                }),
            }
        }
    }

    #[test]
    fn rollover_deadline_republishes_without_capture_changes() {
        let zone = jiff::tz::TimeZone::get("America/Los_Angeles").expect("time zone");
        let before = "2026-03-08T10:59:59.999Z".parse().expect("before 04:00");
        let boundary = "2026-03-08T11:00:00Z".parse().expect("04:00");
        let publisher = StatusPublisher::with_locale(
            InstanceId("rollover".into()),
            quartz_protocol::EffectiveUiSettings {
                resident: Default::default(),
                prewarm: Default::default(),
                ui_language: UiLanguage::English,
                locale: None,
            },
            LogicalDay::in_zone(
                before,
                DayStartOffset::try_from(240).expect("offset"),
                zone.clone(),
            )
            .expect("logical day"),
        );
        let mut receiver = publisher.subscribe();
        assert_eq!(
            receiver.borrow().snapshot().logical_date,
            "2026-03-07"
                .parse::<quartz_protocol::LocalDate>()
                .expect("date")
        );
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::CheckDeadline,
                &TestClock::At(before),
                || Ok(zone.clone())
            ),
            Some(Duration::from_millis(1))
        );
        assert!(!receiver.has_changed().expect("publisher alive"));
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::CheckDeadline,
                &TestClock::At(boundary),
                || Ok(zone.clone())
            ),
            Some(Duration::from_hours(24))
        );
        assert!(receiver.has_changed().expect("date change published"));
        assert_eq!(
            receiver.borrow_and_update().snapshot().logical_date,
            "2026-03-08"
                .parse::<quartz_protocol::LocalDate>()
                .expect("date")
        );
        publisher.update_logical_day(
            RefreshLogicalDay::CheckDeadline,
            &TestClock::At(boundary),
            || Ok(zone),
        );
        assert!(!receiver.has_changed().expect("no duplicate publication"));
    }

    #[test]
    fn timezone_and_offset_changes_replace_date_and_rollover_deadline() {
        let now = "2026-11-21T02:00:00Z".parse().expect("UTC time");
        let publisher = StatusPublisher::with_locale(
            InstanceId("calendar-settings".into()),
            quartz_protocol::EffectiveUiSettings {
                resident: Default::default(),
                prewarm: Default::default(),
                ui_language: UiLanguage::English,
                locale: None,
            },
            LogicalDay::in_zone(now, DayStartOffset::MIDNIGHT, jiff::tz::TimeZone::UTC)
                .expect("UTC day"),
        );
        let mut receiver = publisher.subscribe();
        let offset = DayStartOffset::try_from(240).expect("offset");
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::SetOffset(offset),
                &TestClock::At(now),
                || Ok(jiff::tz::TimeZone::UTC)
            ),
            Some(Duration::from_hours(2))
        );
        assert_eq!(
            receiver.borrow_and_update().snapshot().logical_date,
            "2026-11-20"
                .parse::<quartz_protocol::LocalDate>()
                .expect("previous date")
        );
        assert_eq!(
            publisher.update_logical_day(RefreshLogicalDay::ResetZone, &TestClock::At(now), || {
                jiff::tz::TimeZone::get("Asia/Tokyo")
            }),
            Some(Duration::from_hours(17))
        );
        assert!(receiver.has_changed().expect("zone publishes"));
        assert_eq!(
            receiver.borrow_and_update().snapshot().logical_date,
            "2026-11-21"
                .parse::<quartz_protocol::LocalDate>()
                .expect("new date")
        );
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::SetOffset(DayStartOffset::MIDNIGHT),
                &TestClock::At(now),
                || jiff::tz::TimeZone::get("Asia/Tokyo")
            ),
            Some(Duration::from_hours(13))
        );
        assert!(
            !receiver
                .has_changed()
                .expect("same date suppresses snapshot")
        );
    }

    #[test]
    fn failed_zone_and_clock_reads_retain_state_and_retry_on_the_next_wake() {
        let now = "2026-11-21T02:00:00Z".parse().expect("UTC time");
        let offset = DayStartOffset::try_from(240).expect("offset");
        let publisher = StatusPublisher::with_locale(
            InstanceId("calendar-recovery".into()),
            quartz_protocol::EffectiveUiSettings {
                resident: Default::default(),
                prewarm: Default::default(),
                ui_language: UiLanguage::English,
                locale: None,
            },
            LogicalDay::in_zone(now, DayStartOffset::MIDNIGHT, jiff::tz::TimeZone::UTC)
                .expect("UTC day"),
        );
        let mut receiver = publisher.subscribe();
        let previous_date = receiver.borrow().snapshot().logical_date;
        let previous_deadline = publisher.logical_day.borrow().next_start_utc_ms();
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::SetOffset(offset),
                &TestClock::At(now),
                || jiff::tz::TimeZone::get("Invalid/InjectedFailure")
            ),
            Some(Duration::from_hours(22)),
            "the existing future deadline remains armed"
        );
        assert_eq!(
            publisher.logical_day.borrow().next_start_utc_ms(),
            previous_deadline
        );
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::ResetZone,
                &TestClock::Failed,
                || panic!("clock failure must skip the zone read")
            ),
            None
        );
        assert_eq!(
            publisher.pending_offset.get(),
            Some(offset),
            "reset preserves the pending committed offset"
        );
        assert_eq!(receiver.borrow().snapshot().logical_date, previous_date);
        assert_eq!(
            publisher.logical_day.borrow().next_start_utc_ms(),
            previous_deadline
        );
        assert!(
            !receiver
                .has_changed()
                .expect("failures keep the last publication")
        );
        publisher.publish(
            Some(&CaptureState::NoForeground(NoForegroundReason::Empty)),
            PersistenceState::Ready,
        );
        assert!(
            receiver
                .has_changed()
                .expect("capture publication continues during recovery")
        );
        assert_eq!(
            receiver.borrow_and_update().status.stop_reason,
            Some(StopReason::NoForeground)
        );
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::CheckDeadline,
                &TestClock::At(now),
                || Ok(jiff::tz::TimeZone::UTC)
            ),
            Some(Duration::from_hours(2))
        );
        assert_eq!(publisher.pending_offset.get(), None);
        assert_eq!(
            receiver.borrow_and_update().snapshot().logical_date,
            "2026-11-20"
                .parse::<quartz_protocol::LocalDate>()
                .expect("offset date")
        );
        let boundary = "2026-11-21T04:00:00Z".parse().expect("boundary");
        let previous_deadline = publisher.logical_day.borrow().next_start_utc_ms();
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::ResetZone,
                &TestClock::At(boundary),
                || jiff::tz::TimeZone::get("Invalid/InjectedFailure")
            ),
            None,
            "an expired retained deadline cannot trigger a polling loop"
        );
        assert_eq!(
            publisher.logical_day.borrow().next_start_utc_ms(),
            previous_deadline
        );
        assert!(
            !receiver
                .has_changed()
                .expect("expired failure keeps the previous date")
        );
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::CheckDeadline,
                &TestClock::At(boundary),
                || Ok(jiff::tz::TimeZone::UTC)
            ),
            Some(Duration::from_hours(24))
        );
        assert_eq!(
            receiver.borrow().snapshot().logical_date,
            "2026-11-21"
                .parse::<quartz_protocol::LocalDate>()
                .expect("recovered date")
        );
    }

    #[test]
    fn a_new_offset_event_replaces_the_offset_pending_after_failure() {
        let now = "2026-11-21T02:00:00Z".parse().expect("UTC time");
        let publisher = StatusPublisher::with_locale(
            InstanceId("latest-offset".into()),
            quartz_protocol::EffectiveUiSettings {
                resident: Default::default(),
                prewarm: Default::default(),
                ui_language: UiLanguage::English,
                locale: None,
            },
            LogicalDay::in_zone(now, DayStartOffset::MIDNIGHT, jiff::tz::TimeZone::UTC)
                .expect("UTC day"),
        );
        publisher.update_logical_day(
            RefreshLogicalDay::SetOffset(DayStartOffset::try_from(240).expect("offset")),
            &TestClock::Failed,
            || Ok(jiff::tz::TimeZone::UTC),
        );
        publisher.update_logical_day(
            RefreshLogicalDay::SetOffset(DayStartOffset::MIDNIGHT),
            &TestClock::Failed,
            || Ok(jiff::tz::TimeZone::UTC),
        );
        assert_eq!(
            publisher.update_logical_day(
                RefreshLogicalDay::CheckDeadline,
                &TestClock::At(now),
                || Ok(jiff::tz::TimeZone::UTC)
            ),
            Some(Duration::from_hours(22))
        );
        assert_eq!(
            publisher.logical_day.borrow().offset(),
            DayStartOffset::MIDNIGHT
        );
    }
    #[test]
    fn projected_foreground_preserves_state_body_identity_title_and_interval_start() {
        use quartz_domain::capture::{
            CaptureLevel, CapturedFields, Timestamp, TitleStatus, WindowId,
        };
        use serde_json::json;
        for app in [
            AppIdentity::Exe("c:\\app.exe".into()),
            AppIdentity::Aumid("Package!App".into()),
            AppIdentity::Unresolved("WindowClass".into()),
        ] {
            for level in [
                CaptureLevel::Full,
                CaptureLevel::TitleHidden,
                CaptureLevel::Excluded,
            ] {
                let publisher = StatusPublisher::new(InstanceId("state-test".into()));
                let title = (level == CaptureLevel::Full).then(|| "allowed title".to_owned());
                let identity = (level != CaptureLevel::Excluded).then_some(app.clone());
                let expected_app = match &identity {
                    Some(AppIdentity::Exe(value)) => json!({"kind":"exe", "value":value}),
                    Some(AppIdentity::Aumid(value)) => json!({"kind":"aumid", "value":value}),
                    Some(AppIdentity::Unresolved(value)) => {
                        json!({"kind":"unresolved", "value":value})
                    }
                    None => serde_json::Value::Null,
                };
                let capture = CaptureState::Tracking(IntervalFact::Window {
                    instance: (level != CaptureLevel::Excluded)
                        .then_some(WindowId::from_persisted(1)),
                    fields: CapturedFields {
                        level,
                        title: title.clone(),
                        title_status: if title.is_some() {
                            TitleStatus::Ok
                        } else {
                            TitleStatus::Redacted
                        },
                        privacy: None,
                        web: quartz_domain::rules::WebAction::None,
                    },
                });
                publisher.publish_snapshot(
                    Some(&capture),
                    PersistenceState::Ready,
                    Some(ForegroundView {
                        app: identity,
                        title: title.clone(),
                        start: Timestamp {
                            utc: quartz_domain::capture::UtcInstant::try_from(123)
                                .expect("test UTC"),
                            mono: quartz_domain::capture::MonoInstant::from_millis(0),
                        },
                    }),
                );
                let receiver = publisher.subscribe();
                let body = serde_json::to_value(receiver.borrow().snapshot()).expect("state body");
                assert_eq!(
                    body,
                    json!({
                        "ui_resident":true,
                        "logical_date":receiver.borrow().snapshot().logical_date,
                        "foreground": {"app":expected_app, "title":title, "interval_start_utc_ms":123},
                        "timed_policies":{"pause_until_utc_ms":null,"privacy_mode":null,"keep_timing_until_utc_ms":null},"recording":true, "stop_reason":null, "browser_streams":[], "ui_language":"en",
                        "locale":{"hour_cycle":"h24", "first_day_of_week":"monday"}
                    })
                );
                publisher.publish(Some(&CaptureState::Ended), PersistenceState::Ready);
                assert_eq!(receiver.borrow().snapshot().foreground, None);
            }
        }
    }

    #[test]
    fn snapshots_follow_capture_and_persistence_with_fixed_priority() {
        let publisher = StatusPublisher::new(InstanceId("status-test".into()));
        let receiver = publisher.subscribe();
        let tracking =
            CaptureState::Tracking(IntervalFact::NoForeground(NoForegroundReason::Empty));
        publisher.publish(Some(&tracking), PersistenceState::Ready);
        assert!(receiver.borrow().status.recording);
        assert_eq!(receiver.borrow().status.stop_reason, None);
        publisher.publish(Some(&tracking), PersistenceState::Buffered { commands: 1 });
        assert_eq!(receiver.borrow().status.database, DatabaseState::Ok);
        let stopped = CaptureState::Stopped {
            reasons: [
                CaptureStop::Paused,
                CaptureStop::Locked,
                CaptureStop::Suspended,
            ]
            .into(),
            session: SessionReadiness::Ready,
        };
        publisher.publish(Some(&stopped), PersistenceState::Ready);
        assert_eq!(
            receiver.borrow().status.stop_reason,
            Some(StopReason::Suspended)
        );
        publisher.publish(
            Some(&CaptureState::Gap(GapReason::EventOverload)),
            PersistenceState::Failed { commands: 1 },
        );
        assert_eq!(
            receiver.borrow().status.stop_reason,
            Some(StopReason::PersistenceFailed)
        );
        assert_eq!(
            receiver.borrow().status.database,
            DatabaseState::PersistenceFailed
        );
        publisher.publish(
            Some(&CaptureState::Stopped {
                reasons: [CaptureStop::Locked].into(),
                session: SessionReadiness::AwaitingSession,
            }),
            PersistenceState::Ready,
        );
        assert_eq!(
            receiver.borrow().status.stop_reason,
            Some(StopReason::SessionUnavailable)
        );
    }
    #[test]
    fn settings_setter_publishes_locale_changes_and_suppresses_identical_values() {
        let publisher = StatusPublisher::new(InstanceId("locale-update".into()));
        let mut receiver = publisher.subscribe();
        let mut settings = publisher.ui_settings();
        publisher.set_ui_settings(settings);
        assert!(!receiver.has_changed().expect("publisher alive"));
        settings.locale = Some(LocaleSettings {
            hour_cycle: quartz_protocol::HourCycle::H12,
            first_day_of_week: quartz_protocol::Weekday::Sunday,
        });
        publisher.set_ui_settings(settings);
        assert!(receiver.has_changed().expect("publisher alive"));
        assert_eq!(
            receiver.borrow_and_update().snapshot().locale,
            settings.locale
        );
        publisher.set_ui_settings(settings);
        assert!(!receiver.has_changed().expect("identical settings"));
    }
    #[test]
    fn timed_snapshots_publish_changes_while_stopped_and_clear_on_expiry() {
        let publisher = StatusPublisher::new(InstanceId("timed-state".into()));
        let mut receiver = publisher.subscribe();
        let stopped = CaptureState::Stopped {
            reasons: [CaptureStop::Paused].into(),
            session: SessionReadiness::Ready,
        };
        let policies = quartz_protocol::TimedPolicies {
            pause_until_utc_ms: Some(100),
            privacy_mode: Some(quartz_protocol::TimedPrivacyMode {
                mode: quartz_protocol::PrivacyMode::Private,
                until_utc_ms: 200,
            }),
            keep_timing_until_utc_ms: Some(300),
        };
        publisher.publish_capture(Some(&stopped), PersistenceState::Ready, None, policies);
        assert_eq!(
            receiver.borrow_and_update().snapshot().timed_policies,
            policies
        );
        publisher.publish_capture(Some(&stopped), PersistenceState::Ready, None, policies);
        assert!(!receiver.has_changed().expect("publisher alive"));
        publisher.publish_capture(
            Some(&stopped),
            PersistenceState::Ready,
            None,
            quartz_protocol::TimedPolicies::default(),
        );
        assert!(receiver.has_changed().expect("expiry publishes"));
        assert_eq!(
            receiver.borrow().snapshot().timed_policies,
            quartz_protocol::TimedPolicies::default()
        );
    }
}
