//! Real in-process pipe tests use random names and never start the production core.

use super::server::StartPipeServerError;
use super::*;
use crate::ipc::{StatusPublisher, UiCommand};
use crossbeam_channel::{Receiver, bounded};
use quartz_pipe::CallCoreError;
use quartz_protocol::{
    CoreStatusResponse, Envelope, ErrorKind, HelloRole, MAX_FRAME_LEN, Op, PROTOCOL_VERSION,
    ProtocolError, RequestId, Response, ResponseOutcome, UiHello, UiState,
};
use quartz_protocol::{encode_json, frame_codec};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::{bytes::BytesMut, codec::Encoder};

struct Fixture {
    server: PipeServer,
    name: PipeName,
    status: StatusPublisher,
    commands: Receiver<UiCommand>,
    database: crate::query::dispatch::test_support::TestDatabase,
}
impl Fixture {
    fn new() -> Self {
        Self::with_timeouts(server::ConnectionTimeouts::default())
    }
    fn with_timeouts(timeouts: server::ConnectionTimeouts) -> Self {
        Self::with_query_port(timeouts, None)
    }
    fn with_query_port(
        timeouts: server::ConnectionTimeouts,
        query_port: Option<crate::query::dispatch::QueryPort>,
    ) -> Self {
        let database = crate::query::dispatch::test_support::TestDatabase::start();
        let suffix = windows::core::GUID::new().expect("unique pipe suffix");
        let name = PipeName::for_test(suffix);
        let status = StatusPublisher::new(quartz_protocol::InstanceId(format!("{suffix:?}")));
        let (commands, received) = bounded(8);
        let port: crate::ipc::UiPort = Arc::new(move |command| {
            commands
                .try_send(command)
                .map_err(|_| crate::ipc::QueueUiError)
        });
        let server = PipeServer::start(server::PipeServerConfig {
            shutdown_timeout: crate::core::app::SHUTDOWN_TIMEOUT,
            owner: ProcessIdentity::current().expect("identity"),
            name: name.clone(),
            timeouts,
            services: crate::ipc::Services {
                summary: None,
                timed: crate::ipc::TimedPolicyPort::unavailable(),
                queries: query_port.unwrap_or_else(|| database.port.clone()),
                writes: crate::ipc::WriteChannel::new().port,
                versions: database.versions.clone(),
                status: status.subscribe(),
                ui: port,
            },
        })
        .expect("start test server");
        Self {
            server,
            name,
            status,
            commands: received,
            database,
        }
    }
    fn cli(&self) -> CoreClient {
        CoreClient::connect_named(&self.name, HelloRole::Cli).expect("cli handshake")
    }
    fn ui_registration(&self) -> crate::ipc::registry::ConnectionId {
        let UiCommand::Registered {
            connection, pid, ..
        } = self
            .commands
            .recv_timeout(CONNECT_TIMEOUT)
            .expect("UI registration")
        else {
            panic!("expected UI registration")
        };
        assert_eq!(
            pid,
            crate::ipc::ProcessId::new(std::process::id()),
            "pipe transport reports verified peer PID"
        );
        connection
    }
    fn ui(&self) -> CoreClient {
        CoreClient::connect_named(
            &self.name,
            HelloRole::Ui {
                ui: UiHello {
                    state: UiState::Starting,
                },
            },
        )
        .expect("UI handshake")
    }
    fn raw(&self) -> CoreClient {
        CoreClient::open_named(&self.name).expect("open test pipe")
    }
    fn stop(self) {
        self.server.shutdown().expect("bounded server shutdown");
    }
}
fn wire(payload: &[u8]) -> BytesMut {
    let mut buffer = BytesMut::new();
    frame_codec()
        .encode(payload, &mut buffer)
        .expect("test frame");
    buffer
}
fn exchange(client: &mut CoreClient, request: Value) -> Response {
    client
        .write_bytes(&wire(&encode_json(&request).expect("test JSON")))
        .expect("write request");
    response(client)
}
fn response(client: &mut CoreClient) -> Response {
    let Envelope::Response(response) = client.read_envelope().expect("read response") else {
        panic!("expected response");
    };
    response
}
fn assert_error(response: Response, id: Option<RequestId>, expected: ErrorKind) {
    assert_eq!(response.id, id);
    assert_eq!(
        response.outcome,
        ResponseOutcome::Err {
            error: ProtocolError::new(expected)
        }
    );
}
fn hello() -> Value {
    json!({"t":"req","id":1,"op":"hello","body":{"proto":{"min":1,"max":1},"client_version":"test","role":"cli"}})
}
fn assert_closed(client: &mut CoreClient) {
    assert!(
        match client.read_envelope() {
            Err(CallCoreError::Io { source }) => source.kind() != std::io::ErrorKind::TimedOut,
            Err(CallCoreError::Frame { source }) => source.kind() != std::io::ErrorKind::TimedOut,
            _ => false,
        },
        "server must close after final error"
    );
}

