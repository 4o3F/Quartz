//! Main-thread UI ownership and display decisions; all transport delivery is non-blocking.

use crate::{
    ipc::registry::{ConnectionId, UiPeer},
    process_id::ProcessId,
};
use quartz_protocol::{
    ControlOp, PrewarmEnabled, PrewarmOnBattery, PrewarmSettings, ResidentEnabled,
    UiState as HelloState,
};
use std::time::{Duration, Instant};

/// Hover-prewarm lifetime from authenticated hello (01 section 4.2).
pub(super) const PREWARM_TTL: Duration = Duration::from_secs(8);
/// Minimum interval between successful hover launches (01 section 4.2).
pub(super) const PREWARM_COOLDOWN: Duration = Duration::from_secs(60);
/// Minimum interval between resident launch attempts.
pub(super) const RESIDENT_RELAUNCH_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum CoreLifecycle {
    #[default]
    Running,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LaunchIntent {
    Show,
    Prewarm,
    Resident,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PowerSource {
    Ac,
    Battery,
}

pub(super) trait UiProcess {
    fn pid(&self) -> ProcessId;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Starting,
    Prewarming,
    Hidden,
    Visible,
}
impl From<HelloState> for Phase {
    fn from(state: HelloState) -> Self {
        match state {
            HelloState::Starting => Self::Starting,
            HelloState::Prewarming => Self::Prewarming,
            HelloState::Visible => Self::Visible,
        }
    }
}
#[derive(Debug, Default, PartialEq, Eq)]
enum State<Process> {
    #[default]
    None,
    Starting {
        process: Process,
        intent: LaunchIntent,
    },
    Closing {
        process: Process,
    },
    Registered {
        process: Process,
        connection: ConnectionId,
        phase: Phase,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UiControl {
    pub connection: ConnectionId,
    pub pid: ProcessId,
    pub op: ControlOp,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RegisterUi {
    Accepted { replay: Option<UiControl> },
    Ignored,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExitUi {
    Closed,
    PendingShowLost,
    Unrelated,
    OpenRequested,
}
impl ExitUi {
    pub(super) fn log_ui_exit(&self) {
        if *self == Self::PendingShowLost {
            tracing::warn!(
                "UI closed before completing its display request; pending open cleared, waiting for another user request"
            );
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum DisplayRequest {
    #[default]
    Idle,
    Pending,
}
/// Owns a launched or adopted process and the first registered UI connection.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct UiState<Process> {
    state: State<Process>,
    show_requested: DisplayRequest,
    prewarm_deadline: Option<Instant>,
    last_prewarm: Option<Instant>,
    resident: ResidentEnabled,
    lifecycle: CoreLifecycle,
    last_launch: Option<Instant>,
    resident_deadline: Option<Instant>,
}
impl<Process> Default for UiState<Process> {
    fn default() -> Self {
        Self {
            state: State::None,
            show_requested: DisplayRequest::Idle,
            prewarm_deadline: None,
            last_prewarm: None,
            resident: ResidentEnabled::Disabled,
            lifecycle: CoreLifecycle::Running,
            last_launch: None,
            resident_deadline: None,
        }
    }
}
impl<Process: UiProcess> UiState<Process> {
    pub(super) fn configure_resident(
        &mut self,
        enabled: ResidentEnabled,
        now: Instant,
    ) -> Option<UiControl> {
        let previous = std::mem::replace(&mut self.resident, enabled);
        if self.lifecycle == CoreLifecycle::Stopping {
            return None;
        }
        match enabled {
            ResidentEnabled::Enabled => {
                self.prewarm_deadline = None;
                if previous == ResidentEnabled::Disabled && matches!(self.state, State::None) {
                    self.resident_deadline = Some(now);
                }
                match &mut self.state {
                    State::Registered {
                        phase: phase @ Phase::Prewarming,
                        ..
                    } => *phase = Phase::Hidden,
                    State::Starting {
                        intent: intent @ LaunchIntent::Prewarm,
                        ..
                    } => *intent = LaunchIntent::Resident,
                    State::None
                    | State::Starting { .. }
                    | State::Closing { .. }
                    | State::Registered { .. } => {}
                }
                None
            }
            ResidentEnabled::Disabled => {
                self.resident_deadline = None;
                if matches!(
                    self.state,
                    State::Registered {
                        phase: Phase::Hidden,
                        ..
                    }
                ) && !self.opening()
                {
                    self.close_registered()
                } else {
                    None
                }
            }
        }
    }
    fn close_registered(&mut self) -> Option<UiControl> {
        match std::mem::take(&mut self.state) {
            State::Registered {
                process,
                connection,
                ..
            } => {
                let control = UiControl {
                    connection,
                    pid: process.pid(),
                    op: ControlOp::UiShutdown,
                };
                self.state = State::Closing { process };
                self.prewarm_deadline = None;
                Some(control)
            }
            other => {
                self.state = other;
                None
            }
        }
    }
    pub(super) fn ensure_resident<E>(
        &mut self,
        now: Instant,
        launch: impl FnOnce() -> Result<Process, E>,
    ) -> Result<(), E> {
        if self.lifecycle == CoreLifecycle::Running
            && self.resident == ResidentEnabled::Enabled
            && matches!(self.state, State::None)
            && self
                .resident_deadline
                .is_some_and(|deadline| deadline <= now)
        {
            // A failed launch also receives a deadline, preventing repeated attempts in a busy loop.
            self.resident_deadline = Some(now + RESIDENT_RELAUNCH_COOLDOWN);
            self.last_launch = Some(now);
            let process = launch()?;
            self.state = State::Starting {
                process,
                intent: LaunchIntent::Resident,
            };
            self.resident_deadline = None;
        }
        Ok(())
    }
    pub(super) fn begin_shutdown(&mut self) {
        self.lifecycle = CoreLifecycle::Stopping;
        self.resident_deadline = None;
        self.prewarm_deadline = None;
        self.show_requested = DisplayRequest::Idle;
    }
    pub(super) fn resident_wait(&self, now: Instant) -> Option<Duration> {
        self.resident_deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }
    pub(super) fn hidden(&mut self, source: ConnectionId) -> Option<UiControl> {
        if let State::Registered {
            connection,
            phase,
            process,
        } = &mut self.state
            && *connection == source
        {
            *phase = Phase::Hidden;
            self.prewarm_deadline = None;
            if self.show_requested == DisplayRequest::Pending {
                return Some(UiControl {
                    connection: source,
                    pid: process.pid(),
                    op: ControlOp::UiShow,
                });
            }
            if self.resident == ResidentEnabled::Disabled {
                return self.close_registered();
            }
        }
        None
    }
    pub(super) fn prewarm_available(&self, now: Instant, enabled: PrewarmEnabled) -> bool {
        self.resident == ResidentEnabled::Disabled
            && self.lifecycle == CoreLifecycle::Running
            && matches!(self.state, State::None)
            && enabled == PrewarmEnabled::Enabled
            && self
                .last_prewarm
                .is_none_or(|last| now.saturating_duration_since(last) >= PREWARM_COOLDOWN)
    }
    pub(super) fn can_prewarm(
        &self,
        now: Instant,
        settings: PrewarmSettings,
        power: PowerSource,
    ) -> bool {
        self.prewarm_available(now, settings.enabled)
            && (power == PowerSource::Ac || settings.on_battery == PrewarmOnBattery::Allowed)
    }
    pub(super) fn hover<E>(
        &mut self,
        now: Instant,
        settings: PrewarmSettings,
        power: PowerSource,
        launch: impl FnOnce() -> Result<Process, E>,
    ) -> Result<(), E> {
        if self.can_prewarm(now, settings, power) {
            let process = launch()?;
            self.state = State::Starting {
                process,
                intent: LaunchIntent::Prewarm,
            };
            self.last_prewarm = Some(now);
            self.last_launch = Some(now);
        }
        Ok(())
    }
    pub(super) fn prewarm_wait(&self, now: Instant) -> Option<Duration> {
        self.prewarm_deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }
    pub(super) fn expire_prewarm(&mut self, now: Instant) -> Option<UiControl> {
        if self.show_requested == DisplayRequest::Pending
            || self.prewarm_deadline.is_none_or(|deadline| deadline > now)
        {
            return None;
        }
        self.prewarm_deadline = None;
        match std::mem::take(&mut self.state) {
            State::Registered {
                process,
                connection,
                phase: Phase::Prewarming,
            } => {
                let control = UiControl {
                    connection,
                    pid: process.pid(),
                    op: ControlOp::UiShutdown,
                };
                self.state = State::Closing { process };
                Some(control)
            }
            other => {
                self.state = other;
                None
            }
        }
    }

    /// Requests display, launching only when no process is tracked.
    pub(super) fn open<E>(
        &mut self,
        now: Instant,
        launch: impl FnOnce() -> Result<Process, E>,
    ) -> Result<Option<UiControl>, E> {
        self.show_requested = DisplayRequest::Pending;
        self.prewarm_deadline = None;
        match &self.state {
            State::None => match launch() {
                Ok(process) => {
                    self.last_launch = Some(now);
                    self.resident_deadline = None;
                    self.state = State::Starting {
                        process,
                        intent: LaunchIntent::Show,
                    };
                    Ok(None)
                }
                Err(error) => {
                    self.show_requested = DisplayRequest::Idle;
                    if self.resident == ResidentEnabled::Enabled {
                        self.last_launch = Some(now);
                        self.resident_deadline = Some(now + RESIDENT_RELAUNCH_COOLDOWN);
                    }
                    Err(error)
                }
            },
            State::Starting { .. } | State::Closing { .. } => Ok(None),
            State::Registered {
                process,
                connection,
                phase,
            } => Ok(Some(UiControl {
                connection: *connection,
                pid: process.pid(),
                op: match phase {
                    Phase::Starting | Phase::Prewarming | Phase::Hidden => ControlOp::UiShow,
                    Phase::Visible => ControlOp::UiActivate,
                },
            })),
        }
    }
    /// Adopts the first authenticated UI, retaining our child's handle when its PID matches.
    pub(super) fn register<E>(
        &mut self,
        connection: ConnectionId,
        peer: UiPeer,
        now: Instant,
        adopt: impl FnOnce(ProcessId) -> Result<Process, E>,
    ) -> Result<RegisterUi, E> {
        let stopped_resident = matches!(
            self.state,
            State::Starting {
                intent: LaunchIntent::Resident,
                ..
            }
        ) && self.resident == ResidentEnabled::Disabled;
        let process = match std::mem::take(&mut self.state) {
            other @ (State::Registered { .. } | State::Closing { .. }) => {
                self.state = other;
                return Ok(RegisterUi::Ignored);
            }
            State::Starting { process, .. } if process.pid() == peer.pid => process,
            previous => match adopt(peer.pid) {
                Ok(process) => process,
                Err(error) => {
                    self.state = previous;
                    return Err(error);
                }
            },
        };
        let phase =
            if peer.state == HelloState::Prewarming && self.resident == ResidentEnabled::Enabled {
                Phase::Hidden
            } else {
                Phase::from(peer.state)
            };
        if self.last_launch.is_none() {
            self.last_launch = Some(now);
        }
        self.prewarm_deadline =
            if phase == Phase::Prewarming && self.show_requested == DisplayRequest::Idle {
                Some(now + PREWARM_TTL)
            } else {
                None
            };
        // A starting UI creates its own window. Only a hidden or already visible UI needs replay.
        let replay = if self.show_requested == DisplayRequest::Pending {
            match phase {
                Phase::Starting => None,
                Phase::Prewarming | Phase::Hidden | Phase::Visible => Some(UiControl {
                    connection,
                    pid: peer.pid,
                    op: if phase == Phase::Visible {
                        ControlOp::UiActivate
                    } else {
                        ControlOp::UiShow
                    },
                }),
            }
        } else {
            None
        };
        self.state = State::Registered {
            process,
            connection,
            phase,
        };
        let replay = if stopped_resident && !self.opening() {
            self.close_registered()
        } else {
            replay
        };
        Ok(RegisterUi::Accepted { replay })
    }
    pub(super) fn process(&self) -> Option<&Process> {
        match &self.state {
            State::None => None,
            State::Starting { process, .. }
            | State::Closing { process }
            | State::Registered { process, .. } => Some(process),
        }
    }
    pub(super) fn opening(&self) -> bool {
        self.show_requested == DisplayRequest::Pending
    }
    pub(super) fn first_frame(&mut self, source: ConnectionId) {
        if let State::Registered {
            connection, phase, ..
        } = &mut self.state
            && *connection == source
            && (*phase != Phase::Hidden || self.show_requested == DisplayRequest::Pending)
        {
            *phase = Phase::Visible;
            self.prewarm_deadline = None;
            self.show_requested = DisplayRequest::Idle;
        }
    }
    pub(super) fn unregister(&mut self, source: ConnectionId) -> ExitUi {
        match std::mem::take(&mut self.state) {
            State::Registered {
                process,
                connection,
                ..
            } if connection == source => {
                self.state = State::Closing { process };
                self.prewarm_deadline = None;
                match std::mem::take(&mut self.show_requested) {
                    DisplayRequest::Idle => ExitUi::Closed,
                    DisplayRequest::Pending => ExitUi::PendingShowLost,
                }
            }
            other => {
                self.state = other;
                ExitUi::Unrelated
            }
        }
    }
    pub(super) fn exited(&mut self, now: Instant) -> ExitUi {
        self.resident_deadline = if self.lifecycle == CoreLifecycle::Running
            && self.resident == ResidentEnabled::Enabled
        {
            Some(
                self.last_launch
                    .map_or(now, |last| (last + RESIDENT_RELAUNCH_COOLDOWN).max(now)),
            )
        } else {
            None
        };
        let previous = std::mem::take(&mut self.state);
        self.prewarm_deadline = None;
        let request = std::mem::take(&mut self.show_requested);
        match previous {
            State::Closing { .. } => match request {
                DisplayRequest::Idle => ExitUi::Closed,
                DisplayRequest::Pending => ExitUi::OpenRequested,
            },
            State::None | State::Starting { .. } | State::Registered { .. } => match request {
                DisplayRequest::Idle => ExitUi::Closed,
                DisplayRequest::Pending => ExitUi::PendingShowLost,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    const CHILD_PID: ProcessId = ProcessId::new(10);
    const EXTERNAL_PID: ProcessId = ProcessId::new(20);
    const CONNECTION: ConnectionId = ConnectionId::for_test(1);
    const OTHER: ConnectionId = ConnectionId::for_test(2);
    const PHASES: [HelloState; 3] = [
        HelloState::Starting,
        HelloState::Prewarming,
        HelloState::Visible,
    ];
    const REQUESTS: [DisplayRequest; 2] = [DisplayRequest::Idle, DisplayRequest::Pending];

    #[derive(Debug, PartialEq, Eq)]
    struct Process {
        pid: ProcessId,
    }
    impl UiProcess for Process {
        fn pid(&self) -> ProcessId {
            self.pid
        }
    }
    fn launch() -> Result<Process, &'static str> {
        Ok(Process { pid: CHILD_PID })
    }
    fn adopt(pid: ProcessId) -> Result<Process, &'static str> {
        Ok(Process { pid })
    }
    fn peer(state: HelloState) -> UiPeer {
        UiPeer {
            pid: CHILD_PID,
            state,
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Setup {
        None,
        Starting,
        Registered { phase: HelloState },
        Closing,
    }
    impl Setup {
        fn ui(self, request: DisplayRequest) -> UiState<Process> {
            let state = match self {
                Self::None => State::None,
                Self::Starting => State::Starting {
                    process: Process { pid: CHILD_PID },
                    intent: LaunchIntent::Show,
                },
                Self::Registered { phase } => State::Registered {
                    process: Process { pid: CHILD_PID },
                    connection: CONNECTION,
                    phase: phase.into(),
                },
                Self::Closing => State::Closing {
                    process: Process { pid: CHILD_PID },
                },
            };
            UiState {
                state,
                show_requested: request,
                prewarm_deadline: None,
                last_prewarm: None,
                ..UiState::default()
            }
        }
    }
    const SETUPS: [Setup; 6] = [
        Setup::None,
        Setup::Starting,
        Setup::Registered {
            phase: HelloState::Starting,
        },
        Setup::Registered {
            phase: HelloState::Prewarming,
        },
        Setup::Registered {
            phase: HelloState::Visible,
        },
        Setup::Closing,
    ];

    #[test]
    fn open_from_each_state_launches_waits_or_routes() {
        for setup in SETUPS {
            let mut ui = setup.ui(DisplayRequest::Idle);
            let mut launches = 0;
            let control = ui
                .open(Instant::now(), || {
                    launches += 1;
                    launch()
                })
                .expect("open");
            let expected = match setup {
                Setup::None => {
                    assert_eq!(launches, 1);
                    assert!(matches!(ui.state, State::Starting { .. }));
                    None
                }
                Setup::Starting | Setup::Closing => {
                    assert_eq!(launches, 0);
                    None
                }
                Setup::Registered { phase } => {
                    assert_eq!(launches, 0);
                    Some(UiControl {
                        connection: CONNECTION,
                        pid: CHILD_PID,
                        op: match phase {
                            HelloState::Starting | HelloState::Prewarming => ControlOp::UiShow,
                            HelloState::Visible => ControlOp::UiActivate,
                        },
                    })
                }
            };
            assert_eq!(control, expected, "{setup:?}");
            assert_eq!(ui.show_requested, DisplayRequest::Pending, "{setup:?}");
            assert!(ui.opening());
        }
    }

    #[test]
    fn hello_mapping_replay_and_adoption_are_table_driven() {
        for phase in PHASES {
            for request in REQUESTS {
                for pid in [CHILD_PID, EXTERNAL_PID] {
                    let mut ui = match request {
                        DisplayRequest::Idle => Setup::None,
                        DisplayRequest::Pending => Setup::Starting,
                    }
                    .ui(request);
                    let mut adoptions = 0;
                    let outcome = ui
                        .register(
                            CONNECTION,
                            UiPeer { pid, state: phase },
                            Instant::now(),
                            |observed| {
                                adoptions += 1;
                                assert_eq!(observed, pid);
                                adopt(observed)
                            },
                        )
                        .expect("register");
                    assert_eq!(
                        adoptions,
                        if request == DisplayRequest::Pending && pid == CHILD_PID {
                            0
                        } else {
                            1
                        }
                    );
                    let replay = match request {
                        DisplayRequest::Idle => None,
                        DisplayRequest::Pending => match phase {
                            HelloState::Starting => None,
                            HelloState::Prewarming | HelloState::Visible => Some(UiControl {
                                connection: CONNECTION,
                                pid,
                                op: if phase == HelloState::Visible {
                                    ControlOp::UiActivate
                                } else {
                                    ControlOp::UiShow
                                },
                            }),
                        },
                    };
                    assert_eq!(outcome, RegisterUi::Accepted { replay });
                    assert_eq!(ui.process().expect("tracked process").pid(), pid);
                    assert!(
                        matches!(ui.state, State::Registered { phase: registered, .. } if registered == Phase::from(phase))
                    );
                    assert_eq!(
                        ui.show_requested, request,
                        "hello alone does not complete display"
                    );
                    ui.first_frame(OTHER);
                    assert_eq!(ui.show_requested, request);
                    ui.first_frame(CONNECTION);
                    assert_eq!(ui.show_requested, DisplayRequest::Idle);
                    assert!(matches!(
                        ui.state,
                        State::Registered {
                            phase: Phase::Visible,
                            ..
                        }
                    ));
                }
            }
        }
    }

    #[test]
    fn registration_and_first_frame_cannot_replace_a_registered_or_closing_process() {
        for setup in SETUPS {
            if !matches!(setup, Setup::Registered { .. } | Setup::Closing) {
                continue;
            }
            for request in REQUESTS {
                for pid in [CHILD_PID, EXTERNAL_PID] {
                    let mut ui = setup.ui(request);
                    assert_eq!(
                        ui.register(
                            OTHER,
                            UiPeer {
                                pid,
                                state: HelloState::Starting
                            },
                            Instant::now(),
                            |_| panic!("tracked UI must not be adopted again")
                        ),
                        Ok::<_, &str>(RegisterUi::Ignored)
                    );
                    assert_eq!(ui, setup.ui(request));
                    ui.first_frame(OTHER);
                    assert_eq!(ui, setup.ui(request));
                    if matches!(setup, Setup::Closing) {
                        ui.first_frame(CONNECTION);
                        assert_eq!(
                            ui,
                            setup.ui(request),
                            "late frames must not clear a new request while closing"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn exit_from_each_state_requests_replacement_only_for_a_new_open_while_closing() {
        for setup in SETUPS {
            for request in REQUESTS {
                if matches!(setup, Setup::None) && request == DisplayRequest::Pending {
                    continue;
                }
                let mut ui = setup.ui(request);
                let expected = match request {
                    DisplayRequest::Idle => ExitUi::Closed,
                    DisplayRequest::Pending => match setup {
                        Setup::Closing => ExitUi::OpenRequested,
                        Setup::None | Setup::Starting | Setup::Registered { .. } => {
                            ExitUi::PendingShowLost
                        }
                    },
                };
                assert_eq!(
                    ui.exited(Instant::now()),
                    expected,
                    "{setup:?}, {request:?}"
                );
                assert_eq!(ui, UiState::default());
                ui.first_frame(CONNECTION);
                assert_eq!(ui, UiState::default());
            }
        }
    }

    #[derive(Debug)]
    struct TrackedProcess {
        pid: ProcessId,
        releases: Rc<Cell<usize>>,
    }
    impl UiProcess for TrackedProcess {
        fn pid(&self) -> ProcessId {
            self.pid
        }
    }
    impl Drop for TrackedProcess {
        fn drop(&mut self) {
            self.releases.set(self.releases.get() + 1);
        }
    }
    #[test]
    fn disconnect_retains_the_handle_until_exit_and_coalesces_new_opens_into_one_launch() {
        let releases = Rc::new(Cell::new(0));
        let mut ui = UiState::default();
        ui.open(Instant::now(), || {
            Ok::<_, &str>(TrackedProcess {
                pid: CHILD_PID,
                releases: releases.clone(),
            })
        })
        .expect("launch");
        ui.register::<&str>(
            CONNECTION,
            peer(HelloState::Starting),
            Instant::now(),
            |_| panic!("child handle retained"),
        )
        .expect("register");
        assert_eq!(ui.unregister(CONNECTION), ExitUi::PendingShowLost);
        assert_eq!(
            releases.get(),
            0,
            "disconnect must retain the process handle"
        );
        assert!(!ui.opening(), "old request cleared");
        for _ in 0..3 {
            assert_eq!(
                ui.open::<&str>(Instant::now(), || panic!("old UI still owns its mutex")),
                Ok(None)
            );
        }
        assert_eq!(ui.unregister(CONNECTION), ExitUi::Unrelated);
        ui.first_frame(CONNECTION);
        assert!(ui.opening(), "late notices must preserve the new request");
        assert_eq!(releases.get(), 0);
        assert_eq!(ui.exited(Instant::now()), ExitUi::OpenRequested);
        assert_eq!(releases.get(), 1, "exit releases the old process handle");
        let mut launches = 0;
        ui.open(Instant::now(), || {
            launches += 1;
            Ok::<_, &str>(TrackedProcess {
                pid: EXTERNAL_PID,
                releases: releases.clone(),
            })
        })
        .expect("replacement launch");
        ui.open::<&str>(Instant::now(), || panic!("replacement is already starting"))
            .expect("repeated open");
        assert_eq!(launches, 1);
        assert_eq!(
            ui.process().expect("replacement tracked").pid(),
            EXTERNAL_PID
        );
        assert_eq!(
            ui.exited(Instant::now()),
            ExitUi::PendingShowLost,
            "replacement failure must not restart again"
        );
        assert_eq!(releases.get(), 2);
    }

    #[test]
    fn launch_and_adoption_failures_preserve_retry_and_pending_child() {
        let mut ui = UiState::default();
        assert_eq!(
            ui.open(Instant::now(), || Err::<Process, _>("launch failed")),
            Err("launch failed")
        );
        assert_eq!(ui, UiState::default());
        ui.open(Instant::now(), launch).expect("retry");
        assert_eq!(
            ui.register(
                CONNECTION,
                UiPeer {
                    pid: EXTERNAL_PID,
                    state: HelloState::Starting
                },
                Instant::now(),
                |_| Err("adopt failed")
            ),
            Err("adopt failed")
        );
        assert_eq!(ui.state, Setup::Starting.ui(DisplayRequest::Pending).state);
        assert!(ui.opening());
        ui.first_frame(CONNECTION);
        assert_eq!(ui.state, Setup::Starting.ui(DisplayRequest::Pending).state);
        assert!(ui.opening());
        let mut closing = Setup::Closing.ui(DisplayRequest::Pending);
        assert_eq!(closing.exited(Instant::now()), ExitUi::OpenRequested);
        assert_eq!(
            closing.open(Instant::now(), || Err::<Process, _>("replacement failed")),
            Err("replacement failed")
        );
        assert_eq!(closing, UiState::default());
        closing
            .open(Instant::now(), launch)
            .expect("user retries failed replacement");
    }
    #[derive(Debug)]
    struct HoverCase {
        settings: PrewarmSettings,
        power: PowerSource,
        launches: usize,
    }
    #[test]
    fn hover_respects_settings_and_battery_policy() {
        let now = Instant::now();
        for case in [
            HoverCase {
                settings: PrewarmSettings::default(),
                power: PowerSource::Ac,
                launches: 1,
            },
            HoverCase {
                settings: PrewarmSettings::default(),
                power: PowerSource::Battery,
                launches: 0,
            },
            HoverCase {
                settings: PrewarmSettings {
                    on_battery: PrewarmOnBattery::Allowed,
                    ..Default::default()
                },
                power: PowerSource::Battery,
                launches: 1,
            },
            HoverCase {
                settings: PrewarmSettings {
                    enabled: PrewarmEnabled::Disabled,
                    ..Default::default()
                },
                power: PowerSource::Ac,
                launches: 0,
            },
            HoverCase {
                settings: PrewarmSettings {
                    enabled: PrewarmEnabled::Disabled,
                    on_battery: PrewarmOnBattery::Allowed,
                },
                power: PowerSource::Battery,
                launches: 0,
            },
        ] {
            let mut ui = UiState::default();
            let mut launches = 0;
            ui.hover(now, case.settings, case.power, || {
                launches += 1;
                launch()
            })
            .expect("hover");
            assert_eq!(launches, case.launches, "{case:?}");
        }
    }
    #[test]
    fn ttl_starts_at_hello_targets_the_hidden_ui_and_cooldown_survives_exit() {
        let now = Instant::now();
        let handshake = now + Duration::from_secs(2);
        let mut ui = UiState::default();
        ui.hover(now, PrewarmSettings::default(), PowerSource::Ac, launch)
            .expect("prewarm launch");
        assert_eq!(ui.prewarm_wait(now), None);
        assert_eq!(
            ui.register(
                CONNECTION,
                peer(HelloState::Prewarming),
                handshake,
                |_| panic!("retain child")
            ),
            Ok::<_, &str>(RegisterUi::Accepted { replay: None })
        );
        assert_eq!(ui.prewarm_wait(handshake), Some(PREWARM_TTL));
        assert_eq!(
            ui.expire_prewarm(handshake + PREWARM_TTL - Duration::from_millis(1)),
            None
        );
        assert_eq!(
            ui.expire_prewarm(handshake + PREWARM_TTL),
            Some(UiControl {
                connection: CONNECTION,
                pid: CHILD_PID,
                op: ControlOp::UiShutdown
            })
        );
        assert!(matches!(ui.state, State::Closing { .. }));
        assert!(ui.process().is_some(), "retain handle until actual exit");
        assert_eq!(ui.prewarm_wait(handshake + PREWARM_TTL), None);
        assert_eq!(
            ui.expire_prewarm(handshake + PREWARM_TTL),
            None,
            "shutdown is emitted once"
        );
        assert_eq!(
            ui.exited(Instant::now()),
            ExitUi::Closed,
            "prewarm expiration never relaunches"
        );
        assert!(ui.process().is_none());
        ui.hover::<&str>(
            now + PREWARM_COOLDOWN - Duration::from_millis(1),
            PrewarmSettings::default(),
            PowerSource::Ac,
            || panic!("cooldown blocks"),
        )
        .expect("skip");
        assert!(!ui.can_prewarm(now, PrewarmSettings::default(), PowerSource::Ac));
        ui.hover(
            now + PREWARM_COOLDOWN,
            PrewarmSettings::default(),
            PowerSource::Ac,
            launch,
        )
        .expect("next hover");
        assert!(matches!(
            ui.state,
            State::Starting {
                intent: LaunchIntent::Prewarm,
                ..
            }
        ));
    }
    #[test]
    fn open_after_handshake_wins_at_the_deadline_and_late_click_waits_for_shutdown_exit() {
        let now = Instant::now();
        let mut ui = UiState::default();
        ui.register(CONNECTION, peer(HelloState::Prewarming), now, adopt)
            .expect("adopt");
        assert_eq!(
            ui.open::<&str>(Instant::now(), || panic!("already registered")),
            Ok(Some(UiControl {
                connection: CONNECTION,
                pid: CHILD_PID,
                op: ControlOp::UiShow
            }))
        );
        assert_eq!(
            ui.expire_prewarm(now + PREWARM_TTL),
            None,
            "the main loop handles input before expiry"
        );
        assert!(ui.opening());
        ui.first_frame(CONNECTION);
        assert_eq!(ui.prewarm_wait(now), None);
        assert_eq!(
            ui.expire_prewarm(now + PREWARM_TTL),
            None,
            "visible UI cannot expire"
        );
        let mut expired = UiState::default();
        expired
            .register(CONNECTION, peer(HelloState::Prewarming), now, adopt)
            .expect("adopt");
        assert!(expired.expire_prewarm(now + PREWARM_TTL).is_some());
        expired
            .open::<&str>(Instant::now(), || panic!("wait for mutex release"))
            .expect("remember late click");
        assert_eq!(expired.unregister(CONNECTION), ExitUi::Unrelated);
        assert_eq!(expired.exited(Instant::now()), ExitUi::OpenRequested);
    }
    #[test]
    fn resident_hidden_show_first_frame_and_close_have_no_ttl() {
        let now = Instant::now();
        let mut ui = UiState::default();
        ui.configure_resident(ResidentEnabled::Enabled, now);
        ui.ensure_resident::<&str>(now, launch)
            .expect("resident launch");
        assert_eq!(
            ui.register(CONNECTION, peer(HelloState::Prewarming), now, adopt),
            Ok(RegisterUi::Accepted { replay: None })
        );
        assert!(matches!(
            ui.state,
            State::Registered {
                phase: Phase::Hidden,
                ..
            }
        ));
        assert_eq!(ui.prewarm_wait(now), None);
        assert_eq!(ui.expire_prewarm(now + PREWARM_TTL), None);
        // A frame already queued before a close must not make a hidden UI visible.
        ui.first_frame(CONNECTION);
        assert!(matches!(
            ui.state,
            State::Registered {
                phase: Phase::Hidden,
                ..
            }
        ));
        let control = ui
            .open(Instant::now(), launch)
            .expect("open")
            .expect("show hidden");
        assert_eq!(control.op, ControlOp::UiShow);
        assert!(ui.opening());
        ui.first_frame(CONNECTION);
        assert!(matches!(
            ui.state,
            State::Registered {
                phase: Phase::Visible,
                ..
            }
        ));
        assert!(!ui.opening());
        assert_eq!(ui.hidden(OTHER), None);
        assert!(matches!(
            ui.state,
            State::Registered {
                phase: Phase::Visible,
                ..
            }
        ));
        assert_eq!(ui.hidden(CONNECTION), None);
        assert!(matches!(
            ui.state,
            State::Registered {
                phase: Phase::Hidden,
                ..
            }
        ));
        assert_eq!(ui.expire_prewarm(now + PREWARM_COOLDOWN), None);
    }
    #[test]
    fn resident_open_before_hello_and_during_close_is_replayed() {
        let now = Instant::now();
        let mut ui = UiState::default();
        ui.configure_resident(ResidentEnabled::Enabled, now);
        ui.ensure_resident::<&str>(now, launch).expect("launch");
        assert_eq!(ui.open(Instant::now(), launch), Ok(None));
        assert_eq!(
            ui.register(CONNECTION, peer(HelloState::Prewarming), now, adopt),
            Ok(RegisterUi::Accepted {
                replay: Some(UiControl {
                    connection: CONNECTION,
                    pid: CHILD_PID,
                    op: ControlOp::UiShow
                })
            })
        );
        assert_eq!(
            ui.hidden(CONNECTION).expect("open wins close").op,
            ControlOp::UiShow
        );
        ui.first_frame(CONNECTION);
        assert!(!ui.opening());
    }
    #[test]
    fn resident_toggles_cover_each_phase_and_wait_for_exiting_processes() {
        let now = Instant::now();
        for setup in SETUPS {
            for request in REQUESTS {
                let mut ui = setup.ui(request);
                ui.configure_resident(ResidentEnabled::Enabled, now);
                let control = ui.configure_resident(ResidentEnabled::Disabled, now);
                let hidden_idle = matches!(
                    setup,
                    Setup::Registered {
                        phase: HelloState::Prewarming
                    }
                ) && request == DisplayRequest::Idle;
                assert_eq!(control.is_some(), hidden_idle, "{setup:?} {request:?}");
                if hidden_idle {
                    assert!(matches!(ui.state, State::Closing { .. }));
                }
                ui.configure_resident(ResidentEnabled::Enabled, now);
                let mut launches = 0;
                ui.ensure_resident::<&str>(now, || {
                    launches += 1;
                    launch()
                })
                .expect("toggle launch");
                assert_eq!(launches, usize::from(matches!(setup, Setup::None)));
            }
        }
        let mut ui = UiState::default();
        ui.configure_resident(ResidentEnabled::Enabled, now);
        ui.ensure_resident::<&str>(now, launch).expect("launch");
        ui.configure_resident(ResidentEnabled::Disabled, now);
        let replay = ui
            .register(CONNECTION, peer(HelloState::Prewarming), now, adopt)
            .expect("hello");
        assert_eq!(
            replay,
            RegisterUi::Accepted {
                replay: Some(UiControl {
                    connection: CONNECTION,
                    pid: CHILD_PID,
                    op: ControlOp::UiShutdown
                })
            }
        );
        assert!(matches!(ui.state, State::Closing { .. }));
        let mut visible = Setup::Registered {
            phase: HelloState::Visible,
        }
        .ui(DisplayRequest::Idle);
        visible.configure_resident(ResidentEnabled::Enabled, now);
        assert_eq!(
            visible.configure_resident(ResidentEnabled::Disabled, now),
            None
        );
        assert_eq!(
            visible.hidden(CONNECTION).expect("off on last close").op,
            ControlOp::UiShutdown
        );
    }
    #[test]
    fn resident_crashes_back_off_from_launch_and_shutdown_never_restarts() {
        let now = Instant::now();
        let mut ui = UiState::default();
        ui.configure_resident(ResidentEnabled::Enabled, now);
        ui.ensure_resident::<&str>(now, launch).expect("launch");
        let crash = now + Duration::from_secs(1);
        assert_eq!(ui.exited(crash), ExitUi::Closed);
        assert_eq!(
            ui.resident_wait(crash),
            Some(RESIDENT_RELAUNCH_COOLDOWN - Duration::from_secs(1))
        );
        ui.ensure_resident::<&str>(
            now + RESIDENT_RELAUNCH_COOLDOWN - Duration::from_nanos(1),
            || panic!("early relaunch"),
        )
        .expect("wait");
        ui.ensure_resident::<&str>(now + RESIDENT_RELAUNCH_COOLDOWN, launch)
            .expect("relaunch");
        assert!(matches!(
            ui.state,
            State::Starting {
                intent: LaunchIntent::Resident,
                ..
            }
        ));
        ui.begin_shutdown();
        ui.exited(now + RESIDENT_RELAUNCH_COOLDOWN);
        assert_eq!(ui.resident_wait(now), None);
        ui.ensure_resident::<&str>(
            now + RESIDENT_RELAUNCH_COOLDOWN + RESIDENT_RELAUNCH_COOLDOWN,
            || panic!("shutdown relaunch"),
        )
        .expect("stopped");
    }
}
