//! Typed day summaries, timelines, and version-bound pagination.

use crate::{AnnotationId, LocalDate};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

/// Maximum encoded response size, including its envelope allowance (04 §6.2).
pub const QUERY_PAGE_BYTES: usize = 200 * 1024;

/// Opaque, version-bound document reference issued by the core.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocumentRef(String);
impl DocumentRef {
    /// Wraps an encoded core reference; validation belongs to the core query boundary.
    pub fn from_encoded(value: String) -> Self {
        Self(value)
    }
    /// Borrows the encoded value for core validation or unchanged client forwarding.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for DocumentRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DocumentRef([opaque])")
    }
}
/// Half-open UTC range whose inputs determine a document reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceRange {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
}
impl ReferenceRange {
    fn deserialize_request<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Self>, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RequestRange {
            start_utc_ms: i64,
            end_utc_ms: i64,
        }
        Option::<RequestRange>::deserialize(deserializer).map(|range| {
            range.map(|range| Self {
                start_utc_ms: range.start_utc_ms,
                end_utc_ms: range.end_utc_ms,
            })
        })
    }
}
/// Extraction outcome, independent of display language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionStatus {
    Extracted,
    Fallback,
    Empty,
    Unknown,
}
/// Visible extraction output; hidden or unavailable fields are null.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleExtraction {
    pub document: Option<String>,
    pub project: Option<String>,
    pub profile: Option<String>,
    pub dirty: Option<bool>,
    pub normalized_title: Option<String>,
    pub extractor_rule: Option<String>,
    pub status: Option<ExtractionStatus>,
}
/// Maximum encoded cursor JSON size, including all nested string escaping.
/// Document-detail cursors carry a complete document reference, while rule cursors carry
/// keys with no smaller protocol limit; use the 256 KiB frame ceiling for both.
pub const MAX_CURSOR_BYTES: usize = crate::MAX_FRAME_LEN;

/// Counts encoded bytes without allocating a second copy of the cursor JSON.
struct CursorByteBudget {
    remaining: usize,
}
impl std::io::Write for CursorByteBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Opaque continuation object; clients return all JSON fields unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "serde_json::Value")]
pub struct QueryCursor(serde_json::Value);
impl QueryCursor {
    /// Issues a continuation at the next list item using the core's current schema.
    pub fn new(versions: QueryVersions, query: CursorQuery, position: usize) -> Self {
        Self(
            serde_json::to_value(CursorPosition {
                versions,
                query,
                position,
                annotation_resume: None,
            })
            .expect("typed cursor fields serialize to JSON"),
        )
    }
    /// Issues an annotation continuation at a stored row and text fragment.
    pub fn new_annotation(
        versions: QueryVersions,
        query: CursorQuery,
        resume: AnnotationResume,
    ) -> Self {
        Self(
            serde_json::to_value(CursorPosition {
                versions,
                query,
                position: resume.chunk,
                annotation_resume: Some(resume),
            })
            .expect("typed annotation cursor serializes"),
        )
    }
    /// Validates the core's cursor schema when admitting a continuation request.
    ///
    /// # Errors
    /// Returns an error for oversized JSON, malformed fields, or unknown fields and query kinds.
    pub fn decode(&self) -> Result<CursorPosition, serde_json::Error> {
        self.check_size().map_err(serde::de::Error::custom)?;
        CursorPosition::deserialize(&self.0)
    }
    /// Checks query parameters before the core admits database or scan work.
    ///
    /// # Errors
    /// Returns an error for an invalid cursor schema or parameters from another query.
    pub fn validate_query(&self, expected: &CursorQuery) -> Result<(), serde_json::Error> {
        let position = self.decode()?;
        if position.annotation_resume().is_some()
            && !matches!(position.query(), CursorQuery::Annotations { .. })
        {
            return Err(serde::de::Error::custom(
                "annotation resume requires an annotation query",
            ));
        }
        if position.query() != expected {
            return Err(serde::de::Error::custom(
                "cursor parameters do not match the request",
            ));
        }
        Ok(())
    }
    fn check_size(&self) -> Result<(), ParseQueryCursorError> {
        serde_json::to_writer(
            CursorByteBudget {
                remaining: MAX_CURSOR_BYTES,
            },
            &self.0,
        )
        .map_err(|_| ParseQueryCursorError)
    }
}
impl Serialize for QueryCursor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl TryFrom<serde_json::Value> for QueryCursor {
    type Error = ParseQueryCursorError;
    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        if !value.is_object() {
            return Err(ParseQueryCursorError);
        }
        let cursor = Self(value);
        cursor.check_size()?;
        Ok(cursor)
    }
}
impl From<QueryCursor> for serde_json::Value {
    fn from(cursor: QueryCursor) -> Self {
        cursor.0
    }
}
/// A continuation must be a JSON object within the cursor byte limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
#[snafu(display("query cursor must be a JSON object of at most {MAX_CURSOR_BYTES} bytes"))]
pub struct ParseQueryCursorError;

/// Core-validated continuation parameters, separate from the client's opaque JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorPosition {
    versions: QueryVersions,
    query: CursorQuery,
    position: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    annotation_resume: Option<AnnotationResume>,
}
/// Stored annotation ordering key and next fragment within that row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationResume {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub uuid: crate::AnnotationId,
    pub chunk: usize,
}
impl CursorPosition {
    /// Returns the annotation row and fragment retained by the core.
    pub fn annotation_resume(&self) -> Option<&AnnotationResume> {
        self.annotation_resume.as_ref()
    }

