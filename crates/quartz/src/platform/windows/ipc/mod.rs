//! Windows pipe server, DACL and session integration.
mod security;
pub(super) mod server;
mod win32_error;

pub(crate) use quartz_pipe::{CheckPeerError, PipeName, ProcessIdentity};
#[cfg(test)]
pub(crate) type CoreClient =
    quartz_pipe::CoreClient<tokio::net::windows::named_pipe::NamedPipeClient>;
#[cfg(test)]
pub(crate) use quartz_pipe::{CONNECT_TIMEOUT, OpenCorePipeError};
pub(crate) use security::CreatePipeSecurityError;
pub(crate) use server::{PipeServer, PipeServerConfig};
#[cfg(test)]
mod tests;
