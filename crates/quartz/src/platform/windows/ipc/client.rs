//! Tokio pipe connection retry and mutual server authentication.

use super::{CheckPeerError, PipeName, ProcessIdentity, win32_error::Win32ErrorCode};
use snafu::{ResultExt, Snafu};
use std::{io, os::windows::io::AsRawHandle, time::Duration};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use windows::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_PIPE_BUSY, HANDLE},
    System::Pipes::GetNamedPipeServerProcessId,
};

/// Bounds CLI discovery and second-launch forwarding (04 §1, 01 §3).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Retry busy instances using Tokio's named-pipe client pattern (04 §1).
const RETRY_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum OpenCorePipeError {
    #[snafu(display("Quartz core is not running"))]
    CoreNotRunning { source: io::Error },
    #[snafu(display("could not connect to Quartz core"))]
    Connect { source: io::Error },
    #[snafu(display("Quartz core pipe remained busy until the connection deadline"))]
    Busy { source: tokio::time::error::Elapsed },
    #[snafu(display("could not verify Quartz core identity"))]
    ServerIdentity { source: CheckPeerError },
}

#[derive(Debug)]
pub(crate) struct PipeClient {
    pipe: NamedPipeClient,
}
impl PipeClient {
    pub(crate) async fn connect() -> Result<Self, OpenCorePipeError> {
        let owner =
            ProcessIdentity::current().context(open_core_pipe_error::ServerIdentitySnafu)?;
        Self::open_named(&owner.pipe_name()).await
    }
    pub(crate) async fn open_named(name: &PipeName) -> Result<Self, OpenCorePipeError> {
        let owner =
            ProcessIdentity::current().context(open_core_pipe_error::ServerIdentitySnafu)?;
        let pipe = tokio::time::timeout(CONNECT_TIMEOUT, async {
            loop {
                match ClientOptions::new().open(name) {
                    Ok(pipe) => return Ok(pipe),
                    Err(source)
                        if source.is_win32(ERROR_FILE_NOT_FOUND)
                            || source.is_win32(ERROR_PATH_NOT_FOUND) =>
                    {
                        return Err(OpenCorePipeError::CoreNotRunning { source });
                    }
                    Err(error) if error.is_win32(ERROR_PIPE_BUSY) => {
                        tokio::time::sleep(RETRY_INTERVAL).await
                    }
                    Err(source) => return Err(OpenCorePipeError::Connect { source }),
                }
            }
        })
        .await
        .context(open_core_pipe_error::BusySnafu)??;
        let mut pid = 0;
        // SAFETY: The client owns a connected pipe handle, and pid is writable output storage.
        unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }.map_err(
            |source| OpenCorePipeError::ServerIdentity {
                source: CheckPeerError::Windows {
                    call: "GetNamedPipeServerProcessId",
                    source,
                },
            },
        )?;
        owner
            .check_server(pid)
            .context(open_core_pipe_error::ServerIdentitySnafu)?;
        Ok(Self { pipe })
    }
    pub(crate) fn into_inner(self) -> NamedPipeClient {
        self.pipe
    }
}

pub(crate) type CoreClient = crate::ipc::client::CoreClient<NamedPipeClient>;
impl CoreClient {
    pub(crate) fn connect() -> Result<Self, crate::ipc::client::CallCoreError> {
        Self::open(
            async {
                PipeClient::connect()
                    .await
                    .map(PipeClient::into_inner)
                    .map_err(crate::ipc::client::CallCoreError::connect)
            },
            Some(quartz_protocol::HelloRole::Cli),
        )
    }
    #[cfg(test)]
    pub(crate) fn connect_named(
        name: &PipeName,
        role: quartz_protocol::HelloRole,
    ) -> Result<Self, crate::ipc::client::CallCoreError> {
        Self::open(
            async {
                PipeClient::open_named(name)
                    .await
                    .map(PipeClient::into_inner)
                    .map_err(crate::ipc::client::CallCoreError::connect)
            },
            Some(role),
        )
    }
    #[cfg(test)]
    pub(crate) fn open_named(name: &PipeName) -> Result<Self, crate::ipc::client::CallCoreError> {
        Self::open(
            async {
                PipeClient::open_named(name)
                    .await
                    .map(PipeClient::into_inner)
                    .map_err(crate::ipc::client::CallCoreError::connect)
            },
            None,
        )
    }
}