#[test]
fn handshake_status_and_ui_open_delivery() {
    let fixture = Fixture::new();
    let mut cli = fixture.cli();
    let status = cli.status().expect("status");
    assert_eq!(status, fixture.status.subscribe().borrow().status);
    cli.open_ui().expect("accepted open");
    assert_eq!(
        fixture
            .commands
            .recv_timeout(CONNECT_TIMEOUT)
            .expect("main command"),
        UiCommand::Open
    );
    let mut ui = fixture.ui();
    let UiCommand::Registered {
        connection,
        pid,
        state,
        ..
    } = fixture
        .commands
        .recv_timeout(CONNECT_TIMEOUT)
        .expect("registration")
    else {
        panic!("expected UI registration")
    };
    assert_eq!(pid, crate::ipc::ProcessId::new(std::process::id()));
    assert_eq!(state, quartz_protocol::UiState::Starting);
    assert_eq!(
        ui.request(Op::UiFirstFrame, &json!({}))
            .expect("first frame"),
        json!({"accepted":true})
    );
    assert_eq!(
        fixture
            .commands
            .recv_timeout(CONNECT_TIMEOUT)
            .expect("first frame command"),
        UiCommand::FirstFrame { connection }
    );
    fixture.stop();
    assert_closed(&mut cli);
}
#[test]
fn first_frame_requires_hello_and_closes() {
    let fixture = Fixture::new();
    let mut client = fixture.raw();
    assert_error(
        exchange(
            &mut client,
            json!({"t":"req","id":7,"op":"core.status","body":{}}),
        ),
        Some(RequestId(7)),
        ErrorKind::HandshakeRequired,
    );
    assert_closed(&mut client);
}
#[test]
fn incompatible_protocol_reports_supported_range_and_closes() {
    let fixture = Fixture::new();
    let mut client = fixture.raw();
    let mut request = hello();
    request["body"]["proto"] = json!({"min":2,"max":2});
    assert_error(
        exchange(&mut client, request),
        Some(RequestId(1)),
        ErrorKind::ProtoMismatch {
            supported: quartz_protocol::ProtoRange::new(PROTOCOL_VERSION, PROTOCOL_VERSION)
                .expect("range"),
            core_version: quartz_protocol::CoreVersion(env!("CARGO_PKG_VERSION").into()),
        },
    );
    assert_closed(&mut client);
}
#[test]
fn oversize_declared_length_is_rejected_before_payload() {
    let fixture = Fixture::new();
    let mut client = fixture.raw();
    let length = u32::try_from(MAX_FRAME_LEN + 1)
        .expect("frame length")
        .to_le_bytes();
    client.write_bytes(&length).expect("declared length only");
    assert_error(response(&mut client), None, ErrorKind::FrameTooLarge);
    assert_closed(&mut client);
}
#[test]
fn garbage_json_is_connection_error() {
    let fixture = Fixture::new();
    let mut client = fixture.raw();
    client
        .write_bytes(&wire(b"{garbage"))
        .expect("write garbage");
    assert_error(response(&mut client), None, ErrorKind::BadRequest);
    assert_closed(&mut client);
}
#[test]
fn unknown_operation_preserves_connection_and_permission_errors_are_typed() {
    let fixture = Fixture::new();
    let mut client = fixture.raw();
    assert!(matches!(
        exchange(&mut client, hello()).outcome,
        ResponseOutcome::Ok { .. }
    ));
    assert_error(
        exchange(
            &mut client,
            json!({"t":"req","id":2,"op":"future.op","body":{}}),
        ),
        Some(RequestId(2)),
        ErrorKind::UnknownOp,
    );
    let status = exchange(
        &mut client,
        json!({"t":"req","id":3,"op":"core.status","body":{}}),
    );
    let ResponseOutcome::Ok { body } = status.outcome else {
        panic!("usable after unknown operation");
    };
    let _: CoreStatusResponse = serde_json::from_value(body).expect("typed status");
    let mut cli = fixture.cli();
    assert!(matches!(
        cli.request(Op::AnnotationsCreate, &json!({})),
        Err(CallCoreError::Protocol {
            kind: ErrorKind::Forbidden
        })
    ));
    assert!(cli.status().is_ok());
    let mut ui = fixture.ui();
    assert!(matches!(
        ui.request(Op::RuleSetsTree, &json!({})),
        Err(CallCoreError::Protocol {
            kind: ErrorKind::BadRequest
        })
    ));
    assert!(matches!(
        ui.request(Op::RuleSetsEdit, &json!({})),
        Err(CallCoreError::Protocol {
            kind: ErrorKind::BadRequest
        })
    ));
    for client in [&mut ui, &mut cli] {
        let body = client
            .request(Op::RuleSetsTree, &json!({"kind":"privacy"}))
            .expect("rule tree over named pipe");
        let tree: quartz_protocol::RuleSetsTreeResponse =
            serde_json::from_value(body).expect("typed rule tree");
        assert!(tree.nodes.iter().any(|node| node.key == "builtin.privacy"));
        let body = client
            .request(
                Op::RuleSetsGet,
                &json!({"key":"builtin.privacy","view":"effective"}),
            )
            .expect("effective contents over named pipe");
        let contents: quartz_protocol::RuleSetsGetResponse =
            serde_json::from_value(body).expect("typed contents");
        assert!(!contents.entries.is_empty());
    }
}
#[test]
fn unknown_host_role_is_rejected_as_bad_request_and_connection_closed() {
    let fixture = Fixture::new();
    let mut client = fixture.raw();
    let mut request = hello();
    request["body"]["role"] = json!("host");
    assert_error(
        exchange(&mut client, request),
        Some(RequestId(1)),
        ErrorKind::BadRequest,
    );
    assert_closed(&mut client);
}
#[test]
fn eight_clients_hold_all_slots_and_disconnect_releases_one() {
    let fixture = Fixture::new();
    let mut clients: Vec<_> = (0..8).map(|_| fixture.cli()).collect();
    assert!(matches!(
        CoreClient::open_named(&fixture.name),
        Err(CallCoreError::Open { source }) if matches!(source, OpenCorePipeError::Busy { .. })
    ));
    drop(clients.pop());
    assert!(fixture.cli().status().is_ok());
}
#[test]
fn silent_client_does_not_block_another_client_or_shutdown() {
    let fixture = Fixture::new();
    let mut silent = fixture.raw();
    assert!(fixture.cli().status().is_ok());
    fixture.stop();
    assert_closed(&mut silent);
}
#[test]
fn first_instance_refuses_an_occupied_name() {
    let fixture = Fixture::new();
    let port: crate::ipc::UiPort = Arc::new(|_| Ok(()));
    let result = PipeServer::start(server::PipeServerConfig {
        shutdown_timeout: crate::core::app::SHUTDOWN_TIMEOUT,
        owner: ProcessIdentity::current().expect("identity"),
        name: fixture.name.clone(),
        timeouts: server::ConnectionTimeouts::default(),
        services: crate::ipc::Services {
            summary: None,
            timed: crate::ipc::TimedPolicyPort::unavailable(),
            queries: fixture.database.port.clone(),
            writes: crate::ipc::WriteChannel::new().port,
            versions: fixture.database.versions.clone(),
            status: fixture.status.subscribe(),
            ui: port,
        },
    });
    assert!(matches!(
        result,
        Err(StartPipeServerError::NameOccupied { .. })
    ));
    assert!(fixture.cli().status().is_ok());
    let waiter =
        quartz_pipe::CoreReadyWaiter::new(&fixture.name).expect("existing server readiness");
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()
        .expect("readiness test runtime")
        .block_on(async { tokio::time::timeout(CONNECT_TIMEOUT, waiter.wait()).await })
        .expect("failed duplicate startup must preserve readiness")
        .expect("existing server remains ready");
}