    /// Returns the input versions to compare with the current snapshot.
    pub fn versions(&self) -> &QueryVersions {
        &self.versions
    }
    /// Returns the query parameters bound to this continuation.
    pub fn query(&self) -> &CursorQuery {
        &self.query
    }
    /// Returns the next list item index, or the fragment index for an annotation row.
    pub fn position(&self) -> usize {
        self.position
    }
}
/// Query parameters retained in a continuation, without recursive cursor fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CursorQuery {
    /// Rule family tree inputs.
    RuleSetsTree { rule_kind: crate::rules::RuleKind },
    /// Exact set revision and projection inputs.
    RuleSetsGet {
        key: String,
        version: Option<std::num::NonZeroU32>,
        view: crate::rules::RuleView,
    },
    /// Retained revision listing inputs.
    RuleSetsVersions { key: String },
    /// Saved annotation entities intersecting a bounded UTC range.
    Annotations { start_utc_ms: i64, end_utc_ms: i64 },
    /// Category hierarchy with the archive filter bound to the continuation.
    Categories { include_archived: bool },
    /// A calendar date, from local midnight to the next local midnight.
    Facts { local_date: LocalDate },
    /// A logical date in the core's current system time zone.
    Day { local_date: LocalDate },
    /// Pending idle pieces in a logical day.
    PendingAway { local_date: LocalDate },
    /// Unmerged slices within a bounded UTC range and optional lane.
    SegmentDetail {
        start_utc_ms: i64,
        end_utc_ms: i64,
        app: Option<ApplicationId>,
        window_instance: Option<WindowInstanceRef>,
    },
    /// Seven logical days of an application's activity.
    EntityDetail {
        entity: DetailEntity,
        local_date: LocalDate,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "ReferenceRange::deserialize_request"
        )]
        reference_range: Option<ReferenceRange>,
    },
    /// Grouped unclassified work in a logical-date range.
    Unclassified {
        start_date: LocalDate,
        end_date: LocalDate,
        group_by: UnclassifiedGroupBy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        search: Option<String>,
    },
    /// Bounded daily or weekly independent dimension summaries.
    Range {
        start_date: LocalDate,
        end_date: LocalDate,
        bucket: RangeBucket,
        dimensions: Vec<RangeDimension>,
    },
    /// A UTC range and its display projection.
    Timeline {
        start_utc_ms: i64,
        end_utc_ms: i64,
        detail: TimelineDetail,
        ms_per_pixel: MillisecondsPerPixel,
    },
}
impl From<&QueryDayRequest> for CursorQuery {
    fn from(request: &QueryDayRequest) -> Self {
        Self::Day {
            local_date: request.local_date,
        }
    }
}
impl From<&QueryTimelineRequest> for CursorQuery {
    fn from(request: &QueryTimelineRequest) -> Self {
        Self::Timeline {
            start_utc_ms: request.start_utc_ms,
            end_utc_ms: request.end_utc_ms,
            detail: request.detail,
            ms_per_pixel: request.ms_per_pixel,
        }
    }
}
impl From<&QueryPendingAwayRequest> for CursorQuery {
    fn from(request: &QueryPendingAwayRequest) -> Self {
        Self::PendingAway {
            local_date: request.local_date,
        }
    }
}
impl From<&QueryFactsRequest> for CursorQuery {
    fn from(request: &QueryFactsRequest) -> Self {
        Self::Facts {
            local_date: request.local_date,
        }
    }
}
/// Maximum logical dates in one grouped query (04 §6.2.1).
pub const UNCLASSIFIED_MAX_DAYS: usize = 366;
/// Maximum logical dates in a range summary (04 §6.2.1).
pub const RANGE_MAX_DAYS: usize = UNCLASSIFIED_MAX_DAYS;

/// Calendar granularity for range summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeBucket {
    Day,
    Week,
}
/// Independently aggregated identities, in the requested presentation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeDimension {
    Category,
    App,
    Document,
}
/// A half-open logical-date range with one or more distinct dimensions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRangeRequest {
    pub start_date: LocalDate,
    pub end_date: LocalDate,
    pub bucket: RangeBucket,
    pub dimensions: Vec<RangeDimension>,
    #[serde(default)]
    pub cursor: Option<QueryCursor>,
}
impl From<&QueryRangeRequest> for CursorQuery {
    fn from(request: &QueryRangeRequest) -> Self {
        Self::Range {
            start_date: request.start_date,
            end_date: request.end_date,
            bucket: request.bucket,
            dimensions: request.dimensions.clone(),
        }
    }
}
/// Category or visible work identity within one dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RangeEntity {
    Category(CategoryEntity),
    Work(WorkEntity),
}
/// Category identity using the same category reference as day summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CategoryEntity {
    Category { category: CategoryRef },
}
/// One dimension's duration for an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeGroup {
    pub dimension: RangeDimension,
    pub entity: RangeEntity,
    pub active_ms: i64,
}
/// A clipped bucket; its total is omitted when groups continue on another page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeSummaryBucket {
    pub start_date: LocalDate,
    pub end_date: LocalDate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_ms: Option<i64>,
    pub groups: Vec<RangeGroup>,
}
/// One bounded page of range summaries in the core's effective calendar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRangeResponse {
    pub time_zone: String,
    pub day_start_offset_min: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_range: Option<ReferenceRange>,
    pub buckets: Vec<RangeSummaryBucket>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}

/// Maximum search length in Unicode scalar values (04 §6.2.1).
pub const UNCLASSIFIED_MAX_SEARCH_SCALARS: usize = 256;
/// Maximum chronological samples retained per group (04 §6.2.1).
pub const UNCLASSIFIED_MAX_SAMPLES: usize = 5;

/// Entity identity used to group unclassified active work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnclassifiedGroupBy {
    App,
    Document,
}
/// A half-open range of logical dates, with core-side search and pagination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryUnclassifiedRequest {
    pub start_date: LocalDate,
    pub end_date: LocalDate,
    pub group_by: UnclassifiedGroupBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default)]
    pub cursor: Option<QueryCursor>,
}
impl From<&QueryUnclassifiedRequest> for CursorQuery {
    fn from(request: &QueryUnclassifiedRequest) -> Self {
        Self::Unclassified {
            start_date: request.start_date,
            end_date: request.end_date,
            group_by: request.group_by,
            search: request.search.clone(),
        }
    }
}
/// Visible application or document identity, shared with range aggregation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkEntity {
    App {
        id: ApplicationId,
        exe_name: Option<String>,
        display_name: String,
    },
    Document {
        document_ref: DocumentRef,
        app: ApplicationId,
        document: String,
        project: Option<String>,
        profile: Option<String>,
    },
}
/// One active slice that can preselect a logical day and classification range.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UnclassifiedSampleRange {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub clock_segment_id: Option<ClockSegmentId>,
}
/// Total unclassified active time and earliest samples for a visible entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnclassifiedItem {
    pub entity: WorkEntity,
    pub active_ms: i64,
    pub sample_ranges: Vec<UnclassifiedSampleRange>,
}
/// One page of grouped work, using the core's effective logical-day calendar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryUnclassifiedResponse {
    pub time_zone: String,
    pub day_start_offset_min: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_range: Option<ReferenceRange>,
    pub items: Vec<UnclassifiedItem>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}

