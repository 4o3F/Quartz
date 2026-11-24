//! Snapshot query orchestration around pure slicing, aggregation, and paging.

mod annotations;
pub(crate) mod calendar;
pub(crate) mod classification;
mod day_walker;
mod details;
#[cfg(any(windows, test))]
pub(crate) mod dispatch;
pub(crate) mod documents;
pub(crate) mod input;
pub(crate) mod paging;
mod pending_away;
pub(crate) mod pipeline;
mod range;
#[cfg(any(windows, test))]
pub(crate) mod runner;
#[cfg(test)]
mod tests;
pub(crate) mod timing;
mod unclassified;
pub(crate) mod versions;

use crate::store::{ReadOnlyStore, query::ReadSnapshot, query_error::ReadQueryError};
use calendar::DayCalendar;
use day_walker::DayWalker;
use details::{EntityCalendar, EntityDetailAccumulator, EntitySelection, SegmentDetailProjection};
use input::QueryRange;
use paging::{PageBudget, PagePosition};
use pipeline::{DayAccumulator, DerivedSegment, Pipeline, QUERY_BATCH_ROWS, TimelineProjection};
use quartz_protocol::{
    CursorQuery, ErrorKind, QueryCursor, QueryDayRequest, QueryDayResponse,
    QueryEntityDetailRequest, QueryEntityDetailResponse, QueryFactsRequest, QueryFactsResponse,
    QueryPendingAwayRequest, QueryPendingAwayResponse, QuerySegmentDetailRequest,
    QuerySegmentDetailResponse, QueryTimelineRequest, QueryTimelineResponse, QueryVersions,
    SEGMENT_DETAIL_MAX_SPAN_MS,
};
use range::{FirstWeekday, RangeAccumulator};
use serde::Serialize;
use timing::{QueryStage, QueryTimer, QueryTimings};
use unclassified::UnclassifiedAccumulator;

/// Increment whenever matching, slicing, duration, aggregation, or projection semantics change.
pub(crate) const QUERY_SEMANTICS_VERSION: u32 = 8;
/// UTC timeline ranges are bounded to one leap year to limit one job's retained inputs.
pub(crate) const MAX_QUERY_RANGE_MS: i64 = 366 * quartz_domain::capture::limits::UTC_DAY_MS;
/// A 366-logical-day reference can include a backward UTC offset change of up to one day.
const MAX_DOCUMENT_REFERENCE_MS: i64 =
    MAX_QUERY_RANGE_MS + quartz_domain::capture::limits::UTC_DAY_MS;

