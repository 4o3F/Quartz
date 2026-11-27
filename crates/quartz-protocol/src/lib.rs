//! Shared IPC data types, framing, permissions, and startup failures.

mod annotations;
mod browser;
pub use browser::{
    BROWSER_TITLE_MAX_UTF16_UNITS, BrowserAudible, BrowserFocus, BrowserHello, BrowserHostHello,
    BrowserKind, BrowserStateRequest, BrowserTab, BrowserTitleStatus, BrowserWindow,
    BrowserWindowState, INTERNAL_BROWSER_DOMAIN, LOCAL_FILE_BROWSER_DOMAIN,
    ValidateBrowserStateError, validate_browser_domain, validate_browser_title,
};
mod categories;
pub use categories::{
    CATEGORIES_MAX_ENTITIES, CategoriesArchiveRequest, CategoriesCreateRequest,
    CategoriesListRequest, CategoriesListResponse, CategoriesReorderRequest,
    CategoriesUpdateRequest, CategoryEdit, CategoryResponse, EditableCategory, NewCategory,
};
mod client_request;
pub use client_request::CoreRequest;
mod data;
pub use data::{
    BackupEntry, BackupKind, DataBackupRequest, DataCompactRequest, DataCompactResponse,
    DataDeleteRangeRequest, DataDeleteRangeResponse, DataInfoRequest, DataInfoResponse,
};
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

pub use envelope::{
    Envelope, Event, EventSeq, Progress, Request, RequestId, Response, ResponseOutcome,
};
pub use error::{ErrorKind, ProtocolError, RuleSetVersion};
pub use frame::{
    BROWSER_MAX_FRAME_LEN, DecodeJsonError, EncodeFrameError, MAX_FRAME_LEN, decode_json,
    encode_json, encode_json_with_limit,
};
#[cfg(feature = "codec")]
pub use frame::{browser_frame_codec, frame_codec, frame_codec_with_max_len};
pub use handshake::{
    ClientVersion, CoreVersion, Hello, HelloResponse, HelloRole, InstanceId, NegotiateProtoError,
    PROTOCOL_VERSION, ParseHelloError, ParseProtoRangeError, ParseUuidError, ProtoRange,
    ProtoVersion, UiHello, UiState, Uuid,
};
pub use op::{ControlBody, ControlOp, Op, OpFamily};
pub use role::{AuthorizeOpError, Role};
pub use rules::{
    MAX_RULE_DIAGNOSTICS, MAX_RULE_TEXT_BYTES, MutatedRuleSet, PREVIEW_MAX_DAYS,
    PREVIEW_MAX_SAMPLES, PreviewCategoryRank, RULE_SETS_GET_MAX_ENTRIES, RULE_SETS_TREE_MAX_NODES,
    RuleAction, RuleApp, RuleCondition, RuleDiagnostic, RuleDiagnosticLocation, RuleEntryContent,
    RuleEntryKind, RuleEntryState, RuleField, RuleKind, RuleMatch, RuleOperator,
    RulePreviewMutation, RulePreviewOutput, RulePreviewSample, RulePreviewSideTotals,
    RulePreviewStatistics, RulePreviewTotals, RuleProvenance, RuleSetCounts, RuleSetEntry,
    RuleSetIdentityField, RuleSetNode, RuleSetSource, RuleSetWarning, RuleSetsDeleteRequest,
    RuleSetsEditRequest, RuleSetsGetRequest, RuleSetsGetResponse, RuleSetsSetEnabledRequest,
    RuleSetsTreeRequest, RuleSetsTreeResponse, RuleSetsWriteResponse, RuleView, RuleWarningCode,
    RuleWebAction, RulesPreviewRequest, RulesPreviewResponse, RulesTextExportRequest,
    RulesTextExportResponse, RulesTextImportRequest, RulesTextValidateRequest,
    RulesTextValidateResponse, ValidatePreviewDatesError,
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
    Activity, ApplicationId, ApplicationRank, ApplicationRef, BuiltinCategory, Category,
    CategoryDuration, CategoryEntity, CategoryId, CategoryRef, CategorySource, CategoryTendency,
    CategoryUuid, ClassificationRuleId, ClockSegmentId, DAY_MAX_APPLICATIONS, DAY_MAX_DOCUMENTS,
    DayChanged, DayMetrics, DetailEntity, DocumentRank, DocumentRef, ENTITY_DETAIL_DAYS,
    ENTITY_DETAIL_MAX_EXTRACTION, ENTITY_DETAIL_MAX_RULES, ENTITY_DETAIL_TOP_TITLES,
    EntityClassificationRule, EntityDailyDuration, EntityExtractionProvenance, EntityTitleDuration,
    ExtractionStatus, FACTS_DEFAULT_LIMIT, FACTS_MAX_LIMIT, HourStack, MillisecondsPerPixel,
    PENDING_AWAY_MAX_RANGES, ParseClassificationRuleIdError, PendingAway, QueryDayRequest,
    QueryDayResponse, QueryEntityDetailRequest, QueryEntityDetailResponse, QueryFactsRequest,
    QueryFactsResponse, QueryGapReason, QueryPendingAwayRequest, QueryPendingAwayResponse,
    QueryRangeRequest, QueryRangeResponse, QuerySegmentDetailRequest, QuerySegmentDetailResponse,
    QueryStopReason, QueryTimelineRequest, QueryTimelineResponse, QueryUnclassifiedRequest,
    QueryUnclassifiedResponse, QueryVersions, RANGE_MAX_DAYS, RANGE_MAX_GROUPS_PER_DIMENSION,
    RangeBucket, RangeDimension, RangeEntity, RangeGroup, RangeSummaryBucket, ReferenceRange,
    SEGMENT_DETAIL_MAX_INTERVALS, SEGMENT_DETAIL_MAX_SPAN_MS, TIMELINE_MAX_SEGMENTS,
    TimelineDetail, TimelineSegment, TitleExtraction, UNCLASSIFIED_MAX_DAYS,
    UNCLASSIFIED_MAX_ITEMS, UNCLASSIFIED_MAX_SAMPLES, UNCLASSIFIED_MAX_SEARCH_SCALARS,
    UnclassifiedGroupBy, UnclassifiedItem, UnclassifiedSampleRange, WindowInstanceRef, WorkEntity,
};

pub use annotations::{
    ANNOTATION_MAX_LABEL_SCALARS, ANNOTATION_MAX_NOTE_SCALARS, ANNOTATIONS_MAX_ENTITIES,
    Annotation, AnnotationCategory, AnnotationId, AnnotationKind, AnnotationLabelPreset,
    AnnotationValues, AnnotationsCreateRequest, AnnotationsDeleteRequest, AnnotationsResponse,
    AnnotationsUpdateRequest, CreateAnnotation, QueryAnnotationsRequest, QueryAnnotationsResponse,
    SavedAnnotation, UpdateAnnotation,
};

pub use settings::{
    AutostartEnabled, EffectiveUiSettings, FirstDayOfWeekSetting, HourCycleSetting, PrewarmEnabled,
    PrewarmOnBattery, PrewarmSettings, PrewarmUpdate, ResidentEnabled, ResidentSettings,
    ResidentUpdate, SettingsGetRequest, SettingsResponse, SettingsSetRequest, TitleRetention,
    UiLanguageSetting, UiSettings,
};
