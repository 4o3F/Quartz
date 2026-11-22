// In-process pipe tests; no application windows or production endpoints.
use super::*;
use quartz_protocol::{
    ApplicationId, ApplicationRank, ApplicationRef, CursorQuery, DayMetrics, HourStack,
    PendingAway, QueryCursor, QueryDayResponse, QueryPendingAwayResponse, QueryTimelineRequest,
    QueryTimelineResponse, QueryVersions, TimelineDetail,
};
use std::num::NonZeroU32;

struct DayHarness {
    ui: Harness,
    core: Core,
    date: quartz_protocol::LocalDate,
}
#[derive(Clone, Copy)]
enum Paging {
    Single,
    Multiple,
}
impl DayHarness {
    async fn start() -> Self {
        let listener = TestPipeServer::bind().expect("isolated pipe");
        let mut ui = Harness::start(listener.name().clone());
        ui.connecting().await;
        let mut core = Core::accept(listener).await;
        core.handshake().await;
        ui.connected().await;
        Self {
            ui,
            core,
            date: "1970-01-01".parse().expect("date"),
        }
    }
    fn request(&self) -> DayRequest {
        DayRequest {
            date: self.date,
            view: DayView::WholeDay,
            physical_width: NonZeroU32::new(480).expect("plot width"),
        }
    }
    async fn start_load(&mut self, generation: DayGeneration) -> Request {
        self.ui
            .link
            .load_day(self.request(), generation, DayLoad::Immediate)
            .await
            .expect("load intent");
        self.core.subscribe(vec![Topic::Day(self.date)]).await;
        self.core.request(Op::QueryDay).await
    }
    async fn finish(&mut self, first: Request, paging: Paging) {
        let version = QueryVersions("day-v1".into());
        let cursor = matches!(paging, Paging::Multiple).then(|| {
            QueryCursor::new(
                version.clone(),
                CursorQuery::Day {
                    local_date: self.date,
                },
                1,
            )
        });
        let app = |id| ApplicationRank {
            app: ApplicationRef {
                id: ApplicationId(id),
                exe_name: None,
                display_name: format!("app {id}"),
                aumid: None,
            },
            active_ms: 1000,
        };
        self.core
            .ok(
                first,
                serde_json::to_value(QueryDayResponse {
                    metrics: Some(DayMetrics {
                        active_ms: 2000,
                        focus_ratio: 0.,
                        distract_ratio: 0.,
                        unclassified_ms: 2000,
                        longest_focus_ms: 0,
                        switches_per_hour: 0.,
                    }),
                    hours: Some(vec![HourStack {
                        start_utc_ms: 0,
                        end_utc_ms: 86_400_000,
                        local_start: "1970-01-01T00:00:00+00:00[UTC]".into(),
                        categories: Vec::new(),
                    }]),
                    applications: vec![app(1)],
                    versions: version.clone(),
                    next_cursor: cursor.clone(),
                })
                .expect("day JSON"),
            )
            .await;
        if let Some(cursor) = cursor {
            let next = self.core.request(Op::QueryDay).await;
            assert_eq!(
                serde_json::from_value::<quartz_protocol::QueryDayRequest>(next.body.clone())
                    .expect("day cursor")
                    .cursor,
                Some(cursor)
            );
            self.core
                .ok(
                    next,
                    serde_json::to_value(QueryDayResponse {
                        metrics: None,
                        hours: None,
                        applications: vec![app(2)],
                        versions: version.clone(),
                        next_cursor: None,
                    })
                    .expect("next day JSON"),
                )
                .await;
        }
        for detail in [TimelineDetail::Category, TimelineDetail::Raw] {
            let query = self.core.request(Op::QueryTimeline).await;
            let parameters: QueryTimelineRequest =
                serde_json::from_value(query.body.clone()).expect("timeline request");
            assert_eq!(parameters.detail, detail);
            assert!(parameters.ms_per_pixel.0.get() > 0);
            let cursor = matches!(paging, Paging::Multiple)
                .then(|| QueryCursor::new(version.clone(), CursorQuery::from(&parameters), 1));
            self.core
                .ok(
                    query,
                    serde_json::to_value(QueryTimelineResponse {
                        segments: Vec::new(),
                        versions: version.clone(),
                        next_cursor: cursor.clone(),
                    })
                    .expect("timeline JSON"),
                )
                .await;
            if let Some(cursor) = cursor {
                let next = self.core.request(Op::QueryTimeline).await;
                assert_eq!(
                    serde_json::from_value::<QueryTimelineRequest>(next.body.clone())
                        .expect("timeline cursor")
                        .cursor,
                    Some(cursor)
                );
                self.core
                    .ok(
                        next,
                        serde_json::to_value(QueryTimelineResponse {
                            segments: Vec::new(),
                            versions: version.clone(),
                            next_cursor: None,
                        })
                        .expect("next timeline JSON"),
                    )
                    .await;
            }
        }
        let query = self.core.request(Op::QueryPendingAway).await;
        let cursor = matches!(paging, Paging::Multiple).then(|| {
            QueryCursor::new(
                version.clone(),
                CursorQuery::PendingAway {
                    local_date: self.date,
                },
                1,
            )
        });
        self.core
            .ok(
                query,
                serde_json::to_value(QueryPendingAwayResponse {
                    ranges: vec![PendingAway {
                        start_utc_ms: 0,
                        end_utc_ms: 900_000,
                        duration_ms: 900_000,
                    }],
                    versions: version.clone(),
                    next_cursor: cursor.clone(),
                })
                .expect("away JSON"),
            )
            .await;
        if let Some(cursor) = cursor {
            let next = self.core.request(Op::QueryPendingAway).await;
            assert_eq!(
                serde_json::from_value::<quartz_protocol::QueryPendingAwayRequest>(
                    next.body.clone()
                )
                .expect("away cursor")
                .cursor,
                Some(cursor)
            );
            self.core
                .ok(
                    next,
                    serde_json::to_value(QueryPendingAwayResponse {
                        ranges: vec![PendingAway {
                            start_utc_ms: 1_800_000,
                            end_utc_ms: 2_700_000,
                            duration_ms: 900_000,
                        }],
                        versions: version,
                        next_cursor: None,
                    })
                    .expect("next away JSON"),
                )
                .await;
        }
    }
    async fn loaded(&mut self) -> Result<DayData, DayFailure> {
        loop {
            match self.ui.events.recv().await.expect("UI event") {
                LinkEvent::DayLoaded { result, .. } => return result,
                LinkEvent::DayLoading { .. } | LinkEvent::Day(_) => {}
                event => panic!("unexpected event {event:?}"),
            }
        }
    }
}
#[tokio::test]
async fn every_query_pages_and_the_raw_projection_is_used_for_lanes() {
    let mut harness = DayHarness::start().await;
    let first = harness.start_load(DayGeneration::default().next()).await;
    harness.finish(first, Paging::Multiple).await;
    let day = harness.loaded().await.expect("complete day");
    assert_eq!(day.applications.len(), 2);
    assert_eq!(day.pending.len(), 2);
    harness.ui.stop().await;
}
#[tokio::test]
async fn stale_restarts_the_entire_query_set_then_succeeds() {
    let mut harness = DayHarness::start().await;
    let first = harness.start_load(DayGeneration::default().next()).await;
    harness
        .core
        .reject(first, quartz_protocol::ErrorKind::StaleCursor)
        .await;
    let first = harness.core.request(Op::QueryDay).await;
    assert!(
        serde_json::from_value::<quartz_protocol::QueryDayRequest>(first.body.clone())
            .expect("fresh day")
            .cursor
            .is_none()
    );
    harness.finish(first, Paging::Single).await;
    harness.loaded().await.expect("restart succeeded");
    harness.ui.stop().await;
}
#[tokio::test]
async fn stale_exhaustion_and_busy_are_typed_inline_failures() {
    for kind in [
        quartz_protocol::ErrorKind::StaleCursor,
        quartz_protocol::ErrorKind::Busy,
    ] {
        let mut harness = DayHarness::start().await;
        let mut query = harness.start_load(DayGeneration::default().next()).await;
        let attempts = if kind == quartz_protocol::ErrorKind::StaleCursor {
            super::super::day::MAX_STALE_RESTARTS + 1
        } else {
            1
        };
        for attempt in 0..attempts {
            harness.core.reject(query, kind.clone()).await;
            if attempt + 1 == attempts {
                break;
            }
            query = harness.core.request(Op::QueryDay).await;
        }
        let error = harness.loaded().await.expect_err("typed failure");
        let expected = match kind {
            quartz_protocol::ErrorKind::StaleCursor => {
                DayFailureCode::Local(LocalFailureCode::StaleExhausted)
            }
            kind => DayFailureCode::Protocol(kind),
        };
        assert_eq!(error.code(), expected);
        harness.ui.stop().await;
    }
}
#[tokio::test]
async fn day_events_coalesce_while_a_query_is_in_flight() {
    let mut harness = DayHarness::start().await;
    let first = harness.start_load(DayGeneration::default().next()).await;
    for _ in 0..3 {
        harness
            .core
            .event(
                Topic::Day(harness.date),
                serde_json::to_value(quartz_protocol::DayChanged {
                    date: harness.date,
                    versions: QueryVersions("v2".into()),
                })
                .expect("day event"),
            )
            .await;
    }
    harness.finish(first, Paging::Single).await;
    harness.loaded().await.expect("first day");
    let next = harness.core.request(Op::QueryDay).await;
    harness.finish(next, Paging::Single).await;
    harness.loaded().await.expect("one coalesced refresh");
    assert!(
        tokio::time::timeout(Duration::from_millis(25), harness.core.peer.receive())
            .await
            .is_err(),
        "no third query set"
    );
    harness.ui.stop().await;
}
#[tokio::test]
async fn date_switch_cancels_a_slow_load_before_its_response_arrives() {
    const NO_EXTRA_EVENT: Duration = Duration::from_millis(25);
    let mut harness = DayHarness::start().await;
    let first_generation = DayGeneration::default().next();
    // Hold this response until the complete result for the new date has arrived.
    let delayed = harness.start_load(first_generation).await;
    let second_generation = first_generation.next();
    let mut request = harness.request();
    request.date = harness.date.yesterday().expect("previous date");
    harness
        .ui
        .link
        .load_day(request, second_generation, DayLoad::Immediate)
        .await
        .expect("switch date");
    let remove = harness.core.request(Op::Unsubscribe).await;
    assert_eq!(
        serde_json::from_value::<quartz_protocol::Unsubscribe>(remove.body.clone())
            .expect("unsubscribe")
            .topics,
        vec![Topic::Day(harness.date)]
    );
    harness.core.ok(remove, serde_json::json!({})).await;
    harness.core.subscribe(vec![Topic::Day(request.date)]).await;
    let query = harness.core.request(Op::QueryDay).await;
    assert_eq!(
        serde_json::from_value::<quartz_protocol::QueryDayRequest>(query.body.clone())
            .expect("new date query")
            .local_date,
        request.date
    );
    harness.finish(query, Paging::Single).await;
    loop {
        match harness.ui.events.recv().await.expect("new result") {
            LinkEvent::DayLoaded { generation, result } => {
                assert_eq!(generation, second_generation);
                assert!(!generation.accepts(first_generation));
                assert_eq!(result.expect("new day").date, request.date);
                break;
            }
            LinkEvent::DayLoading { .. } => {}
            event => panic!("unexpected event {event:?}"),
        }
    }
    // This intentionally incomplete old reply must never reach the model mapper.
    harness.core.ok(delayed, serde_json::json!({})).await;
    assert!(
        tokio::time::timeout(NO_EXTRA_EVENT, harness.ui.events.recv())
            .await
            .is_err(),
        "cancelled generation has no completion event"
    );
    harness.ui.stop().await;
}
#[tokio::test]
async fn subscribe_failure_reconciles_the_actual_subscription_on_retry_or_date_switch() {
    enum Recovery {
        Retry,
        Switch,
    }
    for recovery in [Recovery::Retry, Recovery::Switch] {
        let mut harness = DayHarness::start().await;
        let generation = DayGeneration::default().next();
        let first = harness.start_load(generation).await;
        harness.finish(first, Paging::Single).await;
        harness.loaded().await.expect("initial day");
        let mut request = harness.request();
        request.date = request.date.yesterday().expect("previous date");
        harness
            .ui
            .link
            .load_day(request, generation.next(), DayLoad::Immediate)
            .await
            .expect("date switch");
        let remove = harness.core.request(Op::Unsubscribe).await;
        harness.core.ok(remove, serde_json::json!({})).await;
        let subscribe = harness.core.request(Op::Subscribe).await;
        harness
            .core
            .reject(subscribe, quartz_protocol::ErrorKind::Busy)
            .await;
        let failure = harness.loaded().await.expect_err("subscription rejected");
        assert_eq!(
            failure.code(),
            DayFailureCode::Protocol(quartz_protocol::ErrorKind::Busy)
        );
        if let Recovery::Switch = recovery {
            request.date = request.date.yesterday().expect("another date");
        }
        harness
            .ui
            .link
            .load_day(request, generation.next().next(), DayLoad::Immediate)
            .await
            .expect("recover subscription");
        // Unsubscribe already succeeded; the next operation must directly subscribe the target.
        harness.core.subscribe(vec![Topic::Day(request.date)]).await;
        let query = harness.core.request(Op::QueryDay).await;
        harness.finish(query, Paging::Single).await;
        assert_eq!(
            harness.loaded().await.expect("recovered day").date,
            request.date
        );
        harness.ui.stop().await;
    }
}
#[tokio::test]
async fn settled_zoom_cancels_the_in_flight_generation_without_changing_subscription() {
    let mut harness = DayHarness::start().await;
    let first_generation = DayGeneration::default().next();
    let delayed = harness.start_load(first_generation).await;
    tokio::time::pause();
    let generation = first_generation.next();
    harness
        .ui
        .link
        .load_day(harness.request(), generation, DayLoad::Settled)
        .await
        .expect("zoom intent");
    const EXECUTOR_TURNS: usize = 8;
    for _ in 0..EXECUTOR_TURNS {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(super::super::day::ZOOM_SETTLE_DELAY).await;
    tokio::time::resume();
    let query = harness.core.request(Op::QueryDay).await;
    harness.finish(query, Paging::Single).await;
    loop {
        if let LinkEvent::DayLoaded {
            generation: received,
            result,
        } = harness.ui.events.recv().await.expect("zoom result")
        {
            assert_eq!(received, generation);
            result.expect("new view");
            break;
        }
    }
    harness.core.ok(delayed, serde_json::json!({})).await;
    harness.ui.stop().await;
}
#[tokio::test]
async fn zoom_settle_debounce_uses_paused_time() {
    let mut harness = DayHarness::start().await;
    let first = harness.start_load(DayGeneration::default().next()).await;
    harness.finish(first, Paging::Single).await;
    harness.loaded().await.expect("initial day");
    tokio::time::pause();
    let request = harness.request();
    let generation = DayGeneration::default().next().next();
    harness
        .ui
        .link
        .load_day(request, generation, DayLoad::Settled)
        .await
        .expect("zoom");
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(super::super::day::ZOOM_SETTLE_DELAY / 2).await;
    harness
        .ui
        .link
        .load_day(request, generation.next(), DayLoad::Settled)
        .await
        .expect("new zoom");
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(super::super::day::ZOOM_SETTLE_DELAY / 2).await;
    assert!(
        harness.ui.events.receiver.try_recv().is_err(),
        "the latest zoom has not settled"
    );
    tokio::time::advance(super::super::day::ZOOM_SETTLE_DELAY / 2).await;
    tokio::time::resume();
    let query = harness.core.request(Op::QueryDay).await;
    harness.finish(query, Paging::Single).await;
    harness.loaded().await.expect("settled view");
    harness.ui.stop().await;
}
