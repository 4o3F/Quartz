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
}
impl SummaryTask {
    pub(crate) fn channel(wake: Arc<dyn Fn() + Send + Sync>) -> SummaryChannel {
        let (requests, received) = watch::channel(());
        let (published, latest) = watch::channel(None);
        SummaryChannel {
            port: SummaryPort { requests, latest },
            task: Self {
                requests: received,
                published,
                wake,
            },
        }
    }
    pub(crate) fn start(self, queries: QueryPort, versions: VersionPort) {
        tokio::spawn(self.run(queries, versions));
    }
    async fn run(self, queries: QueryPort, versions: VersionPort) {
        self.run_with(queries, versions, jiff::Timestamp::now).await;
    }
    async fn run_with(
        mut self,
        queries: QueryPort,
        mut versions: VersionPort,
        now_clock: impl Fn() -> jiff::Timestamp,
    ) {
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
                    let published = versions.updates.borrow_and_update().clone();
                    let current = registered.and_then(|date| published.days.get(&date));
                    if current.map(|day| &day.versions) == digest.as_ref() { continue; }
                }
            }
            let now = now_clock();
            let (offset, zone) = {
                let published = versions.updates.borrow();
                (published.day_start_offset, published.time_zone.clone())
            };
            let date = match DayCalendar::current_date_in_zone(now, offset, zone) {
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
            .call(QueryRequest::Day(QueryDayRequest { local_date: date }))
            .await?
        else {
            return Err(ProtocolError::new(ErrorKind::Internal));
        };
        let versions = day.versions.clone();
        let mut summary = TraySummary::from_day(day);
        summary.recent = Self::recent(queries, now_ms).await?;
        Ok(ComputedSummary { summary, versions })
    }
    async fn recent(
        queries: &QueryPort,
        now_ms: i64,
    ) -> Result<Option<ApplicationRank>, ProtocolError> {
        let request = QueryTimelineRequest {
            start_utc_ms: now_ms.saturating_sub(RECENT_WINDOW_MS),
            end_utc_ms: now_ms,
            detail: TimelineDetail::App,
            ms_per_pixel: MillisecondsPerPixel(NonZeroU64::MIN),
        };
        let QueryResponse::Timeline(response) =
            queries.call(QueryRequest::Timeline(request)).await?
        else {
            return Err(ProtocolError::new(ErrorKind::Internal));
        };
        let mut applications: BTreeMap<quartz_protocol::ApplicationId, ApplicationRank> =
            BTreeMap::new();
        for segment in response.segments {
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
        let summary = SummaryTask::channel(Arc::new(|| {}));
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
        assert_eq!(result.summary.metrics, direct.metrics);
        assert_eq!(result.versions, direct.versions);
        let hours = direct.hours;
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
        let channel = SummaryTask::channel(Arc::new(|| {}));
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

    fn run_in_zone(
        fixture: &mut QueryFixture,
        rules: Arc<quartz_domain::rules::registry::RuleSnapshot>,
        request: QueryRequest,
        zone: jiff::tz::TimeZone,
    ) -> Result<QueryResponse, ProtocolError> {
        let mut reader = fixture.store.take_query_reader();
        match crate::query::QueryJob::start_in_zone(&mut reader, request, rules, zone) {
            Ok(mut job) => {
                let result = loop {
                    match job.advance() {
                        Ok(Some(response)) => break Ok(response),
                        Ok(None) => {}
                        Err(error) => break Err(ProtocolError::new(error.kind())),
                    }
                };
                fixture
                    .store
                    .return_query_reader(job.finish().expect("finish query"));
                result
            }
            Err(error) => {
                fixture
                    .store
                    .return_query_reader(reader.expect("reader returned after error"));
                Err(ProtocolError::new(error.kind()))
            }
        }
    }

    struct SummaryHarness {
        fixture: QueryFixture,
        publisher: crate::query::versions::VersionPublisher,
        queries: QueryChannel,
        snapshots: quartz_domain::rules::registry::SnapshotCell,
        queried: Option<quartz_protocol::LocalDate>,
    }
    impl SummaryHarness {
        async fn wait_for(&mut self, zone: &jiff::tz::TimeZone, ready: impl Fn() -> bool) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !ready() {
                    while let Ok(command) = self.publisher.requests.try_recv() {
                        self.publisher
                            .receive_in_zone(&mut self.fixture.store, command, zone.clone())
                            .expect("service version command");
                    }
                    while let Ok(command) = self.queries.receiver.try_recv() {
                        let crate::query::dispatch::QueryCommand::Query { request, reply } =
                            command
                        else {
                            panic!("summary submits fact queries")
                        };
                        if let QueryRequest::Day(day) = &request {
                            self.queried = Some(day.local_date);
                        }
                        let response = run_in_zone(
                            &mut self.fixture,
                            self.snapshots.load(),
                            request,
                            zone.clone(),
                        );
                        let _ = reply.send(response);
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("summary task settled");
        }
    }

    #[tokio::test]
    async fn tray_summary_refreshes_on_global_rule_change() {
        use crate::{
            query::versions::{VersionChannel, VersionPublisher},
            rules::registry::RegistryShell,
        };
        use quartz_domain::{capture::UtcInstant, rules::registry::RegistryCommand};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mut fixture = QueryFixture::new(1);
        let now_utc = UtcInstant::try_from(0).expect("UTC");
        let mut shell = RegistryShell::open(&mut fixture.store, now_utc).expect("registry");
        let snapshots = shell.snapshots();
        let VersionChannel {
            publisher,
            port: versions,
        } = VersionPublisher::open(&mut fixture.store, snapshots.clone()).expect("versions");
        let zone = versions.updates.borrow().time_zone.clone();
        let now = jiff::Timestamp::from_millisecond(fixture.start + 60_000).expect("now");
        let mut harness = SummaryHarness {
            fixture,
            publisher,
            queries: QueryChannel::new(),
            snapshots,
            queried: None,
        };
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wakes);
        let summary = SummaryTask::channel(Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::Relaxed);
        }));
        let task = tokio::spawn(summary.task.run_with(
            harness.queries.port.clone(),
            versions,
            move || now,
        ));
        harness
            .wait_for(&zone, || wakes.load(Ordering::Relaxed) >= 1)
            .await;
        let initial = summary.port.latest().expect("initial summary");
        assert_eq!(initial.metrics.active_ms, 1000);
        assert_eq!(initial.metrics.unclassified_ms, 1000);
        let before = wakes.load(Ordering::Relaxed);
        shell
            .run(
                &mut harness.fixture.store,
                now_utc,
                RegistryCommand::Apply {
                    key: "user.tray-rule".parse().expect("key"),
                    file: "tray.toml".into(),
                    text: "format='quartz-rules/1'\n[set]\nkey='user.tray-rule'\nkind='classification'\nname='Tray'\nparent='builtin.classification'\n[[rule]]\nid='dev'\nlabel='Dev'\ncategory='builtin:development'\nwhen=[{field='app.exe_name',op='equals',value='app-1.exe'}]\n".into(),
                    base_version: 0,
                },
            )
            .expect("apply classification rule");
        harness
            .publisher
            .publish(&mut harness.fixture.store)
            .expect("publish rule change");
        harness
            .wait_for(&zone, || wakes.load(Ordering::Relaxed) > before)
            .await;
        let updated = summary.port.latest().expect("updated summary");
        assert_eq!(updated.metrics.unclassified_ms, 0);
        task.abort();
    }

    #[tokio::test]
    async fn tray_summary_follows_day_offset_and_timezone_updates() {
        use crate::query::versions::{VersionChannel, VersionPublisher};
        use quartz_domain::rules::registry::RuleRegistry;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mut fixture = QueryFixture::new(0);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel {
            mut publisher,
            port: versions,
        } = VersionPublisher::open(&mut fixture.store, snapshots.clone()).expect("versions");
        let utc = jiff::tz::TimeZone::UTC;
        versions.refresh();
        let prime = publisher.requests.try_recv().expect("prime refresh");
        publisher
            .receive_in_zone(&mut fixture.store, prime, utc.clone())
            .expect("prime UTC");
        let mut harness = SummaryHarness {
            fixture,
            publisher,
            queries: QueryChannel::new(),
            snapshots,
            queried: None,
        };
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wakes);
        let summary = SummaryTask::channel(Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::Relaxed);
        }));
        let now: jiff::Timestamp = "2025-01-02T02:00:00Z".parse().expect("now");
        let task = tokio::spawn(summary.task.run_with(
            harness.queries.port.clone(),
            versions.clone(),
            move || now,
        ));
        harness
            .wait_for(&utc, || wakes.load(Ordering::Relaxed) >= 1)
            .await;
        assert_eq!(harness.queried, Some("2025-01-02".parse().expect("jan2")));
        let before_offset = wakes.load(Ordering::Relaxed);
        harness
            .fixture
            .store
            .test_day_offset_write(240)
            .expect("commit offset");
        versions.refresh();
        harness
            .wait_for(&utc, || wakes.load(Ordering::Relaxed) > before_offset)
            .await;
        assert_eq!(harness.queried, Some("2025-01-01".parse().expect("jan1")));
        let tokyo = jiff::tz::TimeZone::get("Asia/Tokyo").expect("tokyo");
        let before_zone = wakes.load(Ordering::Relaxed);
        versions.refresh();
        harness
            .wait_for(&tokyo, || wakes.load(Ordering::Relaxed) > before_zone)
            .await;
        assert_eq!(harness.queried, Some("2025-01-02".parse().expect("jan2")));
        task.abort();
    }
}