#[test]
fn disconnects_before_hello_do_not_stop_the_listener() {
    let fixture = Fixture::new();
    for _ in 0..32 {
        drop(fixture.raw());
    }
    assert!(fixture.cli().status().is_ok());
}

#[test]
fn missing_pipe_is_a_typed_core_not_running_error() {
    let suffix = windows::core::GUID::new().expect("unique absent pipe");
    let name = PipeName::for_test(suffix);
    assert!(matches!(
        CoreClient::open_named(&name),
        Err(CallCoreError::Open { source }) if matches!(source, OpenCorePipeError::CoreNotRunning { .. })
    ));
}

/// Short deadlines make connection lifecycle tests independent of the production five-second hello deadline.
const TEST_CONNECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);
/// Allow scheduling and pipe replacement to complete before inspecting an expired connection.
const TEST_DEADLINE_HEADROOM: std::time::Duration = std::time::Duration::from_millis(100);

#[test]
fn unread_final_error_releases_its_slot_by_the_close_deadline() {
    let fixture = Fixture::with_timeouts(server::ConnectionTimeouts {
        hello: std::time::Duration::from_secs(5),
        close_flush: TEST_CONNECTION_TIMEOUT,
    });
    let _other_clients: Vec<_> = (0..7).map(|_| fixture.cli()).collect();
    let mut unread = fixture.raw();
    unread
        .write_bytes(
            &u32::try_from(MAX_FRAME_LEN + 1)
                .expect("oversize length")
                .to_le_bytes(),
        )
        .expect("send oversize header");
    std::thread::sleep(TEST_CONNECTION_TIMEOUT + TEST_DEADLINE_HEADROOM);
    // Keep unread's handle open while proving that the eighth slot accepts a new client.
    assert!(fixture.cli().status().is_ok());
    drop(unread);
}

#[test]
fn partial_hello_expires_without_a_reply_and_releases_its_slot() {
    let fixture = Fixture::with_timeouts(server::ConnectionTimeouts {
        hello: TEST_CONNECTION_TIMEOUT,
        close_flush: TEST_CONNECTION_TIMEOUT,
    });
    let _other_clients: Vec<_> = (0..7).map(|_| fixture.cli()).collect();
    let mut partial = fixture.raw();
    let bytes = wire(&encode_json(&hello()).expect("hello JSON"));
    partial
        .write_bytes(&bytes[..bytes.len() - 1])
        .expect("partial hello");
    std::thread::sleep(TEST_CONNECTION_TIMEOUT + TEST_DEADLINE_HEADROOM);
    assert_closed(&mut partial);
    assert!(fixture.cli().status().is_ok());
}

#[test]
fn silent_hello_expires_and_a_ready_client_survives_the_same_deadline() {
    let fixture = Fixture::with_timeouts(server::ConnectionTimeouts {
        hello: TEST_CONNECTION_TIMEOUT,
        close_flush: TEST_CONNECTION_TIMEOUT,
    });
    let mut ready = fixture.cli();
    let mut silent = fixture.raw();
    std::thread::sleep(TEST_CONNECTION_TIMEOUT + TEST_DEADLINE_HEADROOM);
    assert_closed(&mut silent);
    assert!(
        ready.status().is_ok(),
        "hello timeout must stop applying after the handshake"
    );
}

/// Scaled duration for the continuous capture driver and response flood (04 §9).
const SLOW_CLIENT_TEST_DURATION: std::time::Duration = std::time::Duration::from_secs(2);
/// Allows loaded Windows test runners headroom while catching a stalled I/O runtime.
const STATUS_LATENCY_LIMIT: std::time::Duration = std::time::Duration::from_millis(500);
/// A watch publication must finish independently of client reads (01 §6).
const PUBLISH_LATENCY_LIMIT: std::time::Duration = std::time::Duration::from_millis(100);
/// Enough small status responses to exhaust a stalled connection's 1 MiB budget.
const FLOOD_REQUESTS: u64 = 20_000;
/// Bound the publisher's stress rate while keeping several changes per throttle window.
const STRESS_PUBLISH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

