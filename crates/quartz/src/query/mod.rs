//! Snapshot query orchestration around pure slicing, aggregation, and frame-bounded output.

mod annotations;
pub(crate) mod calendar;
pub(crate) mod classification;
mod day_walker;
mod details;
#[cfg(any(windows, test))]
pub(crate) mod dispatch;
pub(crate) mod documents;
mod facts;
pub(crate) mod preview;
#[cfg(test)]
pub(crate) use facts::FINALIZE_BATCH_RECORDS;
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
use paging::JsonBytes;
use pipeline::{DayAccumulator, DerivedSegment, Pipeline, QUERY_BATCH_ROWS, TimelineProjection};
use quartz_protocol::{
    DAY_MAX_APPLICATIONS, DAY_MAX_DOCUMENTS, ErrorKind, FACTS_DEFAULT_LIMIT, FACTS_MAX_LIMIT,
    PENDING_AWAY_MAX_RANGES, QueryDayRequest, QueryDayResponse, QueryEntityDetailRequest,
    QueryEntityDetailResponse, QueryFactsRequest, QueryFactsResponse, QueryPendingAwayRequest,
    QueryPendingAwayResponse, QueryRangeResponse, QuerySegmentDetailRequest,
    QuerySegmentDetailResponse, QueryTimelineRequest, QueryTimelineResponse, QueryVersions,
    SEGMENT_DETAIL_MAX_INTERVALS, SEGMENT_DETAIL_MAX_SPAN_MS, TIMELINE_MAX_SEGMENTS,
    UNCLASSIFIED_MAX_ITEMS,
};
use range::{FirstWeekday, RangeAccumulator};
use serde::Serialize;
use timing::{QueryStage, QueryTimer, QueryTimings};
use unclassified::UnclassifiedAccumulator;

/// Increment whenever matching, slicing, duration, aggregation, or projection semantics change.
pub(crate) const QUERY_SEMANTICS_VERSION: u32 = 11;
/// UTC timeline ranges are bounded to one leap year to limit one job's retained inputs.
pub(crate) const MAX_QUERY_RANGE_MS: i64 = 366 * quartz_domain::capture::limits::UTC_DAY_MS;
/// A 366-logical-day reference can include a backward UTC offset change of up to one day.
const MAX_DOCUMENT_REFERENCE_MS: i64 =
    MAX_QUERY_RANGE_MS + quartz_domain::capture::limits::UTC_DAY_MS;
/// Reserve space for the enclosing JSON response envelope (`t`, `id`, `ok`, `body`).
const QUERY_ENVELOPE_RESERVE_BYTES: usize = 256;
/// Maximum encoded JSON body size for a single-frame query response (1 MiB frame ceiling).
pub(crate) const QUERY_MAX_RESPONSE_BYTES: usize =
    quartz_protocol::MAX_FRAME_LEN - QUERY_ENVELOPE_RESERVE_BYTES;