/// Maximum detail range: two UTC days accommodate local-day boundaries and DST.
pub const SEGMENT_DETAIL_MAX_SPAN_MS: i64 = 48 * 60 * 60 * 1000;
/// Number of logical days included in an entity detail response.
pub const ENTITY_DETAIL_DAYS: usize = 7;
/// Five maximum-length escaped titles leave room inside the 200 KiB page budget.
pub const ENTITY_DETAIL_TOP_TITLES: usize = 5;

/// Unmerged raw projection for a clicked timeline range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuerySegmentDetailRequest {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    #[serde(default)]
    pub app: Option<ApplicationId>,
    #[serde(default)]
    pub window_instance: Option<WindowInstanceRef>,
    #[serde(default)]
    pub cursor: Option<QueryCursor>,
}
impl From<&QuerySegmentDetailRequest> for CursorQuery {
    fn from(request: &QuerySegmentDetailRequest) -> Self {
        Self::SegmentDetail {
            start_utc_ms: request.start_utc_ms,
            end_utc_ms: request.end_utc_ms,
            app: request.app,
            window_instance: request.window_instance,
        }
    }
}
/// One page of clipped slices, preserving the timeline's clock-segment order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuerySegmentDetailResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_range: Option<ReferenceRange>,
    pub intervals: Vec<TimelineSegment>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Entity identities supported by application and document details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetailEntity {
    App { id: ApplicationId },
    Document { document_ref: DocumentRef },
}
/// Seven logical days ending at the requested date, inclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryEntityDetailRequest {
    pub entity: DetailEntity,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "ReferenceRange::deserialize_request"
    )]
    pub reference_range: Option<ReferenceRange>,
    pub local_date: LocalDate,
    #[serde(default)]
    pub cursor: Option<QueryCursor>,
}
impl From<&QueryEntityDetailRequest> for CursorQuery {
    fn from(request: &QueryEntityDetailRequest) -> Self {
        Self::EntityDetail {
            entity: request.entity.clone(),
            local_date: request.local_date,
            reference_range: request.reference_range,
        }
    }
}
/// Active elapsed duration in one logical day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityDailyDuration {
    pub local_date: LocalDate,
    pub active_ms: i64,
}
/// A retained Full-level title ranked by active elapsed duration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityTitleDuration {
    pub title: String,
    pub duration_ms: i64,
}
/// A rule that supplied the final classification, with current display metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityClassificationRule {
    pub rule: ClassificationRuleId,
    pub label: Option<String>,
    pub label_key: Option<String>,
    pub category: CategoryRef,
}
/// One extraction rule and status observed on active, visible document facts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityExtractionProvenance {
    pub extractor_rule: Option<String>,
    pub status: ExtractionStatus,
}
/// First-page daily totals and top titles; rules and extraction continue with the cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryEntityDetailResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily: Option<Vec<EntityDailyDuration>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub titles: Option<Vec<EntityTitleDuration>>,
    pub rules: Vec<EntityClassificationRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extraction: Vec<EntityExtractionProvenance>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Raw stored facts intersecting a calendar day in the core's system time zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryFactsRequest {
    pub local_date: LocalDate,
    #[serde(default)]
    pub cursor: Option<QueryCursor>,
}
/// One chronological page of full stored facts and its input versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryFactsResponse {
    pub facts: Vec<crate::facts::FactRecord>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Pending idle query in the core's current system time zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPendingAwayRequest {
    pub local_date: LocalDate,
    #[serde(default)]
    pub cursor: Option<QueryCursor>,
}
/// Uncovered contiguous idle time, with UTC boundaries and elapsed duration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingAway {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub duration_ms: i64,
}
/// One version-bound page of pending idle pieces ordered by start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPendingAwayResponse {
    pub ranges: Vec<PendingAway>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Digest of all persisted and semantic query inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QueryVersions(pub String);
