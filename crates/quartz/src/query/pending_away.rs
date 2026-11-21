//! Uncovered idle ranges, independent of the foreground activity projection.

use super::{input::QueryRange, pipeline::DerivedSegment};
use quartz_protocol::PendingAway;

/// 04 §6.2 and 07 Today item 6 require idle pieces longer than fifteen minutes.
const MIN_PENDING_AWAY_MS: i64 = 15 * 60 * 1000;

#[derive(Debug, Default)]
pub(super) struct PendingAwayAccumulator {
    ranges: Vec<QueryRange>,
}
impl PendingAwayAccumulator {
    pub(super) fn push(&mut self, segment: &DerivedSegment) {
        if let Some(range) = segment.pending_idle {
            self.ranges.push(range);
        }
    }
    pub(super) fn finish(mut self) -> Vec<PendingAway> {
        self.ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<QueryRange> = Vec::new();
        for range in self.ranges {
            if let Some(last) = merged.last_mut()
                && last.end >= range.start
            {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        merged
            .into_iter()
            .filter(|range| range.end - range.start > MIN_PENDING_AWAY_MS)
            .map(|range| PendingAway {
                start_utc_ms: range.start,
                end_utc_ms: range.end,
                duration_ms: range.end - range.start,
            })
            .collect()
    }
}
