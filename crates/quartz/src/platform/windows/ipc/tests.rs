//! Real in-process pipe tests use random names and never start the production core.

use super::*;
use crate::ipc::{
    StatusPublisher, UiCommand,
    client::{CallCoreError, CoreClient},
};
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
}
impl Fixture {
    fn new() -> Self {
        Self::with_timeouts(server::ConnectionTimeouts::default())
    }
    fn with_timeouts(timeouts: server::ConnectionTimeouts) -> Self {
        let suffix = windows::core::GUID::new().expect("unique pipe suffix");
        let name = PipeName::for_test(suffix);
        let status = StatusPublisher::new(quartz_protocol::InstanceId(format!("{suffix:?}")));
        let (commands, received) = bounded(8);
        let port: crate::ipc::UiPort = Arc::new(move |command| {
            commands
                .try_send(command)
                .map_err(|_| crate::ipc::QueueUiError)
        });
        let server = PipeServer::start_configured(
            server::PipeServerConfig {
                name: name.clone(),
                timeouts,
            },
            ProcessIdentity::current().expect("identity"),
            status.subscribe(),
            port,
        )
        .expect("start test server");
        Self {
            server,
            name,
            status,
            commands: received,
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
        matches!(client.read_envelope(), Err(CallCoreError::Io { source } | CallCoreError::DecodeFrame { source }) if source.kind() != std::io::ErrorKind::TimedOut),
        "server must close after final error"
    );
}

#[test]
fn handshake_status_and_ui_open_delivery() {
    let fixture = Fixture::new();
    let mut cli = fixture.cli();
    let status = cli.status().expect("status");
    assert_eq!(status, *fixture.status.subscribe().borrow());
    cli.open_ui().expect("accepted open");
    assert_eq!(
        fixture
            .commands
            .recv_timeout(crate::ipc::client::CONNECT_TIMEOUT)
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
            .recv_timeout(crate::ipc::client::CONNECT_TIMEOUT)
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
        ui.request(Op::QueryDay, &json!({})),
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
        Err(CallCoreError::Connect {
            source: OpenCorePipeError::Busy { .. }
        })
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
    let result = PipeServer::start_named(
        fixture.name.clone(),
        ProcessIdentity::current().expect("identity"),
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
        Err(CallCoreError::Connect {
            source: OpenCorePipeError::CoreNotRunning { .. }
        })
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
