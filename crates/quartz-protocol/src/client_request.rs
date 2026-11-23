//! Request types bind each operation to its response schema.

use crate::{
    AcceptedBody, AnnotationsCreateRequest, AnnotationsDeleteRequest, AnnotationsResponse,
    AnnotationsUpdateRequest, CoreStatusRequest, CoreStatusResponse, Hello, HelloResponse, Op,
    PrivacyModeSetRequest, QueryDayRequest, QueryDayResponse, QueryEntityDetailRequest,
    QueryEntityDetailResponse, QueryFactsRequest, QueryFactsResponse, QueryPendingAwayRequest,
    QueryPendingAwayResponse, QuerySegmentDetailRequest, QuerySegmentDetailResponse,
    QueryTimelineRequest, QueryTimelineResponse, QueryUnclassifiedRequest,
    QueryUnclassifiedResponse, SettingsGetRequest, SettingsResponse, SettingsSetRequest, Subscribe,
    SubscribeResponse, TimedPolicyResponse, TrackingKeepAliveRequest, TrackingPauseRequest,
    TrackingResumeRequest, UiFirstFrameRequest, UiHiddenRequest, UiOpenRequest, Unsubscribe,
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
    UiHiddenRequest => UiHidden => AcceptedBody,
    Subscribe => Subscribe => SubscribeResponse,
    Unsubscribe => Unsubscribe => SubscribeResponse,
    QueryFactsRequest => QueryFacts => QueryFactsResponse,
    QueryDayRequest => QueryDay => QueryDayResponse,
    QueryUnclassifiedRequest => QueryUnclassified => QueryUnclassifiedResponse,
    QuerySegmentDetailRequest => QuerySegmentDetail => QuerySegmentDetailResponse,
    QueryEntityDetailRequest => QueryEntityDetail => QueryEntityDetailResponse,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DetailEntity, QueryVersions};

    #[test]
    fn detail_requests_bind_operations_and_round_trip_typed_responses() {
        let segment = QuerySegmentDetailRequest {
            start_utc_ms: 100,
            end_utc_ms: 200,
            app: None,
            window_instance: None,
            cursor: None,
        };
        assert_eq!(QuerySegmentDetailRequest::OP, Op::QuerySegmentDetail);
        assert_eq!(
            serde_json::from_value::<QuerySegmentDetailRequest>(
                segment.body().expect("segment request JSON")
            )
            .expect("typed segment request"),
            segment
        );
        let response = QuerySegmentDetailResponse {
            reference_range: None,
            intervals: Vec::new(),
            versions: QueryVersions("segment".into()),
            next_cursor: None,
        };
        assert_eq!(
            QuerySegmentDetailRequest::response(
                serde_json::to_value(&response).expect("segment response JSON")
            )
            .expect("typed segment response"),
            response
        );
        let entity = QueryEntityDetailRequest {
            reference_range: None,
            entity: DetailEntity::App {
                id: crate::ApplicationId(3),
            },
            local_date: "2026-11-21".parse().expect("date"),
            cursor: None,
        };
        assert_eq!(QueryEntityDetailRequest::OP, Op::QueryEntityDetail);
        assert_eq!(
            serde_json::from_value::<QueryEntityDetailRequest>(
                entity.body().expect("entity request JSON")
            )
            .expect("typed entity request"),
            entity
        );
        let response = QueryEntityDetailResponse {
            extraction: Vec::new(),
            daily: Some(
                (0..crate::ENTITY_DETAIL_DAYS)
                    .map(|index| crate::EntityDailyDuration {
                        local_date: entity
                            .local_date
                            .checked_sub(
                                jiff::Span::new().days(
                                    i64::try_from(crate::ENTITY_DETAIL_DAYS - index - 1)
                                        .expect("seven day offset"),
                                ),
                            )
                            .expect("daily date"),
                        active_ms: 0,
                    })
                    .collect(),
            ),
            titles: Some(Vec::new()),
            rules: Vec::new(),
            versions: QueryVersions("entity".into()),
            next_cursor: None,
        };
        assert_eq!(
            QueryEntityDetailRequest::response(
                serde_json::to_value(&response).expect("entity response JSON")
            )
            .expect("typed entity response"),
            response
        );
    }
}