fn state_event(client: &mut CoreClient) -> quartz_protocol::StateSnapshot {
    let Envelope::Event(event) = client.read_envelope().expect("state event") else {
        panic!("expected event after subscription response");
    };
    assert_eq!(event.topic, quartz_protocol::Topic::State);
    serde_json::from_value(event.body).expect("typed state body")
}
fn publish_change(publisher: &StatusPublisher, change: u64) {
    let persistence = if change.is_multiple_of(2) {
        crate::persistence::PersistenceState::Ready
    } else {
        crate::persistence::PersistenceState::Failed { commands: 1 }
    };
    publisher.publish(None, persistence);
}

/// Fits the capture title cap and fills the 64 KiB pipe buffer within the scaled test.
const STRESS_TITLE_CHARACTERS: usize = 4000;
fn publish_large_foreground(publisher: &StatusPublisher, change: u64) {
    use quartz_domain::capture::*;
    let view = ForegroundView {
        app: Some(AppIdentity::Exe("c:\\app.exe".into())),
        title: Some(format!("{change}{}", "界".repeat(STRESS_TITLE_CHARACTERS))),
        start: Timestamp {
            utc: UtcInstant::try_from(123).expect("test UTC"),
            mono: MonoInstant::from_millis(0),
        },
    };
    let tracking = CaptureState::Tracking(IntervalFact::Window {
        instance: Some(WindowId::from_persisted(1)),
        fields: CapturedFields {
            level: CaptureLevel::Full,
            title: view.title.clone(),
            title_status: TitleStatus::Ok,
            privacy: None,
            web: quartz_domain::rules::WebAction::None,
        },
    });
    publisher.publish_foreground(Some(view));
    publisher.publish(Some(&tracking), crate::persistence::PersistenceState::Ready);
}

#[test]
fn subscribe_response_precedes_snapshot_and_ten_changes_coalesce() {
    let fixture = Fixture::new();
    let mut ui = fixture.ui();
    assert_eq!(
        ui.request(Op::Subscribe, &json!({"topics":["state"]}))
            .expect("subscribe"),
        json!({})
    );
    let first = state_event(&mut ui);
    assert_eq!(first, fixture.status.subscribe().borrow().snapshot());
    for change in 0..10 {
        publish_change(&fixture.status, change);
    }
    let latest = state_event(&mut ui);
    assert_eq!(latest, fixture.status.subscribe().borrow().snapshot());
    assert!(
        matches!(
            ui.read_envelope(),
            Err(CallCoreError::ResponseTimeout { .. })
        ),
        "no third snapshot without a change"
    );
}

#[test]
fn subscribe_rejects_cli_and_unknown_topics() {
    let fixture = Fixture::new();
    let mut cli = fixture.cli();
    assert!(matches!(
        cli.request(Op::Subscribe, &json!({"topics":["state"]})),
        Err(CallCoreError::Protocol {
            kind: ErrorKind::Forbidden
        })
    ));
    let mut ui = fixture.ui();
    assert!(matches!(
        ui.request(Op::Subscribe, &json!({"topics":["weather"]})),
        Err(CallCoreError::Protocol {
            kind: ErrorKind::BadRequest
        })
    ));
    ui.status()
        .expect("connection remains unsubscribed and usable");
}

/// Full 04 §9 acceptance duration; run explicitly with a serial test harness.
const SLOW_CLIENT_ACCEPTANCE_DURATION: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);
/// Same capture commit budget as core/app's large-query contention test (08 §1.3).
const CAPTURE_COMMIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);
/// Two SQLite caches, worker/runtime stacks, frame encoding and allocator retention.
const PRIVATE_BYTES_SLACK: usize = 32 * 1024 * 1024;