/// Stable application database identity; paths are never included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApplicationId(pub i64);
/// Stable clock-segment database identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClockSegmentId(pub i64);
/// Opaque persistent window identity; clients compare and return it without interpreting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowInstanceRef(i64);
impl WindowInstanceRef {
    /// Projects an identity supplied by the stored-facts boundary.
    pub fn from_stored_id(id: crate::facts::WindowInstanceId) -> Self {
        Self(id.0)
    }
}
/// Safe application display fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationRef {
    pub id: ApplicationId,
    pub exe_name: Option<String>,
    pub display_name: String,
    pub aumid: Option<String>,
}
/// Database identity carried in category metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryId(pub i64);
/// Durable category identity carried by the wire format.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryUuid(pub String);
/// Builtin localization key carried by category responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinCategory {
    Development,
    Learning,
    Communication,
    Entertainment,
    System,
}
/// Tendency stored in category response metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryTendency {
    Focus,
    Neutral,
    Distraction,
}
/// Category presentation fields with a protocol-owned JSON representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: CategoryId,
    pub uuid: CategoryUuid,
    pub builtin_key: Option<BuiltinCategory>,
    pub name: Option<String>,
    /// Palette string `cat.1` through `cat.14`, or `neutral`.
    pub color: String,
    pub tendency: CategoryTendency,
}
/// Revision-qualified classification source in `<set>/<rule>@<revision>` form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ClassificationRuleId(std::sync::Arc<str>);
/// A classification source contains an invalid target or revision.
#[derive(Debug, snafu::Snafu)]
#[snafu(display("invalid revision-qualified classification rule"))]
pub struct ParseClassificationRuleIdError;
impl ClassificationRuleId {
    /// Borrows the exact representation supplied by the core.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::str::FromStr for ClassificationRuleId {
    type Err = ParseClassificationRuleIdError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = value.rsplit_once('@').is_some_and(|(target, revision)| {
            revision.parse::<std::num::NonZeroU32>().is_ok()
                && target.split_once('/').is_some_and(|(set, rule)| {
                    !set.is_empty()
                        && set.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || b".-_".contains(&byte)
                        })
                        && !rule.is_empty()
                        && rule
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
                })
        });
        if !valid {
            return Err(ParseClassificationRuleIdError);
        }
        Ok(Self(value.into()))
    }
}
impl TryFrom<String> for ClassificationRuleId {
    type Error = ParseClassificationRuleIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<ClassificationRuleId> for String {
    fn from(value: ClassificationRuleId) -> Self {
        value.as_str().to_owned()
    }
}

/// Classification result with language-independent category metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CategoryRef {
    Unclassified,
    Classified {
        #[serde(flatten)]
        category: std::sync::Arc<Category>,
    },
}
/// Provenance of a classification result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CategorySource {
    Unclassified,
    Rule {
        rule: ClassificationRuleId,
    },
    Annotation {
        annotation: AnnotationId,
    },
    /// A category projection merged slices with different classification sources.
    Mixed,
}
/// Reasons that stop recording; multiple reasons can overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryStopReason {
    Locked,
    Disconnected,
    Suspended,
    Paused,
    CoreOffline,
}
/// Reasons for missing trustworthy capture coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryGapReason {
    EventOverload,
    WriteQueueOverload,
    DatabaseError,
}
/// Activity after applying availability masks and capture privacy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Activity {
    Active,
    ActiveOverride { annotation: AnnotationId },
    OfflineActivity { annotation: AnnotationId },
    Idle,
    NoForeground,
    Excluded,
    Stopped { reasons: Vec<QueryStopReason> },
    Gap { reason: QueryGapReason },
}
/// Request for a logical day; the start offset comes from core settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryDayRequest {
    pub local_date: LocalDate,
    #[serde(default)]
    pub cursor: Option<QueryCursor>,
}
/// The six metrics in the Today status row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayMetrics {
    pub active_ms: i64,
    pub focus_ratio: f64,
    pub distract_ratio: f64,
    pub unclassified_ms: i64,
    pub longest_focus_ms: i64,
    pub switches_per_hour: f64,
}
/// Duration assigned to a category in an aggregate stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryDuration {
    pub category: CategoryRef,
    pub duration_ms: i64,
}
/// One hourly bucket with the local-clock label provided by jiff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HourStack {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub local_start: String,
    pub categories: Vec<CategoryDuration>,
}
/// One complete application ranking entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationRank {
    pub app: ApplicationRef,
    pub active_ms: i64,
}
/// One complete document ranking entry, using its visible normalized identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRank {
    pub document_ref: DocumentRef,
    pub app: ApplicationId,
    pub document: String,
    pub project: Option<String>,
    pub profile: Option<String>,
    pub active_ms: i64,
}
/// First-page metrics, hours and reference range; each ranking continues separately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryDayResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_range: Option<ReferenceRange>,
    #[serde(default)]
    pub documents: Vec<DocumentRank>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<DayMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hours: Option<Vec<HourStack>>,
    pub applications: Vec<ApplicationRank>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Fields retained by the timeline projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineDetail {
    Category,
    App,
    Raw,
}
/// Positive milliseconds represented by one physical pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MillisecondsPerPixel(pub NonZeroU64);
/// UTC-range timeline request. Day callers use the range returned by the hour stacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryTimelineRequest {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub detail: TimelineDetail,
    pub ms_per_pixel: MillisecondsPerPixel,
    #[serde(default)]
    pub cursor: Option<QueryCursor>,
}
/// A projected display segment; durations retain segment-local monotonic time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSegment {
    /// Supplied for app/raw projections, including null fields after privacy filtering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction: Option<TitleExtraction>,
    #[serde(default)]
    pub document_ref: Option<DocumentRef>,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub duration_ms: i64,
    pub clock_segment_id: Option<ClockSegmentId>,
    pub activity: Activity,
    pub category: CategoryRef,
    pub category_source: CategorySource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<ApplicationRef>,
    /// Present only for captured windows in app/raw projections, after privacy filtering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_instance: Option<WindowInstanceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
