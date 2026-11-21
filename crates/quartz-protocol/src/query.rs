//! Typed day summaries, timelines, and version-bound pagination.

use crate::{AnnotationId, LocalDate};
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
    /// Pending idle pieces in a logical day.
    PendingAway { local_date: LocalDate },
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
            segments: vec![
                TimelineSegment {
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
                    title: Some("document".into()),
                },
                TimelineSegment {
                    start_utc_ms: 1000,
                    end_utc_ms: 2000,
                    duration_ms: 1000,
                    clock_segment_id: Some(ClockSegmentId(2)),
                    activity: Activity::Excluded,
                    category: CategoryRef::Unclassified,
                    category_source: CategorySource::Unclassified,
                    app: None,
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
    fn annotated_timeline_and_pending_away_match_new_golden_json() {
        let id = |number: u32| {
            AnnotationId(
                format!("abcdef00-0000-4000-8000-{number:012x}")
                    .parse()
                    .expect("UUID"),
            )
        };
        let response = QueryTimelineResponse {
            segments: vec![
                TimelineSegment {
                    start_utc_ms: 0,
                    end_utc_ms: 1000,
                    duration_ms: 1000,
                    clock_segment_id: Some(ClockSegmentId(2)),
                    activity: Activity::ActiveOverride { annotation: id(1) },
                    category: CategoryRef::Unclassified,
                    category_source: CategorySource::Annotation { annotation: id(1) },
                    app: None,
                    title: None,
                },
                TimelineSegment {
                    start_utc_ms: 1000,
                    end_utc_ms: 2000,
                    duration_ms: 1000,
                    clock_segment_id: None,
                    activity: Activity::OfflineActivity { annotation: id(2) },
                    category: CategoryRef::Unclassified,
                    category_source: CategorySource::Annotation { annotation: id(2) },
                    app: None,
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
        assert_eq!(decoded.query(), &CursorQuery::from(&request));
        assert_eq!(decoded.position(), 10);
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
}
