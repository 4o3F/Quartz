//! Shared IPC data types, framing, permissions, and startup failures.

mod envelope;
mod error;
mod frame;
mod handshake;
mod op;
mod role;
mod startup_error;
mod topic;

pub use envelope::{Envelope, Event, EventSeq, Request, RequestId, Response, ResponseOutcome};
pub use error::{ErrorCode, ErrorKind, ProtocolError, RuleSetVersion};
pub use frame::{
    DecodeFrameError, DecodeJsonError, EncodeFrameError, Frame, FrameDecoder, MAX_FRAME_LEN,
};
pub use handshake::{
    BrowserHello, BrowserKind, Capability, ClientVersion, CoreVersion, Epoch, ExtensionId, Hello,
    HelloResponse, HelloRole, HostHello, InstanceId, NegotiateProtoError, PROTOCOL_VERSION,
    ParseProtoRangeError, ParseUuidError, ProtoRange, ProtoVersion, UiHello, UiState, Uuid,
};
pub use op::{ControlBody, ControlOp, Op, OpFamily};
pub use role::{AuthorizeOpError, Role};
pub use startup_error::{CoreError, CoreErrorCode, ParseCoreErrorCodeError, cli};
pub use topic::{LocalDate, ParseTopicError, Subscribe, Topic};
