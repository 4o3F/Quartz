//! Window-free launch decisions while a prewarm process waits for an explicit display request.

use crate::link::{ConnectionState, LinkEvent};
use quartz_protocol::ControlOp;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrewarmGate {
    #[default]
    Hidden,
    Opened,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrewarmAction {
    Wait,
    Open,
    Quit,
}
impl PrewarmGate {
    pub(crate) fn receive(&mut self, event: &LinkEvent) -> PrewarmAction {
        match event {
            LinkEvent::Disconnected
            | LinkEvent::Control(ControlOp::UiShutdown)
            | LinkEvent::Connection(ConnectionState::Failed(_)) => PrewarmAction::Quit,
            LinkEvent::Control(ControlOp::UiShow | ControlOp::UiActivate) => match self {
                Self::Hidden => {
                    *self = Self::Opened;
                    PrewarmAction::Open
                }
                Self::Opened => PrewarmAction::Wait,
            },
            LinkEvent::State(_)
            | LinkEvent::Day(_)
            | LinkEvent::Connection(
                ConnectionState::Connecting | ConnectionState::Connected { .. },
            )
            | LinkEvent::DayLoading { .. }
            | LinkEvent::DayLoaded { .. }
            | LinkEvent::DetailLoading { .. }
            | LinkEvent::DetailProgress { .. }
            | LinkEvent::DetailLoaded { .. } => PrewarmAction::Wait,
        }
    }
}
/// High-level lifecycle of the UI session loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionLifecycle {
    Running,
    Stopping,
}

/// Whether a window-close request (`WM_CLOSE`) should hide the existing window or destroy it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowCloseDecision {
    Hide,
    Destroy,
}
impl WindowCloseDecision {
    pub(crate) fn decide(
        lifecycle: SessionLifecycle,
        resident: quartz_protocol::ResidentEnabled,
        connection: &ConnectionState,
    ) -> Self {
        match lifecycle {
            SessionLifecycle::Stopping => Self::Destroy,
            SessionLifecycle::Running => match LastWindowAction::decide(resident, connection) {
                LastWindowAction::HideAndNotify => Self::Hide,
                LastWindowAction::Quit => Self::Destroy,
            },
        }
    }
}

/// Whether the UI session already owns a GPUI window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowPresence {
    Absent,
    Present,
}
impl WindowPresence {
    pub(crate) const fn from_option<T>(window: Option<&T>) -> Self {
        match window {
            Some(_) => Self::Present,
            None => Self::Absent,
        }
    }
}

/// Whether showing the UI should create a new window or unhide and reuse the existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShowWindowDecision {
    CreateWindow,
    ReuseWindow,
}
impl ShowWindowDecision {
    pub(crate) const fn decide(presence: WindowPresence) -> Self {
        match presence {
            WindowPresence::Absent => Self::CreateWindow,
            WindowPresence::Present => Self::ReuseWindow,
        }
    }
}

