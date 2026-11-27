//! Native Messaging host launched by browsers with browser-defined arguments (05 §2.2).
//!
//! Chrome supplies an origin and optional `--parent-window=N`; Firefox supplies a manifest path
//! and extension ID. Arguments are ignored; all routing uses framed stdin/stdout messages.

#[cfg(any(windows, test))]
mod bridge;
mod framing;
mod protocol;

use std::{
    io::{self, Write},
    process::ExitCode,
};

#[cfg(windows)]
use std::thread;

#[cfg(windows)]
use quartz_pipe::{AsyncCoreClient, PipeClient};
#[cfg(windows)]
use tokio::sync::mpsc;

use framing::{ReadNativeFrameError, read_native_frame, write_native_frame};
use protocol::{encode_response_payload, error_response, parse_browser_hello};

/// Host runtime blocking-thread ceiling (01 §7.2).
#[cfg(windows)]
const HOST_BLOCKING_THREADS: usize = 1;

fn main() -> ExitCode {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();

    let first_payload = match read_native_frame(&mut stdin) {
        Ok(Some(payload)) => payload,
        Ok(None) => return ExitCode::SUCCESS,
        Err(ReadNativeFrameError::FrameTooLarge { .. }) => {
            let payload = encode_response_payload(&error_response(
                None,
                quartz_protocol::ErrorKind::FrameTooLarge,
            ));
            let _ = write_native_frame(&mut stdout, &payload);
            return ExitCode::SUCCESS;
        }
        Err(_) => return ExitCode::SUCCESS,
    };

    let parsed_hello = match parse_browser_hello(&first_payload) {
        Ok(hello) => hello,
        Err(error) => {
            let payload = encode_response_payload(&error.into_response());
            let _ = write_native_frame(&mut stdout, &payload);
            return ExitCode::SUCCESS;
        }
    };

    drop(stdin);
    run_connected(parsed_hello, &mut stdout)
}

#[cfg(windows)]
fn run_connected(hello: protocol::ParsedBrowserHello, stdout: &mut impl Write) -> ExitCode {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .max_blocking_threads(HOST_BLOCKING_THREADS)
        .build()
    else {
        return ExitCode::FAILURE;
    };

    let pipe = match runtime.block_on(PipeClient::connect_host()) {
        Ok(pipe) => pipe,
        Err(_) => {
            let _ = stdout.write_all(&protocol::core_offline_frame());
            let _ = stdout.flush();
            return ExitCode::SUCCESS;
        }
    };

    let (stdin_tx, stdin_rx) = mpsc::channel(bridge::STDIN_CHANNEL_CAPACITY);
    let _reader = thread::Builder::new()
        .name("quartz-host-stdin".into())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            loop {
                match read_native_frame(&mut stdin) {
                    Ok(Some(payload)) => {
                        if stdin_tx.blocking_send(Ok(payload)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = stdin_tx.blocking_send(Err(error));
                        break;
                    }
                }
            }
        });

    let core = AsyncCoreClient::new(pipe.into_inner());
    runtime.block_on(bridge::run_host_bridge(hello, core, stdin_rx, stdout));
    runtime.shutdown_background();
    ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn run_connected(_hello: protocol::ParsedBrowserHello, stdout: &mut impl Write) -> ExitCode {
    let _ = stdout.write_all(&protocol::core_offline_frame());
    let _ = stdout.flush();
    ExitCode::SUCCESS
}
