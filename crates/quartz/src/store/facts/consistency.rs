//! Closed-run coverage and duration checks for offline validation.

use super::super::{ReadOnlyStore, enums};
use super::{Fact, FactEnd, FactKind, FactReason, ReadFactsError};
use quartz_domain::capture::limits::UTC_DAY_MS;
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CoverageCounts {
    facts: i32,
    stops: i32,
    gaps: i32,
    idle: i32,
}
impl CoverageCounts {
    fn adjust(&mut self, fact: &Fact, delta: i32) -> Result<(), ReadFactsError> {
        let invalid = || ReadFactsError::InvalidCoverage { kind: fact.kind };
        let count = match fact.kind {
            FactKind::Availability => match fact.reason {
                Some(FactReason::Availability(enums::AvailabilityReason::Idle)) => &mut self.idle,
                Some(FactReason::Availability(
                    enums::AvailabilityReason::Locked
                    | enums::AvailabilityReason::Disconnected
                    | enums::AvailabilityReason::Suspended
                    | enums::AvailabilityReason::Paused,
                )) => &mut self.stops,
                None | Some(FactReason::Gap(_)) => return Err(invalid()),
            },
            FactKind::CaptureGap => match fact.reason {
                Some(FactReason::Gap(_)) => &mut self.gaps,
                None | Some(FactReason::Availability(_)) => return Err(invalid()),
            },
            FactKind::Foreground | FactKind::NoForeground => &mut self.facts,
            FactKind::Run | FactKind::ClockSegment | FactKind::TimedPolicy => return Err(invalid()),
        };
        *count += delta;
        Ok(())
    }
    fn apply(&mut self, delta: Self) {
        self.facts += delta.facts;
        self.stops += delta.stops;
        self.gaps += delta.gaps;
        self.idle += delta.idle;
    }
    fn conserved(self) -> bool {
        self.facts + i32::from(self.stops > 0) + self.gaps == 1
            && self.idle <= 1
            && [self.facts, self.stops, self.gaps, self.idle]
                .iter()
                .all(|count| *count >= 0)
    }
}

impl ReadOnlyStore {
    /// Checks complete sampled runs using monotonic coverage and independent idle masks.
    pub(crate) fn check_consistency(&mut self, from: i64, to: i64) -> Result<(), ReadFactsError> {
        let facts = self.dump_range(from, to)?;
        let fail = |detail| ReadFactsError::Inconsistent { detail };
        for run in facts.iter().filter(|row| row.kind == FactKind::Run) {
            if run.start_utc_ms < from
                || run.endpoint() > to
                || matches!(run.end, FactEnd::Open { .. })
            {
                return Err(fail(format!(
                    "run {} is not complete within the check range",
                    run.id
                )));
            }
            let mut segments: Vec<_> = facts
                .iter()
                .filter(|row| row.kind == FactKind::ClockSegment && row.run_id == Some(run.id))
                .collect();
            segments.sort_by_key(|row| (row.start_mono_ms, row.id));
            let Some(first) = segments.first() else {
                return Err(fail(format!("run {} has no segments", run.id)));
            };
            let last = segments.last().expect("nonempty segments");
            if first.start_utc_ms != run.start_utc_ms
                || last.endpoint() != run.endpoint()
                || segments
                    .windows(2)
                    .any(|pair| pair[0].end_mono_ms != pair[1].start_mono_ms)
            {
                return Err(fail(format!(
                    "run {} has discontinuous clock segments",
                    run.id
                )));
            }
        }
        for segment in facts
            .iter()
            .filter(|row| row.kind == FactKind::ClockSegment)
        {
            let left = segment
                .start_mono_ms
                .ok_or_else(|| fail("segment has no monotonic start".into()))?;
            let right = segment
                .end_mono_ms
                .ok_or_else(|| fail("segment has no monotonic end".into()))?;
            if right < left {
                return Err(fail(format!(
                    "segment {} has reversed endpoints",
                    segment.id
                )));
            }
            // Sweep boundaries rather than every pair of spans; stop masks are unioned.
            let mut boundaries = BTreeMap::<i64, CoverageCounts>::new();
            boundaries.entry(left).or_default();
            boundaries.entry(right).or_default();
            for fact in facts.iter().filter(|row| {
                row.segment_id == Some(segment.id) && row.kind != FactKind::ClockSegment
            }) {
                let start = fact
                    .start_mono_ms
                    .ok_or_else(|| fail("fact has no monotonic start".into()))?;
                let end = fact
                    .end_mono_ms
                    .ok_or_else(|| fail("fact has no monotonic end".into()))?;
                if start < left
                    || end > right
                    || end < start
                    || fact.endpoint() < fact.start_utc_ms
                    || fact.duration_ms != Some(end - start)
                {
                    return Err(fail(format!(
                        "invalid endpoints or duration in {} {}",
                        fact.kind, fact.id
                    )));
                }
                if fact.kind != FactKind::CaptureGap
                    && fact.endpoint() - fact.start_utc_ms > UTC_DAY_MS
                {
                    return Err(fail("row exceeds 24 hours".into()));
                }
                boundaries.entry(start).or_default().adjust(fact, 1)?;
                boundaries.entry(end).or_default().adjust(fact, -1)?;
            }
            let mut active = CoverageCounts::default();
            let mut previous = left;
            for (at, delta) in boundaries {
                if at > previous && !active.conserved() {
                    return Err(fail(format!(
                        "segment {} coverage at [{previous}, {at}): {active:?}",
                        segment.id
                    )));
                }
                active.apply(delta);
                previous = at;
            }
            if active != CoverageCounts::default() {
                return Err(fail(format!(
                    "segment {} has unsettled coverage",
                    segment.id
                )));
            }
        }
        Ok(())
    }
}
