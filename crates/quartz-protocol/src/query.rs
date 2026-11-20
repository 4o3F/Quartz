//! Typed day summaries, timelines, and version-bound pagination.

use crate::LocalDate;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

/// Maximum encoded response size, including its envelope allowance (04 §6.2).
pub const QUERY_PAGE_BYTES: usize = 200 * 1024;

/// Opaque continuation value; clients preserve and return its JSON object unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryCursor {
    versions: QueryVersions,
    query: CursorQuery,
    position: usize,
}
impl QueryCursor {
    /// Issues a continuation at the next list item.
    pub fn new(versions: QueryVersions, query: CursorQuery, position: usize) -> Self {
        Self {
            versions,
            query,
            position,
        }
    }
    /// Returns the input versions to validate against the current snapshot.
    pub fn versions(&self) -> &QueryVersions {
        &self.versions
    }
    /// Returns the typed query parameters, excluding the cursor itself.
    pub fn query(&self) -> &CursorQuery {
        &self.query
    }
    /// Returns the zero-based position of the next list item.
    pub fn position(&self) -> usize {
        self.position
    }
}
/// Query parameters retained in a continuation, without recursive cursor fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CursorQuery {
    /// A logical date in the core's current system time zone.
    Day { local_date: LocalDate },
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
/// Safe application display fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationRef {
    pub id: ApplicationId,
    pub exe_name: Option<String>,
    pub display_name: String,
    pub aumid: Option<String>,
}
/// Classification result; C2 adds classified references here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryRef {
    Unclassified,
}
/// Provenance of a classification result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategorySource {
    Unclassified,
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
/// First-page metrics and hours; application ranking continues across pages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryDayResponse {
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
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub duration_ms: i64,
    pub clock_segment_id: Option<ClockSegmentId>,
    pub activity: Activity,
    pub category: CategoryRef,
    pub category_source: CategorySource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<ApplicationRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
/// One timeline page with the input version token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryTimelineResponse {
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
        assert_eq!(decoded.query(), &CursorQuery::from(&request));
        assert_eq!(decoded.position(), 10);
    }
}
