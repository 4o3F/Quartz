//! Asynchronous forwarding loop between the browser stdio frames and `AsyncCoreClient`.

use std::io::Write;

use quartz_pipe::{AsyncCoreClient, CallCoreError, IncomingFrame, RESPONSE_TIMEOUT};
use quartz_protocol::{ErrorKind, Op, Response, ResponseOutcome};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};

use crate::{
    framing::{ReadNativeFrameError, write_native_frame},
    protocol::{
        ParsedBrowserHello, RequestIdMap, encode_response_payload, error_response,
        validate_forward_request,
    },
};

/// Bounded channel capacity between the blocking stdin reader and the async forwarder.
pub(crate) const STDIN_CHANNEL_CAPACITY: usize = 32;

/// Performs the core `hello` handshake and forwards `browser.state` traffic until disconnect.
///
/// Never writes anything other than length-prefixed JSON protocol frames to `output`.
pub(crate) async fn run_host_bridge<T, W>(
    hello: ParsedBrowserHello,
    mut core: AsyncCoreClient<T>,
    mut stdin_rx: mpsc::Receiver<Result<Vec<u8>, ReadNativeFrameError>>,
    output: &mut W,
) where
    T: AsyncRead + AsyncWrite + Unpin,
    W: Write,
{
    let (ext_hello_id, core_hello) = hello.into_core_hello();
    let Ok(hello_body) = serde_json::to_value(&core_hello) else {
        let _ = write_response(
            output,
            &error_response(Some(ext_hello_id), ErrorKind::Internal),
        );
        return;
    };
    let Ok(core_hello_id) = core.send_request(Op::Hello, hello_body).await else {
        return;
    };
    let hello_response = match tokio::time::timeout(RESPONSE_TIMEOUT, core.receive()).await {
        Ok(Ok(IncomingFrame::Response(response))) if response.id == Some(core_hello_id) => response,
        Ok(Err(CallCoreError::ConnectionProtocol { kind })) => {
            let _ = write_response(output, &error_response(None, kind));
            return;
        }
        _ => return,
    };
    let remapped_hello = Response {
        id: Some(ext_hello_id),
        outcome: hello_response.outcome,
    };
    let succeeded = matches!(remapped_hello.outcome, ResponseOutcome::Ok { .. });
    if write_response(output, &remapped_hello).is_err() || !succeeded {
        return;
    }

    let mut id_map = RequestIdMap::default();
    loop {
        tokio::select! {
            inbound = stdin_rx.recv() => {
                match inbound {
                    None => break,
                    Some(Err(ReadNativeFrameError::FrameTooLarge { .. })) => {
                        let _ = write_response(output, &error_response(None, ErrorKind::FrameTooLarge));
                        break;
                    }
                    Some(Err(_)) => break,
                    Some(Ok(payload)) => {
                        let request = match validate_forward_request(&payload) {
                            Ok(request) => request,
                            Err(error) => {
                                let closes = error.closes_connection();
                                if write_response(output, &error.into_response()).is_err() || closes {
                                    break;
                                }
                                continue;
                            }
                        };
                        if id_map.is_full() {
                            if write_response(output, &error_response(Some(request.ext_id), ErrorKind::Busy)).is_err() {
                                break;
                            }
                            continue;
                        }
                        let Ok(core_id) = core.send_request(request.op, request.body).await else {
                            break;
                        };
                        if id_map.insert(core_id, request.ext_id).is_err() {
                            break;
                        }
                    }
                }
            }
            incoming = core.receive() => {
                match incoming {
                    Ok(IncomingFrame::Response(response)) => {
                        if let Some(remapped) = id_map.remap_response(response)
                            && write_response(output, &remapped).is_err()
                        {
                            break;
                        }
                    }
                    Ok(IncomingFrame::Event(_) | IncomingFrame::Progress(_)) => {}
                    Err(CallCoreError::ConnectionProtocol { kind }) => {
                        let _ = write_response(output, &error_response(None, kind));
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

fn write_response<W: Write>(output: &mut W, response: &Response) -> Result<(), ()> {
    let payload = encode_response_payload(response);
    write_native_frame(output, &payload).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        framing::{FRAME_HEADER_LEN, encode_native_frame, read_native_frame},
        protocol::{BROWSER_HELLO_OP, parse_browser_hello},
    };
    use quartz_protocol::{
        BrowserKind, Envelope, PROTOCOL_VERSION, RequestId, decode_json, encode_json,
    };
    use std::io::Cursor;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TEST_UUID: &str = "a286737e-3855-43cb-98ee-c366344372d0";

    async fn read_core_envelope(peer: &mut tokio::io::DuplexStream) -> Envelope {
        let mut header = [0u8; FRAME_HEADER_LEN];
        peer.read_exact(&mut header)
            .await
            .expect("read core frame header");
        let len = usize::try_from(u32::from_le_bytes(header)).expect("frame len fits usize");
        let mut payload = vec![0u8; len];
        peer.read_exact(&mut payload)
            .await
            .expect("read core frame payload");
        decode_json(&payload).expect("decode core envelope")
    }

    async fn write_core_envelope(peer: &mut tokio::io::DuplexStream, envelope: &Envelope) {
        let payload = encode_json(envelope).expect("encode core envelope");
        let wire = encode_native_frame(&payload).expect("frame core envelope");
        peer.write_all(&wire).await.expect("write core transport");
    }

    fn parse_stdout_envelopes(bytes: &[u8]) -> Vec<Envelope> {
        let mut cursor = Cursor::new(bytes);
        let mut out = Vec::new();
        while let Some(payload) = read_native_frame(&mut cursor).expect("read stdout frame") {
            out.push(decode_json(&payload).expect("decode stdout envelope"));
        }
        out
    }

    #[tokio::test]
    async fn bridge_translates_hello_remaps_ids_and_filters_disallowed_ops() {
        let (client_stream, mut core_peer) = tokio::io::duplex(16 * 1024);
        let core = AsyncCoreClient::new(client_stream);
        let (stdin_tx, stdin_rx) = mpsc::channel(STDIN_CHANNEL_CAPACITY);

        let hello_bytes = serde_json::to_vec(&serde_json::json!({
            "t": "req",
            "id": 42,
            "op": BROWSER_HELLO_OP,
            "body": {
                "profile_instance_id": TEST_UUID,
                "browser": "chrome",
                "proto": { "min": 1, "max": 1 }
            }
        }))
        .expect("hello bytes");
        let parsed_hello = parse_browser_hello(&hello_bytes).expect("parse hello");

        let core_task = tokio::spawn(async move {
            // 1. Expect translated core `hello` with role="browser".
            let Envelope::Request(req) = read_core_envelope(&mut core_peer).await else {
                panic!("expected core hello request");
            };
            assert_eq!(req.id, RequestId(1));
            assert_eq!(req.op, Op::Hello);
            assert_eq!(req.body["role"], "browser");
            assert_eq!(req.body["browser"]["browser"], "chrome");
            assert_eq!(req.body["browser"]["profile_instance_id"], TEST_UUID);

            write_core_envelope(
                &mut core_peer,
                &Envelope::Response(Response {
                    id: Some(req.id),
                    outcome: ResponseOutcome::Ok {
                        body: serde_json::json!({
                            "proto": PROTOCOL_VERSION,
                            "core_version": "0.1.0",
                            "instance_id": "test-instance"
                        }),
                    },
                }),
            )
            .await;

            // 2. Expect forwarded `browser.state` with core-allocated id 2.
            let Envelope::Request(state_req) = read_core_envelope(&mut core_peer).await else {
                panic!("expected core browser.state request");
            };
            assert_eq!(state_req.id, RequestId(2));
            assert_eq!(state_req.op, Op::BrowserState);
            assert_eq!(state_req.body["focused"], true);

            write_core_envelope(
                &mut core_peer,
                &Envelope::Response(Response {
                    id: Some(state_req.id),
                    outcome: ResponseOutcome::Ok {
                        body: serde_json::json!({ "accepted": true }),
                    },
                }),
            )
            .await;
        });

        // Queue a forbidden op (must be rejected locally) followed by a valid browser.state.
        let forbidden = serde_json::to_vec(&serde_json::json!({
            "t": "req",
            "id": 77,
            "op": "core.status",
            "body": {}
        }))
        .expect("forbidden JSON");
        let state = serde_json::to_vec(&serde_json::json!({
            "t": "req",
            "id": 99,
            "op": "browser.state",
            "body": {
                "focused": true,
                "window": { "id": 7, "state": "normal" },
                "tab": {
                    "id": 91,
                    "domain": "github.com",
                    "title": "4o3F/quartz",
                    "title_status": "ok",
                    "audible": false
                }
            }
        }))
        .expect("state JSON");
        stdin_tx.send(Ok(forbidden)).await.expect("queue forbidden");
        stdin_tx.send(Ok(state)).await.expect("queue state");

        let mut stdout = Vec::new();
        // The mock core drops its pipe end after the last response; the bridge drains that
        // response before seeing the disconnect. Stdin stays open until the bridge returns.
        run_host_bridge(parsed_hello, core, stdin_rx, &mut stdout).await;
        core_task.await.expect("core task");
        drop(stdin_tx);

        let frames = parse_stdout_envelopes(&stdout);
        assert_eq!(frames.len(), 3);

        // Hello response mapped back to extension ID 42.
        let Envelope::Response(hello_res) = &frames[0] else {
            panic!("expected hello response");
        };
        assert_eq!(hello_res.id, Some(RequestId(42)));
        assert!(matches!(hello_res.outcome, ResponseOutcome::Ok { .. }));

        // Forbidden op rejected locally with extension ID 77.
        let Envelope::Response(forbidden_res) = &frames[1] else {
            panic!("expected forbidden response");
        };
        assert_eq!(forbidden_res.id, Some(RequestId(77)));
        assert_eq!(
            forbidden_res.outcome,
            ResponseOutcome::Err {
                error: quartz_protocol::ProtocolError::new(ErrorKind::Forbidden),
            }
        );

        // State response mapped back to extension ID 99.
        let Envelope::Response(state_res) = &frames[2] else {
            panic!("expected state response");
        };
        assert_eq!(state_res.id, Some(RequestId(99)));
        assert_eq!(
            state_res.outcome,
            ResponseOutcome::Ok {
                body: serde_json::json!({ "accepted": true }),
            }
        );
    }

    #[tokio::test]
    async fn bridge_exits_when_core_disconnects() {
        let (client_stream, mut core_peer) = tokio::io::duplex(8 * 1024);
        let core = AsyncCoreClient::new(client_stream);
        let (_stdin_tx, stdin_rx) = mpsc::channel(STDIN_CHANNEL_CAPACITY);

        let hello_bytes = serde_json::to_vec(&serde_json::json!({
            "t": "req",
            "id": 1,
            "op": BROWSER_HELLO_OP,
            "body": {
                "profile_instance_id": TEST_UUID,
                "browser": BrowserKind::Firefox,
                "proto": { "min": 1, "max": 1 }
            }
        }))
        .expect("hello bytes");
        let parsed_hello = parse_browser_hello(&hello_bytes).expect("parse hello");

        let core_task = tokio::spawn(async move {
            let Envelope::Request(req) = read_core_envelope(&mut core_peer).await else {
                panic!("expected core hello");
            };
            write_core_envelope(
                &mut core_peer,
                &Envelope::Response(Response {
                    id: Some(req.id),
                    outcome: ResponseOutcome::Ok {
                        body: serde_json::json!({
                            "proto": PROTOCOL_VERSION,
                            "core_version": "0.1.0",
                            "instance_id": "test-instance"
                        }),
                    },
                }),
            )
            .await;
            drop(core_peer);
        });

        let mut stdout = Vec::new();
        run_host_bridge(parsed_hello, core, stdin_rx, &mut stdout).await;
        core_task.await.expect("core task");

        let frames = parse_stdout_envelopes(&stdout);
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            &frames[0],
            Envelope::Response(Response {
                id: Some(RequestId(1)),
                outcome: ResponseOutcome::Ok { .. },
            })
        ));
    }
}