/// One timeline page with the input version token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryTimelineResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_range: Option<ReferenceRange>,
    pub segments: Vec<TimelineSegment>,
    pub versions: QueryVersions,
    pub next_cursor: Option<QueryCursor>,
}
/// Latest invalidation for a subscribed logical day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayChanged {
    pub date: LocalDate,
    pub versions: QueryVersions,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detail_requests_and_cursors_preserve_typed_filters() {
        let wire =
            serde_json::json!({"start_utc_ms":100,"end_utc_ms":200,"app":3,"window_instance":5});
        let request: QuerySegmentDetailRequest =
            serde_json::from_value(wire).expect("segment request");
        assert_eq!(request.app, Some(ApplicationId(3)));
        assert_eq!(
            request.window_instance,
            Some(WindowInstanceRef::from_stored_id(
                crate::facts::WindowInstanceId(5)
            ))
        );
        let cursor = QueryCursor::new(
            QueryVersions("detail-version".into()),
            CursorQuery::from(&request),
            2,
        );
        let encoded = serde_json::to_value(&cursor).expect("cursor");
        assert_eq!(encoded["query"]["kind"], "segment_detail");
        assert_eq!(encoded["query"]["app"], 3);
        assert_eq!(encoded["query"]["window_instance"], 5);
        assert_eq!(
            serde_json::from_value::<QueryCursor>(encoded).expect("cursor round trip"),
            cursor
        );
        let wire = serde_json::json!({"entity":{"kind":"app","id":3},"local_date":"2026-11-21"});
        let request: QueryEntityDetailRequest =
            serde_json::from_value(wire).expect("entity request");
        let cursor = QueryCursor::new(
            QueryVersions("entity-version".into()),
            CursorQuery::from(&request),
            1,
        );
        assert_eq!(
            serde_json::to_value(cursor).expect("cursor")["query"],
            serde_json::json!({"kind":"entity_detail","entity":{"kind":"app","id":3},"local_date":"2026-11-21"})
        );
        for kind in ["document", "domain"] {
            assert!(
                serde_json::from_value::<DetailEntity>(serde_json::json!({"kind":kind,"id":3}))
                    .is_err()
            );
        }
    }
    #[test]
    fn window_instance_reference_round_trips_without_exposing_a_scalar_accessor() {
        let identity = WindowInstanceRef::from_stored_id(crate::facts::WindowInstanceId(42));
        let encoded = serde_json::to_value(identity).expect("window identity");
        assert_eq!(encoded, serde_json::json!(42));
        assert_eq!(
            serde_json::from_value::<WindowInstanceRef>(encoded).expect("opaque identity"),
            identity
        );
    }
    #[test]
    fn facts_request_response_and_cursor_preserve_raw_wire_fields() {
        let request: QueryFactsRequest =
            serde_json::from_value(serde_json::json!({"local_date":"2025-01-02"}))
                .expect("facts request");
        assert!(request.cursor.is_none());
        let cursor = QueryCursor::new(
            QueryVersions("facts-version".into()),
            CursorQuery::from(&request),
            1,
        );
        let cursor_json = serde_json::json!({"versions":"facts-version","query":{"kind":"facts","local_date":"2025-01-02"},"position":1});
        assert_eq!(
            serde_json::to_value(&cursor).expect("cursor JSON"),
            cursor_json
        );
        assert_eq!(
            serde_json::from_value::<QueryCursor>(cursor_json).expect("cursor round trip"),
            cursor
        );
        let body = serde_json::json!({
            "facts":[{"kind":"run","id":1,"run_id":1,"segment_id":null,
            "start_utc_ms":0,"end_utc_ms":null,"confirmed_utc_ms":1000,
            "start_mono_ms":null,"end_mono_ms":null,"confirmed_mono_ms":null,
            "duration_ms":1000,"end_reason":null,"device_id":"test-device",
            "win_session_id":1,"app_version":"test"}],
            "versions":"facts-version","next_cursor":cursor
        });
        let page: QueryFactsResponse =
            serde_json::from_value(body.clone()).expect("typed facts response");
        assert_eq!(serde_json::to_value(page).expect("facts round trip"), body);
    }
    #[test]
    fn unclassified_wire_snapshots_preserve_groups_samples_ranges_and_cursors() {
        let request_wire: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/unclassified-request.json"))
                .expect("request snapshot");
        let request: QueryUnclassifiedRequest =
            serde_json::from_value(request_wire.clone()).expect("request");
        assert_eq!(
            serde_json::to_value(&request).expect("request wire"),
            request_wire
        );
        assert_eq!(
            <QueryUnclassifiedRequest as crate::CoreRequest>::OP,
            crate::Op::QueryUnclassified
        );
        for fixture in [
            include_str!("../tests/fixtures/unclassified-app.json"),
            include_str!("../tests/fixtures/unclassified-document.json"),
            include_str!("../tests/fixtures/unclassified-continuation.json"),
        ] {
            let wire: serde_json::Value = serde_json::from_str(fixture).expect("response snapshot");
            let response: QueryUnclassifiedResponse =
                serde_json::from_value(wire.clone()).expect("response");
            assert_eq!(
                serde_json::to_value(&response).expect("response wire"),
                wire
            );
            if let Some(cursor) = response.next_cursor {
                assert_eq!(
                    cursor.decode().expect("issued cursor").query(),
                    &CursorQuery::from(&request)
                );
                assert_eq!(cursor.decode().expect("issued cursor").position(), 1);
            }
        }
        let mut invalid = request_wire;
        invalid["unused"] = serde_json::json!(true);
        assert!(serde_json::from_value::<QueryUnclassifiedRequest>(invalid).is_err());
        for group_by in ["domain", "category", "APP"] {
            assert!(serde_json::from_value::<QueryUnclassifiedRequest>(serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-03","group_by":group_by})).is_err());
        }
        assert_eq!(
            serde_json::to_value(crate::Capability::QueryUnclassifiedV1).expect("capability"),
            "query.unclassified.v1"
        );
    }
    #[test]
    fn range_wire_snapshots_preserve_entities_dimensions_and_bucket_continuations() {
        let wire: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/range-request.json"))
                .expect("request snapshot");
        let request: QueryRangeRequest = serde_json::from_value(wire.clone()).expect("typed range");
        assert_eq!(serde_json::to_value(&request).expect("encode"), wire);
        assert_eq!(request.bucket, RangeBucket::Week);
        assert_eq!(
            request.dimensions,
            [
                RangeDimension::Document,
                RangeDimension::Category,
                RangeDimension::App
            ]
        );
        for snapshot in [
            include_str!("../tests/fixtures/range-first.json"),
            include_str!("../tests/fixtures/range-continuation.json"),
        ] {
            let expected: serde_json::Value = serde_json::from_str(snapshot).expect("snapshot");
            let response: QueryRangeResponse =
                serde_json::from_value(expected.clone()).expect("response");
            assert_eq!(serde_json::to_value(&response).expect("encode"), expected);
            if let Some(cursor) = &response.next_cursor {
                assert_eq!(
                    cursor.decode().expect("issued cursor").query(),
                    &CursorQuery::from(&request)
                );
                assert_eq!(cursor.decode().expect("issued cursor").position(), 2);
                assert_eq!(response.buckets[0].active_ms, Some(1000));
                assert!(matches!(
                    response.buckets[0].groups[0].entity,
                    RangeEntity::Work(WorkEntity::Document { .. })
                ));
            } else {
                assert_eq!(response.buckets[0].active_ms, None);
                assert!(response.reference_range.is_none());
                assert!(matches!(
                    response.buckets[0].groups[0].entity,
                    RangeEntity::Category(CategoryEntity::Category { .. })
                ));
                assert!(matches!(
                    response.buckets[0].groups[1].entity,
                    RangeEntity::Work(WorkEntity::App { .. })
                ));
            }
        }
        for field in ["time_zone", "day_start_offset_min", "unknown"] {
            let mut invalid = wire.clone();
            invalid[field] = serde_json::json!("unsupported");
            assert!(serde_json::from_value::<QueryRangeRequest>(invalid).is_err());
        }
        for bucket in ["month", "DAY"] {
            let mut invalid = wire.clone();
            invalid["bucket"] = serde_json::json!(bucket);
            assert!(serde_json::from_value::<QueryRangeRequest>(invalid).is_err());
        }
        let mut invalid = wire;
        invalid["dimensions"] = serde_json::json!(["domain"]);
        assert!(serde_json::from_value::<QueryRangeRequest>(invalid).is_err());
        assert_eq!(
            serde_json::to_value(crate::Capability::QueryRangeV1).expect("capability"),
            "query.range.v1"
        );
        assert_eq!(
            <QueryRangeRequest as crate::CoreRequest>::OP,
            crate::Op::QueryRange
        );
    }
    fn golden_category() -> CategoryRef {
        CategoryRef::Classified {
            category: Category {
                id: CategoryId(7),
                uuid: CategoryUuid("71f5a800-423a-4000-8000-000000000001".into()),
                builtin_key: Some(BuiltinCategory::Development),
                name: None,
                color: "cat.8".into(),
                tendency: CategoryTendency::Focus,
            }
            .into(),
        }
    }
    fn golden_app() -> ApplicationRef {
        ApplicationRef {
            id: ApplicationId(3),
            exe_name: Some("code.exe".into()),
            display_name: "Code".into(),
            aumid: None,
        }
    }
    #[test]
    fn day_response_matches_golden_json() {
        let response = QueryDayResponse {
            reference_range: None,
            documents: Vec::new(),
            metrics: Some(DayMetrics {
                active_ms: 1000,
                focus_ratio: 1.0,
                distract_ratio: 0.0,
                unclassified_ms: 0,
                longest_focus_ms: 1000,
                switches_per_hour: 0.0,
            }),
            hours: Some(vec![HourStack {
                start_utc_ms: 0,
                end_utc_ms: 3600000,
                local_start: "1970-01-01T00:00:00+00:00[UTC]".into(),
                categories: vec![CategoryDuration {
                    category: golden_category(),
                    duration_ms: 1000,
                }],
            }]),
            applications: vec![ApplicationRank {
                app: golden_app(),
                active_ms: 1000,
            }],
            versions: QueryVersions("version-token".into()),
            next_cursor: None,
        };
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/day.json"))
                .expect("day golden JSON");
        assert_eq!(serde_json::to_value(&response).expect("day JSON"), expected);
        assert_eq!(
            serde_json::from_value::<QueryDayResponse>(expected).expect("day response"),
            response
        );
    }
    #[test]
    fn timeline_response_matches_golden_json() {
        let response = QueryTimelineResponse {
            reference_range: Some(ReferenceRange {
                start_utc_ms: 0,
                end_utc_ms: 2000,
            }),
            segments: vec![
                TimelineSegment {
                    extraction: Some(TitleExtraction {
                        document: Some("document".into()),
                        dirty: Some(false),
                        normalized_title: Some("document".into()),
                        status: Some(ExtractionStatus::Fallback),
                        ..Default::default()
                    }),
                    document_ref: Some(DocumentRef::from_encoded("doc:opaque".into())),
                    start_utc_ms: 0,
                    end_utc_ms: 1000,
                    duration_ms: 1000,
                    clock_segment_id: Some(ClockSegmentId(2)),
                    activity: Activity::Active,
                    category: golden_category(),
                    category_source: CategorySource::Rule {
                        rule: "builtin.classification/vscode@2"
                            .parse()
                            .expect("rule revision"),
                    },
                    app: Some(golden_app()),
                    window_instance: Some(WindowInstanceRef::from_stored_id(
                        crate::facts::WindowInstanceId(5),
                    )),
                    title: Some("document".into()),
                },
                TimelineSegment {
                    extraction: Some(TitleExtraction::default()),
                    document_ref: None,
                    start_utc_ms: 1000,
                    end_utc_ms: 2000,
                    duration_ms: 1000,
                    clock_segment_id: Some(ClockSegmentId(2)),
                    activity: Activity::Excluded,
                    category: CategoryRef::Unclassified,
                    category_source: CategorySource::Unclassified,
                    app: None,
                    window_instance: None,
                    title: None,
                },
            ],
            versions: QueryVersions("version-token".into()),
            next_cursor: None,
        };
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/timeline.json"))
                .expect("timeline golden JSON");
        assert_eq!(
            serde_json::to_value(&response).expect("timeline JSON"),
            expected
        );
        assert_eq!(
            serde_json::from_value::<QueryTimelineResponse>(expected).expect("timeline response"),
            response
        );
    }
    #[test]
    fn additive_document_fields_preserve_legacy_and_detail_json() {
        let wire: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/timeline.json"))
                .expect("timeline snapshot");
        let timeline: QueryTimelineResponse =
            serde_json::from_value(wire.clone()).expect("timeline");
        let detail = QuerySegmentDetailResponse {
            reference_range: timeline.reference_range,
            intervals: timeline.segments.clone(),
            versions: timeline.versions.clone(),
            next_cursor: None,
        };
        let expected = serde_json::json!({"reference_range": wire["reference_range"], "intervals": wire["segments"], "versions": wire["versions"], "next_cursor": null});
        assert_eq!(
            serde_json::to_value(&detail).expect("detail JSON"),
            expected
        );
        assert_eq!(
            serde_json::from_value::<QuerySegmentDetailResponse>(expected)
                .expect("detail roundtrip"),
            detail
        );
        let mut legacy = wire;
        let object = legacy.as_object_mut().expect("response object");
        object.remove("reference_range");
        for segment in object["segments"].as_array_mut().expect("segments") {
            let object = segment.as_object_mut().expect("segment");
            object.remove("extraction");
            object.remove("document_ref");
        }
        let decoded: QueryTimelineResponse =
            serde_json::from_value(legacy).expect("old core response");
        assert!(decoded.reference_range.is_none());
        assert!(
            decoded
                .segments
                .iter()
                .all(|segment| segment.extraction.is_none() && segment.document_ref.is_none())
        );
    }
    #[test]
    fn annotated_timeline_and_pending_away_match_new_golden_json() {
        let id = |number: u32| {
            AnnotationId(
                format!("abcdef00-0000-4000-8000-{number:012x}")
                    .parse()
                    .expect("UUID"),
            )
        };
        let response = QueryTimelineResponse {
            reference_range: None,
            segments: vec![
                TimelineSegment {
                    extraction: None,
                    document_ref: None,
                    start_utc_ms: 0,
                    end_utc_ms: 1000,
                    duration_ms: 1000,
                    clock_segment_id: Some(ClockSegmentId(2)),
                    activity: Activity::ActiveOverride { annotation: id(1) },
                    category: CategoryRef::Unclassified,
                    category_source: CategorySource::Annotation { annotation: id(1) },
                    app: None,
                    window_instance: None,
                    title: None,
                },
                TimelineSegment {
                    extraction: None,
                    document_ref: None,
                    start_utc_ms: 1000,
                    end_utc_ms: 2000,
                    duration_ms: 1000,
                    clock_segment_id: None,
                    activity: Activity::OfflineActivity { annotation: id(2) },
                    category: CategoryRef::Unclassified,
                    category_source: CategorySource::Annotation { annotation: id(2) },
                    app: None,
                    window_instance: None,
                    title: None,
                },
            ],
            versions: QueryVersions("version-token".into()),
            next_cursor: None,
        };
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/timeline-annotations.json"))
                .expect("annotated golden");
        assert_eq!(serde_json::to_value(&response).expect("encode"), expected);
        assert_eq!(
            serde_json::from_value::<QueryTimelineResponse>(expected).expect("decode"),
            response
        );
        let response = QueryPendingAwayResponse {
            ranges: vec![PendingAway {
                start_utc_ms: 0,
                end_utc_ms: 1_200_000,
                duration_ms: 1_200_000,
            }],
            versions: QueryVersions("version-token".into()),
            next_cursor: None,
        };
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pending-away.json"))
                .expect("pending golden");
        assert_eq!(serde_json::to_value(&response).expect("encode"), expected);
        assert_eq!(
            serde_json::from_value::<QueryPendingAwayResponse>(expected).expect("decode"),
            response
        );
        let request = QueryPendingAwayRequest {
            local_date: "2025-01-02".parse().expect("date"),
            cursor: None,
        };
        let cursor = QueryCursor::new(
            QueryVersions("version-token".into()),
            CursorQuery::from(&request),
            1,
        );
        let value = serde_json::to_value(&cursor).expect("cursor");
        assert_eq!(
            value["query"],
            serde_json::json!({"kind":"pending_away", "local_date":"2025-01-02"})
        );
        assert_eq!(
            serde_json::from_value::<QueryCursor>(value).expect("cursor round trip"),
            cursor
        );
    }
    #[test]
    fn cursor_round_trips_as_an_object_with_typed_query_parameters() {
        let request = QueryDayRequest {
            local_date: "2025-01-02".parse().expect("date"),
            cursor: None,
        };
        let cursor = QueryCursor::new(
            QueryVersions("versions".into()),
            CursorQuery::from(&request),
            200,
        );
        let value = serde_json::to_value(&cursor).expect("cursor object");
        assert_eq!(
            value,
            serde_json::json!({"versions":"versions","query":{"kind":"day","local_date":"2025-01-02"},"position":200})
        );
        assert_eq!(
            serde_json::from_value::<QueryCursor>(value).expect("cursor round trip"),
            cursor
        );
        assert!(serde_json::from_value::<QueryCursor>(serde_json::json!("legacy token")).is_err());
        let request = QueryTimelineRequest {
            start_utc_ms: 0,
            end_utc_ms: 1000,
            detail: TimelineDetail::App,
            ms_per_pixel: MillisecondsPerPixel(NonZeroU64::MIN),
            cursor: None,
        };
        let cursor = QueryCursor::new(
            QueryVersions("versions".into()),
            CursorQuery::from(&request),
            10,
        );
        let decoded: QueryCursor =
            serde_json::from_value(serde_json::to_value(&cursor).expect("timeline cursor"))
                .expect("typed timeline cursor");
        assert_eq!(
            decoded.decode().expect("issued cursor").query(),
            &CursorQuery::from(&request)
        );
        assert_eq!(decoded.decode().expect("issued cursor").position(), 10);
    }
    #[test]
    fn category_metadata_and_revision_qualified_sources_round_trip() {
        let category = CategoryRef::Classified {
            category: Category {
                id: CategoryId(7),
                uuid: CategoryUuid("71f5a800-423a-4000-8000-000000000001".into()),
                builtin_key: Some(BuiltinCategory::Development),
                name: None,
                color: "cat.8".into(),
                tendency: CategoryTendency::Focus,
            }
            .into(),
        };
        let value = serde_json::to_value(&category).expect("category JSON");
        assert_eq!(value["builtin_key"], "development");
        assert_eq!(value["tendency"], "focus");
        assert!(value["name"].is_null());
        assert_eq!(
            category,
            serde_json::from_value(value).expect("category round trip")
        );
        let source = CategorySource::Rule {
            rule: "builtin.classification/vscode@2".parse().expect("rule id"),
        };
        assert_eq!(
            source,
            serde_json::from_value(serde_json::to_value(&source).expect("source JSON"))
                .expect("source round trip")
        );
        assert!(
            serde_json::from_value::<CategorySource>(
                serde_json::json!({"kind":"rule","rule":"vscode"})
            )
            .is_err()
        );
    }
    #[test]
    fn document_wire_snapshots_preserve_identity_range_provenance_and_continuations() {
        let wire: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/document-day.json"))
                .expect("document day snapshot");
        let day: QueryDayResponse = serde_json::from_value(wire.clone()).expect("document ranking");
        assert_eq!(day.documents[0].app, ApplicationId(3));
        assert_eq!(day.documents[0].project.as_deref(), Some("Quartz"));
        assert_eq!(serde_json::to_value(day).expect("day wire"), wire);
        let wire: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/document-entity-request.json"
        ))
        .expect("document request snapshot");
        let request: QueryEntityDetailRequest =
            serde_json::from_value(wire.clone()).expect("document request");
        assert!(matches!(request.entity, DetailEntity::Document { .. }));
        assert_eq!(serde_json::to_value(&request).expect("request wire"), wire);
        let cursor = QueryCursor::new(
            QueryVersions("version-token".into()),
            CursorQuery::from(&request),
            1,
        );
        let value = serde_json::to_value(&cursor).expect("cursor wire");
        assert_eq!(value["query"]["reference_range"], wire["reference_range"]);
        assert_eq!(value["query"]["entity"], wire["entity"]);
        assert_eq!(
            serde_json::from_value::<QueryCursor>(value).expect("cursor"),
            cursor
        );
        for snapshot in [
            include_str!("../tests/fixtures/document-entity.json"),
            include_str!("../tests/fixtures/document-entity-continuation.json"),
        ] {
            let wire: serde_json::Value =
                serde_json::from_str(snapshot).expect("document detail snapshot");
            let detail: QueryEntityDetailResponse =
                serde_json::from_value(wire.clone()).expect("document detail");
            assert!(!detail.extraction.is_empty());
            assert_eq!(serde_json::to_value(detail).expect("detail wire"), wire);
        }
        let legacy = serde_json::json!({"applications":[],"versions":"legacy","next_cursor":null});
        let decoded: QueryDayResponse = serde_json::from_value(legacy).expect("legacy day");
        assert!(decoded.documents.is_empty() && decoded.reference_range.is_none());
        assert_eq!(
            serde_json::to_value(crate::Capability::QueryDocumentsV1).expect("capability"),
            "query.documents.v1"
        );
    }
    #[test]
    fn reference_range_is_strict_in_requests_and_extensible_in_responses() {
        let range = serde_json::json!({"start_utc_ms":0,"end_utc_ms":1,"extra":true});
        assert!(serde_json::from_value::<ReferenceRange>(range.clone()).is_ok());
        assert!(serde_json::from_value::<QueryEntityDetailRequest>(serde_json::json!({
            "entity":{"kind":"document","document_ref":"opaque"},"local_date":"2025-01-02","reference_range":range
        })).is_err());
    }
    #[test]
    fn cursor_json_has_a_frame_sized_byte_limit_even_outside_transport() {
        let wire = serde_json::json!({"extension":"x".repeat(crate::MAX_FRAME_LEN)});
        assert!(QueryCursor::try_from(wire).is_err());
    }
    #[test]
    fn cursor_byte_limit_counts_utf8_and_json_escaping_at_the_exact_boundary() {
        let empty = serde_json::json!({"extension":""});
        let overhead = serde_json::to_vec(&empty).expect("empty cursor JSON").len();
        let mut at_limit = empty;
        at_limit["extension"] = serde_json::json!("x".repeat(MAX_CURSOR_BYTES - overhead));
        assert_eq!(
            serde_json::to_vec(&at_limit).expect("boundary JSON").len(),
            MAX_CURSOR_BYTES
        );
        assert!(QueryCursor::try_from(at_limit.clone()).is_ok());
        at_limit["extension"] = serde_json::json!("x".repeat(MAX_CURSOR_BYTES - overhead + 1));
        assert!(QueryCursor::try_from(at_limit).is_err());
        for text in [
            "😀".repeat(MAX_CURSOR_BYTES / '😀'.len_utf8()),
            "\0".repeat(MAX_CURSOR_BYTES),
        ] {
            assert!(QueryCursor::try_from(serde_json::json!({"extension":text})).is_err());
        }
        let empty_rule = QueryCursor::new(
            QueryVersions("v".into()),
            CursorQuery::RuleSetsGet {
                key: String::new(),
                version: None,
                view: crate::RuleView::Own,
            },
            usize::MAX,
        );
        let overhead = serde_json::to_vec(&empty_rule).expect("rule cursor").len();
        let largest_rule = QueryCursor::new(
            QueryVersions("v".into()),
            CursorQuery::RuleSetsGet {
                key: "x".repeat(MAX_CURSOR_BYTES - overhead),
                version: None,
                view: crate::RuleView::Own,
            },
            usize::MAX,
        );
        assert!(
            largest_rule.decode().is_ok(),
            "full-width rule keys remain supported"
        );
        let oversized_rule = QueryCursor::new(
            QueryVersions("v".into()),
            CursorQuery::RuleSetsGet {
                key: "x".repeat(MAX_CURSOR_BYTES),
                version: None,
                view: crate::RuleView::Own,
            },
            usize::MAX,
        );
        assert!(
            oversized_rule.decode().is_err(),
            "admission bounds internally constructed cursors too"
        );
    }
    #[test]
    fn annotation_resume_is_rejected_on_other_query_kinds() {
        let resume = AnnotationResume {
            start_utc_ms: 0,
            end_utc_ms: 1,
            uuid: crate::AnnotationId(
                "abcdef00-0000-4000-8000-000000000001"
                    .parse()
                    .expect("UUID"),
            ),
            chunk: 0,
        };
        let date = "2025-01-02".parse().expect("date");
        for query in [
            CursorQuery::Day { local_date: date },
            CursorQuery::Facts { local_date: date },
            CursorQuery::PendingAway { local_date: date },
        ] {
            let cursor = QueryCursor::new_annotation(
                QueryVersions("v".into()),
                query.clone(),
                resume.clone(),
            );
            assert!(cursor.validate_query(&query).is_err());
        }
        let query = CursorQuery::Annotations {
            start_utc_ms: 0,
            end_utc_ms: 1,
        };
        QueryCursor::new_annotation(QueryVersions("v".into()), query.clone(), resume)
            .validate_query(&query)
            .expect("annotation resume");
        QueryCursor::new(QueryVersions("older".into()), query.clone(), 1)
            .validate_query(&query)
            .expect("old annotation cursor reaches version validation");
    }
}