#[derive(Debug, snafu::Snafu)]
pub(crate) enum RunQueryError {
    #[snafu(display("invalid query range"))]
    InvalidRange,
    #[snafu(display("query protocol request was rejected"))]
    RejectQueryError {
        error: quartz_protocol::ProtocolError,
    },
    #[snafu(display("segment detail range exceeds its bound or has invalid endpoints"))]
    InvalidDetailRange,
    #[snafu(display("invalid unclassified query dates or search length"))]
    InvalidUnclassifiedRequest,
    #[snafu(display("invalid range query dates or dimensions"))]
    InvalidRangeRequest,
    #[snafu(display("invalid facts query row limit"))]
    InvalidFactsRequest,
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
    #[snafu(display("query job has an invalid computation state"))]
    AdvanceQueryError,
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
            Self::RejectQueryError { error } => error.kind.clone(),
            Self::InvalidRange | Self::Calendar { .. } => ErrorKind::InvalidRange,
            Self::InvalidCursor
            | Self::InvalidDetailRange
            | Self::InvalidDocumentRef
            | Self::InvalidUnclassifiedRequest
            | Self::InvalidRangeRequest
            | Self::InvalidFactsRequest => ErrorKind::BadRequest,
            Self::StaleDocumentRef => ErrorKind::StaleDocumentRef,
            Self::StaleCursor => ErrorKind::StaleCursor,
            Self::PageTooLarge => ErrorKind::PageTooLarge,
            Self::ReadFirstWeekdayError
            | Self::InvalidDayOffset
            | Self::ReaderUnavailable
            | Self::Cleanup { .. }
            | Self::RecoverReader { .. }
            | Self::Derive { .. }
            | Self::AdvanceQueryError
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
}
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum QueryResponse {
    Preview(quartz_protocol::RulesPreviewResponse),
    Annotations(quartz_protocol::QueryAnnotationsResponse),
    Facts(QueryFactsResponse),
    Day(QueryDayResponse),
    Unclassified(quartz_protocol::QueryUnclassifiedResponse),
    Range(QueryRangeResponse),
    PendingAway(QueryPendingAwayResponse),
    Timeline(QueryTimelineResponse),
    SegmentDetail(QuerySegmentDetailResponse),
    EntityDetail(QueryEntityDetailResponse),
}
#[derive(Debug)]
enum Stage {
    Annotations(Box<annotations::AnnotationRead>),
    Facts {
        ordered: facts::OrderedFacts,
        limit: usize,
    },
    FinalizeFacts(Box<facts::FinalizeFacts>),
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
                stage: if let Some(read) = prepared.annotations {
                    Stage::Annotations(Box::new(read))
                } else if let Some((after, limit)) = prepared.facts {
                    Stage::Facts {
                        ordered: facts::OrderedFacts::new(after),
                        limit,
                    }
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
        let facts = match request {
            QueryRequest::Facts(facts) => {
                let limit = match facts.limit {
                    None => FACTS_DEFAULT_LIMIT,
                    Some(value) => {
                        let value = usize::try_from(value)
                            .map_err(|_| RunQueryError::InvalidFactsRequest)?;
                        if value == 0 || value > FACTS_MAX_LIMIT {
                            return Err(RunQueryError::InvalidFactsRequest);
                        }
                        value
                    }
                };
                Some((facts.after, limit))
            }
            _ => None,
        };
        snapshot.begin()?;
        let settings = snapshot.settings()?;
        let calendar = match request {
            QueryRequest::Day(day) => Some(DayCalendar::in_zone(
                day.local_date,
                settings.day_start_offset,
                zone.clone(),
            )?),
            _ => None,
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
            QueryRequest::Day(_) => calendar.as_ref().expect("day calendar").range,
            QueryRequest::PendingAway(away) => DayCalendar::range_in_zone(
                away.local_date,
                settings.day_start_offset,
                zone.clone(),
            )?,
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
        let (versions, reference_versions) = if facts.is_some() {
            (QueryVersions(String::new()), QueryVersions(String::new()))
        } else {
            let base = versions::VersionState {
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
            let bound = match first_weekday {
                Some(first) => first.bind_versions(&base),
                None => base.clone(),
            };
            (bound, base)
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
        if let Some(accumulator) = &mut range_accumulator {
            let calendar = day_walker.as_ref().expect("range calendar");
            let reference = quartz_protocol::ReferenceRange {
                start_utc_ms: reference_range.start,
                end_utc_ms: reference_range.end,
            };
            let response = QueryRangeResponse {
                time_zone: calendar.time_zone.clone(),
                day_start_offset_min: calendar.offset.minutes(),
                reference_range: Some(reference),
                buckets: Vec::new(),
                versions: versions.clone(),
                truncated: false,
            };
            accumulator.start_output(response, reference, reference_versions);
        }
        let annotations = match request {
            QueryRequest::Annotations(_) => {
                Some(annotations::AnnotationRead::new(versions.clone())?)
            }
            _ => None,
        };
        let categories = if annotations.is_some() || facts.is_some() {
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
            facts,
            scan_range,
            day_walker,
            range_accumulator,
            categories,
            range,
            reference_range,
            entity,
            versions,
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
            Stage::Annotations(mut read) => {
                let response = timer.measure(QueryStage::Read, || {
                    read.advance(&mut self.snapshot, self.range)
                })?;
                match response {
                    Some(response) => Ok(Some(QueryResponse::Annotations(response))),
                    None => {
                        self.stage = Stage::Annotations(read);
                        Ok(None)
                    }
                }
            }
            Stage::Facts { mut ordered, limit } => {
                match timer.measure(QueryStage::Read, || {
                    self.snapshot.read_fact_batch(self.range)
                })? {
                    Some(batch) => {
                        ordered.extend(batch);
                        self.stage = Stage::Facts { ordered, limit };
                        Ok(None)
                    }
                    None => {
                        let finalizer =
                            timer.measure(QueryStage::PageEncode, || ordered.finalize(limit))?;
                        self.stage = Stage::FinalizeFacts(Box::new(finalizer));
                        Ok(None)
                    }
                }
            }
            Stage::FinalizeFacts(mut finalizer) => {
                if timer.measure(QueryStage::PageEncode, || finalizer.advance())?
                    == facts::FinalizeProgress::More
                {
                    self.stage = Stage::FinalizeFacts(finalizer);
                    return Ok(None);
                }
                Ok(Some(QueryResponse::Facts(finalizer.finish())))
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
                    .measure(QueryStage::PageEncode, || self.fit_response(response))
                    .map(Some)
            }
            Stage::Complete => Ok(None),
        }
    }
    fn finish_aggregation(&self, output: Aggregation) -> Result<QueryResponse, RunQueryError> {
        Ok(match output {
            Aggregation::Range(groups) => QueryResponse::Range(groups.finish()?),
            Aggregation::Unclassified(groups) => {
                let calendar = self.day_walker.as_ref().expect("fold calendar");
                let reference_range = quartz_protocol::ReferenceRange {
                    start_utc_ms: self.range.start,
                    end_utc_ms: self.range.end,
                };
                let mut groups = groups.finish();
                let mut truncated = false;
                if groups.len() > UNCLASSIFIED_MAX_ITEMS {
                    groups.truncate(UNCLASSIFIED_MAX_ITEMS);
                    truncated = true;
                }
                let items = groups
                    .into_iter()
                    .map(|group| group.into_item(reference_range, &self.versions))
                    .collect::<Result<Vec<_>, _>>()?;
                QueryResponse::Unclassified(quartz_protocol::QueryUnclassifiedResponse {
                    time_zone: calendar.time_zone.clone(),
                    day_start_offset_min: calendar.offset.minutes(),
                    reference_range: Some(reference_range),
                    items,
                    versions: self.versions.clone(),
                    truncated,
                })
            }
            Aggregation::Day(day) => {
                let aggregate = day.finish();
                let mut truncated = false;
                let mut applications = aggregate.ranking;
                if applications.len() > DAY_MAX_APPLICATIONS {
                    applications.truncate(DAY_MAX_APPLICATIONS);
                    truncated = true;
                }
                let mut raw_documents = aggregate.documents;
                if raw_documents.len() > DAY_MAX_DOCUMENTS {
                    raw_documents.truncate(DAY_MAX_DOCUMENTS);
                    truncated = true;
                }
                let documents = raw_documents
                    .into_iter()
                    .map(|entry| entry.into_rank(self.reference_range, &self.versions))
                    .collect::<Result<Vec<_>, _>>()?;
                QueryResponse::Day(QueryDayResponse {
                    reference_range: Some(self.reference_range()),
                    documents,
                    metrics: aggregate.metrics,
                    hours: aggregate.calendar.hours,
                    applications,
                    versions: self.versions.clone(),
                    truncated,
                })
            }
            Aggregation::PendingAway(away) => {
                let mut ranges = away.finish();
                let mut truncated = false;
                if ranges.len() > PENDING_AWAY_MAX_RANGES {
                    ranges.truncate(PENDING_AWAY_MAX_RANGES);
                    truncated = true;
                }
                QueryResponse::PendingAway(QueryPendingAwayResponse {
                    ranges,
                    versions: self.versions.clone(),
                    truncated,
                })
            }
            Aggregation::SegmentDetail(detail) => {
                let mut intervals = detail.finish();
                let mut truncated = false;
                if intervals.len() > SEGMENT_DETAIL_MAX_INTERVALS {
                    intervals.truncate(SEGMENT_DETAIL_MAX_INTERVALS);
                    truncated = true;
                }
                let mut references = documents::PageDocumentReferences::new(
                    quartz_protocol::ReferenceRange {
                        start_utc_ms: self.range.start,
                        end_utc_ms: self.range.end,
                    },
                    &self.versions,
                );
                for segment in &mut intervals {
                    references.attach(segment)?;
                }
                QueryResponse::SegmentDetail(QuerySegmentDetailResponse {
                    reference_range: Some(self.reference_range()),
                    intervals,
                    versions: self.versions.clone(),
                    truncated,
                })
            }
            Aggregation::EntityDetail(detail) => {
                QueryResponse::EntityDetail(detail.finish(self.versions.clone()))
            }
            Aggregation::Timeline(projection) => {
                let mut segments = projection.into_segments();
                let mut truncated = false;
                if segments.len() > TIMELINE_MAX_SEGMENTS {
                    segments.truncate(TIMELINE_MAX_SEGMENTS);
                    truncated = true;
                }
                let mut references = documents::PageDocumentReferences::new(
                    quartz_protocol::ReferenceRange {
                        start_utc_ms: self.range.start,
                        end_utc_ms: self.range.end,
                    },
                    &self.versions,
                );
                for segment in &mut segments {
                    references.attach(segment)?;
                }
                QueryResponse::Timeline(QueryTimelineResponse {
                    reference_range: Some(self.reference_range()),
                    segments,
                    versions: self.versions.clone(),
                    truncated,
                })
            }
        })
    }
    fn reference_range(&self) -> quartz_protocol::ReferenceRange {
        quartz_protocol::ReferenceRange {
            start_utc_ms: self.reference_range.start,
            end_utc_ms: self.reference_range.end,
        }
    }
    fn fit_response(&self, response: QueryResponse) -> Result<QueryResponse, RunQueryError> {
        Ok(match response {
            QueryResponse::Preview(_) | QueryResponse::Annotations(_) | QueryResponse::Facts(_) => {
                response
            }
            QueryResponse::Day(day) => QueryResponse::Day(fit_two_lists(
                day,
                QUERY_MAX_RESPONSE_BYTES,
                |r| &mut r.applications,
                |r| &mut r.documents,
                |r| r.truncated = true,
            )?),
            QueryResponse::Timeline(timeline) => QueryResponse::Timeline(fit_single_list(
                timeline,
                QUERY_MAX_RESPONSE_BYTES,
                |r| &mut r.segments,
                |r| r.truncated = true,
            )?),
            QueryResponse::SegmentDetail(detail) => QueryResponse::SegmentDetail(fit_single_list(
                detail,
                QUERY_MAX_RESPONSE_BYTES,
                |r| &mut r.intervals,
                |r| r.truncated = true,
            )?),
            QueryResponse::EntityDetail(detail) => QueryResponse::EntityDetail(fit_two_lists(
                detail,
                QUERY_MAX_RESPONSE_BYTES,
                |r| &mut r.rules,
                |r| &mut r.extraction,
                |r| r.truncated = true,
            )?),
            QueryResponse::Range(range) => {
                QueryResponse::Range(fit_range_response(range, QUERY_MAX_RESPONSE_BYTES)?)
            }
            QueryResponse::Unclassified(unclassified) => {
                QueryResponse::Unclassified(fit_single_list(
                    unclassified,
                    QUERY_MAX_RESPONSE_BYTES,
                    |r| &mut r.items,
                    |r| r.truncated = true,
                )?)
            }
            QueryResponse::PendingAway(away) => QueryResponse::PendingAway(fit_single_list(
                away,
                QUERY_MAX_RESPONSE_BYTES,
                |r| &mut r.ranges,
                |r| r.truncated = true,
            )?),
        })
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

fn fit_single_list<R, T>(
    mut response: R,
    max_bytes: usize,
    list: impl Fn(&mut R) -> &mut Vec<T>,
    set_truncated: impl Fn(&mut R),
) -> Result<R, RunQueryError>
where
    R: Serialize,
    T: Serialize,
{
    if JsonBytes::measure(&response)? <= max_bytes {
        return Ok(response);
    }
    let items = std::mem::take(list(&mut response));
    set_truncated(&mut response);
    let mut used = JsonBytes::measure(&response)?;
    if used <= max_bytes {
        let mut kept = Vec::new();
        for item in items {
            let item_bytes = JsonBytes::measure(&item)? + 1;
            if used.saturating_add(item_bytes) > max_bytes {
                break;
            }
            used += item_bytes;
            kept.push(item);
        }
        *list(&mut response) = kept;
    }
    Ok(response)
}

fn fit_two_lists<R, A, B>(
    mut response: R,
    max_bytes: usize,
    first: impl Fn(&mut R) -> &mut Vec<A>,
    second: impl Fn(&mut R) -> &mut Vec<B>,
    set_truncated: impl Fn(&mut R),
) -> Result<R, RunQueryError>
where
    R: Serialize,
    A: Serialize,
    B: Serialize,
{
    if JsonBytes::measure(&response)? <= max_bytes {
        return Ok(response);
    }
    set_truncated(&mut response);
    loop {
        if JsonBytes::measure(&response)? <= max_bytes {
            return Ok(response);
        }
        let first_len = first(&mut response).len();
        let second_len = second(&mut response).len();
        if first_len == 0 && second_len == 0 {
            return Ok(response);
        }
        if first_len >= second_len {
            let target = if first_len > second_len {
                second_len
            } else {
                first_len / 2
            };
            let removed = first(&mut response).split_off(target);
            let mut used = JsonBytes::measure(&response)?;
            if used <= max_bytes {
                let mut extra = Vec::new();
                for item in removed {
                    let item_bytes = JsonBytes::measure(&item)? + 1;
                    if used.saturating_add(item_bytes) > max_bytes {
                        break;
                    }
                    used += item_bytes;
                    extra.push(item);
                }
                first(&mut response).extend(extra);
                while JsonBytes::measure(&response)? > max_bytes {
                    if first(&mut response).pop().is_none() {
                        break;
                    }
                }
                return Ok(response);
            }
        } else {
            let target = first_len;
            let removed = second(&mut response).split_off(target);
            let mut used = JsonBytes::measure(&response)?;
            if used <= max_bytes {
                let mut extra = Vec::new();
                for item in removed {
                    let item_bytes = JsonBytes::measure(&item)? + 1;
                    if used.saturating_add(item_bytes) > max_bytes {
                        break;
                    }
                    used += item_bytes;
                    extra.push(item);
                }
                second(&mut response).extend(extra);
                while JsonBytes::measure(&response)? > max_bytes {
                    if second(&mut response).pop().is_none() {
                        break;
                    }
                }
                return Ok(response);
            }
        }
    }
}

fn fit_range_response(
    mut response: QueryRangeResponse,
    max_bytes: usize,
) -> Result<QueryRangeResponse, RunQueryError> {
    if JsonBytes::measure(&response)? <= max_bytes {
        return Ok(response);
    }
    response.truncated = true;
    let mut max_len = response
        .buckets
        .iter()
        .map(|bucket| bucket.groups.len())
        .max()
        .unwrap_or(0);
    while max_len > 0 {
        let next_cap = max_len / 2;
        let mut saved = Vec::new();
        for (index, bucket) in response.buckets.iter_mut().enumerate() {
            if bucket.groups.len() > next_cap {
                saved.push((index, bucket.groups.split_off(next_cap)));
            }
        }
        let mut used = JsonBytes::measure(&response)?;
        if used <= max_bytes {
            'restore: for (index, groups) in saved {
                for group in groups {
                    let bytes = JsonBytes::measure(&group)? + 1;
                    if used.saturating_add(bytes) > max_bytes {
                        break 'restore;
                    }
                    used += bytes;
                    response.buckets[index].groups.push(group);
                }
            }
            return Ok(response);
        }
        max_len = next_cap;
    }
    fit_single_list(
        response,
        max_bytes,
        |r| &mut r.buckets,
        |r| r.truncated = true,
    )
}

#[derive(Debug)]
pub(crate) struct CompletedQuery {
    pub response: QueryResponse,
    pub reader: ReadOnlyStore,
    pub timings: QueryTimings,
}

#[derive(Debug)]
struct PreparedQuery {
    annotations: Option<annotations::AnnotationRead>,
    facts: Option<(Option<quartz_protocol::facts::FactsCursor>, usize)>,
    scan_range: QueryRange,
    day_walker: Option<DayWalker>,
    range_accumulator: Option<RangeAccumulator>,
    categories: Vec<quartz_domain::categories::Category>,
    range: QueryRange,
    reference_range: QueryRange,
    entity: Option<EntitySelection>,
    versions: QueryVersions,
    calendar: Option<DayCalendar>,
    entity_calendar: Option<EntityCalendar>,
}