struct ProcessPrivateBytes;
impl ProcessPrivateBytes {
    fn sample() -> usize {
        use windows::Win32::System::{
            ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            },
            Threading::GetCurrentProcess,
        };
        let size =
            u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>()).expect("counter size");
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size,
            ..Default::default()
        };
        // SAFETY: The live current-process handle and writable EX structure include PrivateUsage.
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                size,
            )
        }
        .expect("sample process Private Bytes");
        counters.PrivateUsage
    }
}
struct StressClock(std::time::Instant);
impl crate::core::sources::ClockReader for StressClock {
    fn read(
        &self,
    ) -> Result<quartz_domain::capture::Timestamp, crate::core::sources::ReadClockError> {
        Ok(crate::store::capture::test_support::at(
            u64::try_from(self.0.elapsed().as_millis()).expect("stress clock"),
        ))
    }
}
struct StressResolver;
impl crate::core::foreground::ForegroundResolve for StressResolver {
    fn resolve(
        &mut self,
        _snapshot: crate::core::foreground::Snapshot,
    ) -> crate::core::foreground::Resolution {
        crate::core::foreground::Resolution::Review
    }
}
struct SlowClientRun;
impl SlowClientRun {
    fn run(duration: std::time::Duration) {
        use crate::core::{
            app::{MainCommand, ObservedEvent, Workers},
            sources::ClockReader,
        };
        use crate::store::capture::test_support::{Inspection, metadata};
        use quartz_domain::capture::*;
        let temporary = tempfile::tempdir().expect("slow-client isolated database");
        let store =
            crate::store::Store::open(&crate::data_directory::DataDirectory::new(temporary.path()))
                .expect("slow-client store");
        let clock = Arc::new(StressClock(std::time::Instant::now()));
        let workers = Workers::start(
            store,
            metadata(),
            clock.clone(),
            Arc::new(|| {}),
            Box::new(|| Ok(Box::new(StressResolver))),
        )
        .expect("real capture and database workers");
        let name = PipeName::for_test(windows::core::GUID::new().expect("stress pipe"));
        let status = StatusPublisher::new(quartz_protocol::InstanceId("slow-client-test".into()));
        let server = PipeServer::start(server::PipeServerConfig {
            shutdown_timeout: crate::core::app::SHUTDOWN_TIMEOUT,
            owner: ProcessIdentity::current().expect("stress identity"),
            name: name.clone(),
            timeouts: server::ConnectionTimeouts::default(),
            services: crate::ipc::Services {
                summary: None,
                timed: crate::ipc::TimedPolicyPort::unavailable(),
                queries: workers.queries.clone(),
                writes: workers.writes.clone(),
                versions: workers.versions.clone(),
                status: status.subscribe(),
                ui: Arc::new(|_| Ok(())),
            },
        })
        .expect("real test pipe server");
        let mut slow = CoreClient::connect_named(
            &name,
            HelloRole::Ui {
                ui: UiHello {
                    state: UiState::Starting,
                },
            },
        )
        .expect("slow UI hello");
        slow.request(Op::Subscribe, &json!({"topics":["state"]}))
            .expect("subscribe slow reader");
        // Retain the initial snapshot unread for the entire measured duration.
        let mut cli = CoreClient::connect_named(&name, HelloRole::Cli).expect("healthy CLI hello");
        let observe = |event| {
            workers
                .sender
                .observe(ObservedEvent {
                    at: clock.read().expect("stress time"),
                    event,
                })
                .expect("capture admission")
        };
        observe(CaptureEvent::Session(SessionStatus::Ready(
            Default::default(),
        )));
        observe(CaptureEvent::Foreground {
            source: ObservationSource::Review,
            foreground: Foreground::None(NoForegroundReason::Empty),
        });
        let mut inspection = Inspection::open(temporary.path());
        let baseline = ProcessPrivateBytes::sample();
        let mut peak = baseline;
        let started = std::time::Instant::now();
        let mut change = 0;
        let mut committed = 0;
        let mut latencies = Vec::new();
        let mut queued_peak = 0;
        while started.elapsed() < duration {
            let submitted = std::time::Instant::now();
            observe(CaptureEvent::Stop(StopChange::Begin(StopReason::Paused)));
            // Confirm the exact new capture row in SQLite, rather than consuming an older Ready notice.
            while inspection.pause_count() <= committed {
                for command in workers.commands.try_iter() {
                    if let MainCommand::Fatal(worker) = command {
                        panic!("{worker} worker failed");
                    }
                }
                let publishing = std::time::Instant::now();
                publish_large_foreground(&status, change);
                assert!(
                    publishing.elapsed() < PUBLISH_LATENCY_LIMIT,
                    "capture state publication stalled"
                );
                change += 1;
                let requesting = std::time::Instant::now();
                cli.status().expect("healthy client during capture writes");
                assert!(
                    requesting.elapsed() < STATUS_LATENCY_LIMIT,
                    "status waited on slow client"
                );
                let metrics = server.metrics();
                assert_eq!(metrics.len(), 2, "stalled reader remains connected");
                for metric in metrics {
                    queued_peak = queued_peak.max(metric.peak);
                    assert!(metric.queued <= crate::ipc::outbox::SEND_BUFFER_LIMIT);
                    assert!(metric.peak <= crate::ipc::outbox::SEND_BUFFER_LIMIT);
                }
                peak = peak.max(ProcessPrivateBytes::sample());
                assert!(
                    peak.saturating_sub(baseline)
                        <= crate::ipc::outbox::SEND_BUFFER_LIMIT + PRIVATE_BYTES_SLACK,
                    "process growth exceeded send budget plus 32 MiB slack"
                );
                assert!(
                    submitted.elapsed() < crate::core::app::SHUTDOWN_TIMEOUT,
                    "capture write stopped committing"
                );
                std::thread::sleep(STRESS_PUBLISH_INTERVAL);
            }
            let latency = submitted.elapsed();
            committed += 1;
            latencies.push(latency);
            observe(CaptureEvent::Stop(StopChange::End(StopReason::Paused)));
            observe(CaptureEvent::Foreground {
                source: ObservationSource::Review,
                foreground: Foreground::None(NoForegroundReason::Empty),
            });
        }
        assert!(committed > 1, "continuous capture commits required");
        assert!(queued_peak > 0, "unread pipe must have pending state bytes");
        latencies.sort_unstable();
        const PERCENTILE: usize = 95;
        const PERCENT: usize = 100;
        let rank = (latencies.len() * PERCENTILE).div_ceil(PERCENT);
        let p95 = latencies[rank - 1];
        assert!(
            p95 < CAPTURE_COMMIT_LIMIT,
            "capture commit P95 {p95:?} exceeds {CAPTURE_COMMIT_LIMIT:?}"
        );
        println!(
            "duration={:?} commits={committed} commit_p95={p95:?} queued_peak={queued_peak} private_baseline={baseline} private_peak={peak} private_growth={} private_limit={}",
            started.elapsed(),
            peak.saturating_sub(baseline),
            crate::ipc::outbox::SEND_BUFFER_LIMIT + PRIVATE_BYTES_SLACK
        );
        drop(slow);
        drop(cli);
        server.shutdown().expect("stop stress pipe");
        workers
            .request_shutdown(clock.read().expect("shutdown time"), RunEndReason::UserExit)
            .expect("stop stress capture");
        workers.join().expect("join real stress workers");
    }
}