/// Last-window-close behavior, independent of GPUI windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LastWindowAction {
    Quit,
    HideAndNotify,
}
impl LastWindowAction {
    pub(crate) fn decide(
        resident: quartz_protocol::ResidentEnabled,
        connection: &ConnectionState,
    ) -> Self {
        if resident == quartz_protocol::ResidentEnabled::Enabled
            && matches!(connection, ConnectionState::Connected { .. })
        {
            Self::HideAndNotify
        } else {
            Self::Quit
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn connected() -> ConnectionState {
        ConnectionState::Connected {
            instance_id: quartz_protocol::InstanceId("resident-test".into()),
            core_version: quartz_protocol::CoreVersion("test".into()),
        }
    }
    #[test]
    fn prewarm_waits_for_show_and_opens_exactly_once() {
        let mut gate = PrewarmGate::default();
        assert_eq!(
            gate.receive(&LinkEvent::Connection(ConnectionState::Connecting)),
            PrewarmAction::Wait
        );
        assert_eq!(gate, PrewarmGate::Hidden);
        assert_eq!(
            gate.receive(&LinkEvent::Control(ControlOp::UiShow)),
            PrewarmAction::Open
        );
        assert_eq!(
            gate.receive(&LinkEvent::Control(ControlOp::UiShow)),
            PrewarmAction::Wait
        );
        assert_eq!(gate.receive(&LinkEvent::Disconnected), PrewarmAction::Quit);
    }
    #[test]
    fn shutdown_and_connection_failure_exit_without_opening() {
        for event in [
            LinkEvent::Control(ControlOp::UiShutdown),
            LinkEvent::Disconnected,
            LinkEvent::Connection(ConnectionState::Failed(crate::failure::Failure::new(
                crate::failure::FailureKind::CoreNotRunning,
            ))),
        ] {
            let mut gate = PrewarmGate::Hidden;
            assert_eq!(gate.receive(&event), PrewarmAction::Quit);
            assert_eq!(gate, PrewarmGate::Hidden);
        }
    }
    #[test]
    fn last_window_close_uses_effective_resident_policy_and_connection() {
        let connected = connected();
        assert_eq!(
            LastWindowAction::decide(quartz_protocol::ResidentEnabled::Enabled, &connected),
            LastWindowAction::HideAndNotify
        );
        assert_eq!(
            LastWindowAction::decide(quartz_protocol::ResidentEnabled::Disabled, &connected),
            LastWindowAction::Quit
        );
        assert_eq!(
            LastWindowAction::decide(
                quartz_protocol::ResidentEnabled::Enabled,
                &ConnectionState::Connecting
            ),
            LastWindowAction::Quit
        );
        let mut gate = PrewarmGate::Opened;
        gate = match LastWindowAction::decide(quartz_protocol::ResidentEnabled::Enabled, &connected)
        {
            LastWindowAction::HideAndNotify => PrewarmGate::Hidden,
            LastWindowAction::Quit => gate,
        };
        assert_eq!(
            gate.receive(&LinkEvent::Control(ControlOp::UiShow)),
            PrewarmAction::Open
        );
    }
    #[test]
    fn resident_close_hides_without_destroying_while_on_demand_and_stopping_destroy() {
        let connected = connected();
        let failed = ConnectionState::Failed(crate::failure::Failure::new(
            crate::failure::FailureKind::CoreNotRunning,
        ));
        assert_eq!(
            WindowCloseDecision::decide(
                SessionLifecycle::Running,
                quartz_protocol::ResidentEnabled::Enabled,
                &connected
            ),
            WindowCloseDecision::Hide
        );
        assert_eq!(
            WindowCloseDecision::decide(
                SessionLifecycle::Running,
                quartz_protocol::ResidentEnabled::Disabled,
                &connected
            ),
            WindowCloseDecision::Destroy
        );
        assert_eq!(
            WindowCloseDecision::decide(
                SessionLifecycle::Running,
                quartz_protocol::ResidentEnabled::Enabled,
                &ConnectionState::Connecting
            ),
            WindowCloseDecision::Destroy
        );
        assert_eq!(
            WindowCloseDecision::decide(
                SessionLifecycle::Running,
                quartz_protocol::ResidentEnabled::Enabled,
                &failed
            ),
            WindowCloseDecision::Destroy
        );
        assert_eq!(
            WindowCloseDecision::decide(
                SessionLifecycle::Stopping,
                quartz_protocol::ResidentEnabled::Enabled,
                &connected
            ),
            WindowCloseDecision::Destroy
        );
    }
    #[test]
    fn show_reuses_an_existing_window_and_creates_when_absent() {
        let absent: Option<()> = None;
        let present = Some(());
        assert_eq!(
            ShowWindowDecision::decide(WindowPresence::from_option(absent.as_ref())),
            ShowWindowDecision::CreateWindow
        );
        assert_eq!(
            ShowWindowDecision::decide(WindowPresence::from_option(present.as_ref())),
            ShowWindowDecision::ReuseWindow
        );
    }
    #[test]
    fn hidden_reused_window_reopens_on_show_and_quits_on_shutdown() {
        let connected = connected();
        assert_eq!(
            WindowCloseDecision::decide(
                SessionLifecycle::Running,
                quartz_protocol::ResidentEnabled::Enabled,
                &connected
            ),
            WindowCloseDecision::Hide
        );
        let mut gate = PrewarmGate::Hidden;
        assert_eq!(
            gate.receive(&LinkEvent::Control(ControlOp::UiShow)),
            PrewarmAction::Open
        );
        assert_eq!(
            ShowWindowDecision::decide(WindowPresence::Present),
            ShowWindowDecision::ReuseWindow
        );
        assert_eq!(gate, PrewarmGate::Opened);
        gate = PrewarmGate::Hidden;
        assert_eq!(
            gate.receive(&LinkEvent::Control(ControlOp::UiShutdown)),
            PrewarmAction::Quit
        );
        assert_eq!(
            WindowCloseDecision::decide(
                SessionLifecycle::Stopping,
                quartz_protocol::ResidentEnabled::Enabled,
                &connected
            ),
            WindowCloseDecision::Destroy
        );
    }
    #[cfg(windows)]
    #[test]
    fn reopening_rejects_the_previous_windows_query_results() {
        let previous = crate::app::QueryGenerations::default().next();
        let reopened = previous.next();
        assert!(!reopened.day.accepts(previous.day));
        assert!(!reopened.details.accepts(previous.details));
    }
}
