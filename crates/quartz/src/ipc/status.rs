//! Main-thread publication of status independently of SQLite and client writes.

use quartz_domain::capture::{CaptureState, SessionReadiness, StopReason as CaptureStop};
use quartz_protocol::{CoreStatusResponse, CoreVersion, DatabaseState, InstanceId, StopReason};
use tokio::sync::watch;

use crate::capture::PersistenceState;
use quartz_domain::capture::{AppIdentity, IntervalFact, OpenInterval};
use quartz_protocol::{ForegroundState, LocaleSettings, StateSnapshot, UiLanguage};

/// Main-thread facts; the registry constructs wire state only when subscribed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishedStatus {
    pub(crate) status: CoreStatusResponse,
    interval: Option<OpenInterval>,
    language: UiLanguage,
    locale: Option<LocaleSettings>,
}
impl PublishedStatus {
    pub(crate) fn snapshot(&self) -> StateSnapshot {
        let foreground = self
            .interval
            .as_ref()
            .filter(|_| self.status.recording)
            .and_then(|row| {
                let IntervalFact::Window { app, fields, .. } = &row.fact else {
                    return None;
                };
                Some(ForegroundState {
                    app: app.as_ref().map(|app| match app {
                        AppIdentity::Exe(value) => quartz_protocol::AppIdentity::Exe(value.clone()),
                        AppIdentity::Aumid(value) => {
                            quartz_protocol::AppIdentity::Aumid(value.clone())
                        }
                        AppIdentity::Unresolved(value) => {
                            quartz_protocol::AppIdentity::Unresolved(value.clone())
                        }
                    }),
                    title: fields.title.clone(),
                    interval_start_utc_ms: row.start.utc.get().as_millisecond(),
                })
            });
        StateSnapshot {
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
}
impl StatusPublisher {
    #[cfg(test)]
    pub(crate) fn new(instance_id: InstanceId) -> Self {
        Self::with_locale(
            instance_id,
            UiLanguage::English,
            Some(LocaleSettings {
                hour_cycle: quartz_protocol::HourCycle::H24,
                first_day_of_week: quartz_protocol::Weekday::Monday,
            }),
        )
    }
    pub(crate) fn with_locale(
        instance_id: InstanceId,
        language: UiLanguage,
        locale: Option<LocaleSettings>,
    ) -> Self {
        Self {
            sender: watch::Sender::new(PublishedStatus {
                interval: None,
                language,
                locale,
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
        }
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<PublishedStatus> {
        self.sender.subscribe()
    }
    #[cfg(test)]
    pub(crate) fn publish_interval(&self, interval: Option<OpenInterval>) {
        self.sender.send_if_modified(|published| {
            if published.interval == interval {
                return false;
            }
            published.interval = interval;
            true
        });
    }
    pub(crate) fn publish(&self, capture: Option<&CaptureState>, persistence: PersistenceState) {
        let interval = self.sender.borrow().interval.clone();
        self.publish_snapshot(capture, persistence, interval);
    }
    pub(crate) fn publish_snapshot(
        &self,
        capture: Option<&CaptureState>,
        persistence: PersistenceState,
        interval: Option<OpenInterval>,
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
            Some(CaptureState::AwaitingBrowserContext) => Some(StopReason::AwaitingBrowserContext),
            Some(CaptureState::NoForeground(_)) => Some(StopReason::NoForeground),
            Some(CaptureState::Ended) => Some(StopReason::Ended),
        };
        self.sender.send_if_modified(|published| {
            let status = &mut published.status;
            if status.database == database
                && status.stop_reason == stop_reason
                && published.interval == interval
            {
                return false;
            }
            published.interval = interval;
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
}