#[derive(Debug, snafu::Snafu)]
pub(crate) enum RunQueryError {
    #[snafu(display("invalid query range"))]
    InvalidRange,
    #[snafu(display("segment detail range exceeds its bound or has invalid endpoints"))]
    InvalidDetailRange,
    #[snafu(display("invalid unclassified query dates or search length"))]
    InvalidUnclassifiedRequest,
    #[snafu(display("invalid range query dates or dimensions"))]
    InvalidRangeRequest,
    #[snafu(display("could not resolve the effective first weekday"))]
    ReadFirstWeekdayError,
    #[snafu(display("invalid query cursor"))]
    InvalidCursor,
    #[snafu(display("query input versions changed"))]
    StaleCursor,
    #[snafu(display("invalid document reference"))]
    InvalidDocumentRef,
    #[snafu(display("document reference inputs changed"))]
    StaleDocumentRef,
    #[snafu(display("invalid logical day offset"))]
    InvalidDayOffset,
    #[snafu(display("query reader is unavailable"))]
    ReaderUnavailable,
    #[snafu(display("could not finish query preparation: {source}"))]
    Cleanup { source: ReadQueryError },
    #[snafu(display("could not reopen query reader after cleanup failed ({cleanup}): {source}"))]
    RecoverReader {
        source: ReadQueryError,
        cleanup: Box<RunQueryError>,
    },
    #[snafu(display("could not derive query segments: {source}"))]
    Derive {
        source: pipeline::DeriveSegmentsError,
    },
    #[snafu(display("one query item exceeds the page size"))]
    PageTooLarge,
    #[snafu(display("stored rule entry violates the read contract"))]
    InvalidRuleEntry,
    #[snafu(display("could not read query snapshot: {source}"))]
    Read { source: ReadQueryError },
    #[snafu(display("could not interpret query calendar: {source}"))]
    Calendar { source: jiff::Error },
    #[snafu(display("could not read system time zone: {source}"))]
    ReadSystemTimeZoneError { source: jiff::Error },
    #[snafu(display("could not encode time zone: {source}"))]
    EncodeTimeZoneError { source: jiff::Error },
    #[snafu(display("could not encode query result: {source}"))]
    Encode { source: serde_json::Error },
}
impl From<ReadQueryError> for RunQueryError {
    fn from(source: ReadQueryError) -> Self {
        Self::Read { source }
    }
}
impl From<pipeline::DeriveSegmentsError> for RunQueryError {
    fn from(source: pipeline::DeriveSegmentsError) -> Self {
        Self::Derive { source }
    }
}
impl From<jiff::Error> for RunQueryError {
    fn from(source: jiff::Error) -> Self {
        Self::Calendar { source }
    }
}
impl From<calendar::ParseDayStartOffsetError> for RunQueryError {
    fn from(_: calendar::ParseDayStartOffsetError) -> Self {
        Self::InvalidDayOffset
    }
}
impl From<serde_json::Error> for RunQueryError {
    fn from(source: serde_json::Error) -> Self {
        Self::Encode { source }
    }
}
impl RunQueryError {
    pub(crate) fn system_time_zone(
        read: impl FnOnce() -> Result<jiff::tz::TimeZone, jiff::Error>,
    ) -> Result<jiff::tz::TimeZone, Self> {
        read().map_err(|source| Self::ReadSystemTimeZoneError { source })
    }
    pub(crate) fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidRange | Self::Calendar { .. } => ErrorKind::InvalidRange,
            Self::InvalidCursor
            | Self::InvalidDetailRange
            | Self::InvalidDocumentRef
            | Self::InvalidUnclassifiedRequest
            | Self::InvalidRangeRequest => ErrorKind::BadRequest,
            Self::StaleDocumentRef => ErrorKind::StaleDocumentRef,
            Self::StaleCursor => ErrorKind::StaleCursor,
            Self::ReadFirstWeekdayError
            | Self::InvalidDayOffset
            | Self::ReaderUnavailable
            | Self::Cleanup { .. }
            | Self::RecoverReader { .. }
            | Self::Derive { .. }
            | Self::PageTooLarge
            | Self::InvalidRuleEntry
            | Self::Read { .. }
            | Self::ReadSystemTimeZoneError { .. }
            | Self::EncodeTimeZoneError { .. }
            | Self::Encode { .. } => ErrorKind::Internal,
        }
    }
}
#[derive(Debug, Clone)]
pub(crate) enum QueryRequest {
    Annotations(quartz_protocol::QueryAnnotationsRequest),
    Facts(QueryFactsRequest),
    Day(QueryDayRequest),
    Unclassified(quartz_protocol::QueryUnclassifiedRequest),
    Range(quartz_protocol::QueryRangeRequest),
    PendingAway(QueryPendingAwayRequest),
    Timeline(QueryTimelineRequest),
    SegmentDetail(QuerySegmentDetailRequest),
    EntityDetail(QueryEntityDetailRequest),
}
impl QueryRequest {
    #[cfg(any(windows, test))]
    pub(crate) fn decode(
        op: quartz_protocol::Op,
        body: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        match op {
            quartz_protocol::Op::QueryAnnotations => {
                serde_json::from_value(body).map(Self::Annotations)
            }
            quartz_protocol::Op::QueryFacts => serde_json::from_value(body).map(Self::Facts),
            quartz_protocol::Op::QueryRange => serde_json::from_value(body).map(Self::Range),
            quartz_protocol::Op::QueryUnclassified => {
                serde_json::from_value(body).map(Self::Unclassified)
            }
            quartz_protocol::Op::QueryDay => serde_json::from_value(body).map(Self::Day),
            quartz_protocol::Op::QueryPendingAway => {
                serde_json::from_value(body).map(Self::PendingAway)
            }
            quartz_protocol::Op::QuerySegmentDetail => {
                serde_json::from_value(body).map(Self::SegmentDetail)
            }
            quartz_protocol::Op::QueryEntityDetail => {
                serde_json::from_value(body).map(Self::EntityDetail)
            }
            quartz_protocol::Op::QueryTimeline => serde_json::from_value(body).map(Self::Timeline),
            _ => Err(<serde_json::Error as serde::de::Error>::custom(
                "operation has no implemented query decoder",
            )),
        }
    }

