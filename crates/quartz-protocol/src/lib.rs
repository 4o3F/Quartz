//! Shared IPC data types, framing, permissions, and startup failures.

mod envelope;
mod error;
mod frame;
mod handshake;
mod op;
mod query;
mod role;
mod startup_error;
mod state;
mod status;
mod topic;

pub use envelope::{Envelope, Event, EventSeq, Request, RequestId, Response, ResponseOutcome};
pub use error::{ErrorKind, ProtocolError, RuleSetVersion};
#[cfg(feature = "codec")]
pub use frame::frame_codec;
pub use frame::{DecodeJsonError, EncodeFrameError, MAX_FRAME_LEN, decode_json, encode_json};
pub use handshake::{
    BrowserHello, BrowserKind, Capability, ClientVersion, CoreVersion, Epoch, ExtensionId, Hello,
    HelloResponse, HelloRole, HostHello, InstanceId, NegotiateProtoError, PROTOCOL_VERSION,
    ParseProtoRangeError, ParseUuidError, ProtoRange, ProtoVersion, UiHello, UiState, Uuid,
};
pub use op::{ControlBody, ControlOp, Op, OpFamily};
pub use role::{AuthorizeOpError, Role};
pub use startup_error::{CoreError, CoreErrorCode, ParseCoreErrorCodeError, cli};
pub use topic::{LocalDate, ParseTopicError, Subscribe, SubscribeResponse, Topic};

pub use state::{
    AppIdentity, ForegroundState, HourCycle, LocaleSettings, StateSnapshot, UiLanguage, Weekday,
};

pub use status::{
    AcceptedBody, BrowserStream, BrowserStreamId, CoreStatusRequest, CoreStatusResponse,
    DatabaseState, StopReason, UiFirstFrameRequest, UiOpenRequest,
};

pub use query::{
    Activity, ApplicationId, ApplicationRank, ApplicationRef, CategoryDuration, CategoryRef,
    CategorySource, ClockSegmentId, CursorQuery, DayChanged, DayMetrics, HourStack,
    MillisecondsPerPixel, QUERY_PAGE_BYTES, QueryCursor, QueryDayRequest, QueryDayResponse,
    QueryGapReason, QueryStopReason, QueryTimelineRequest, QueryTimelineResponse, QueryVersions,
    TimelineDetail, TimelineSegment,
};
