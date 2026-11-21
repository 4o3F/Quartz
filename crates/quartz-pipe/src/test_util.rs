//! Isolated named pipes for downstream protocol tests; never uses the production name or data.

use crate::{AsyncCoreClient, CallCoreError, PipeName};
use quartz_protocol::{
    Envelope, JsonValue, RequestId, Response, ResponseOutcome, encode_json, frame_codec,
};
use std::io;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio_util::{bytes::BytesMut, codec::Encoder};

/// A single isolated pipe listener, authenticated by production clients as the test executable.
#[derive(Debug)]
pub struct TestPipeServer {
    name: PipeName,
    pipe: NamedPipeServer,
    _readiness: crate::CoreReadySignal,
}
impl TestPipeServer {
    /// Creates a fresh, local-only byte-mode listener.
    pub fn bind() -> io::Result<Self> {
        let id = windows::core::GUID::new().map_err(io::Error::other)?;
        let name = PipeName::for_test(id);
        Self::bind_named(name)
    }
    /// Recreates an isolated listener after a simulated core restart.
    pub fn bind_named(name: PipeName) -> io::Result<Self> {
        let pipe = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)?;
        let readiness = crate::CoreReadySignal::create(&name).map_err(io::Error::other)?;
        readiness.signal().map_err(io::Error::other)?;
        Ok(Self {
            name,
            pipe,
            _readiness: readiness,
        })
    }
    /// The isolated name passed to a client's test configuration.
    pub fn name(&self) -> &PipeName {
        &self.name
    }
    /// Waits for one client without creating any worker threads.
    pub async fn accept(self) -> io::Result<TestPeer> {
        self.pipe.connect().await?;
        Ok(TestPeer {
            client: AsyncCoreClient::new(self.pipe),
        })
    }
}
/// Framed test peer with raw envelope access for scripted responses and malformed frames.
#[derive(Debug)]
pub struct TestPeer {
    client: AsyncCoreClient<NamedPipeServer>,
}
impl TestPeer {
    /// Wraps an already accepted isolated test connection.
    pub fn new(transport: NamedPipeServer) -> Self {
        Self {
            client: AsyncCoreClient::new(transport),
        }
    }
    /// Receives one envelope with the standard response deadline.
    pub async fn receive(&mut self) -> Result<Envelope, CallCoreError> {
        self.client.read_envelope().await
    }
    /// Sends a wire envelope with protocol encoding and framing.
    pub async fn send(&mut self, envelope: Envelope) -> Result<(), CallCoreError> {
        let payload = encode_json(&envelope).map_err(CallCoreError::encode)?;
        let mut wire = BytesMut::new();
        frame_codec()
            .encode(payload.as_slice(), &mut wire)
            .map_err(|source| CallCoreError::Frame { source })?;
        self.client.write_test_bytes(&wire).await
    }
    /// Replies successfully using the supplied typed request identity.
    pub async fn respond(&mut self, id: RequestId, body: JsonValue) -> Result<(), CallCoreError> {
        self.send(Envelope::Response(Response {
            id: Some(id),
            outcome: ResponseOutcome::Ok { body },
        }))
        .await
    }
}
