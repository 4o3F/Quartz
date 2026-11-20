//! Main-thread publication of status independently of SQLite and client writes.

use quartz_domain::capture::{CaptureState, SessionReadiness, StopReason as CaptureStop};
use quartz_protocol::{CoreStatusResponse, CoreVersion, DatabaseState, InstanceId, StopReason};
use tokio::sync::watch;

use crate::capture::PersistenceState;

#[derive(Debug)]
pub(crate) struct StatusPublisher {
    sender: watch::Sender<CoreStatusResponse>,
}
impl StatusPublisher {
    pub(crate) fn new(instance_id: InstanceId) -> Self {
        Self {
            sender: watch::Sender::new(CoreStatusResponse {
                recording: false,
                stop_reason: Some(StopReason::Starting),
                database: DatabaseState::Ok,
                // TODO(M5): Publish connected browser streams.
                browser_streams: Vec::new(),
                instance_id,
                core_version: CoreVersion(env!("CARGO_PKG_VERSION").into()),
            }),
        }
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<CoreStatusResponse> {
        self.sender.subscribe()
    }
    pub(crate) fn publish(&self, capture: Option<&CaptureState>, persistence: PersistenceState) {
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
        self.sender.send_modify(|status| {
            status.database = database;
            status.stop_reason = stop_reason;
            status.recording = stop_reason.is_none();
        });
        // TODO(M2-B2): Publish state subscriptions with throttling and coalescing.
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
        assert!(receiver.borrow().recording);
        assert_eq!(receiver.borrow().stop_reason, None);
        publisher.publish(Some(&tracking), PersistenceState::Buffered { commands: 1 });
        assert_eq!(receiver.borrow().database, DatabaseState::Ok);
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
        assert_eq!(receiver.borrow().stop_reason, Some(StopReason::Suspended));
        publisher.publish(
            Some(&CaptureState::Gap(GapReason::EventOverload)),
            PersistenceState::Failed { commands: 1 },
        );
        assert_eq!(
            receiver.borrow().stop_reason,
            Some(StopReason::PersistenceFailed)
        );
        assert_eq!(receiver.borrow().database, DatabaseState::PersistenceFailed);
        publisher.publish(
            Some(&CaptureState::Stopped {
                reasons: [CaptureStop::Locked].into(),
                session: SessionReadiness::AwaitingSession,
            }),
            PersistenceState::Ready,
        );
        assert_eq!(
            receiver.borrow().stop_reason,
            Some(StopReason::SessionUnavailable)
        );
    }
}
