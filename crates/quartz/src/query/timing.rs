//! Optional shell timings; the pure pipeline does not read the clock.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
impl QueryTimings {
    pub(crate) fn accumulate(&mut self, other: Self) {
        self.read += other.read;
        self.slice += other.slice;
        self.classify += other.classify;
        self.aggregate += other.aggregate;
        self.page_encode += other.page_encode;
    }
}
#[derive(Debug)]
pub(super) struct QueryTimer<'timings> {
    timings: Option<&'timings mut QueryTimings>,
}
#[cfg(test)]
std::thread_local! { pub(super) static MEASURED_STAGES: std::cell::RefCell<Vec<QueryStage>> = const { std::cell::RefCell::new(Vec::new()) }; }
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
        #[cfg(test)]
        MEASURED_STAGES.with_borrow_mut(|stages| stages.push(stage));
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
