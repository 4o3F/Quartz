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
            | LinkEvent::DetailLoaded { .. }
            | LinkEvent::Control(
                ControlOp::BrowserAck
                | ControlOp::BrowserPolicy
                | ControlOp::BrowserPurge
                | ControlOp::CoreOffline,
            ) => PrewarmAction::Wait,
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
        let connected = ConnectionState::Connected {
            instance_id: quartz_protocol::InstanceId("resident-test".into()),
            core_version: quartz_protocol::CoreVersion("test".into()),
        };
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
    fn reopening_rejects_the_previous_windows_query_results() {
        let previous = crate::app::QueryGenerations::default().next();
        let reopened = previous.next();
        assert!(!reopened.day.accepts(previous.day));
        assert!(!reopened.details.accepts(previous.details));
    }
}
