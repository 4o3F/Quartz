//! Snapshot fact inputs and duration clipping for pipeline-internal segments.

pub(crate) use quartz_domain::facts::{
    Availability, CaptureLevel, ClockSegment, FactId, Foreground, ForegroundKind, QueryRange,
};

impl super::pipeline::DerivedSegment {
    pub(crate) fn duration_within(&self, range: QueryRange) -> i64 {
        self.span.within(range)
    }
}

#[cfg(test)]
pub(crate) use quartz_domain::facts::FactSpan;

pub(crate) use crate::store::query::{InputRow, QueryAnnotation};
