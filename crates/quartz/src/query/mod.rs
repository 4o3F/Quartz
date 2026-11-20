//! Snapshot query orchestration around pure slicing, aggregation, and paging.

pub(crate) mod calendar;
#[cfg(any(windows, test))]
pub(crate) mod dispatch;
pub(crate) mod input;
mod paging;
pub(crate) mod pipeline;
#[cfg(any(windows, test))]
pub(crate) mod runner;
#[cfg(test)]
mod tests;
pub(crate) mod timing;
pub(crate) mod versions;

use crate::store::{ReadOnlyStore, query::ReadSnapshot, query_error::ReadQueryError};
use calendar::DayCalendar;
use input::QueryRange;
use paging::{PageBudget, PagePosition};
use pipeline::{DayAccumulator, DerivedSegment, Pipeline, QUERY_BATCH_ROWS, TimelineProjection};
use quartz_protocol::{
    CursorQuery, ErrorKind, QueryCursor, QueryDayRequest, QueryDayResponse, QueryTimelineRequest,
    QueryTimelineResponse, QueryVersions,
};
use serde::Serialize;
use timing::{QueryStage, QueryTimer, QueryTimings};

/// Increment whenever slicing, duration, aggregation, or projection semantics change.
pub(crate) const QUERY_SEMANTICS_VERSION: u32 = 1;
/// UTC timeline ranges are bounded to one leap year to limit one job's retained inputs.
const MAX_QUERY_RANGE_MS: i64 = 366 * quartz_domain::capture::limits::UTC_DAY_MS;

#[derive(Debug, snafu::Snafu)]
pub(crate) enum RunQueryError {
    #[snafu(display("invalid query range"))]
    InvalidRange,
    #[snafu(display("invalid query cursor"))]
    InvalidCursor,
    #[snafu(display("query input versions changed"))]
    StaleCursor,
    #[snafu(display("invalid persisted query settings"))]
    InvalidSettings,
    #[snafu(display("invalid persisted capture facts"))]
    InvalidFacts,
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
        match source {
            ReadQueryError::InvalidSettings => Self::InvalidSettings,
            ReadQueryError::InvalidFacts => Self::InvalidFacts,
            source => Self::Read { source },
        }
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
impl From<serde_json::Error> for RunQueryError {
    fn from(source: serde_json::Error) -> Self {
        Self::Encode { source }
    }
}
impl RunQueryError {
    pub(crate) fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidRange | Self::Calendar { .. } => ErrorKind::InvalidRange,
            Self::InvalidCursor => ErrorKind::BadRequest,
            Self::StaleCursor => ErrorKind::StaleCursor,
            _ => ErrorKind::Internal,
        }
    }
}
#[derive(Debug, Clone)]
pub(crate) enum QueryRequest {
    Day(QueryDayRequest),
    Timeline(QueryTimelineRequest),
}
impl QueryRequest {
    #[cfg(windows)]
    pub(crate) fn decode(
        op: quartz_protocol::Op,
        body: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        match op {
            quartz_protocol::Op::QueryDay => serde_json::from_value(body).map(Self::Day),
            quartz_protocol::Op::QueryTimeline => serde_json::from_value(body).map(Self::Timeline),
            _ => Err(<serde_json::Error as serde::de::Error>::custom(
                "operation has no implemented query decoder",
            )),
        }
    }

