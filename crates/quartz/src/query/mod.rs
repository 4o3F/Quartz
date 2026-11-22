//! Snapshot query orchestration around pure slicing, aggregation, and paging.

pub(crate) mod calendar;
pub(crate) mod classification;
mod details;
#[cfg(any(windows, test))]
pub(crate) mod dispatch;
pub(crate) mod input;
mod paging;
mod pending_away;
pub(crate) mod pipeline;
#[cfg(any(windows, test))]
pub(crate) mod runner;
#[cfg(test)]
mod tests;
pub(crate) mod timing;
pub(crate) mod versions;

use crate::store::{ReadOnlyStore, query::ReadSnapshot, query_error::ReadQueryError};
use calendar::DayCalendar;
use details::{EntityCalendar, EntityDetailAccumulator, SegmentDetailProjection};
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
use serde::Serialize;
use timing::{QueryStage, QueryTimer, QueryTimings};

/// Increment whenever slicing, duration, aggregation, or projection semantics change.
pub(crate) const QUERY_SEMANTICS_VERSION: u32 = 3;
/// UTC timeline ranges are bounded to one leap year to limit one job's retained inputs.
const MAX_QUERY_RANGE_MS: i64 = 366 * quartz_domain::capture::limits::UTC_DAY_MS;

#[derive(Debug, snafu::Snafu)]
pub(crate) enum RunQueryError {
    #[snafu(display("invalid query range"))]
    InvalidRange,
    #[snafu(display("segment detail range exceeds its bound or has invalid endpoints"))]
    InvalidDetailRange,
    #[snafu(display("invalid query cursor"))]
    InvalidCursor,
    #[snafu(display("query input versions changed"))]
    StaleCursor,
    #[snafu(display("invalid logical day offset"))]
    InvalidDayOffset,
    #[snafu(display("query reader is unavailable"))]
    ReaderUnavailable,
    #[snafu(display("could not derive query segments: {source}"))]
    Derive {
        source: pipeline::DeriveSegmentsError,
    },
    #[snafu(display("one query item exceeds the page size"))]
    PageTooLarge,
    #[snafu(display("could not read query snapshot: {source}"))]
    Read { source: ReadQueryError },
    #[snafu(display("could not interpret query calendar: {source}"))]
    Calendar { source: jiff::Error },
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
    pub(crate) fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidRange | Self::Calendar { .. } => ErrorKind::InvalidRange,
            Self::InvalidCursor | Self::InvalidDetailRange => ErrorKind::BadRequest,
            Self::StaleCursor => ErrorKind::StaleCursor,
            _ => ErrorKind::Internal,
        }
    }
}
#[derive(Debug, Clone)]
pub(crate) enum QueryRequest {
    Facts(QueryFactsRequest),
    Day(QueryDayRequest),
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
            quartz_protocol::Op::QueryFacts => serde_json::from_value(body).map(Self::Facts),
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
            Self::Facts(facts) => CursorQuery::from(facts),
            Self::Day(day) => CursorQuery::from(day),
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
    Facts(QueryFactsResponse),
    Day(QueryDayResponse),
    PendingAway(QueryPendingAwayResponse),
    Timeline(QueryTimelineResponse),
    SegmentDetail(QuerySegmentDetailResponse),
    EntityDetail(QueryEntityDetailResponse),
}
#[derive(Debug)]
enum Stage {
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
    versions: QueryVersions,
    parameters: CursorQuery,
    position: PagePosition,
    calendar: Option<DayCalendar>,
    entity_calendar: Option<EntityCalendar>,
    stage: Stage,
}
impl QueryJob {
    pub(crate) fn start(
        reader: &mut Option<ReadOnlyStore>,
        request: QueryRequest,
        rules: std::sync::Arc<quartz_domain::rules::registry::RuleSnapshot>,
    ) -> Result<Self, RunQueryError> {
        let mut snapshot = reader
            .take()
            .ok_or(RunQueryError::ReaderUnavailable)?
            .snapshot();
        match Self::prepare(&mut snapshot, &request, &rules.hashes().classification) {
            Ok(prepared) => Ok(Self {
                snapshot,
                range: prepared.range,
                versions: prepared.versions,
                parameters: prepared.parameters,
                position: prepared.position,
                calendar: prepared.calendar,
                entity_calendar: prepared.entity_calendar,
                stage: if matches!(request, QueryRequest::Facts(_)) {
                    Stage::Facts(Vec::new())
                } else {
                    Stage::Read(Pipeline::new(
                        prepared.range,
                        classification::Classifier::new(rules, prepared.categories),
                    ))
                },
                request,
            }),
            Err(error) => {
                *reader = Some(snapshot.finish()?);
                Err(error)
            }
        }
    }
    fn prepare(
        snapshot: &mut ReadSnapshot,
        request: &QueryRequest,
        classification_hash: &quartz_domain::rules::ContentHash,
    ) -> Result<PreparedQuery, RunQueryError> {
        snapshot.begin()?;
        let settings = snapshot.settings()?;
        let calendar = match request {
            QueryRequest::Facts(_) => None,
            QueryRequest::Day(day) => {
                Some(DayCalendar::new(day.local_date, settings.day_start_offset)?)
            }
            QueryRequest::PendingAway(away) => Some(DayCalendar::new(
                away.local_date,
                settings.day_start_offset,
            )?),
            QueryRequest::Timeline(_)
            | QueryRequest::SegmentDetail(_)
            | QueryRequest::EntityDetail(_) => None,
        };
        let entity_calendar = match request {
            QueryRequest::EntityDetail(detail) => Some(EntityCalendar::new(
                detail.local_date,
                settings.day_start_offset,
            )?),
            _ => None,
        };
        let range = match request {
            QueryRequest::Day(_) | QueryRequest::PendingAway(_) => {
                calendar.as_ref().expect("day calendar").range
            }
            QueryRequest::Facts(facts) => {
                let zone = jiff::tz::TimeZone::try_system()?;
                QueryRange {
                    start: facts
                        .local_date
                        .to_zoned_datetime(zone.clone())?
                        .timestamp()
                        .as_millisecond(),
                    end: facts
                        .local_date
                        .tomorrow()?
                        .to_zoned_datetime(zone)?
                        .timestamp()
                        .as_millisecond(),
                }
            }
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
            QueryRequest::Timeline(timeline) => QueryRange {
                start: timeline.start_utc_ms,
                end: timeline.end_utc_ms,
            },
        };
        if range.start >= range.end
            || range
                .end
                .checked_sub(range.start)
                .is_none_or(|duration| duration > MAX_QUERY_RANGE_MS)
        {
            return Err(RunQueryError::InvalidRange);
        }
        jiff::Timestamp::from_millisecond(range.start)?;
        jiff::Timestamp::from_millisecond(range.end)?;
        let versions = versions::VersionState {
            days: snapshot
                .versions(range)?
                .into_iter()
                .map(|fact| (fact.key, fact.version))
                .collect(),
            settings,
            classification_hash: classification_hash.to_owned(),
        }
        .digest(range);
        let parameters = request.parameters();
        let cursor = match request {
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
        let categories = snapshot.categories()?;
        Ok(PreparedQuery {
            categories,
            range,
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
                        .read_batch(self.range)
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
                    QueryRequest::Facts(_) => unreachable!("raw facts bypass aggregation"),
                    QueryRequest::Day(_) => Aggregation::Day(DayAccumulator::new(
                        self.calendar.take().expect("day calendar retained"),
                    )),
                    QueryRequest::PendingAway(_) => {
                        Aggregation::PendingAway(pending_away::PendingAwayAccumulator::default())
                    }
                    QueryRequest::SegmentDetail(_) => {
                        Aggregation::SegmentDetail(SegmentDetailProjection::default())
                    }
                    QueryRequest::EntityDetail(detail) => {
                        Aggregation::EntityDetail(EntityDetailAccumulator::new(
                            detail.entity,
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
                            Aggregation::Day(day) => day.push(&segment, &facts),
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
                let response =
                    timer.measure(QueryStage::Aggregate, || self.finish_aggregation(output));
                timer
                    .measure(QueryStage::PageEncode, || self.page_response(response))
                    .map(Some)
            }
            Stage::Complete => Ok(None),
        }
    }
    fn finish_aggregation(&self, output: Aggregation) -> QueryResponse {
        match output {
            Aggregation::Day(day) => {
                let aggregate = day.finish();
                let first = self.position.is_first();
                QueryResponse::Day(QueryDayResponse {
                    metrics: first.then_some(aggregate.metrics),
                    hours: first.then_some(aggregate.calendar.hours),
                    applications: aggregate.ranking,
                    versions: self.versions.clone(),
                    next_cursor: None,
                })
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
                    intervals: detail.finish(),
                    versions: self.versions.clone(),
                    next_cursor: None,
                })
            }
            Aggregation::EntityDetail(detail) => {
                QueryResponse::EntityDetail(detail.finish(self.versions.clone(), self.position))
            }
            Aggregation::Timeline(projection) => QueryResponse::Timeline(QueryTimelineResponse {
                segments: projection.into_segments(),
                versions: self.versions.clone(),
                next_cursor: None,
            }),
        }
    }
    fn page_response(&self, mut response: QueryResponse) -> Result<QueryResponse, RunQueryError> {
        match &mut response {
            QueryResponse::Facts(facts) => {
                let items = std::mem::take(&mut facts.facts);
                let page = PageBudget::new(facts)?.page(items, self.position)?;
                facts.facts = page.items;
                facts.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }

            QueryResponse::Day(day) => {
                let items = std::mem::take(&mut day.applications);
                let page = PageBudget::new(day)?.page(items, self.position)?;
                day.applications = page.items;
                day.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }
            QueryResponse::PendingAway(away) => {
                let items = std::mem::take(&mut away.ranges);
                let page = PageBudget::new(away)?.page(items, self.position)?;
                away.ranges = page.items;
                away.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }
            QueryResponse::SegmentDetail(detail) => {
                let items = std::mem::take(&mut detail.intervals);
                let page = PageBudget::new(detail)?.page(items, self.position)?;
                detail.intervals = page.items;
                detail.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }
            QueryResponse::EntityDetail(detail) => {
                let items = std::mem::take(&mut detail.rules);
                let page = PageBudget::new(detail)?.page(items, self.position)?;
                detail.rules = page.items;
                detail.next_cursor = page.next.map(|position| {
                    QueryCursor::new(self.versions.clone(), self.parameters.clone(), position)
                });
            }
            QueryResponse::Timeline(timeline) => {
                let items = std::mem::take(&mut timeline.segments);
                let page = PageBudget::new(timeline)?.page(items, self.position)?;
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
    categories: Vec<quartz_domain::categories::Category>,
    range: QueryRange,
    versions: QueryVersions,
    parameters: CursorQuery,
    position: PagePosition,
    calendar: Option<DayCalendar>,
    entity_calendar: Option<EntityCalendar>,
}
