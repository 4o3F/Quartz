//! Request types bind each operation to its response schema.

use crate::{
    AcceptedBody, AnnotationsCreateRequest, AnnotationsDeleteRequest, AnnotationsResponse,
    AnnotationsUpdateRequest, CoreStatusRequest, CoreStatusResponse, Hello, HelloResponse, Op,
    PrivacyModeSetRequest, QueryDayRequest, QueryDayResponse, QueryEntityDetailRequest,
    QueryEntityDetailResponse, QueryFactsRequest, QueryFactsResponse, QueryPendingAwayRequest,
    QueryPendingAwayResponse, QueryRangeRequest, QueryRangeResponse, QuerySegmentDetailRequest,
    QuerySegmentDetailResponse, QueryTimelineRequest, QueryTimelineResponse,
    QueryUnclassifiedRequest, QueryUnclassifiedResponse, SettingsGetRequest, SettingsResponse,
    SettingsSetRequest, Subscribe, SubscribeResponse, TimedPolicyResponse,
    TrackingKeepAliveRequest, TrackingPauseRequest, TrackingResumeRequest, UiFirstFrameRequest,
    UiHiddenRequest, UiOpenRequest, Unsubscribe,
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
    crate::CancelRequest => RequestsCancel => AcceptedBody,
    crate::RulesPreviewRequest => RulesPreview => crate::RulesPreviewResponse,
    crate::RulesTextDiffRequest => RulesTextDiff => crate::RulesTextDiffResponse,
    crate::RulesTextPreviewRequest => RulesTextPreview => crate::RulesTextPreviewResponse,
    crate::RulesTextValidateRequest => RulesTextValidate => crate::RulesTextValidateResponse,
    crate::RulesTextExportRequest => RulesTextExport => crate::RulesTextExportResponse,
    crate::RuleSetsEditRequest => RuleSetsEdit => crate::RuleSetsWriteResponse,
    crate::RuleSetsSetEnabledRequest => RuleSetsSetEnabled => crate::RuleSetsWriteResponse,
    crate::RuleSetsReorderRequest => RuleSetsReorder => crate::RuleSetsWriteResponse,
    crate::RuleSetsDeleteRequest => RuleSetsDelete => crate::RuleSetsWriteResponse,
    crate::RuleSetsRevertRequest => RuleSetsRevert => crate::RuleSetsWriteResponse,
    crate::CategoriesListRequest => CategoriesList => crate::CategoriesListResponse,
    crate::CategoriesCreateRequest => CategoriesCreate => crate::CategoryResponse,
    crate::CategoriesUpdateRequest => CategoriesUpdate => crate::CategoryResponse,
    crate::CategoriesReorderRequest => CategoriesReorder => AcceptedBody,
    crate::CategoriesArchiveRequest => CategoriesArchive => crate::CategoryResponse,
    Hello => Hello => HelloResponse,
    CoreStatusRequest => CoreStatus => CoreStatusResponse,
    UiOpenRequest => UiOpen => AcceptedBody,
    UiFirstFrameRequest => UiFirstFrame => AcceptedBody,
    UiHiddenRequest => UiHidden => AcceptedBody,
    Subscribe => Subscribe => SubscribeResponse,
    Unsubscribe => Unsubscribe => SubscribeResponse,
    crate::QueryAnnotationsRequest => QueryAnnotations => crate::QueryAnnotationsResponse,
    QueryFactsRequest => QueryFacts => QueryFactsResponse,
    QueryDayRequest => QueryDay => QueryDayResponse,
    QueryRangeRequest => QueryRange => QueryRangeResponse,
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
    struct AdditiveFields;
    impl AdditiveFields {
        fn insert(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    for child in fields.values_mut() {
                        Self::insert(child);
                    }
                    fields.insert(
                        "future_field".into(),
                        serde_json::json!({"nested":[1,true,null]}),
                    );
                }
                serde_json::Value::Array(items) => {
                    for child in items {
                        Self::insert(child);
                    }
                }
                _ => {}
            }
        }
        fn response<R: CoreRequest>(fixture: &str) {
            let mut wire = serde_json::from_str(fixture).expect("fixture JSON");
            Self::insert(&mut wire);
            assert!(R::response(wire).is_ok(), "additive fields for {:?}", R::OP);
        }
    }
    #[test]
    fn typed_responses_accept_additive_fields_at_every_nested_object() {
        AdditiveFields::response::<crate::QueryAnnotationsRequest>(include_str!(
            "../tests/fixtures/annotations-chunked.json"
        ));
        let mut categories: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/categories.json"))
                .expect("categories");
        AdditiveFields::insert(&mut categories);
        assert!(
            crate::CategoriesListRequest::response(categories["list_response"].clone()).is_ok()
        );
        assert!(
            crate::CategoriesCreateRequest::response(categories["entity_response"].clone()).is_ok()
        );
        AdditiveFields::response::<QueryDayRequest>(include_str!(
            "../tests/fixtures/document-day.json"
        ));
        AdditiveFields::response::<QueryTimelineRequest>(include_str!(
            "../tests/fixtures/timeline-annotations.json"
        ));
        AdditiveFields::response::<QueryRangeRequest>(include_str!(
            "../tests/fixtures/range-first.json"
        ));
        AdditiveFields::response::<QueryEntityDetailRequest>(include_str!(
            "../tests/fixtures/document-entity.json"
        ));
        AdditiveFields::response::<QueryPendingAwayRequest>(include_str!(
            "../tests/fixtures/pending-away.json"
        ));
        AdditiveFields::response::<QueryUnclassifiedRequest>(include_str!(
            "../tests/fixtures/unclassified-document.json"
        ));
    }
    #[test]
    fn cursor_json_is_forwarded_unchanged_including_future_query_shapes() {
        for query in [
            serde_json::json!({"kind":"day","local_date":"2025-01-02","extra":{"id":18446744073709551615_u64}}),
            serde_json::json!({"kind":"future_query","parameters":[null,true,"opaque"]}),
        ] {
            let cursor = serde_json::json!({"versions":"v","query":query,"position":3,"extension":{"nested":[1,2,3]}});
            let wire = serde_json::json!({"metrics":null,"hours":null,"applications":[],"domains":[],"documents":[],"versions":"v","next_cursor":cursor});
            let response = QueryDayRequest::response(wire).expect("opaque cursor response");
            let request = QueryDayRequest {
                local_date: "2025-01-02".parse().expect("date"),
                cursor: response.next_cursor,
            };
            assert_eq!(request.body().expect("forward cursor")["cursor"], cursor);
        }
    }
}