    fn parameters(&self) -> CursorQuery {
        match self {
            Self::Day(day) => CursorQuery::from(day),
            Self::Timeline(timeline) => CursorQuery::from(timeline),
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum QueryResponse {
    Day(QueryDayResponse),
    Timeline(QueryTimelineResponse),
}
#[derive(Debug)]
enum Stage {
    Read(Pipeline),
    Sweep(Pipeline),
    Aggregate {
        segments: std::vec::IntoIter<DerivedSegment>,
        output: Aggregation,
    },
    Complete,
}
#[derive(Debug)]
enum Aggregation {
    Day(DayAccumulator),
    Timeline(TimelineProjection),
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
    stage: Stage,
}
impl QueryJob {
    pub(crate) fn start(
        reader: &mut Option<ReadOnlyStore>,
        request: QueryRequest,
        classification_hash: &str,
    ) -> Result<Self, RunQueryError> {
        let mut snapshot = reader.take().ok_or(RunQueryError::InvalidFacts)?.snapshot();
        match Self::prepare(&mut snapshot, &request, classification_hash) {
            Ok(prepared) => Ok(Self {
                snapshot,
                request,
                range: prepared.range,
                versions: prepared.versions,
                parameters: prepared.parameters,
                position: prepared.position,
                calendar: prepared.calendar,
                stage: Stage::Read(Pipeline::new(prepared.range)),
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
        classification_hash: &str,
    ) -> Result<PreparedQuery, RunQueryError> {
        snapshot.begin()?;
        let settings = snapshot.settings()?;
        let calendar = match request {
            QueryRequest::Day(day) => Some(DayCalendar::new(
                day.local_date,
                settings.day_start_offset_min,
            )?),
            QueryRequest::Timeline(_) => None,
        };
        let range = match request {
            QueryRequest::Day(_) => calendar.as_ref().expect("day calendar").range,
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
            QueryRequest::Day(day) => day.cursor.as_ref(),
            QueryRequest::Timeline(timeline) => timeline.cursor.as_ref(),
        };
        let position = PagePosition::from_cursor(cursor, &versions, &parameters)?;
        Ok(PreparedQuery {
            range,
            versions,
            parameters,
            position,
            calendar,
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
            Stage::Read(mut pipeline) => {
                match timer.measure(QueryStage::Read, || {
                    self.snapshot
                        .read_batch(self.range)
                        .map_err(RunQueryError::from)
                })? {
                    Some(rows) => {
                        timer.measure(QueryStage::Slice, || {
                            for row in rows {
                                pipeline.ingest(row)?;
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
                    QueryRequest::Day(_) => Aggregation::Day(DayAccumulator::new(
                        self.calendar.take().expect("day calendar retained"),
                    )),
                    QueryRequest::Timeline(_) => {
                        Aggregation::Timeline(TimelineProjection::default())
                    }
                };
                self.stage = Stage::Aggregate {
                    segments: timer
                        .measure(QueryStage::Slice, || pipeline.into_segments().into_iter()),
                    output,
                };
                Ok(None)
            }
            Stage::Aggregate {
                mut segments,
                mut output,
            } => {
                timer.measure(QueryStage::Aggregate, || {
                    for segment in segments.by_ref().take(QUERY_BATCH_ROWS) {
                        match &mut output {
                            Aggregation::Day(day) => day.push(&segment),
                            Aggregation::Timeline(timeline) => {
                                let QueryRequest::Timeline(request) = &self.request else {
                                    unreachable!("timeline aggregation")
                                };
                                timeline.push(
                                    &segment,
                                    request.detail,
                                    request.ms_per_pixel.0.get(),
                                );
                            }
                        }
                    }
                });
                if segments.len() > 0 {
                    self.stage = Stage::Aggregate { segments, output };
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
            Aggregation::Timeline(projection) => QueryResponse::Timeline(QueryTimelineResponse {
                segments: projection.into_segments(),
                versions: self.versions.clone(),
                next_cursor: None,
            }),
        }
    }
    fn page_response(&self, mut response: QueryResponse) -> Result<QueryResponse, RunQueryError> {
        match &mut response {
            QueryResponse::Day(day) => {
                let items = std::mem::take(&mut day.applications);
                let page = PageBudget::new(day)?.page(items, self.position)?;
                day.applications = page.items;
                day.next_cursor = page.next.map(|position| {
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
    range: QueryRange,
    versions: QueryVersions,
    parameters: CursorQuery,
    position: PagePosition,
    calendar: Option<DayCalendar>,
}
