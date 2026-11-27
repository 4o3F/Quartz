//! Throttled, lossy progress reporting for multi-day query jobs.

use crate::ipc::outbox::OutboxHandle;
use quartz_protocol::{Progress, RequestId};
#[cfg(test)]
use std::sync::Arc;
use std::{
    fmt,
    time::{Duration, Instant},
};

/// Minimum job runtime before the first progress frame is emitted.
pub(crate) const PROGRESS_FIRST_DELAY: Duration = Duration::from_millis(250);
/// Minimum spacing between consecutive progress frames for one query job.
pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

enum ProgressTarget {
    Outbox(OutboxHandle),
    #[cfg(test)]
    Callback(Arc<dyn Fn(Progress) + Send + Sync>),
}

impl ProgressTarget {
    fn send(&self, progress: Progress) {
        match self {
            Self::Outbox(outbox) => outbox.try_progress(progress),
            #[cfg(test)]
            Self::Callback(callback) => callback(progress),
        }
    }
}

enum ProgressClock {
    System {
        started_at: Instant,
    },
    #[cfg(test)]
    Injected(Arc<dyn Fn() -> Duration + Send + Sync>),
}

impl ProgressClock {
    fn elapsed(&self) -> Duration {
        match self {
            Self::System { started_at } => started_at.elapsed(),
            #[cfg(test)]
            Self::Injected(now) => now(),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::System { started_at } => *started_at = Instant::now(),
            #[cfg(test)]
            Self::Injected(_) => {}
        }
    }
}

struct ActiveProgress {
    id: RequestId,
    target: ProgressTarget,
    clock: ProgressClock,
    first_delay: Duration,
    interval: Duration,
    last_emitted: Option<Duration>,
    last_done: Option<u32>,
}

/// Optional per-request sink threaded from IPC admission into a query job.
/// Jobs without a sink (CLI connections, background summaries, tests) do nothing.
#[derive(Default)]
pub(crate) struct ProgressSink {
    active: Option<ActiveProgress>,
}

impl ProgressSink {
    /// Creates a no-op sink that never emits progress frames.
    pub(crate) fn disabled() -> Self {
        Self { active: None }
    }