#[test]
fn stalled_state_reader_preserves_capture_commits_status_budget_and_private_bytes() {
    // Private Bytes is process-wide: isolate from unrelated parallel tests using the test harness.
    const PROBE_ENV: &str = "QUARTZ_TEST_SLOW_CLIENT_PROBE";
    match std::env::var(PROBE_ENV) {
        Ok(value) => {
            assert_eq!(value, "scaled", "unknown slow-client test mode");
            SlowClientRun::run(SLOW_CLIENT_TEST_DURATION);
        }
        Err(std::env::VarError::NotPresent) => {
            let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "platform::windows::ipc::tests::stalled_state_reader_preserves_capture_commits_status_budget_and_private_bytes", "--nocapture", "--test-threads=1"])
                .env(PROBE_ENV, "scaled").output().expect("isolated scaled test harness");
            assert!(
                output.status.success(),
                "scaled slow-client test failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            print!("{}", String::from_utf8_lossy(&output.stdout));
        }
        Err(error) => panic!("invalid slow-client test environment: {error}"),
    }
}

#[test]
#[ignore = "ten-minute M2 slow-client acceptance; run alone with --test-threads=1"]
fn slow_client_ten_minutes_preserves_capture_commits_status_budget_and_private_bytes() {
    SlowClientRun::run(SLOW_CLIENT_ACCEPTANCE_DURATION);
}

#[test]
fn flooding_unread_responses_closes_only_the_slow_connection() {
    let fixture = Fixture::new();
    let name = fixture.name.clone();
    let (ready, started) = bounded(1);
    let flood = std::thread::spawn(move || {
        let mut slow = CoreClient::connect_named(
            &name,
            HelloRole::Ui {
                ui: UiHello {
                    state: UiState::Starting,
                },
            },
        )
        .expect("flood UI hello");
        slow.request(Op::Subscribe, &json!({"topics":["state"]}))
            .expect("flood UI subscribe");
        ready.send(()).expect("announce flood");
        let mut requests = Vec::new();
        for id in 2..FLOOD_REQUESTS {
            let request = json!({"t":"req","id":id,"op":"core.status","body":{}});
            requests.extend_from_slice(&wire(&encode_json(&request).expect("flood request")));
        }
        match slow.write_bytes(&requests) {
            Ok(()) => {}
            Err(
                CallCoreError::Io { .. }
                | CallCoreError::ResponseTimeout { .. }
                | CallCoreError::WriteTimeout { .. },
            ) => {}
            Err(error) => panic!("unexpected flood failure: {error}"),
        }
        // A forced server disconnect discards unread responses; drain any frame already buffered locally.
        loop {
            match slow.read_envelope() {
                Ok(_) => {}
                Err(
                    CallCoreError::Io { .. }
                    | CallCoreError::Decode { .. }
                    | CallCoreError::Frame { .. },
                ) => break,
                Err(error) => panic!("slow connection must close: {error}"),
            }
        }
    });
    started
        .recv_timeout(CONNECT_TIMEOUT)
        .expect("flood started");
    let mut cli = fixture.cli();
    let start = std::time::Instant::now();
    while start.elapsed() < SLOW_CLIENT_TEST_DURATION {
        let requesting = std::time::Instant::now();
        cli.status().expect("healthy client during flood");
        assert!(requesting.elapsed() < STATUS_LATENCY_LIMIT);
        std::thread::sleep(STRESS_PUBLISH_INTERVAL);
    }
    flood.join().expect("flood closes within response deadline");
    cli.status()
        .expect("healthy client after slow connection closes");
    assert!(
        fixture
            .server
            .metrics()
            .iter()
            .all(|metric| metric.peak <= crate::ipc::outbox::SEND_BUFFER_LIMIT)
    );
}

#[test]
fn quit_control_is_implicit_and_flushed_before_server_shutdown() {
    let fixture = Fixture::new();
    let mut ui = fixture.ui();
    // No explicit control subscription is necessary for UI lifecycle notifications.
    let reader = std::thread::spawn(move || ui.read_envelope().expect("shutdown control"));
    fixture
        .server
        .control(quartz_protocol::ControlOp::UiShutdown);
    fixture.stop();
    let Envelope::Event(event) = reader.join().expect("UI reader") else {
        panic!("expected control event");
    };
    assert_eq!(event.topic, quartz_protocol::Topic::Control);
    let control: quartz_protocol::ControlBody =
        serde_json::from_value(event.body).expect("typed control");
    assert_eq!(control.op, quartz_protocol::ControlOp::UiShutdown);
}

#[test]
fn subscriptions_add_idempotently_and_control_is_not_duplicated() {
    let fixture = Fixture::new();
    let mut ui = fixture.ui();
    ui.request(Op::Subscribe, &json!({"topics":["control"]}))
        .expect("control subscription");
    ui.request(Op::Subscribe, &json!({"topics":["state"]}))
        .expect("add state subscription");
    state_event(&mut ui);
    ui.request(
        Op::Subscribe,
        &json!({"topics":["state","state","control"]}),
    )
    .expect("idempotent subscription");
    fixture
        .server
        .control(quartz_protocol::ControlOp::UiShutdown);
    let Envelope::Event(event) = ui.read_envelope().expect("single control") else {
        panic!("control event");
    };
    assert_eq!(event.topic, quartz_protocol::Topic::Control);
    assert!(
        matches!(
            ui.read_envelope(),
            Err(CallCoreError::ResponseTimeout { .. })
        ),
        "no duplicate state or control event"
    );
}

