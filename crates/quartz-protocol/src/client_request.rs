//! Request types bind each operation to its response schema.

use crate::{
    AcceptedBody, AnnotationsCreateRequest, AnnotationsDeleteRequest, AnnotationsResponse,
    AnnotationsUpdateRequest, CoreStatusRequest, CoreStatusResponse, Hello, HelloResponse, Op,
    PrivacyModeSetRequest, QueryDayRequest, QueryDayResponse, QueryFactsRequest,
    QueryFactsResponse, QueryPendingAwayRequest, QueryPendingAwayResponse, QueryTimelineRequest,
    QueryTimelineResponse, SettingsGetRequest, SettingsResponse, SettingsSetRequest, Subscribe,
    SubscribeResponse, TimedPolicyResponse, TrackingKeepAliveRequest, TrackingPauseRequest,
    TrackingResumeRequest, UiFirstFrameRequest, UiOpenRequest, Unsubscribe,
};
use serde::{Serialize, de::DeserializeOwned};

/// A request with a fixed operation and response type.
pub trait CoreRequest: Serialize {
    /// Response decoded after request identity correlation.
    type Response: DeserializeOwned;
    /// Wire operation for this body.
    const OP: Op;

    /// Encodes a request body without exposing serialization to the transport.
    ///
    /// # Errors
    /// Returns a JSON encoding failure.
    fn body(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Decodes the correlated successful response.
    ///
    /// # Errors
    /// Rejects a body that does not match the response schema.
    fn response(body: serde_json::Value) -> Result<Self::Response, serde_json::Error> {
        serde_json::from_value(body)
    }
}

macro_rules! requests {
    ($($request:ty => $op:ident => $response:ty),* $(,)?) => {$ (
        impl CoreRequest for $request {
            type Response = $response;
            const OP: Op = Op::$op;
        }
    )*};
}
requests! {
    Hello => Hello => HelloResponse,
    CoreStatusRequest => CoreStatus => CoreStatusResponse,
    UiOpenRequest => UiOpen => AcceptedBody,
    UiFirstFrameRequest => UiFirstFrame => AcceptedBody,
    Subscribe => Subscribe => SubscribeResponse,
    Unsubscribe => Unsubscribe => SubscribeResponse,
    QueryFactsRequest => QueryFacts => QueryFactsResponse,
    QueryDayRequest => QueryDay => QueryDayResponse,
    QueryTimelineRequest => QueryTimeline => QueryTimelineResponse,
    QueryPendingAwayRequest => QueryPendingAway => QueryPendingAwayResponse,
    SettingsGetRequest => SettingsGet => SettingsResponse,
    SettingsSetRequest => SettingsSet => SettingsResponse,
    TrackingPauseRequest => TrackingPause => TimedPolicyResponse,
    TrackingResumeRequest => TrackingResume => TimedPolicyResponse,
    TrackingKeepAliveRequest => TrackingKeepAlive => TimedPolicyResponse,
    PrivacyModeSetRequest => PrivacyModeSet => TimedPolicyResponse,
    AnnotationsCreateRequest => AnnotationsCreate => AnnotationsResponse,
    AnnotationsUpdateRequest => AnnotationsUpdate => AnnotationsResponse,
    AnnotationsDeleteRequest => AnnotationsDelete => AcceptedBody,
}
