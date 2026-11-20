//! Windows named-pipe transport and peer authentication.

mod client;
mod identity;
mod pipe_name;
mod security;
pub(super) mod server;
mod win32_error;

pub(crate) use identity::{CheckPeerError, ProcessIdentity};
pub(crate) use security::CreatePipeSecurityError;
pub(crate) use server::{PipeServer, PipeServerConfig, StartPipeServerError};

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use client::CONNECT_TIMEOUT;
pub(crate) use client::{OpenCorePipeError, PipeClient};

pub(crate) use pipe_name::PipeName;

#[cfg(test)]
pub(crate) use server::ConnectionTimeouts;