#[test]
fn cli_queries_temporary_database_over_pipe() {
    let fixture = Fixture::new();
    let mut cli = fixture.cli();
    let day: quartz_protocol::QueryDayResponse = serde_json::from_value(
        cli.request(Op::QueryDay, &json!({"local_date":"2025-01-02"}))
            .expect("cli query.day"),
    )
    .expect("typed day body");
    assert_eq!(day.metrics.active_ms, 4000);
    assert_eq!(day.applications.len(), 4);
    let start = day.hours[0].start_utc_ms;
    let timeline: quartz_protocol::QueryTimelineResponse = serde_json::from_value(
        cli.request(
            Op::QueryTimeline,
            &json!({"start_utc_ms":start,"end_utc_ms":start+5000,"detail":"raw","ms_per_pixel":1}),
        )
        .expect("timeline"),
    )
    .expect("typed timeline");
    assert_eq!(timeline.segments.len(), 5);
    let pending_body = json!({"local_date":"2025-01-02"});
    for mut client in [fixture.cli(), fixture.ui()] {
        let range: quartz_protocol::QueryRangeResponse = serde_json::from_value(client.request(Op::QueryRange, &json!({"start_date":"2025-01-02","end_date":"2025-01-03","bucket":"day","dimensions":["category","app","document"]})).expect("range query")).expect("typed range");
        assert_eq!(range.buckets[0].active_ms, 4000);
        assert_eq!(range.buckets[0].groups.len(), 9);
        for body in [
            json!({"start_date":"2025-01-02","end_date":"2025-01-03","bucket":"day","dimensions":[]}),
            json!({"start_date":"2025-01-02","end_date":"2025-01-03","bucket":"day","dimensions":["app","app"]}),
            json!({"start_date":"2025-01-02","end_date":"2025-01-02","bucket":"day","dimensions":["app"]}),
            json!({"start_date":"2025-01-02","end_date":"2025-01-03","bucket":"day","dimensions":["app"],"unknown":true}),
        ] {
            assert!(matches!(
                client.request(Op::QueryRange, &body),
                Err(CallCoreError::Protocol {
                    kind: ErrorKind::BadRequest
                })
            ));
        }
        let pending: quartz_protocol::QueryPendingAwayResponse = serde_json::from_value(
            client
                .request(Op::QueryPendingAway, &pending_body)
                .expect("pending away query"),
        )
        .expect("typed pending away");
        assert!(pending.ranges.is_empty());
    }
    for mut client in [fixture.cli(), fixture.ui()] {
        let facts = client
            .facts(&quartz_protocol::QueryFactsRequest {
                local_date: "2025-01-02".parse().expect("date"),
                after: None,
                limit: None,
            })
            .expect("facts query");
        assert_eq!(facts.facts.len(), 6, "run, clock and four foreground rows");
    }
}
#[test]
fn overloaded_database_returns_busy_over_pipe() {
    let channel = crate::query::dispatch::QueryChannel::new();
    channel.saturate(crate::query::QueryRequest::Day(
        quartz_protocol::QueryDayRequest {
            local_date: "2025-01-02".parse().expect("date"),
        },
    ));
    let fixture = Fixture::with_query_port(
        server::ConnectionTimeouts::default(),
        Some(channel.port.clone()),
    );
    assert!(matches!(
        fixture
            .cli()
            .request(Op::QueryDay, &json!({"local_date":"2025-01-02"})),
        Err(CallCoreError::Protocol {
            kind: ErrorKind::Busy
        })
    ));
}
#[test]
fn day_subscription_pushes_latest_committed_version_after_throttle() {
    let fixture = Fixture::new();
    let mut ui = fixture.ui();
    ui.request(Op::Subscribe, &json!({"topics":["day:2025-01-02"]}))
        .expect("subscribe day");
    let Envelope::Event(initial) = ui.read_envelope().expect("initial day version") else {
        panic!("day event")
    };
    let first: quartz_protocol::DayChanged =
        serde_json::from_value(initial.body).expect("day changed");
    let received_at = std::time::Instant::now();
    for _ in 0..3 {
        fixture.database.append();
    }
    assert!(
        matches!(
            ui.read_envelope(),
            Err(CallCoreError::ResponseTimeout { .. })
        ),
        "day push must stay pending during the throttle interval"
    );
    fixture.database.append();
    std::thread::sleep(crate::ipc::outbox::DAY_PUSH_INTERVAL);
    let Envelope::Event(event) = ui.read_envelope().expect("committed day push") else {
        panic!("day event")
    };
    assert!(received_at.elapsed() >= crate::ipc::outbox::DAY_PUSH_INTERVAL);
    assert_eq!(event.topic, quartz_protocol::Topic::Day(first.date));
    let changed: quartz_protocol::DayChanged =
        serde_json::from_value(event.body).expect("day changed body");
    let expected = fixture
        .database
        .versions
        .updates
        .borrow()
        .days
        .get(&first.date)
        .expect("latest committed version")
        .clone();
    assert_eq!(changed, expected);
    assert_ne!(first.versions, changed.versions);
}

