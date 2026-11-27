use super::*;

#[test]
fn transport_disconnect_and_shutdown_map_to_application_exit() {
    assert!(LinkEvent::Disconnected.exits_ui());
    assert!(LinkEvent::Control(ControlOp::UiShutdown).exits_ui());
    assert!(!LinkEvent::Control(ControlOp::UiShow).exits_ui());
    assert!(!LinkEvent::Connection(ConnectionState::Connecting).exits_ui());
}

#[test]
fn show_and_activate_select_window_restore_activation_and_frame_acknowledgement() {
    for op in [ControlOp::UiShow, ControlOp::UiActivate] {
        assert_eq!(
            LinkEvent::Control(op).window_action(),
            WindowAction::Activate
        );
    }
    assert_eq!(
        LinkEvent::Control(ControlOp::UiShutdown).window_action(),
        WindowAction::Quit
    );
    assert_eq!(
        LinkEvent::Connection(ConnectionState::Connecting).window_action(),
        WindowAction::Observe
    );
}

#[cfg(windows)]
#[tokio::test]
async fn closed_bridge_reports_a_typed_start_error() {
    let (commands, receive) = mpsc::channel(BRIDGE_CAPACITY);
    drop(receive);
    let link = Link { commands };
    assert!(matches!(
        link.start_core().await,
        Err(StartCoreError::Closed)
    ));
    assert!(matches!(link.retry().await, Err(RequestCoreError::Closed)));
}

#[cfg(windows)]
mod pipe {
    use super::*;
    use crate::failure::{FailureKind, Severity};
    use quartz_pipe::{
        CONNECT_TIMEOUT, CORE_START_TIMEOUT, OpenCorePipeError, PipeClient, PipeName,
        RESPONSE_TIMEOUT,
        test_util::{TestPeer, TestPipeServer},
    };
    use quartz_protocol::{
        Envelope, Event, EventSeq, Hello, HelloRole, Progress, ProtocolError, Request, RequestId,
        Response, ResponseOutcome, Subscribe, UiState,
    };
    use std::time::Duration;
    use tokio::task::JoinHandle;

