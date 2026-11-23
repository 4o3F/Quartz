//! Tray refresh task on the transport's existing I/O runtime.

use crate::{
    query::{
        QueryRequest, QueryResponse, calendar::DayCalendar, dispatch::QueryPort,
        versions::VersionPort,
    },
    tray_summary::TraySummary,
};
use quartz_protocol::{
    Activity, ApplicationRank, ErrorKind, MillisecondsPerPixel, ProtocolError, QueryDayRequest,
    QueryTimelineRequest, TimelineDetail,
};
use std::{collections::BTreeMap, num::NonZeroU64, sync::Arc, time::Duration};
use tokio::sync::watch;

/// Tooltip refresh cadence required by 07 §1.2.
const SUMMARY_REFRESH_PERIOD: Duration = Duration::from_secs(60);
/// The recent-application menu line covers the preceding sixty minutes (07 §1.3).
const RECENT_WINDOW_MS: i64 = 60 * crate::tray_summary::MINUTE_MS;

#[derive(Clone)]
pub(crate) struct SummaryPort {
    requests: watch::Sender<()>,
    latest: watch::Receiver<Option<Arc<TraySummary>>>,
}
impl std::fmt::Debug for SummaryPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SummaryPort").finish_non_exhaustive()
    }
}
impl SummaryPort {
    /// Coalesces requests and wakes the I/O task without awaiting a query.
    pub(crate) fn request(&self) {
        self.requests.send_replace(());
    }
    pub(crate) fn latest(&self) -> Option<Arc<TraySummary>> {
        self.latest.borrow().clone()
    }
}
#[derive(Clone)]
pub(crate) struct SummaryTask {
    requests: watch::Receiver<()>,
    published: watch::Sender<Option<Arc<TraySummary>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    offset: crate::query::calendar::DayStartOffset,
}
impl SummaryTask {
    pub(crate) fn channel(
        offset: crate::query::calendar::DayStartOffset,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> SummaryChannel {
        let (requests, received) = watch::channel(());
        let (published, latest) = watch::channel(None);
        SummaryChannel {
            port: SummaryPort { requests, latest },
            task: Self {
                requests: received,
                published,
                wake,
                offset,
            },
        }
    }
    pub(crate) fn start(self, queries: QueryPort, versions: VersionPort) {
        tokio::spawn(self.run(queries, versions));
    }
    async fn run(mut self, queries: QueryPort, mut versions: VersionPort) {
        let mut timer = tokio::time::interval(SUMMARY_REFRESH_PERIOD);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut registered = None;
        let mut digest = None;
        loop {
            tokio::select! {
                _ = timer.tick() => {},
                changed = self.requests.changed() => { if changed.is_err() { return; } },
                changed = versions.updates.changed() => {
                    if changed.is_err() { return; }
                    let current = registered.and_then(|date| versions.updates.borrow_and_update().days.get(&date).cloned());
                    if current.as_ref().map(|day| &day.versions) == digest.as_ref() { continue; }
                }
            }
            let now = jiff::Timestamp::now();
            let date = match DayCalendar::current_date(now, self.offset) {
                Ok(date) => date,
                Err(error) => {
                    tracing::warn!(%error, "could not resolve tray logical day");
                    continue;
                }
            };
            if registered != Some(date) {
                match versions.set_summary_day(date).await {
                    Ok(_) => registered = Some(date),
                    Err(error) => {
                        tracing::warn!(kind = ?error.kind, "could not subscribe tray logical day");
                        continue;
                    }
                }
            }
            match self.compute(&queries, date, now.as_millisecond()).await {
                Ok(result) => {
                    let current = versions.updates.borrow().days.get(&date).cloned();
                    if current
                        .as_ref()
                        .is_some_and(|day| day.versions != result.versions)
                    {
                        continue;
                    }
                    digest = Some(result.versions);
                    self.published.send_replace(Some(Arc::new(result.summary)));
                }
                Err(error) => tracing::warn!(kind = ?error.kind, "could not refresh tray summary"),
            }
            // Refresh elapsed foreground time even when the query failed; retain the previous summary.
            (self.wake)();
        }
    }
    async fn compute(
        &self,
        queries: &QueryPort,
        date: quartz_protocol::LocalDate,
        now_ms: i64,
    ) -> Result<ComputedSummary, ProtocolError> {
        let QueryResponse::Day(day) = queries
            .call(QueryRequest::Day(QueryDayRequest {
                local_date: date,
                cursor: None,
            }))
            .await?
        else {
            return Err(ProtocolError::new(ErrorKind::Internal));
        };
        let versions = day.versions.clone();
        let mut summary =
            TraySummary::from_day(day).ok_or_else(|| ProtocolError::new(ErrorKind::Internal))?;
        summary.recent = Self::recent(queries, now_ms).await?;
        Ok(ComputedSummary { summary, versions })
    }
    async fn recent(
        queries: &QueryPort,
        now_ms: i64,
    ) -> Result<Option<ApplicationRank>, ProtocolError> {
        let mut request = QueryTimelineRequest {
            start_utc_ms: now_ms.saturating_sub(RECENT_WINDOW_MS),
            end_utc_ms: now_ms,
            detail: TimelineDetail::App,
            ms_per_pixel: MillisecondsPerPixel(NonZeroU64::MIN),
            cursor: None,
        };
        let mut applications: BTreeMap<quartz_protocol::ApplicationId, ApplicationRank> =
            BTreeMap::new();
        let mut versions = None;
        loop {
            let QueryResponse::Timeline(page) = queries
                .call(QueryRequest::Timeline(request.clone()))
                .await?
            else {
                return Err(ProtocolError::new(ErrorKind::Internal));
            };
            if versions
                .as_ref()
                .is_some_and(|version| *version != page.versions)
            {
                return Err(ProtocolError::new(ErrorKind::StaleCursor));
            }
            versions = Some(page.versions);
            for segment in page.segments {
                if matches!(
                    segment.activity,
                    Activity::Active | Activity::ActiveOverride { .. }
                ) && let Some(app) = segment.app
                {
                    let row = applications
                        .entry(app.id)
                        .or_insert(ApplicationRank { app, active_ms: 0 });
                    row.active_ms += segment.duration_ms;
                }
            }
            request.cursor = page.next_cursor;
            if request.cursor.is_none() {
                break;
            }
        }
        // BTreeMap order gives equal durations a stable application-ID tie break.
        Ok(applications.into_values().reduce(|top, row| {
            if row.active_ms > top.active_ms {
                row
            } else {
                top
            }
        }))
    }
}
struct ComputedSummary {
    summary: TraySummary,
    versions: quartz_protocol::QueryVersions,
}
pub(crate) struct SummaryChannel {
    pub port: SummaryPort,
    pub task: SummaryTask,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        query::dispatch::{QueryChannel, test_support::TestDatabase},
        store::query::test_support::QueryFixture,
    };

