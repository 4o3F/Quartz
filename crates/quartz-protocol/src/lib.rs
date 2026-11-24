//! Shared IPC data types, framing, permissions, and startup failures.

mod annotations;
mod categories;
pub use categories::{
    CategoriesArchiveRequest, CategoriesCreateRequest, CategoriesListRequest,
    CategoriesListResponse, CategoriesReorderRequest, CategoriesUpdateRequest, CategoryEdit,
    CategoryResponse, EditableCategory, NewCategory,
};
mod client_request;
pub use client_request::CoreRequest;
/// Raw JSON at the protocol envelope boundary.
pub use serde_json::{Error as JsonError, Value as JsonValue};
mod envelope;
mod error;
pub mod facts;
mod frame;
mod handshake;
mod op;
mod query;
mod role;
mod rules;
mod settings;
mod startup_error;
mod state;
mod status;
mod timed;
mod topic;
pub use timed::{
    ParsePrivacyModeRequestError, PrivacyMode, PrivacyModeSetRequest, RequestedTimedState,
    TimedPolicies, TimedPolicyResponse, TimedPrivacyMode, TrackingKeepAliveRequest,
    TrackingPauseRequest, TrackingResumeRequest,
};

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
pub use rules::{
    RuleAction, RuleApp, RuleCondition, RuleEntryContent, RuleEntryKind, RuleEntryState, RuleField,
    RuleKind, RuleMatch, RuleOperator, RuleProvenance, RuleSetCounts, RuleSetEntry, RuleSetNode,
    RuleSetRevision, RuleSetSource, RuleSetWarning, RuleSetsGetRequest, RuleSetsGetResponse,
    RuleSetsTreeRequest, RuleSetsTreeResponse, RuleSetsVersionsRequest, RuleSetsVersionsResponse,
    RuleView, RuleWarningCode, RuleWebAction,
};
pub use startup_error::{CoreError, CoreErrorCode, ParseCoreErrorCodeError, cli};
pub use topic::{LocalDate, ParseTopicError, Subscribe, SubscribeResponse, Topic, Unsubscribe};

pub use state::{
    AppIdentity, ForegroundState, HourCycle, LocaleSettings, StateSnapshot, UiLanguage, Weekday,
};

pub use status::{
    AcceptedBody, BrowserStream, BrowserStreamId, CoreStatusRequest, CoreStatusResponse,
    DatabaseState, StopReason, UiFirstFrameRequest, UiHiddenRequest, UiOpenRequest,
};

pub use query::{
    Activity, AnnotationResume, ApplicationId, ApplicationRank, ApplicationRef, BuiltinCategory,
    Category, CategoryDuration, CategoryEntity, CategoryId, CategoryRef, CategorySource,
    CategoryTendency, CategoryUuid, ClassificationRuleId, ClockSegmentId, CursorPosition,
    CursorQuery, DayChanged, DayMetrics, DetailEntity, DocumentRank, DocumentRef,
    ENTITY_DETAIL_DAYS, ENTITY_DETAIL_TOP_TITLES, EntityClassificationRule, EntityDailyDuration,
    EntityExtractionProvenance, EntityTitleDuration, ExtractionStatus, HourStack, MAX_CURSOR_BYTES,
    MillisecondsPerPixel, ParseClassificationRuleIdError, ParseQueryCursorError, PendingAway,
    QUERY_PAGE_BYTES, QueryCursor, QueryDayRequest, QueryDayResponse, QueryEntityDetailRequest,
    QueryEntityDetailResponse, QueryFactsRequest, QueryFactsResponse, QueryGapReason,
    QueryPendingAwayRequest, QueryPendingAwayResponse, QueryRangeRequest, QueryRangeResponse,
    QuerySegmentDetailRequest, QuerySegmentDetailResponse, QueryStopReason, QueryTimelineRequest,
    QueryTimelineResponse, QueryUnclassifiedRequest, QueryUnclassifiedResponse, QueryVersions,
    RANGE_MAX_DAYS, RangeBucket, RangeDimension, RangeEntity, RangeGroup, RangeSummaryBucket,
    ReferenceRange, SEGMENT_DETAIL_MAX_SPAN_MS, TimelineDetail, TimelineSegment, TitleExtraction,
    UNCLASSIFIED_MAX_DAYS, UNCLASSIFIED_MAX_SAMPLES, UNCLASSIFIED_MAX_SEARCH_SCALARS,
    UnclassifiedGroupBy, UnclassifiedItem, UnclassifiedSampleRange, WindowInstanceRef, WorkEntity,
};

pub use annotations::{
    Annotation, AnnotationCategory, AnnotationId, AnnotationKind, AnnotationText,
    AnnotationTextChunk, AnnotationValues, AnnotationsCreateRequest, AnnotationsDeleteRequest,
    AnnotationsResponse, AnnotationsUpdateRequest, CreateAnnotation, QueryAnnotationsRequest,
    QueryAnnotationsResponse, SavedAnnotation, UpdateAnnotation,
};

pub use settings::{
    EffectiveUiSettings, FirstDayOfWeekSetting, HourCycleSetting, PrewarmEnabled, PrewarmOnBattery,
    PrewarmSettings, PrewarmUpdate, ResidentEnabled, ResidentSettings, ResidentUpdate,
    SettingsGetRequest, SettingsResponse, SettingsSetRequest, UiLanguageSetting, UiSettings,
};
