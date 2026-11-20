//! Optional shell timings; the pure pipeline does not read the clock.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub(crate) enum QueryStage {
    Read,
    Slice,
    Classify,
    Aggregate,
    PageEncode,
}
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct QueryTimings {
    pub read: Duration,
    pub slice: Duration,
    pub classify: Duration,
    pub aggregate: Duration,
    pub page_encode: Duration,
}
#[derive(Debug)]
pub(super) struct QueryTimer<'timings> {
    timings: Option<&'timings mut QueryTimings>,
}
impl<'timings> QueryTimer<'timings> {
    pub(super) fn disabled() -> Self {
        Self { timings: None }
    }
    pub(super) fn record(timings: &'timings mut QueryTimings) -> Self {
        Self {
            timings: Some(timings),
        }
    }
    pub(super) fn measure<T>(&mut self, stage: QueryStage, operation: impl FnOnce() -> T) -> T {
        let Some(timings) = &mut self.timings else {
            return operation();
        };
        let started = Instant::now();
        let result = operation();
        let elapsed = started.elapsed();
        match stage {
            QueryStage::Read => timings.read += elapsed,
            QueryStage::Slice => timings.slice += elapsed,
            QueryStage::Classify => timings.classify += elapsed,
            QueryStage::Aggregate => timings.aggregate += elapsed,
            QueryStage::PageEncode => timings.page_encode += elapsed,
        }
        result
    }
}