    struct Harness {
        link: Link,
        events: LinkEvents,
        worker: JoinHandle<()>,
    }
    impl Harness {
        fn start(name: PipeName) -> Self {
            Self::with_initial(name, InitialConnection::Connect, None)
        }
        fn with_initial(
            name: PipeName,
            initial: InitialConnection,
            launcher: Option<oneshot::Sender<()>>,
        ) -> Self {
            let (commands, receive) = mpsc::channel(BRIDGE_CAPACITY);
            let (send, events) = mpsc::channel(BRIDGE_CAPACITY);
            let mut worker = worker::Worker::new(receive, send, initial);
            worker.name = Some(name);
            worker.launcher = launcher;
            Self {
                link: Link { commands },
                events: LinkEvents { receiver: events },
                worker: tokio::spawn(worker.run()),
            }
        }
        async fn connecting(&mut self) {
            assert!(matches!(
                self.events.recv().await,
                Some(LinkEvent::Connection(ConnectionState::Connecting))
            ));
        }
        async fn failed(&mut self, kind: FailureKind, severity: Severity) {
            let Some(LinkEvent::Connection(ConnectionState::Failed(failure))) =
                self.events.recv().await
            else {
                panic!("expected a typed initial failure");
            };
            assert_eq!(failure.kind(), kind);
            assert_eq!(failure.severity(), severity);
        }
        async fn connected(&mut self) {
            let Some(LinkEvent::Connection(ConnectionState::Connected {
                instance_id,
                core_version,
            })) = self.events.recv().await
            else {
                panic!("expected connected");
            };
            assert_eq!(instance_id.0, "test-start");
            assert_eq!(core_version.0, "1.0");
        }
        async fn no_automatic_retry(&mut self) {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(3600)).await;
            tokio::task::yield_now().await;
            assert!(
                self.events.receiver.try_recv().is_err(),
                "failure must wait for a user action"
            );
            assert!(
                !self.worker.is_finished(),
                "failed connection stays available for actions"
            );
            tokio::time::resume();
        }
        async fn stop(self) {
            drop(self.events);
            self.worker.await.expect("worker exits when view closes");
        }
    }
    struct Core {
        peer: TestPeer,
    }
    impl Core {
        async fn accept(listener: TestPipeServer) -> Self {
            Self {
                peer: listener.accept().await.expect("accept test UI"),
            }
        }
        async fn request(&mut self, op: Op) -> Request {
            let Envelope::Request(request) = self.peer.receive().await.expect("request") else {
                panic!("request envelope");
            };
            assert_eq!(request.op, op);
            request
        }
        async fn ok(&mut self, request: Request, body: JsonValue) {
            self.peer.respond(request.id, body).await.expect("response");
        }
        async fn reject(&mut self, request: Request, kind: quartz_protocol::ErrorKind) {
            self.peer
                .send(Envelope::Response(Response {
                    id: Some(request.id),
                    outcome: ResponseOutcome::Err {
                        error: ProtocolError::new(kind),
                    },
                }))
                .await
                .expect("rejection");
        }
        async fn handshake(&mut self) {
            self.handshake_phase(UiState::Starting).await;
        }
        async fn handshake_phase(&mut self, state: UiState) {
            let request = self.request(Op::Hello).await;
            let hello: Hello = serde_json::from_value(request.body.clone()).expect("UI hello");
            assert_eq!(
                hello.role,
                HelloRole::Ui {
                    ui: quartz_protocol::UiHello { state }
                }
            );
            self.ok(
                request,
                serde_json::json!({"proto":1,"core_version":"1.0","instance_id":"test-start"}),
            )
            .await;
            self.subscribe(vec![Topic::State]).await;
        }
        async fn subscribe(&mut self, topics: Vec<Topic>) {
            let request = self.request(Op::Subscribe).await;
            assert_eq!(
                serde_json::from_value::<Subscribe>(request.body.clone())
                    .expect("topics")
                    .topics,
                topics
            );
            self.ok(request, serde_json::json!({})).await;
        }
        async fn event(&mut self, topic: Topic, body: JsonValue) {
            self.peer
                .send(Envelope::Event(Event {
                    topic,
                    seq: EventSeq(1),
                    body,
                }))
                .await
                .expect("event");
        }
        async fn progress(&mut self, id: RequestId, done: u32, total: u32) {
            self.peer
                .send(Envelope::Progress(Progress { id, done, total }))
                .await
                .expect("progress");
        }
        fn snapshot() -> JsonValue {
            serde_json::json!({"logical_date":"2026-11-21","foreground":{"app":{"kind":"exe","value":"test.exe"},"title":null,"interval_start_utc_ms":123},"timed_policies":{"pause_until_utc_ms":null,"privacy_mode":null,"keep_timing_until_utc_ms":null},"recording":true,"stop_reason":null,"browser_streams":[],"ui_language":"en","locale":null})
        }
    }
    fn absent_name() -> PipeName {
        PipeName::for_test(windows::core::GUID::new().expect("isolated pipe"))
    }

    #[tokio::test]
    async fn absent_core_waits_for_an_explicit_retry_and_each_retry_attempts_once() {
        let name = absent_name();
        let mut ui = Harness::start(name.clone());
        ui.connecting().await;
        ui.failed(FailureKind::CoreNotRunning, Severity::Recoverable)
            .await;
        ui.no_automatic_retry().await;
        let link = ui.link.clone();
        let retry = tokio::spawn(async move { link.retry().await });
        ui.connecting().await;
        ui.failed(FailureKind::CoreNotRunning, Severity::Recoverable)
            .await;
        assert!(matches!(
            retry.await.expect("retry task"),
            Err(RequestCoreError::Call {
                source: CallCoreError::Open {
                    source: OpenCorePipeError::CoreNotRunning { .. }
                }
            })
        ));
        ui.no_automatic_retry().await;
        let listener = TestPipeServer::bind_named(name).expect("core appears after failure");
        let link = ui.link.clone();
        let retry = tokio::spawn(async move { link.retry().await });
        ui.connecting().await;
        let mut core = Core::accept(listener).await;
        core.handshake().await;
        ui.connected().await;
        retry
            .await
            .expect("retry task")
            .expect("one successful attempt");
        ui.stop().await;
    }

    #[tokio::test]
    async fn mismatch_and_rejection_are_unrecoverable_and_never_retry() {
        for mismatch in [
            quartz_protocol::ErrorKind::ProtoMismatch {
                supported: quartz_protocol::ProtoRange::new(
                    quartz_protocol::ProtoVersion(2),
                    quartz_protocol::ProtoVersion(2),
                )
                .expect("range"),
                core_version: quartz_protocol::CoreVersion("2.0".into()),
            },
            quartz_protocol::ErrorKind::Forbidden,
        ] {
            let expected = match mismatch {
                quartz_protocol::ErrorKind::ProtoMismatch { .. } => FailureKind::VersionMismatch,
                _ => FailureKind::HandshakeRejected,
            };
            let listener = TestPipeServer::bind().expect("listener");
            let mut ui = Harness::start(listener.name().clone());
            ui.connecting().await;
            let mut core = Core::accept(listener).await;
            let hello = core.request(Op::Hello).await;
            core.reject(hello, mismatch).await;
            ui.failed(expected, Severity::Unrecoverable).await;
            drop(core);
            ui.no_automatic_retry().await;
            ui.stop().await;
        }
    }

    #[tokio::test]
    async fn busy_and_handshake_timeout_are_recoverable_without_retries() {
        let listener = TestPipeServer::bind().expect("listener");
        let busy = PipeClient::open_named(listener.name())
            .await
            .expect("occupy only pipe instance");
        let mut ui = Harness::start(listener.name().clone());
        ui.connecting().await;
        tokio::time::pause();
        tokio::time::advance(CONNECT_TIMEOUT).await;
        tokio::time::resume();
        ui.failed(FailureKind::PipeBusy, Severity::Recoverable)
            .await;
        ui.no_automatic_retry().await;
        ui.stop().await;
        drop(busy);
        drop(listener);
        let listener = TestPipeServer::bind().expect("listener");
        let mut ui = Harness::start(listener.name().clone());
        ui.connecting().await;
        let mut core = Core::accept(listener).await;
        core.request(Op::Hello).await;
        tokio::time::pause();
        tokio::time::advance(RESPONSE_TIMEOUT).await;
        tokio::time::resume();
        ui.failed(FailureKind::ConnectTimeout, Severity::Recoverable)
            .await;
        drop(core);
        ui.no_automatic_retry().await;
        ui.stop().await;
    }

    #[tokio::test]
    async fn start_core_waits_for_a_late_pipe_before_its_single_attempt() {
        let name = absent_name();
        let (launch, launched) = oneshot::channel();
        let mut ui =
            Harness::with_initial(name.clone(), InitialConnection::AwaitAction, Some(launch));
        let link = ui.link.clone();
        let starting = tokio::spawn(async move { link.start_core().await });
        ui.connecting().await;
        launched
            .await
            .expect("test launcher called instead of starting a process");
        tokio::time::pause();
        tokio::time::advance(CORE_START_TIMEOUT / 2).await;
        assert!(ui.events.receiver.try_recv().is_err());
        assert!(!starting.is_finished(), "start action waits for readiness");
        let listener = TestPipeServer::bind_named(name).expect("late core pipe");
        tokio::time::resume();
        let mut core = Core::accept(listener).await;
        core.handshake().await;
        ui.connected().await;
        starting
            .await
            .expect("start task")
            .expect("start action connected");
        ui.stop().await;
    }

    #[tokio::test]
    async fn start_core_deadline_reports_failure_without_connecting_or_retrying() {
        let name = absent_name();
        let (launch, launched) = oneshot::channel();
        let mut ui = Harness::with_initial(name, InitialConnection::AwaitAction, Some(launch));
        let link = ui.link.clone();
        let starting = tokio::spawn(async move { link.start_core().await });
        ui.connecting().await;
        launched.await.expect("mock launcher");
        tokio::time::pause();
        tokio::time::advance(CORE_START_TIMEOUT).await;
        tokio::time::resume();
        ui.failed(FailureKind::ConnectTimeout, Severity::Recoverable)
            .await;
        assert!(matches!(
            starting.await.expect("start task"),
            Err(StartCoreError::Connect {
                source: CallCoreError::Wait {
                    source: quartz_pipe::WaitCorePipeError::Deadline { .. }
                }
            })
        ));
        ui.no_automatic_retry().await;
        ui.stop().await;
    }

    #[tokio::test]
    async fn hidden_prewarm_uses_the_prewarming_hello_and_waits_for_show() {
        use crate::prewarm::{PrewarmAction, PrewarmGate};
        let listener = TestPipeServer::bind().expect("listener");
        let (commands, receive) = mpsc::channel(BRIDGE_CAPACITY);
        let (send, events) = mpsc::channel(BRIDGE_CAPACITY);
        let mut worker = worker::Worker::new(receive, send, InitialConnection::Connect)
            .with_hello_state(UiState::Prewarming);
        worker.name = Some(listener.name().clone());
        let mut ui = Harness {
            link: Link { commands },
            events: LinkEvents { receiver: events },
            worker: tokio::spawn(worker.run()),
        };
        ui.connecting().await;
        let mut core = Core::accept(listener).await;
        core.handshake_phase(UiState::Prewarming).await;
        ui.connected().await;
        let mut gate = PrewarmGate::Hidden;
        core.event(Topic::Control, serde_json::json!({"op":"ui.show"}))
            .await;
        let show = ui.events.recv().await.expect("show");
        assert_eq!(gate.receive(&show), PrewarmAction::Open);
        ui.link
            .first_frame()
            .await
            .expect("window first frame hook");
        let frame = core.request(Op::UiFirstFrame).await;
        core.ok(frame, serde_json::json!({"accepted":true})).await;
        ui.link.hidden().expect("last window close");
        let hidden = core.request(Op::UiHidden).await;
        core.ok(hidden, serde_json::json!({"accepted":true})).await;
        gate = PrewarmGate::Hidden;
        core.event(Topic::Control, serde_json::json!({"op":"ui.show"}))
            .await;
        assert_eq!(
            gate.receive(&ui.events.recv().await.expect("reopen")),
            PrewarmAction::Open
        );
        ui.link.first_frame().await.expect("reopened frame");
        let frame = core.request(Op::UiFirstFrame).await;
        core.ok(frame, serde_json::json!({"accepted":true})).await;
        core.event(Topic::Control, serde_json::json!({"op":"ui.shutdown"}))
            .await;
        assert_eq!(
            gate.receive(&ui.events.recv().await.expect("shutdown")),
            PrewarmAction::Quit
        );
        ui.stop().await;
    }
    #[tokio::test]
    async fn state_control_first_frame_and_transport_loss_deliver_exit() {
        let listener = TestPipeServer::bind().expect("listener");
        let mut ui = Harness::start(listener.name().clone());
        ui.connecting().await;
        let mut core = Core::accept(listener).await;
        core.handshake().await;
        ui.connected().await;
        core.event(Topic::State, Core::snapshot()).await;
        let Some(LinkEvent::State(snapshot)) = ui.events.recv().await else {
            panic!("state snapshot");
        };
        assert!(snapshot.recording);
        for op in [ControlOp::UiShow, ControlOp::UiActivate] {
            core.event(Topic::Control, serde_json::json!({"op":op}))
                .await;
            let event = ui.events.recv().await.expect("UI control");
            assert!(matches!(&event, LinkEvent::Control(received) if *received == op));
            assert_eq!(event.window_action(), WindowAction::Activate);
        }
        ui.link.first_frame().await.expect("frame hook");
        let frame = core.request(Op::UiFirstFrame).await;
        core.ok(frame, serde_json::json!({"accepted":true})).await;
        drop(core);
        let event = ui.events.recv().await.expect("disconnect event");
        assert!(matches!(event, LinkEvent::Disconnected));
        assert!(event.exits_ui(), "the application must quit for this event");
        ui.worker
            .await
            .expect("transport loss ends the pipe thread");
    }

    #[tokio::test]
    async fn connection_protocol_errors_deliver_disconnect_without_waiting_for_eof() {
        for kind in [
            quartz_protocol::ErrorKind::BadRequest,
            quartz_protocol::ErrorKind::FrameTooLarge,
        ] {
            let listener = TestPipeServer::bind().expect("listener");
            let mut ui = Harness::start(listener.name().clone());
            ui.connecting().await;
            let mut core = Core::accept(listener).await;
            core.handshake().await;
            ui.connected().await;
            core.peer
                .send(Envelope::Response(Response {
                    id: None,
                    outcome: ResponseOutcome::Err {
                        error: ProtocolError::new(kind),
                    },
                }))
                .await
                .expect("connection-level rejection");
            let event = tokio::time::timeout(RESPONSE_TIMEOUT, ui.events.recv())
                .await
                .expect("connection error immediately ends worker")
                .expect("disconnect event");
            assert!(matches!(event, LinkEvent::Disconnected));
            assert!(event.exits_ui());
            ui.worker.await.expect("typed disconnect ends worker");
            drop(core);
        }
    }

    #[tokio::test]
    async fn a_request_timeout_keeps_other_requests_events_and_late_response_correlation() {
        let listener = TestPipeServer::bind().expect("listener");
        let mut ui = Harness::start(listener.name().clone());
        ui.connecting().await;
        let mut core = Core::accept(listener).await;
        core.handshake().await;
        ui.connected().await;
        let link = ui.link.clone();
        let first =
            tokio::spawn(async move { link.request(quartz_protocol::UiOpenRequest {}).await });
        let expired = core.request(Op::UiOpen).await;
        tokio::time::pause();
        tokio::time::advance(RESPONSE_TIMEOUT / 2).await;
        tokio::time::resume();
        let link = ui.link.clone();
        let second =
            tokio::spawn(async move { link.request(quartz_protocol::UiOpenRequest {}).await });
        let pending = core.request(Op::UiOpen).await;
        tokio::time::pause();
        tokio::time::advance(RESPONSE_TIMEOUT / 2).await;
        tokio::time::resume();
        assert!(matches!(
            first.await.expect("first task"),
            Err(RequestCoreError::Timeout)
        ));
        assert!(
            !second.is_finished(),
            "a different request has its own deadline"
        );
        core.ok(expired, serde_json::json!({"accepted":true})).await;
        core.event(Topic::State, Core::snapshot()).await;
        assert!(matches!(ui.events.recv().await, Some(LinkEvent::State(_))));
        core.ok(pending, serde_json::json!({"accepted":true})).await;
        assert_eq!(
            second.await.expect("second task").expect("second response"),
            quartz_protocol::AcceptedBody
        );
        let link = ui.link.clone();
        let subscribing = tokio::spawn(async move { link.subscribe(vec![Topic::Control]).await });
        core.subscribe(vec![Topic::Control]).await;
        subscribing
            .await
            .expect("subscribe task")
            .expect("connection remains usable");
        let link = ui.link.clone();
        let removing = tokio::spawn(async move { link.unsubscribe(vec![Topic::Control]).await });
        let request = core.request(Op::Unsubscribe).await;
        core.ok(request, serde_json::json!({})).await;
        removing
            .await
            .expect("unsubscribe task")
            .expect("unsubscribe remains usable");
        ui.stop().await;
    }

    #[tokio::test]
    async fn worker_routes_progress_only_to_matching_pending_request_and_ignores_unknown_or_completed_ids()
     {
        let listener = TestPipeServer::bind().expect("listener");
        let mut ui = Harness::start(listener.name().clone());
        ui.connecting().await;
        let mut core = Core::accept(listener).await;
        core.handshake().await;
        ui.connected().await;

        let (tx_a, mut rx_a) = watch::channel(None);
        let link_a = ui.link.clone();
        let task_a = tokio::spawn(async move {
            link_a
                .request_with_progress(quartz_protocol::UiOpenRequest {}, tx_a)
                .await
        });
        let req_a = core.request(Op::UiOpen).await;
        let id_a = req_a.id;

        let (tx_b, mut rx_b) = watch::channel(None);
        let link_b = ui.link.clone();
        let task_b = tokio::spawn(async move {
            link_b
                .request_with_progress(quartz_protocol::UiOpenRequest {}, tx_b)
                .await
        });
        let req_b = core.request(Op::UiOpen).await;
        let id_b = req_b.id;

        core.progress(id_a, 3, 10).await;
        rx_a.changed().await.expect("progress for request A");
        assert_eq!(
            *rx_a.borrow_and_update(),
            Some(QueryProgress { done: 3, total: 10 })
        );
        assert!(
            !rx_b.has_changed().expect("request B watch stays open"),
            "progress for A must not notify request B"
        );
        assert_eq!(*rx_b.borrow(), None);

        core.progress(RequestId(999_999), 1, 5).await;

        core.ok(req_a, serde_json::json!({"accepted":true})).await;
        assert_eq!(
            task_a.await.expect("task A").expect("response A"),
            quartz_protocol::AcceptedBody
        );
        assert!(
            rx_a.changed().await.is_err(),
            "completing request drops its progress observer"
        );
        core.progress(id_a, 4, 10).await;

        core.progress(id_b, 2, 7).await;
        rx_b.changed().await.expect("progress for request B");
        assert_eq!(
            *rx_b.borrow_and_update(),
            Some(QueryProgress { done: 2, total: 7 })
        );
        core.ok(req_b, serde_json::json!({"accepted":true})).await;
        assert_eq!(
            task_b.await.expect("task B").expect("response B"),
            quartz_protocol::AcceptedBody
        );
        ui.stop().await;
    }

    #[tokio::test]
    async fn every_initial_connection_failure_stops_until_an_action() {
        struct Case {
            source: CallCoreError,
            kind: FailureKind,
        }
        let busy = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
            .await
            .expect_err("elapsed timeout");
        let response = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
            .await
            .expect_err("elapsed timeout");
        let cases = [
            Case {
                source: CallCoreError::Open {
                    source: OpenCorePipeError::CoreNotRunning {
                        source: std::io::ErrorKind::NotFound.into(),
                    },
                },
                kind: FailureKind::CoreNotRunning,
            },
            Case {
                source: CallCoreError::Open {
                    source: OpenCorePipeError::Busy { source: busy },
                },
                kind: FailureKind::PipeBusy,
            },
            Case {
                source: CallCoreError::ResponseTimeout { source: response },
                kind: FailureKind::ConnectTimeout,
            },
            Case {
                source: CallCoreError::Open {
                    source: OpenCorePipeError::Connect {
                        source: std::io::ErrorKind::PermissionDenied.into(),
                    },
                },
                kind: FailureKind::ConnectIo,
            },
            Case {
                source: CallCoreError::Open {
                    source: OpenCorePipeError::ServerIdentity {
                        source: quartz_pipe::CheckPeerError::Executable,
                    },
                },
                kind: FailureKind::IdentityRejected,
            },
            Case {
                source: CallCoreError::Protocol {
                    kind: quartz_protocol::ErrorKind::Forbidden,
                },
                kind: FailureKind::HandshakeRejected,
            },
            Case {
                source: CallCoreError::Protocol {
                    kind: quartz_protocol::ErrorKind::ProtoMismatch {
                        supported: quartz_protocol::ProtoRange::new(
                            quartz_protocol::ProtoVersion(2),
                            quartz_protocol::ProtoVersion(2),
                        )
                        .expect("range"),
                        core_version: quartz_protocol::CoreVersion("2.0".into()),
                    },
                },
                kind: FailureKind::VersionMismatch,
            },
        ];
        for case in cases {
            let (commands, receive) = mpsc::channel(BRIDGE_CAPACITY);
            let (send, events) = mpsc::channel(BRIDGE_CAPACITY);
            let mut worker = worker::Worker::new(receive, send, InitialConnection::Connect);
            worker.name = Some(absent_name());
            worker.initial_failure = Some(case.source);
            let mut ui = Harness {
                link: Link { commands },
                events: LinkEvents { receiver: events },
                worker: tokio::spawn(worker.run()),
            };
            ui.connecting().await;
            ui.failed(case.kind, case.kind.severity()).await;
            ui.no_automatic_retry().await;
            ui.stop().await;
        }
    }
    mod detail_queries {
        include!("detail_tests.rs");
    }
    mod day_queries {
        include!("day_tests.rs");
    }
}
