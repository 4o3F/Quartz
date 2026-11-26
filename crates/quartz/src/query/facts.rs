//! Incremental ordering and frame-bounded wire conversion of raw facts.

use super::{QUERY_MAX_RESPONSE_BYTES, RunQueryError, paging::JsonBytes};
use crate::store::facts::{Fact, FactKind as StoreFactKind};
use quartz_protocol::{
    QueryFactsResponse,
    facts::{FactKind, FactRecord, FactRowId, FactsCursor},
};
use std::collections::BTreeMap;

/// Keeps conversion bounded independently of the SQL read batch.
pub(crate) const FINALIZE_BATCH_RECORDS: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FinalizeProgress {
    More,
    Complete,
}

fn wire_kind(kind: StoreFactKind) -> FactKind {
    match kind {
        StoreFactKind::Run => FactKind::Run,
        StoreFactKind::ClockSegment => FactKind::ClockSegment,
        StoreFactKind::Foreground => FactKind::Foreground,
        StoreFactKind::NoForeground => FactKind::NoForeground,
        StoreFactKind::Availability => FactKind::Availability,
        StoreFactKind::CaptureGap => FactKind::CaptureGap,
        StoreFactKind::TimedPolicy => FactKind::TimedPolicy,
    }
}

#[derive(Debug)]
pub(super) struct OrderedFacts {
    after: Option<FactsCursor>,
    rows: BTreeMap<FactsCursor, Fact>,
}
impl OrderedFacts {
    pub(super) fn new(after: Option<FactsCursor>) -> Self {
        Self {
            after,
            rows: BTreeMap::new(),
        }
    }
    pub(super) fn extend(&mut self, facts: Vec<Fact>) {
        for fact in facts {
            let key = FactsCursor {
                start_utc_ms: fact.start_utc_ms,
                kind: wire_kind(fact.kind),
                id: FactRowId(fact.id),
            };
            if self.after.is_some_and(|cursor| key <= cursor) {
                continue;
            }
            self.rows.insert(key, fact);
        }
    }
    pub(super) fn finalize(self, limit: usize) -> Result<FinalizeFacts, RunQueryError> {
        let worst_case_empty = QueryFactsResponse {
            facts: Vec::new(),
            next_after: Some(FactsCursor {
                start_utc_ms: i64::MIN,
                kind: FactKind::ClockSegment,
                id: FactRowId(i64::MIN),
            }),
        };
        let used_bytes = JsonBytes::measure(&worst_case_empty)?;
        Ok(FinalizeFacts {
            remaining: self.rows.into_values(),
            records: Vec::new(),
            limit,
            used_bytes,
            max_bytes: QUERY_MAX_RESPONSE_BYTES,
            more: false,
        })
    }
}

#[derive(Debug)]
pub(super) struct FinalizeFacts {
    remaining: std::collections::btree_map::IntoValues<FactsCursor, Fact>,
    records: Vec<FactRecord>,
    limit: usize,
    used_bytes: usize,
    max_bytes: usize,
    more: bool,
}
impl FinalizeFacts {
    pub(super) fn advance(&mut self) -> Result<FinalizeProgress, RunQueryError> {
        for _ in 0..FINALIZE_BATCH_RECORDS {
            if self.records.len() >= self.limit {
                self.more = self.remaining.len() > 0;
                return Ok(FinalizeProgress::Complete);
            }
            let Some(fact) = self.remaining.next() else {
                return Ok(FinalizeProgress::Complete);
            };
            // Drop the stored representation before retaining its wire replacement.
            let record = fact.record()?;
            drop(fact);
            let record_bytes = JsonBytes::measure(&record)? + 1;
            if self.used_bytes.saturating_add(record_bytes) > self.max_bytes {
                if self.records.is_empty() {
                    return Err(RunQueryError::PageTooLarge);
                }
                self.more = true;
                return Ok(FinalizeProgress::Complete);
            }
            self.used_bytes += record_bytes;
            self.records.push(record);
        }
        if self.records.len() >= self.limit {
            self.more = self.remaining.len() > 0;
            return Ok(FinalizeProgress::Complete);
        }
        Ok(if self.remaining.len() == 0 {
            FinalizeProgress::Complete
        } else {
            FinalizeProgress::More
        })
    }
    pub(super) fn finish(self) -> QueryFactsResponse {
        let next_after = self
            .more
            .then(|| self.records.last().map(FactRecord::cursor))
            .flatten();
        QueryFactsResponse {
            facts: self.records,
            next_after,
        }
    }
}