#[test]
fn core_runtime_with_fake_ports_flushes_shutdown_over_test_pipe() {
    use crate::core::{
        app::{CoreRuntime, ObservedEvent, PlatformPorts, RuntimeExit},
        foreground::{ForegroundResolve, Resolution, Snapshot},
        sources::{ClockReader, ReadClockError},
    };
    use crate::store::capture::{
        WindowsSessionId,
        test_support::{Inspection, at},
    };
    use quartz_domain::capture::{CaptureEvent, RunEndReason, SessionStatus, Timestamp};

    struct TestClock;
    impl ClockReader for TestClock {
        fn read(&self) -> Result<Timestamp, ReadClockError> {
            Ok(at(100))
        }
    }
    struct TestResolver;
    impl ForegroundResolve for TestResolver {
        fn resolve(&mut self, _snapshot: Snapshot) -> Resolution {
            Resolution::Review
        }
    }
    let temporary = tempfile::tempdir().expect("isolated runtime database");
    let name = PipeName::for_test(windows::core::GUID::new().expect("unique pipe name"));
    let server_name = name.clone();
    let runtime = CoreRuntime::start(
        &crate::data_directory::DataDirectory::new(temporary.path()),
        PlatformPorts {
            wake: Arc::new(|| {}),
            ui_wake: Arc::new(|_| Ok(())),
            clock: Arc::new(TestClock),
            session_id: WindowsSessionId::new(7),
            proposed_device_id: "runtime-test-device-guid".into(),
            instance_id: quartz_protocol::InstanceId("runtime-test-instance-guid".into()),
            foreground: Box::new(|| Ok(Box::new(TestResolver))),
            pipe: Box::new(move |ports| {
                Ok(Box::new(PipeServer::start(server::PipeServerConfig {
                    shutdown_timeout: ports.shutdown_timeout,
                    owner: ProcessIdentity::current()?,
                    name: server_name,
                    timeouts: server::ConnectionTimeouts::default(),
                    services: ports.services,
                })?))
            }),
            language: quartz_i18n::Language::English,
            locale: None,
        },
    )
    .expect("start runtime with isolated named pipe");
    runtime
        .sender()
        .observe(ObservedEvent {
            at: at(100),
            event: CaptureEvent::Session(SessionStatus::Ready(Default::default())),
        })
        .expect("inject observation");
    let mut ui = CoreClient::connect_named(
        &name,
        HelloRole::Ui {
            ui: UiHello {
                state: UiState::Starting,
            },
        },
    )
    .expect("UI handshake over test pipe");
    let reader = std::thread::spawn(move || ui.read_envelope().expect("shutdown delivered"));
    let stopping = std::time::Instant::now();
    runtime
        .shutdown(RuntimeExit::UserExit)
        .expect("joined core runtime");
    assert!(
        stopping.elapsed() < crate::core::app::SHUTDOWN_TIMEOUT,
        "runtime shutdown completes within budget"
    );
    let Envelope::Event(event) = reader.join().expect("UI reader joined") else {
        panic!("expected shutdown control");
    };
    let control: quartz_protocol::ControlBody =
        serde_json::from_value(event.body).expect("typed shutdown control");
    assert_eq!(control.op, quartz_protocol::ControlOp::UiShutdown);
    let mut stored = Inspection::open(temporary.path());
    assert_eq!(stored.run(1).end_reason, Some(RunEndReason::UserExit));
    assert_eq!(stored.open_clock_count(), 0);
}

#[test]
fn unsubscribe_stops_state_pushes_and_keeps_implicit_controls() {
    let fixture = Fixture::new();
    let mut ui = fixture.ui();
    let connection = fixture.ui_registration();
    ui.request(Op::Subscribe, &json!({"topics":["state"]}))
        .expect("subscribe state");
    state_event(&mut ui);
    for _ in 0..2 {
        ui.request(Op::Unsubscribe, &json!({"topics":["state","control"]}))
            .expect("idempotent unsubscribe");
    }
    publish_change(&fixture.status, 1);
    fixture
        .server
        .target_control(connection, quartz_protocol::ControlOp::UiShow);
    let Envelope::Event(event) = ui.read_envelope().expect("implicit show") else {
        panic!("control event");
    };
    assert_eq!(event.topic, quartz_protocol::Topic::Control);
    let control: quartz_protocol::ControlBody =
        serde_json::from_value(event.body).expect("control body");
    assert_eq!(control.op, quartz_protocol::ControlOp::UiShow);
    assert!(
        matches!(
            ui.read_envelope(),
            Err(CallCoreError::ResponseTimeout { .. })
        ),
        "state remains unsubscribed"
    );
}

#[test]
fn unsubscribe_removes_state_and_preserves_implicit_ui_control() {
    let fixture = Fixture::new();
    let mut ui = fixture.ui();
    let connection = fixture.ui_registration();
    ui.request(Op::Subscribe, &json!({"topics":["state"]}))
        .expect("subscribe state");
    state_event(&mut ui);
    ui.request(Op::Unsubscribe, &json!({"topics":["state","control"]}))
        .expect("unsubscribe state and explicit control");
    ui.request(Op::Unsubscribe, &json!({"topics":["state"]}))
        .expect("idempotent removal");
    publish_change(&fixture.status, 1);
    fixture
        .server
        .target_control(connection, quartz_protocol::ControlOp::UiShow);
    let Envelope::Event(event) = ui.read_envelope().expect("implicit show control") else {
        panic!("control event");
    };
    assert_eq!(event.topic, quartz_protocol::Topic::Control);
    let control: quartz_protocol::ControlBody =
        serde_json::from_value(event.body).expect("typed control");
    assert_eq!(control.op, quartz_protocol::ControlOp::UiShow);
    assert!(
        matches!(
            ui.read_envelope(),
            Err(CallCoreError::ResponseTimeout { .. })
        ),
        "unsubscribed state must not be pushed"
    );
    ui.request(Op::Subscribe, &json!({"topics":["state"]}))
        .expect("subscribe again");
    assert_eq!(
        state_event(&mut ui),
        fixture.status.subscribe().borrow().snapshot()
    );
}
