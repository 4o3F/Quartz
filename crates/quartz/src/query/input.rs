//! Fact inputs use domain values; wire types are constructed during projection.

use quartz_domain::facts::SpanDuration;
pub(crate) use quartz_domain::facts::{
    Availability, CaptureLevel, ClockSegment, FactId, Foreground, ForegroundKind, InputRow,
    QueryRange,
};

impl super::pipeline::DerivedSegment {
    pub(crate) fn duration_within(&self, range: QueryRange) -> i64 {
        SpanDuration {
            start_utc_ms: self.display.start_utc_ms,
            end_utc_ms: self.display.end_utc_ms,
            duration_ms: self.display.duration_ms,
        }
        .within(range)
    }
}

#[cfg(test)]
pub(crate) use quartz_domain::facts::FactSpan;
