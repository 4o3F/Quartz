//! Tokio pipe connection retry and mutual server authentication.

use super::{CheckPeerError, PipeName, ProcessIdentity, win32_error::Win32ErrorCode};
use snafu::{ResultExt, Snafu};
use std::{io, os::windows::io::AsRawHandle, time::Duration};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use windows::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_PIPE_BUSY, HANDLE},
    System::Pipes::GetNamedPipeServerProcessId,
    UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow},
};

/// Bounds CLI discovery and second-launch forwarding (04 §1, 01 §3).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Retry busy instances using Tokio's named-pipe client pattern (04 §1).
const RETRY_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Snafu)]
#[snafu(module)]
/// Discovery, transport, or peer authentication failure.
pub enum OpenCorePipeError {
    #[snafu(display("Quartz core is not running"))]
    CoreNotRunning { source: io::Error },
    #[snafu(display("could not connect to Quartz core"))]
    Connect { source: io::Error },
    #[snafu(display("Quartz core pipe remained busy until the connection deadline"))]
    Busy { source: tokio::time::error::Elapsed },
    #[snafu(display("could not verify Quartz core identity"))]
    ServerIdentity { source: CheckPeerError },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerExpectation {
    SameExecutable,
    AdjacentQuartz,
}

#[derive(Debug)]
/// Authenticated Windows client transport owned by its runtime thread.
pub struct PipeClient {
    pipe: NamedPipeClient,
}
impl PipeClient {
    /// Discovers the current user's core and verifies its process identity.
    ///
    /// # Errors
    /// Returns absent, busy, transport or server identity failures.
    pub async fn connect() -> Result<Self, OpenCorePipeError> {
        Self::discover_as(
            ProcessIdentity::pipe_name,
            ServerExpectation::SameExecutable,
        )
        .await
    }
    /// Discovers the current user's core from `quartz-host.exe` and verifies the adjacent `quartz.exe`.
    ///
    /// # Errors
    /// Returns absent, busy, transport or server identity failures.
    pub async fn connect_host() -> Result<Self, OpenCorePipeError> {
        Self::discover_as(
            ProcessIdentity::pipe_name,
            ServerExpectation::AdjacentQuartz,
        )
        .await
    }
    #[cfg(test)]
    async fn discover(
        name: impl FnOnce(&ProcessIdentity) -> PipeName,
    ) -> Result<Self, OpenCorePipeError> {
        Self::discover_as(name, ServerExpectation::SameExecutable).await
    }
    async fn discover_as(
        name: impl FnOnce(&ProcessIdentity) -> PipeName,
        expectation: ServerExpectation,
    ) -> Result<Self, OpenCorePipeError> {
        let owner =
            ProcessIdentity::current().context(open_core_pipe_error::ServerIdentitySnafu)?;
        Self::open_as(&name(&owner), &owner, expectation).await
    }
    /// Opens a named endpoint with the same authentication and deadlines as discovery.
    ///
    /// # Errors
    /// Returns absent, busy, transport or server identity failures.
    pub async fn open_named(name: &PipeName) -> Result<Self, OpenCorePipeError> {
        let owner =
            ProcessIdentity::current().context(open_core_pipe_error::ServerIdentitySnafu)?;
        Self::open_as(name, &owner, ServerExpectation::SameExecutable).await
    }
    async fn open_as(
        name: &PipeName,
        owner: &ProcessIdentity,
        expectation: ServerExpectation,
    ) -> Result<Self, OpenCorePipeError> {
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
        match expectation {
            ServerExpectation::SameExecutable => owner.check_server(pid),
            ServerExpectation::AdjacentQuartz => owner.check_server_for_host(pid),
        }
        .context(open_core_pipe_error::ServerIdentitySnafu)?;
        Ok(Self { pipe })
    }
    /// Transfers the authenticated pipe to the protocol client.
    pub fn into_inner(self) -> NamedPipeClient {
        self.pipe
    }
}

/// Hands the launching process's foreground right to any process before forwarding `ui.open`.
pub(crate) fn allow_any_foreground_window() {
    // SAFETY: ASFW_ANY is a Win32 sentinel value with no pointer or lifetime preconditions.
    let permission = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    if let Err(error) = permission {
        tracing::debug!(%error, "could not grant foreground permission before opening UI");
    }
}

pub type CoreClient = crate::CoreClient<NamedPipeClient>;
impl CoreClient {
    /// Connects and handshakes as a CLI client on a caller-thread runtime.
    ///
    /// # Errors
    /// Returns runtime, discovery, authentication or handshake failures.
    pub fn connect() -> Result<Self, crate::CallCoreError> {
        Self::open(
            async {
                PipeClient::connect()
                    .await
                    .map(PipeClient::into_inner)
                    .map_err(|source| crate::CallCoreError::Open { source })
            },
            Some(quartz_protocol::HelloRole::Cli),
        )
    }
    #[cfg(any(test, feature = "test-util"))]
    /// Connects to an isolated test endpoint and declares the supplied role.
    pub fn connect_named(
        name: &PipeName,
        role: quartz_protocol::HelloRole,
    ) -> Result<Self, crate::CallCoreError> {
        Self::open(
            async {
                PipeClient::open_named(name)
                    .await
                    .map(PipeClient::into_inner)
                    .map_err(|source| crate::CallCoreError::Open { source })
            },
            Some(role),
        )
    }
    #[cfg(any(test, feature = "test-util"))]
    /// Opens an isolated test endpoint without a protocol handshake.
    pub fn open_named(name: &PipeName) -> Result<Self, crate::CallCoreError> {
        Self::open(
            async {
                PipeClient::open_named(name)
                    .await
                    .map(PipeClient::into_inner)
                    .map_err(|source| crate::CallCoreError::Open { source })
            },
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::windows::named_pipe::ServerOptions;

    #[tokio::test(start_paused = true)]
    async fn busy_pipe_retries_until_a_slot_is_available_and_honors_discovery_deadline() {
        let name = PipeName::for_test(windows::core::GUID::new().expect("isolated pipe"));
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)
            .expect("first instance");
        let first = PipeClient::open_named(&name)
            .await
            .expect("occupy first slot");
        server.connect().await.expect("first connection");
        let retry_name = name.clone();
        let retrying = tokio::spawn(async move { PipeClient::open_named(&retry_name).await });
        tokio::time::sleep(RETRY_INTERVAL * 2).await;
        assert!(!retrying.is_finished(), "busy pipe must remain pending");
        let next = ServerOptions::new()
            .reject_remote_clients(true)
            .create(&name)
            .expect("new slot");
        let second = retrying
            .await
            .expect("retry task")
            .expect("busy retry discovers new slot");
        next.connect().await.expect("second connection");
        let started = tokio::time::Instant::now();
        assert!(matches!(
            PipeClient::open_named(&name).await,
            Err(OpenCorePipeError::Busy { .. })
        ));
        assert_eq!(started.elapsed(), CONNECT_TIMEOUT);
        drop(second);
        drop(first);
    }
    #[tokio::test]
    async fn discovery_reads_identity_once_and_reuses_it_for_authentication() {
        let name = PipeName::for_test(windows::core::GUID::new().expect("isolated pipe"));
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)
            .expect("test pipe");
        let reads = ProcessIdentity::current_identity_reads();
        let client = PipeClient::discover(|_| name.clone())
            .await
            .expect("authenticated client");
        server.connect().await.expect("connection");
        assert_eq!(
            ProcessIdentity::current_identity_reads(),
            reads + 1,
            "discovery and authentication must share one identity read"
        );
        drop(client);
    }
}