    #[tokio::test]
    async fn summary_matches_direct_day_on_unchanged_database_snapshot() {
        let mut fixture = QueryFixture::new(4);
        fixture.annotate(
            1,
            quartz_protocol::AnnotationKind::CategoryOverride,
            0,
            1000,
            Some(quartz_protocol::BuiltinCategory::Development),
        );
        fixture.annotate(
            2,
            quartz_protocol::AnnotationKind::CategoryOverride,
            1000,
            2000,
            Some(quartz_protocol::BuiltinCategory::Entertainment),
        );
        let request = fixture.request_day();
        let QueryRequest::Day(parameters) = &request else {
            panic!("day request");
        };
        let date = parameters.local_date;
        let QueryResponse::Day(direct) = fixture.run(request).expect("direct query.day") else {
            panic!("day response");
        };
        let now_ms = fixture.start + RECENT_WINDOW_MS;
        let channel = QueryChannel::new();
        let summary = SummaryTask::channel(
            crate::query::calendar::DayStartOffset::MIDNIGHT,
            Arc::new(|| {}),
        );
        let compute = summary.task.compute(&channel.port, date, now_ms);
        tokio::pin!(compute);
        let result = loop {
            tokio::select! {
                result = &mut compute => break result.expect("summary"),
                _ = tokio::task::yield_now() => {
                    while let Ok(command) = channel.receiver.try_recv() {
                        let crate::query::dispatch::QueryCommand::Query { request, reply } = command else { panic!("summary submits a fact query") };
                        let response = fixture.run(request).map_err(|error| ProtocolError::new(error.kind()));
                        reply.send(response).expect("query receiver");
                    }
                }
            }
        };
        assert_eq!(
            result.summary.metrics,
            direct.metrics.expect("direct metrics")
        );
        assert_eq!(result.versions, direct.versions);
        let hours = direct.hours.expect("direct hours");
        for category in &result.summary.categories {
            let expected: i64 = hours
                .iter()
                .flat_map(|hour| &hour.categories)
                .filter(|row| row.category == category.category)
                .map(|row| row.duration_ms)
                .sum();
            assert_eq!(category.duration_ms, expected);
        }
        let recent = result.summary.recent.expect("recent application");
        assert_eq!(recent, direct.applications[0]);
        assert_eq!(
            result
                .summary
                .categories
                .iter()
                .map(|row| row.duration_ms)
                .sum::<i64>(),
            result.summary.metrics.active_ms
        );
    }

    #[tokio::test]
    async fn main_requests_and_write_admission_return_while_summary_query_is_slow() {
        let database = TestDatabase::start();
        let queries = QueryChannel::new();
        let channel = SummaryTask::channel(
            crate::query::calendar::DayStartOffset::MIDNIGHT,
            Arc::new(|| {}),
        );
        let task = tokio::spawn(
            channel
                .task
                .run(queries.port.clone(), database.versions.clone()),
        );
        let slow_query = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(query) = queries.receiver.try_recv() {
                    break query;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("summary query started");
        // Keep the database reply withheld while exercising the synchronous main-side APIs.
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            channel.port.request();
        }
        assert!(channel.port.latest().is_none());
        let writes = crate::persistence::dispatch::WriteChannel::new();
        let mut reply = writes
            .port
            .submit(crate::persistence::dispatch::WriteRequest::SettingsGet)
            .expect("nonblocking write admission");
        assert_eq!(
            reply.try_recv().expect_err("database has not replied"),
            tokio::sync::oneshot::error::TryRecvError::Empty
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "main-side calls must not wait for the query or writer"
        );
        assert!(
            matches!(slow_query, crate::query::dispatch::QueryCommand::Query { reply, .. } if !reply.is_closed())
        );
        task.abort();
        assert!(
            task.await
                .expect_err("cancel slow summary task")
                .is_cancelled()
        );
    }
}