    fn parameters(&self) -> CursorQuery {
        match self {
            Self::Annotations(request) => CursorQuery::from(request),
            Self::Facts(facts) => CursorQuery::from(facts),
            Self::Day(day) => CursorQuery::from(day),
            Self::Unclassified(request) => CursorQuery::from(request),
            Self::Range(request) => CursorQuery::from(request),
            Self::PendingAway(away) => CursorQuery::from(away),
            Self::Timeline(timeline) => CursorQuery::from(timeline),
            Self::SegmentDetail(detail) => CursorQuery::from(detail),
            Self::EntityDetail(detail) => CursorQuery::from(detail),
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum QueryResponse {
    Annotations(quartz_protocol::QueryAnnotationsResponse),
    Facts(QueryFactsResponse),
    Day(QueryDayResponse),
    Unclassified(quartz_protocol::QueryUnclassifiedResponse),
    Range(quartz_protocol::QueryRangeResponse),
    PendingAway(QueryPendingAwayResponse),
    Timeline(QueryTimelineResponse),
    SegmentDetail(QuerySegmentDetailResponse),
    EntityDetail(QueryEntityDetailResponse),
}
#[derive(Debug)]
enum Stage {
    Annotations(Box<annotations::AnnotationPage>),
    Facts(Vec<crate::store::facts::Fact>),
    Read(Pipeline),
    Sweep(Pipeline),
    Aggregate {
        segments: std::vec::IntoIter<DerivedSegment>,
        facts: pipeline::SegmentFacts,
        output: Aggregation,
    },
    Complete,
}
#[derive(Debug)]
enum Aggregation {
    Day(DayAccumulator),
    Unclassified(UnclassifiedAccumulator),
    Range(RangeAccumulator),
    PendingAway(pending_away::PendingAwayAccumulator),
    Timeline(TimelineProjection),
    SegmentDetail(SegmentDetailProjection),
    EntityDetail(EntityDetailAccumulator),
}
#[derive(Debug)]
pub(crate) struct QueryJob {
    snapshot: ReadSnapshot,
    request: QueryRequest,
    range: QueryRange,
    reference_range: QueryRange,
    entity: Option<EntitySelection>,
    versions: QueryVersions,
    parameters: CursorQuery,
    position: PagePosition,
    calendar: Option<DayCalendar>,
    entity_calendar: Option<EntityCalendar>,
    scan_range: QueryRange,
    day_walker: Option<DayWalker>,
    unclassified: Option<UnclassifiedAccumulator>,
    range_accumulator: Option<RangeAccumulator>,
    stage: Stage,
}
impl QueryJob {
    pub(crate) fn start(
        reader: &mut Option<ReadOnlyStore>,
        request: QueryRequest,
        rules: std::sync::Arc<quartz_domain::rules::registry::RuleSnapshot>,
    ) -> Result<Self, RunQueryError> {
        Self::start_reading_zone(reader, request, rules, jiff::tz::TimeZone::try_system)
    }
    fn start_reading_zone(
        reader: &mut Option<ReadOnlyStore>,
        request: QueryRequest,
        rules: std::sync::Arc<quartz_domain::rules::registry::RuleSnapshot>,
        read: impl FnOnce() -> Result<jiff::tz::TimeZone, jiff::Error>,
    ) -> Result<Self, RunQueryError> {
        Self::start_in_zone(
            reader,
            request,
            rules,
            RunQueryError::system_time_zone(read)?,
        )
    }
    pub(crate) fn start_in_zone(
        reader: &mut Option<ReadOnlyStore>,
        request: QueryRequest,
        rules: std::sync::Arc<quartz_domain::rules::registry::RuleSnapshot>,
        zone: jiff::tz::TimeZone,
    ) -> Result<Self, RunQueryError> {
        let mut snapshot = reader
            .take()
            .ok_or(RunQueryError::ReaderUnavailable)?
            .snapshot();
        match Self::prepare(&mut snapshot, &request, rules.hashes(), zone) {
            Ok(prepared) => Ok(Self {
                snapshot,
                range: prepared.range,
                reference_range: prepared.reference_range,
                entity: prepared.entity,
                versions: prepared.versions,
                range_accumulator: prepared.range_accumulator,
                parameters: prepared.parameters,
                position: prepared.position,
                calendar: prepared.calendar,
                entity_calendar: prepared.entity_calendar,
                scan_range: prepared.scan_range,
                day_walker: prepared.day_walker,
                unclassified: match &request {
                    QueryRequest::Unclassified(request) => {
                        Some(UnclassifiedAccumulator::new(request))
                    }
                    _ => None,
                },
                stage: if let Some(page) = prepared.annotations {
                    Stage::Annotations(Box::new(page))
                } else if matches!(request, QueryRequest::Facts(_)) {
                    Stage::Facts(Vec::new())
                } else {
                    Stage::Read(Pipeline::new(
                        prepared.scan_range,
                        classification::Classifier::new(rules, prepared.categories),
                    ))
                },
                request,
            }),
            Err(error) => {
                *reader = Some(
                    snapshot
                        .finish()
                        .map_err(|source| RunQueryError::Cleanup { source })?,
                );
                Err(error)
            }
        }
    }
    fn prepare(
        snapshot: &mut ReadSnapshot,
        request: &QueryRequest,
        rule_hashes: &quartz_domain::rules::registry::SnapshotHashes,
        zone: jiff::tz::TimeZone,
    ) -> Result<PreparedQuery, RunQueryError> {
        snapshot.begin()?;
        let settings = snapshot.settings()?;
        let calendar = match request {
            QueryRequest::Facts(_) | QueryRequest::Annotations(_) => None,
            QueryRequest::Day(day) => Some(DayCalendar::in_zone(
                day.local_date,
                settings.day_start_offset,
                zone.clone(),
            )?),
            QueryRequest::PendingAway(away) => Some(DayCalendar::in_zone(
                away.local_date,
                settings.day_start_offset,
                zone.clone(),
            )?),
            QueryRequest::Unclassified(_)
            | QueryRequest::Range(_)
            | QueryRequest::Timeline(_)
            | QueryRequest::SegmentDetail(_)
            | QueryRequest::EntityDetail(_) => None,
        };
        let entity_calendar = match request {
            QueryRequest::EntityDetail(detail) => Some(EntityCalendar::in_zone(
                detail.local_date,
                settings.day_start_offset,
                zone.clone(),
            )?),
            _ => None,
        };
        let mut day_walker = match request {
            QueryRequest::Unclassified(request) => {
                if request.search.as_ref().is_some_and(|search| {
                    search.chars().count() > quartz_protocol::UNCLASSIFIED_MAX_SEARCH_SCALARS
                }) {
                    return Err(RunQueryError::InvalidUnclassifiedRequest);
                }
                Some(DayWalker::new(
                    request.start_date,
                    request.end_date,
                    settings.day_start_offset,
                    zone.clone(),
                )?)
            }
            QueryRequest::Range(request) => Some(DayWalker::new(
                request.start_date,
                request.end_date,
                settings.day_start_offset,
                zone.clone(),
            )?),
            _ => None,
        };
        let first_weekday = match request {
            QueryRequest::Range(request)
                if request.bucket == quartz_protocol::RangeBucket::Week =>
            {
                Some(FirstWeekday::resolve(snapshot.first_day_of_week()?)?)
            }
            _ => None,
        };
        let mut range_accumulator = match request {
            QueryRequest::Range(request) => Some(RangeAccumulator::new(request, first_weekday)?),
            _ => None,
        };
        let range = match request {
            QueryRequest::Unclassified(_) | QueryRequest::Range(_) => {
                day_walker.as_ref().expect("range calendar").range
            }
            QueryRequest::Day(_) | QueryRequest::PendingAway(_) => {
                calendar.as_ref().expect("day calendar").range
            }
            QueryRequest::Facts(facts) => QueryRange {
                start: facts
                    .local_date
                    .to_zoned_datetime(zone.clone())?
                    .timestamp()
                    .as_millisecond(),
                end: facts
                    .local_date
                    .tomorrow()?
                    .to_zoned_datetime(zone.clone())?
                    .timestamp()
                    .as_millisecond(),
            },
            QueryRequest::EntityDetail(_) => {
                entity_calendar.as_ref().expect("entity calendar").range
            }
            QueryRequest::SegmentDetail(detail) => {
                detail
                    .end_utc_ms
                    .checked_sub(detail.start_utc_ms)
                    .filter(|duration| (1..=SEGMENT_DETAIL_MAX_SPAN_MS).contains(duration))
                    .ok_or(RunQueryError::InvalidDetailRange)?;
                jiff::Timestamp::from_millisecond(detail.start_utc_ms)
                    .map_err(|_| RunQueryError::InvalidDetailRange)?;
                jiff::Timestamp::from_millisecond(detail.end_utc_ms)
                    .map_err(|_| RunQueryError::InvalidDetailRange)?;
                QueryRange {
                    start: detail.start_utc_ms,
                    end: detail.end_utc_ms,
                }
            }
            QueryRequest::Annotations(request) => QueryRange {
                start: request.start_utc_ms,
                end: request.end_utc_ms,
            },
            QueryRequest::Timeline(timeline) => QueryRange {
                start: timeline.start_utc_ms,
                end: timeline.end_utc_ms,
            },
        };
        if day_walker.is_none()
            && (range.start >= range.end
                || range
                    .end
                    .checked_sub(range.start)
                    .is_none_or(|duration| duration > MAX_QUERY_RANGE_MS))
        {
            return Err(RunQueryError::InvalidRange);
        }
        jiff::Timestamp::from_millisecond(range.start)?;
        jiff::Timestamp::from_millisecond(range.end)?;
        let reference_range = match request {
            QueryRequest::Day(day) => {
                EntityCalendar::in_zone(day.local_date, settings.day_start_offset, zone.clone())?
                    .range
            }
            QueryRequest::EntityDetail(detail) => match &detail.entity {
                quartz_protocol::DetailEntity::App { .. } => {
                    if detail.reference_range.is_some() {
                        return Err(RunQueryError::InvalidDocumentRef);
                    }
                    range
                }
                quartz_protocol::DetailEntity::Document { .. } => {
                    let supplied = detail
                        .reference_range
                        .ok_or(RunQueryError::InvalidDocumentRef)?;
                    let reference = QueryRange {
                        start: supplied.start_utc_ms,
                        end: supplied.end_utc_ms,
                    };
                    if reference.start > range.start
                        || reference.end < range.end
                        || reference
                            .end
                            .checked_sub(reference.start)
                            .is_none_or(|duration| {
                                !(1..=MAX_DOCUMENT_REFERENCE_MS).contains(&duration)
                            })
                    {
                        return Err(RunQueryError::InvalidDocumentRef);
                    }
                    jiff::Timestamp::from_millisecond(reference.start)
                        .map_err(|_| RunQueryError::InvalidDocumentRef)?;
                    jiff::Timestamp::from_millisecond(reference.end)
                        .map_err(|_| RunQueryError::InvalidDocumentRef)?;
                    reference
                }
            },
            _ => range,
        };
        let versions = versions::VersionState {
            days: snapshot
                .versions(reference_range)?
                .into_iter()
                .map(|fact| (fact.key, fact.version))
                .collect(),
            settings,
            rule_hashes: rule_hashes.clone(),
            time_zone: zone.clone(),
        }
        .digest(reference_range)?;
        let reference_versions = versions.clone();
        let versions = match first_weekday {
            Some(first) => first.bind_versions(&versions),
            None => versions,
        };
        let entity = match request {
            QueryRequest::EntityDetail(detail) => Some(match &detail.entity {
                quartz_protocol::DetailEntity::App { id } => EntitySelection::App { id: *id },
                quartz_protocol::DetailEntity::Document { document_ref } => {
                    EntitySelection::Document {
                        identity: documents::DocumentReference::validate(
                            document_ref,
                            quartz_protocol::ReferenceRange {
                                start_utc_ms: reference_range.start,
                                end_utc_ms: reference_range.end,
                            },
                            &versions,
                        )?
                        .identity,
                    }
                }
            }),
            _ => None,
        };
        let parameters = request.parameters();
        let cursor = match request {
            QueryRequest::Annotations(request) => request.cursor.as_ref(),
            QueryRequest::Unclassified(request) => request.cursor.as_ref(),
            QueryRequest::Range(request) => request.cursor.as_ref(),
            QueryRequest::Facts(facts) => facts.cursor.as_ref(),
            QueryRequest::Day(day) => day.cursor.as_ref(),
            QueryRequest::PendingAway(away) => away.cursor.as_ref(),
            QueryRequest::Timeline(timeline) => timeline.cursor.as_ref(),
            QueryRequest::SegmentDetail(detail) => detail.cursor.as_ref(),
            QueryRequest::EntityDetail(detail) => detail.cursor.as_ref(),
        };
        let position = if matches!(request, QueryRequest::Facts(_)) {
            // Raw run endpoints and timed policies are included after the staged scan.
            PagePosition::First
        } else {
            PagePosition::from_cursor(cursor, &versions, &parameters)?
        };
        if let Some(accumulator) = &mut range_accumulator {
            let calendar = day_walker.as_ref().expect("range calendar");
            let reference = quartz_protocol::ReferenceRange {
                start_utc_ms: reference_range.start,
                end_utc_ms: reference_range.end,
            };
            let response = quartz_protocol::QueryRangeResponse {
                time_zone: calendar.time_zone.clone(),
                day_start_offset_min: calendar.offset.minutes(),
                reference_range: position.is_first().then_some(reference),
                buckets: Vec::new(),
                versions: versions.clone(),
                next_cursor: None,
            };
            let budget = PageBudget::new(&response)?.with_cursor(&QueryCursor::new(
                versions.clone(),
                parameters.clone(),
                usize::MAX,
            ))?;
            accumulator.start_page(response, budget, position, reference, reference_versions);
        }
        let annotations = match request {
            QueryRequest::Annotations(_) => Some(annotations::AnnotationPage::new(
                versions.clone(),
                parameters.clone(),
                position,
            )?),
            _ => None,
        };
        let categories = if annotations.is_some() {
            Vec::new()
        } else {
            snapshot.categories()?
        };
        let scan_range = match &mut day_walker {
            Some(calendar) => calendar.next_range().expect("validated nonempty dates"),
            None => range,
        };
        Ok(PreparedQuery {
            annotations,
            scan_range,
            day_walker,
            range_accumulator,
            categories,
            range,
            reference_range,
            entity,
            versions,
            parameters,
            position,
            calendar,
            entity_calendar,
        })
    }
    /// The shell services capture writes between these bounded read/sweep turns.
    pub(crate) fn advance(&mut self) -> Result<Option<QueryResponse>, RunQueryError> {
        self.advance_timed(&mut QueryTimer::disabled())
    }
    fn advance_timed(
        &mut self,
        timer: &mut QueryTimer<'_>,
    ) -> Result<Option<QueryResponse>, RunQueryError> {
        let stage = std::mem::replace(&mut self.stage, Stage::Complete);
        match stage {
            Stage::Annotations(mut page) => {
                let response = timer.measure(QueryStage::Read, || {
                    page.advance(&mut self.snapshot, self.range)
                })?;
                match response {
                    Some(response) => Ok(Some(QueryResponse::Annotations(response))),
                    None => {
                        self.stage = Stage::Annotations(page);
                        Ok(None)
                    }
                }
            }
            Stage::Facts(mut facts) => {
                match timer.measure(QueryStage::Read, || {
                    self.snapshot.read_fact_batch(self.range)
                })? {
                    Some(batch) => {
                        facts.extend(batch);
                        self.stage = Stage::Facts(facts);
                        Ok(None)
                    }
                    None => {
                        facts.sort_by(crate::store::facts::Fact::chronological_cmp);
                        let records = facts
                            .iter()
                            .map(crate::store::facts::Fact::record)
                            .collect::<Result<Vec<_>, _>>()?;
                        use sha2::{Digest, Sha256};
                        let mut digest = Sha256::new();
                        digest.update(self.versions.0.as_bytes());
                        for record in &records {
                            digest.update(serde_json::to_vec(record)?);
                        }
                        self.versions =
                            QueryVersions(quartz_domain::text::encode_hex(&digest.finalize()));
                        let QueryRequest::Facts(request) = &self.request else {
                            unreachable!("facts stage")
                        };
                        self.position = PagePosition::from_cursor(
                            request.cursor.as_ref(),
                            &self.versions,
                            &self.parameters,
                        )?;
                        timer
                            .measure(QueryStage::PageEncode, || {
                                self.page_response(QueryResponse::Facts(QueryFactsResponse {
                                    facts: records,
                                    versions: self.versions.clone(),
                                    next_cursor: None,
                                }))
                            })
                            .map(Some)
                    }
                }
            }

            Stage::Read(mut pipeline) => {
                match timer.measure(QueryStage::Read, || {
                    self.snapshot
                        .read_batch(self.scan_range)
                        .map_err(RunQueryError::from)
                })? {
                    Some(rows) => {
                        let rows =
                            timer.measure(QueryStage::Classify, || pipeline.classify_rows(rows));
                        timer.measure(QueryStage::Slice, || {
                            for row in rows {
                                pipeline.ingest_classified(row)?;
                            }
                            Ok::<(), RunQueryError>(())
                        })?;
                        self.stage = Stage::Read(pipeline);
                    }
                    None => {
                        timer.measure(QueryStage::Slice, || pipeline.finish_input());
                        self.stage = Stage::Sweep(pipeline);
                    }
                }
                Ok(None)
            }
            Stage::Sweep(mut pipeline) => {
                if !timer.measure(QueryStage::Slice, || pipeline.advance()) {
                    self.stage = Stage::Sweep(pipeline);
                    return Ok(None);
                }
                let output = match &self.request {
                    QueryRequest::Range(_) => Aggregation::Range(
                        self.range_accumulator.take().expect("range fold retained"),
                    ),
                    QueryRequest::Unclassified(_) => Aggregation::Unclassified(
                        self.unclassified.take().expect("daily fold retained"),
                    ),
                    QueryRequest::Annotations(_) | QueryRequest::Facts(_) => {
                        unreachable!("stored entities bypass aggregation")
                    }
                    QueryRequest::Day(_) => Aggregation::Day(DayAccumulator::new(
                        self.calendar.take().expect("day calendar retained"),
                    )),
                    QueryRequest::PendingAway(_) => {
                        Aggregation::PendingAway(pending_away::PendingAwayAccumulator::default())
                    }
                    QueryRequest::SegmentDetail(_) => {
                        Aggregation::SegmentDetail(SegmentDetailProjection::default())
                    }
                    QueryRequest::EntityDetail(_) => {
                        Aggregation::EntityDetail(EntityDetailAccumulator::new(
                            self.entity.take().expect("validated entity retained"),
                            self.entity_calendar
                                .take()
                                .expect("entity calendar retained"),
                        ))
                    }
                    QueryRequest::Timeline(_) => {
                        Aggregation::Timeline(TimelineProjection::default())
                    }
                };
                let derived = timer.measure(QueryStage::Slice, || pipeline.into_segments());
                self.stage = Stage::Aggregate {
                    segments: derived.segments.into_iter(),
                    facts: derived.facts,
                    output,
                };
                Ok(None)
            }
            Stage::Aggregate {
                mut segments,
                mut output,
                facts,
            } => {
                timer.measure(QueryStage::Aggregate, || {
                    for segment in segments.by_ref().take(QUERY_BATCH_ROWS) {
                        match &mut output {
                            Aggregation::Range(range) => range.push(&segment, &facts),
                            Aggregation::Day(day) => day.push(&segment, &facts),
                            Aggregation::Unclassified(groups) => groups.push(&segment, &facts),
                            Aggregation::PendingAway(away) => away.push(&segment),
                            Aggregation::SegmentDetail(detail) => {
                                let QueryRequest::SegmentDetail(request) = &self.request else {
                                    unreachable!("segment detail aggregation")
                                };
                                detail.push(&segment, &facts, request);
                            }
                            Aggregation::EntityDetail(detail) => detail.push(&segment, &facts),
                            Aggregation::Timeline(timeline) => {
                                let QueryRequest::Timeline(request) = &self.request else {
                                    unreachable!("timeline aggregation")
                                };
                                timeline.push(
                                    &segment,
                                    &facts,
                                    request.detail,
                                    request.ms_per_pixel.0.get(),
                                );
                            }
                        }
                    }
                });
                if segments.len() > 0 {
                    self.stage = Stage::Aggregate {
                        segments,
                        facts,
                        output,
                    };
                    return Ok(None);
                }
                if let Some(walker) = &mut self.day_walker
                    && let Some(range) = walker.next_range()
                {
                    match output {
                        Aggregation::Unclassified(groups) => self.unclassified = Some(groups),
                        Aggregation::Range(mut groups) => {
                            timer
                                .measure(QueryStage::Aggregate, || groups.next_day(walker.date))?;
                            self.range_accumulator = Some(groups);
                        }
                        _ => unreachable!("daily scan aggregation"),
                    }
                    self.scan_range = range;
                    self.snapshot.restart_scan();
                    self.stage = Stage::Read(Pipeline::new(range, facts.into_classifier()));
                    return Ok(None);
                }
                let response =
                    timer.measure(QueryStage::Aggregate, || self.finish_aggregation(output))?;
                timer
                    .measure(QueryStage::PageEncode, || self.page_response(response))
                    .map(Some)
            }
            Stage::Complete => Ok(None),
        }
    }
    fn finish_aggregation(&self, output: Aggregation) -> Result<QueryResponse, RunQueryError> {
        Ok(match output {
            Aggregation::Range(groups) => {
                let page = groups.finish()?;
                let mut response = page.response;
                response.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
                QueryResponse::Range(response)
            }
            Aggregation::Unclassified(groups) => {
                let calendar = self.day_walker.as_ref().expect("fold calendar");
                let reference_range = quartz_protocol::ReferenceRange {
                    start_utc_ms: self.range.start,
                    end_utc_ms: self.range.end,
                };
                let mut response = quartz_protocol::QueryUnclassifiedResponse {
                    time_zone: calendar.time_zone.clone(),
                    day_start_offset_min: calendar.offset.minutes(),
                    reference_range: self.reference_range(),
                    items: Vec::new(),
                    versions: self.versions.clone(),
                    next_cursor: None,
                };
                let page = self.page_budget(&response)?.page_mapped(
                    groups.finish(),
                    self.position,
                    |group| group.into_item(reference_range, &self.versions),
                )?;
                response.items = page.items;
                response.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
                QueryResponse::Unclassified(response)
            }
            Aggregation::Day(day) => {
                let aggregate = day.finish();
                let first = self.position.is_first();
                let mut response = QueryDayResponse {
                    reference_range: self.reference_range(),
                    documents: Vec::new(),
                    metrics: first.then_some(aggregate.metrics),
                    hours: first.then_some(aggregate.calendar.hours),
                    applications: Vec::new(),
                    versions: self.versions.clone(),
                    next_cursor: None,
                };
                let page = self.page_budget(&response)?.lists_mapped(
                    aggregate.ranking,
                    aggregate.documents,
                    self.position,
                    |entry| entry.into_rank(self.reference_range, &self.versions),
                )?;
                response.applications = page.first;
                response.documents = page.second;
                response.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
                QueryResponse::Day(response)
            }
            Aggregation::PendingAway(away) => {
                QueryResponse::PendingAway(QueryPendingAwayResponse {
                    ranges: away.finish(),
                    versions: self.versions.clone(),
                    next_cursor: None,
                })
            }
            Aggregation::SegmentDetail(detail) => {
                QueryResponse::SegmentDetail(QuerySegmentDetailResponse {
                    reference_range: self.reference_range(),
                    intervals: detail.finish(),
                    versions: self.versions.clone(),
                    next_cursor: None,
                })
            }
            Aggregation::EntityDetail(detail) => {
                QueryResponse::EntityDetail(detail.finish(self.versions.clone(), self.position))
            }
            Aggregation::Timeline(projection) => QueryResponse::Timeline(QueryTimelineResponse {
                reference_range: self.reference_range(),
                segments: projection.into_segments(),
                versions: self.versions.clone(),
                next_cursor: None,
            }),
        })
    }
    fn reference_range(&self) -> Option<quartz_protocol::ReferenceRange> {
        self.position
            .is_first()
            .then_some(quartz_protocol::ReferenceRange {
                start_utc_ms: self.reference_range.start,
                end_utc_ms: self.reference_range.end,
            })
    }
    fn page_budget(&self, response: &impl Serialize) -> Result<PageBudget, RunQueryError> {
        PageBudget::new(response)?.with_cursor(&QueryCursor::new(
            self.versions.clone(),
            self.parameters.clone(),
            usize::MAX,
        ))
    }
    fn page_response(&self, mut response: QueryResponse) -> Result<QueryResponse, RunQueryError> {
        match &mut response {
            QueryResponse::Facts(facts) => {
                let items = std::mem::take(&mut facts.facts);
                let page = self.page_budget(facts)?.page(items, self.position)?;
                facts.facts = page.items;
                facts.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }

            QueryResponse::Annotations(_)
            | QueryResponse::Day(_)
            | QueryResponse::Unclassified(_)
            | QueryResponse::Range(_) => {}
            QueryResponse::PendingAway(away) => {
                let items = std::mem::take(&mut away.ranges);
                let page = self.page_budget(away)?.page(items, self.position)?;
                away.ranges = page.items;
                away.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }
            QueryResponse::SegmentDetail(detail) => {
                let items = std::mem::take(&mut detail.intervals);
                let mut references = documents::PageDocumentReferences::new(
                    quartz_protocol::ReferenceRange {
                        start_utc_ms: self.range.start,
                        end_utc_ms: self.range.end,
                    },
                    &self.versions,
                );
                let page =
                    self.page_budget(detail)?
                        .page_with(items, self.position, |segment| references.attach(segment))?;
                detail.intervals = page.items;
                detail.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }
            QueryResponse::EntityDetail(detail) => {
                let items = std::mem::take(&mut detail.rules);
                let extraction = std::mem::take(&mut detail.extraction);
                let page = self
                    .page_budget(detail)?
                    .lists(items, extraction, self.position)?;
                detail.rules = page.first;
                detail.extraction = page.second;
                detail.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }
            QueryResponse::Timeline(timeline) => {
                let items = std::mem::take(&mut timeline.segments);
                let mut references = documents::PageDocumentReferences::new(
                    quartz_protocol::ReferenceRange {
                        start_utc_ms: self.range.start,
                        end_utc_ms: self.range.end,
                    },
                    &self.versions,
                );
                let page =
                    self.page_budget(timeline)?
                        .page_with(items, self.position, |segment| references.attach(segment))?;
                timeline.segments = page.items;
                timeline.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }
        }
        Ok(response)
    }
    pub(crate) fn finish(self) -> Result<ReadOnlyStore, RunQueryError> {
        Ok(self.snapshot.finish()?)
    }
    pub(crate) fn run_measured(mut self) -> Result<CompletedQuery, RunQueryError> {
        let mut timings = QueryTimings::default();
        loop {
            if let Some(response) = self.advance_timed(&mut QueryTimer::record(&mut timings))? {
                let reader =
                    QueryTimer::record(&mut timings).measure(QueryStage::Read, || self.finish())?;
                return Ok(CompletedQuery {
                    response,
                    reader,
                    timings,
                });
            }
        }
    }
}
#[derive(Debug)]
pub(crate) struct CompletedQuery {
    pub response: QueryResponse,
    pub reader: ReadOnlyStore,
    pub timings: QueryTimings,
}

#[derive(Debug)]
struct PreparedQuery {
    annotations: Option<annotations::AnnotationPage>,
    scan_range: QueryRange,
    day_walker: Option<DayWalker>,
    range_accumulator: Option<RangeAccumulator>,
    categories: Vec<quartz_domain::categories::Category>,
    range: QueryRange,
    reference_range: QueryRange,
    entity: Option<EntitySelection>,
    versions: QueryVersions,
    parameters: CursorQuery,
    position: PagePosition,
    calendar: Option<DayCalendar>,
    entity_calendar: Option<EntityCalendar>,
}
