//! Main-thread UI ownership and display decisions; all transport delivery is non-blocking.

use crate::{
    ipc::registry::{ConnectionId, UiPeer},
    process_id::ProcessId,
};
use quartz_protocol::{ControlOp, UiState as HelloState};

pub(super) trait UiProcess {
    fn pid(&self) -> ProcessId;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Starting,
    Prewarming,
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
}
impl<Process> Default for UiState<Process> {
    fn default() -> Self {
        Self {
            state: State::None,
            show_requested: DisplayRequest::Idle,
        }
    }
}
impl<Process: UiProcess> UiState<Process> {
    /// Requests display, launching only when no process is tracked.
    pub(super) fn open<E>(
        &mut self,
        launch: impl FnOnce() -> Result<Process, E>,
    ) -> Result<Option<UiControl>, E> {
        self.show_requested = DisplayRequest::Pending;
        match &self.state {
            State::None => match launch() {
                Ok(process) => {
                    self.state = State::Starting { process };
                    Ok(None)
                }
                Err(error) => {
                    self.show_requested = DisplayRequest::Idle;
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
                    Phase::Starting | Phase::Prewarming => ControlOp::UiShow,
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
        adopt: impl FnOnce(ProcessId) -> Result<Process, E>,
    ) -> Result<RegisterUi, E> {
        let process = match std::mem::take(&mut self.state) {
            other @ (State::Registered { .. } | State::Closing { .. }) => {
                self.state = other;
                return Ok(RegisterUi::Ignored);
            }
            State::Starting { process } if process.pid() == peer.pid => process,
            previous => match adopt(peer.pid) {
                Ok(process) => process,
                Err(error) => {
                    self.state = previous;
                    return Err(error);
                }
            },
        };
        let phase = Phase::from(peer.state);
        // A starting UI creates its own window. Only a hidden or already visible UI needs replay.
        let replay = if self.show_requested == DisplayRequest::Pending {
            match phase {
                Phase::Starting => None,
                Phase::Prewarming | Phase::Visible => Some(UiControl {
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
        Ok(RegisterUi::Accepted { replay })
    }
    pub(super) fn process(&self) -> Option<&Process> {
        match &self.state {
            State::None => None,
            State::Starting { process }
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
        {
            *phase = Phase::Visible;
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
    pub(super) fn exited(&mut self) -> ExitUi {
        let previous = std::mem::take(&mut self.state);
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
                .open(|| {
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
                        .register(CONNECTION, UiPeer { pid, state: phase }, |observed| {
                            adoptions += 1;
                            assert_eq!(observed, pid);
                            adopt(observed)
                        })
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
    fn unregistration_retains_the_process_and_clears_only_its_old_display_request() {
        for setup in SETUPS {
            for request in REQUESTS {
                if matches!(setup, Setup::None) && request == DisplayRequest::Pending {
                    continue;
                }
                for source in [CONNECTION, OTHER] {
                    let mut ui = setup.ui(request);
                    let outcome = ui.unregister(source);
                    if matches!(setup, Setup::Registered { .. }) && source == CONNECTION {
                        assert_eq!(ui, Setup::Closing.ui(DisplayRequest::Idle));
                        assert_eq!(
                            outcome,
                            match request {
                                DisplayRequest::Idle => ExitUi::Closed,
                                DisplayRequest::Pending => ExitUi::PendingShowLost,
                            }
                        );
                    } else {
                        assert_eq!(outcome, ExitUi::Unrelated, "{setup:?}");
                        assert_eq!(
                            ui,
                            setup.ui(request),
                            "unrelated or repeated unregister preserves the request"
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
                assert_eq!(ui.exited(), expected, "{setup:?}, {request:?}");
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
        ui.open(|| {
            Ok::<_, &str>(TrackedProcess {
                pid: CHILD_PID,
                releases: releases.clone(),
            })
        })
        .expect("launch");
        ui.register::<&str>(CONNECTION, peer(HelloState::Starting), |_| {
            panic!("child handle retained")
        })
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
                ui.open::<&str>(|| panic!("old UI still owns its mutex")),
                Ok(None)
            );
        }
        assert_eq!(ui.unregister(CONNECTION), ExitUi::Unrelated);
        ui.first_frame(CONNECTION);
        assert!(ui.opening(), "late notices must preserve the new request");
        assert_eq!(releases.get(), 0);
        assert_eq!(ui.exited(), ExitUi::OpenRequested);
        assert_eq!(releases.get(), 1, "exit releases the old process handle");
        let mut launches = 0;
        ui.open(|| {
            launches += 1;
            Ok::<_, &str>(TrackedProcess {
                pid: EXTERNAL_PID,
                releases: releases.clone(),
            })
        })
        .expect("replacement launch");
        ui.open::<&str>(|| panic!("replacement is already starting"))
            .expect("repeated open");
        assert_eq!(launches, 1);
        assert_eq!(
            ui.process().expect("replacement tracked").pid(),
            EXTERNAL_PID
        );
        assert_eq!(
            ui.exited(),
            ExitUi::PendingShowLost,
            "replacement failure must not restart again"
        );
        assert_eq!(releases.get(), 2);
    }

    #[test]
    fn launch_and_adoption_failures_preserve_retry_and_pending_child() {
        let mut ui = UiState::default();
        assert_eq!(
            ui.open(|| Err::<Process, _>("launch failed")),
            Err("launch failed")
        );
        assert_eq!(ui, UiState::default());
        ui.open(launch).expect("retry");
        assert_eq!(
            ui.register(
                CONNECTION,
                UiPeer {
                    pid: EXTERNAL_PID,
                    state: HelloState::Starting
                },
                |_| Err("adopt failed")
            ),
            Err("adopt failed")
        );
        assert_eq!(ui, Setup::Starting.ui(DisplayRequest::Pending));
        ui.first_frame(CONNECTION);
        assert_eq!(ui, Setup::Starting.ui(DisplayRequest::Pending));
        let mut closing = Setup::Closing.ui(DisplayRequest::Pending);
        assert_eq!(closing.exited(), ExitUi::OpenRequested);
        assert_eq!(
            closing.open(|| Err::<Process, _>("replacement failed")),
            Err("replacement failed")
        );
        assert_eq!(closing, UiState::default());
        closing
            .open(launch)
            .expect("user retries failed replacement");
    }
}
