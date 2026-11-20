//! Real in-process pipe tests use random names and never start the production core.

use super::*;
use crate::ipc::{StatusPublisher, UiCommand, client::CallCoreError};
use crossbeam_channel::{Receiver, bounded};
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
        let server = PipeServer::start(
            server::PipeServerConfig {
                shutdown_timeout: crate::core::app::SHUTDOWN_TIMEOUT,
                owner: ProcessIdentity::current().expect("identity"),
                name: name.clone(),
                timeouts,
                queries: query_port.unwrap_or_else(|| database.port.clone()),
                versions: database.versions.clone(),
            },
            status.subscribe(),
            port,
        )
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
            Err(CallCoreError::Decode { source }) => source
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() != std::io::ErrorKind::TimedOut),
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
        UiCommand::FirstFrame
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
        ui.request(Op::QueryRange, &json!({})),
        Err(CallCoreError::Protocol {
            kind: ErrorKind::NotImplemented
        })
    ));
}
#[test]
fn host_role_is_rejected_and_connection_closed() {
    let fixture = Fixture::new();
    let mut client = fixture.raw();
    let mut request = hello();
    request["body"]["role"] = json!("host");
    request["body"]["browser"] = json!({"profile_instance_id":"12345678-1234-1234-1234-123456789abc","browser_session_id":"12345678-1234-1234-1234-123456789abc","epoch":1,"proto":{"min":1,"max":1},"browser":"edge","extension_id":"test"});
    assert_error(
        exchange(&mut client, request),
        Some(RequestId(1)),
        ErrorKind::Forbidden,
    );
    assert_closed(&mut client);
}
#[test]
fn eight_clients_hold_all_slots_and_disconnect_releases_one() {
    let fixture = Fixture::new();
    let mut clients: Vec<_> = (0..8).map(|_| fixture.cli()).collect();
    assert!(matches!(
        CoreClient::open_named(&fixture.name),
        Err(CallCoreError::Connect { source }) if matches!(source.downcast_ref::<OpenCorePipeError>(), Some(OpenCorePipeError::Busy { .. }))
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
    let result = PipeServer::start(
        server::PipeServerConfig {
            shutdown_timeout: crate::core::app::SHUTDOWN_TIMEOUT,
            owner: ProcessIdentity::current().expect("identity"),
            name: fixture.name.clone(),
            timeouts: server::ConnectionTimeouts::default(),
            queries: fixture.database.port.clone(),
            versions: fixture.database.versions.clone(),
        },
        fixture.status.subscribe(),
        port,
    );
    assert!(matches!(
        result,
        Err(StartPipeServerError::NameOccupied { .. })
    ));
    assert!(fixture.cli().status().is_ok());
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
        Err(CallCoreError::Connect { source }) if matches!(source.downcast_ref::<OpenCorePipeError>(), Some(OpenCorePipeError::CoreNotRunning { .. }))
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

/// Short transport stress substitutes for the ten-minute M2-F acceptance run (04 §9).
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
    let row = OpenInterval {
        id: RowId::from_persisted(1),
        segment: SegmentId::from_persisted(1),
        session: SessionKey::from_persisted(1),
        start: Timestamp {
            utc: UtcInstant::try_from(123).expect("test UTC"),
            mono: MonoInstant::from_millis(0),
        },
        fact: IntervalFact::Window {
            app: Some(AppIdentity::Exe("c:\\app.exe".into())),
            instance: Some(WindowId::from_persisted(1)),
            fields: CapturedFields {
                level: CaptureLevel::Full,
                title: Some(format!("{change}{}", "界".repeat(STRESS_TITLE_CHARACTERS))),
                title_status: TitleStatus::Ok,
                privacy: None,
                web: quartz_domain::rules::WebAction::None,
            },
        },
    };
    let tracking = CaptureState::Tracking(row.fact.clone());
    publisher.publish_interval(Some(row));
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

#[test]
fn stalled_state_reader_never_blocks_publication_or_another_clients_status() {
    let fixture = Fixture::new();
    let mut slow = fixture.ui();
    slow.request(Op::Subscribe, &json!({"topics":["state"]}))
        .expect("subscribe slow reader");
    // Retain the immediate snapshot unread and never read this connection again.
    let mut cli = fixture.cli();
    let started = std::time::Instant::now();
    let mut change = 0;
    while started.elapsed() < SLOW_CLIENT_TEST_DURATION {
        let publishing = std::time::Instant::now();
        publish_large_foreground(&fixture.status, change);
        assert!(
            publishing.elapsed() < PUBLISH_LATENCY_LIMIT,
            "publisher waited on stalled reader"
        );
        let requesting = std::time::Instant::now();
        cli.status().expect("other connection status");
        assert!(
            requesting.elapsed() < STATUS_LATENCY_LIMIT,
            "other connection waited on stalled reader"
        );
        change += 1;
        std::thread::sleep(STRESS_PUBLISH_INTERVAL);
    }
    assert!(change > 10);
    let metrics = fixture.server.metrics();
    assert_eq!(metrics.len(), 2, "both connections remain alive");
    assert!(
        metrics.iter().any(|metric| metric.queued > 0),
        "large state frames must actually fill the unread pipe"
    );
    for metric in metrics {
        assert!(metric.queued <= crate::ipc::outbox::SEND_BUFFER_LIMIT);
        assert!(metric.peak <= crate::ipc::outbox::SEND_BUFFER_LIMIT);
    }
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
            Err(CallCoreError::Io { .. } | CallCoreError::ResponseTimeout { .. }) => {}
            Err(error) => panic!("unexpected flood failure: {error}"),
        }
        // A forced server disconnect discards unread responses; drain any frame already buffered locally.
        loop {
            match slow.read_envelope() {
                Ok(_) => {}
                Err(CallCoreError::Io { .. } | CallCoreError::Decode { .. }) => break,
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
fn cli_queries_temporary_database_over_pipe_and_host_cannot_query() {
    let fixture = Fixture::new();
    let mut cli = fixture.cli();
    let day: quartz_protocol::QueryDayResponse = serde_json::from_value(
        cli.request(Op::QueryDay, &json!({"local_date":"2025-01-02"}))
            .expect("cli query.day"),
    )
    .expect("typed day body");
    assert_eq!(day.metrics.expect("metrics").active_ms, 4000);
    assert_eq!(day.applications.len(), 4);
    let start = day.hours.expect("hours")[0].start_utc_ms;
    let timeline: quartz_protocol::QueryTimelineResponse = serde_json::from_value(
        cli.request(
            Op::QueryTimeline,
            &json!({"start_utc_ms":start,"end_utc_ms":start+5000,"detail":"raw","ms_per_pixel":1}),
        )
        .expect("timeline"),
    )
    .expect("typed timeline");
    assert_eq!(timeline.segments.len(), 5);
    assert!(quartz_protocol::Role::Host.authorize(Op::QueryDay).is_err());
    assert!(
        quartz_protocol::Role::Host
            .authorize(Op::QueryTimeline)
            .is_err()
    );
}
#[test]
fn overloaded_database_returns_busy_over_pipe() {
    let channel = crate::query::dispatch::QueryChannel::new();
    channel.saturate(crate::query::QueryRequest::Day(
        quartz_protocol::QueryDayRequest {
            local_date: "2025-01-02".parse().expect("date"),
            cursor: None,
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
