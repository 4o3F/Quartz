//! Bounded snapshot reads for linked web intervals overlapping a logical day.

use super::{READ_BATCH_ROWS, ReadSnapshot, StartCursor, input::WebInterval};
use crate::store::{
    query_error::ReadQueryError,
    schema::{domains, web_intervals},
};
use diesel::{BoolExpressionMethods, ExpressionMethods, JoinOnDsl, QueryDsl, RunQueryDsl};
use quartz_domain::{
    capture::limits::UTC_DAY_MS,
    facts::{FactId, QueryRange},
};

/// `web_intervals.link_status` code for `linked` (schema v1 `enum_labels`).
const LINK_STATUS_LINKED: i32 = 1;

#[derive(Debug, Clone, Copy)]
pub(super) struct StoredStreamId(i64);

#[derive(Debug, Clone, Copy)]
pub(super) enum WebScanCursor {
    Closed(StartCursor<FactId>),
    Open(StoredStreamId),
    Done,
}
impl WebScanCursor {
    pub(super) fn initial() -> Self {
        Self::Closed(StartCursor::initial(FactId(i64::MIN)))
    }
}

#[derive(diesel::Queryable)]
struct WebDetails {
    id: Option<i64>,
    stream_id: i64,
    interval_id: i64,
    start_utc_ms: i64,
    end_utc_ms: Option<i64>,
    confirmed_utc_ms: Option<i64>,
    domain: String,
}

impl ReadSnapshot {
    /// Reads at most `READ_BATCH_ROWS` linked web rows with a domain overlapping `range`.
    pub(crate) fn read_web_batch(
        &mut self,
        range: QueryRange,
    ) -> Result<Option<Vec<WebInterval>>, ReadQueryError> {
        let raw = match self.web_cursor {
            WebScanCursor::Closed(cursor) => {
                let rows = load_web_closed(self.connection(), range, cursor)?;
                if rows.len() < READ_BATCH_ROWS {
                    self.web_cursor = WebScanCursor::Open(StoredStreamId(i64::MIN));
                } else if let Some(last) = rows.last() {
                    let id = last.id.ok_or(ReadQueryError::InvalidFacts)?;
                    self.web_cursor = WebScanCursor::Closed(StartCursor {
                        start_utc_ms: last.start_utc_ms,
                        id: FactId(id),
                    });
                }
                rows
            }
            WebScanCursor::Open(cursor) => {
                let rows = load_web_open(self.connection(), range, cursor)?;
                if rows.len() < READ_BATCH_ROWS {
                    self.web_cursor = WebScanCursor::Done;
                } else if let Some(last) = rows.last() {
                    self.web_cursor = WebScanCursor::Open(StoredStreamId(last.stream_id));
                }
                rows
            }
            WebScanCursor::Done => return Ok(None),
        };
        let mut result = Vec::with_capacity(raw.len());
        for row in raw {
            let _id = row.id.ok_or(ReadQueryError::InvalidFacts)?;
            let end_utc_ms = Self::endpoint(row.end_utc_ms, row.confirmed_utc_ms)?;
            if end_utc_ms < row.start_utc_ms {
                return Err(ReadQueryError::InvalidFacts);
            }
            if end_utc_ms == row.start_utc_ms {
                continue;
            }
            result.push(WebInterval {
                interval_id: FactId(row.interval_id),
                start_utc_ms: row.start_utc_ms,
                end_utc_ms,
                domain: row.domain,
            });
        }
        Ok(Some(result))
    }
}

fn load_web_closed(
    connection: &mut diesel::SqliteConnection,
    range: QueryRange,
    cursor: StartCursor<FactId>,
) -> diesel::QueryResult<Vec<WebDetails>> {
    let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
    let after_start = cursor.start_utc_ms;
    let after_id = cursor.id.0;
    web_intervals::table
        .inner_join(domains::table.on(web_intervals::domain_id.eq(domains::id)))
        .filter(web_intervals::link_status.eq(LINK_STATUS_LINKED))
        .filter(web_intervals::domain_id.is_not_null())
        .filter(
            web_intervals::start_utc_ms.ge(range.start.saturating_sub(UTC_DAY_MS).max(after_start)),
        )
        .filter(web_intervals::end_utc_ms.gt(range.start))
        .filter(
            web_intervals::start_utc_ms
                .gt(after_start)
                .or(web_intervals::start_utc_ms
                    .eq(after_start)
                    .and(web_intervals::id.gt(after_id))),
        )
        .filter(web_intervals::start_utc_ms.lt(range.end))
        .order((web_intervals::start_utc_ms, web_intervals::id))
        .limit(limit)
        .select((
            web_intervals::id,
            web_intervals::stream_id,
            web_intervals::interval_id,
            web_intervals::start_utc_ms,
            web_intervals::end_utc_ms,
            web_intervals::confirmed_utc_ms,
            domains::host,
        ))
        .load::<WebDetails>(connection)
}

fn load_web_open(
    connection: &mut diesel::SqliteConnection,
    range: QueryRange,
    cursor: StoredStreamId,
) -> diesel::QueryResult<Vec<WebDetails>> {
    let limit = i64::try_from(READ_BATCH_ROWS).expect("batch limit fits i64");
    let after_stream = cursor.0;
    web_intervals::table
        .inner_join(domains::table.on(web_intervals::domain_id.eq(domains::id)))
        .filter(web_intervals::link_status.eq(LINK_STATUS_LINKED))
        .filter(web_intervals::domain_id.is_not_null())
        .filter(web_intervals::end_utc_ms.is_null())
        .filter(web_intervals::confirmed_utc_ms.gt(range.start))
        .filter(web_intervals::start_utc_ms.lt(range.end))
        .filter(web_intervals::stream_id.gt(after_stream))
        .order(web_intervals::stream_id)
        .limit(limit)
        .select((
            web_intervals::id,
            web_intervals::stream_id,
            web_intervals::interval_id,
            web_intervals::start_utc_ms,
            web_intervals::end_utc_ms,
            web_intervals::confirmed_utc_ms,
            domains::host,
        ))
        .load::<WebDetails>(connection)
}