    /// Creates a throttled sink targeting one UI connection's outbox.
    pub(crate) fn for_outbox(id: RequestId, outbox: OutboxHandle) -> Self {
        Self {
            active: Some(ActiveProgress {
                id,
                target: ProgressTarget::Outbox(outbox),
                clock: ProgressClock::System {
                    started_at: Instant::now(),
                },
                first_delay: PROGRESS_FIRST_DELAY,
                interval: PROGRESS_INTERVAL,
                last_emitted: None,
                last_done: None,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_outbox_with_clock(
        id: RequestId,
        outbox: OutboxHandle,
        clock: Arc<dyn Fn() -> Duration + Send + Sync>,
        first_delay: Duration,
        interval: Duration,
    ) -> Self {
        Self {
            active: Some(ActiveProgress {
                id,
                target: ProgressTarget::Outbox(outbox),
                clock: ProgressClock::Injected(clock),
                first_delay,
                interval,
                last_emitted: None,
                last_done: None,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_callback(
        id: RequestId,
        callback: Arc<dyn Fn(Progress) + Send + Sync>,
        clock: Arc<dyn Fn() -> Duration + Send + Sync>,
        first_delay: Duration,
        interval: Duration,
    ) -> Self {
        Self {
            active: Some(ActiveProgress {
                id,
                target: ProgressTarget::Callback(callback),
                clock: ProgressClock::Injected(clock),
                first_delay,
                interval,
                last_emitted: None,
                last_done: None,
            }),
        }
    }

    /// Starts the runtime clock when the database thread begins executing the job.
    pub(crate) fn mark_started(&mut self) {
        if let Some(active) = &mut self.active {
            active.clock.reset();
        }
    }

    /// Reports `(done, total)` logical days, emitting at most once per interval after first delay.
    pub(crate) fn report(&mut self, done: u32, total: u32) {
        let Some(active) = &mut self.active else {
            return;
        };
        if total == 0 {
            return;
        }
        let elapsed = active.clock.elapsed();
        match active.last_emitted {
            None => {
                if elapsed < active.first_delay {
                    return;
                }
            }
            Some(last) => {
                if elapsed.saturating_sub(last) < active.interval {
                    return;
                }
            }
        }
        let done = done.min(total);
        let done = match active.last_done {
            Some(previous) => done.max(previous),
            None => done,
        };
        active.last_emitted = Some(elapsed);
        active.last_done = Some(done);
        active.target.send(Progress {
            id: active.id,
            done,
            total,
        });
    }
}

impl fmt::Debug for ProgressSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.active {
            Some(active) => formatter
                .debug_struct("ProgressSink")
                .field("id", &active.id)
                .field("last_done", &active.last_done)
                .finish_non_exhaustive(),
            None => formatter.write_str("ProgressSink::Disabled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    };

    #[test]
    fn fast_single_day_query_emits_no_progress() {
        let mut fixture = crate::store::query::test_support::QueryFixture::new(4);
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let sink = ProgressSink::with_callback(
            RequestId(7),
            {
                let emitted = Arc::clone(&emitted);
                Arc::new(move |progress| {
                    emitted.lock().expect("emitted lock").push(progress);
                })
            },
            Arc::new(|| Duration::from_millis(10)),
            PROGRESS_FIRST_DELAY,
            PROGRESS_INTERVAL,
        );
        let mut reader = fixture.store.take_query_reader();
        let rules = fixture.rules();
        let mut job = crate::query::QueryJob::start(
            &mut reader,
            crate::query::QueryRequest::Range(quartz_protocol::QueryRangeRequest {
                start_date: crate::store::query::test_support::FIXTURE_DATE
                    .parse()
                    .expect("start date"),
                end_date: "2025-01-03".parse().expect("end date"),
                bucket: quartz_protocol::RangeBucket::Day,
                dimensions: vec![quartz_protocol::RangeDimension::Category],
            }),
            rules,
        )
        .expect("start single-day range")
        .with_progress(sink);
        let mut response = None;
        while response.is_none() {
            response = job.advance().expect("advance single-day query");
        }
        fixture
            .store
            .return_query_reader(job.finish().expect("finish query"));
        assert!(response.is_some());
        assert!(emitted.lock().expect("emitted lock").is_empty());
    }

    #[test]
    fn slow_multi_day_query_emits_monotonic_progress_throttled_by_clock() {
        let mut fixture = crate::store::query::test_support::QueryFixture::new(4);
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let elapsed_ms = Arc::new(AtomicU64::new(0));
        let sink = ProgressSink::with_callback(
            RequestId(11),
            {
                let emitted = Arc::clone(&emitted);
                Arc::new(move |progress| {
                    emitted.lock().expect("emitted lock").push(progress);
                })
            },
            {
                let elapsed_ms = Arc::clone(&elapsed_ms);
                Arc::new(move || {
                    let current = elapsed_ms.fetch_add(150, Ordering::Relaxed) + 150;
                    Duration::from_millis(current)
                })
            },
            PROGRESS_FIRST_DELAY,
            PROGRESS_INTERVAL,
        );
        let mut reader = fixture.store.take_query_reader();
        let rules = fixture.rules();
        let mut job = crate::query::QueryJob::start(
            &mut reader,
            crate::query::QueryRequest::Range(quartz_protocol::QueryRangeRequest {
                start_date: "2025-01-02".parse().expect("start date"),
                end_date: "2025-01-08".parse().expect("end date"),
                bucket: quartz_protocol::RangeBucket::Day,
                dimensions: vec![quartz_protocol::RangeDimension::Category],
            }),
            rules,
        )
        .expect("start multi-day range")
        .with_progress(sink);
        let mut response = None;
        while response.is_none() {
            response = job.advance().expect("advance multi-day query");
        }
        fixture
            .store
            .return_query_reader(job.finish().expect("finish query"));
        assert!(response.is_some());
        let updates = emitted.lock().expect("emitted lock");
        // Day boundaries occur at 150, 300 (emit 2/6), 450, 600 (emit 4/6), 750 ms.
        assert_eq!(
            updates.as_slice(),
            [
                Progress {
                    id: RequestId(11),
                    done: 2,
                    total: 6,
                },
                Progress {
                    id: RequestId(11),
                    done: 4,
                    total: 6,
                },
            ]
        );
    }
}
