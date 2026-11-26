// In-process pipe tests; no application windows or production endpoints.
use super::*;
use quartz_protocol::{
    ApplicationId, ApplicationRank, ApplicationRef, DayMetrics, HourStack, PendingAway,
    QueryDayResponse, QueryPendingAwayResponse, QueryTimelineRequest, QueryTimelineResponse,
    QueryVersions, TimelineDetail,
};
use std::num::NonZeroU32;

struct DayHarness {
    ui: Harness,
    core: Core,
    date: quartz_protocol::LocalDate,
}
enum DayVersionScope {
    Day,
    Documents { verified: QueryVersions },
}
fn empty_day_body(
    reference_range: Option<quartz_protocol::ReferenceRange>,
    versions: &str,
) -> serde_json::Value {
    serde_json::to_value(QueryDayResponse {
        reference_range,
        documents: Vec::new(),
        metrics: DayMetrics {
            active_ms: 0,
            focus_ratio: 0.0,
            distract_ratio: 0.0,
            unclassified_ms: 0,
            longest_focus_ms: 0,
            switches_per_hour: 0.0,
        },
        hours: vec![HourStack {
            start_utc_ms: 0,
            end_utc_ms: 86_400_000,
            local_start: "1970-01-01T00:00:00+00:00[UTC]".into(),
            categories: Vec::new(),
        }],
        applications: Vec::new(),
        versions: QueryVersions(versions.into()),
        truncated: false,
    })
    .expect("day JSON")
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
    async fn finish(&mut self, first: Request) {
        self.finish_scope(first, DayVersionScope::Day).await;
    }
    async fn finish_scope(&mut self, first: Request, scope: DayVersionScope) {
        let reference_range = match &scope {
            DayVersionScope::Day => None,
            DayVersionScope::Documents { .. } => Some(quartz_protocol::ReferenceRange {
                start_utc_ms: -6 * 86_400_000,
                end_utc_ms: 86_400_000,
            }),
        };
        let version = QueryVersions(
            if reference_range.is_some() {
                "ranking-v1"
            } else {
                "day-v1"
            }
            .into(),
        );
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
                    reference_range,
                    documents: Vec::new(),
                    metrics: DayMetrics {
                        active_ms: 2000,
                        focus_ratio: 0.,
                        distract_ratio: 0.,
                        unclassified_ms: 2000,
                        longest_focus_ms: 0,
                        switches_per_hour: 0.,
                    },
                    hours: vec![HourStack {
                        start_utc_ms: 0,
                        end_utc_ms: 86_400_000,
                        local_start: "1970-01-01T00:00:00+00:00[UTC]".into(),
                        categories: Vec::new(),
                    }],
                    applications: vec![app(1), app(2)],
                    versions: version,
                    truncated: false,
                })
                .expect("day JSON"),
            )
            .await;
        let version = QueryVersions("day-v1".into());
        for detail in [TimelineDetail::Category, TimelineDetail::Raw] {
            let query = self.core.request(Op::QueryTimeline).await;
            let parameters: QueryTimelineRequest =
                serde_json::from_value(query.body.clone()).expect("timeline request");
            assert_eq!(parameters.detail, detail);
            assert!(parameters.ms_per_pixel.0.get() > 0);
            self.core
                .ok(
                    query,
                    serde_json::to_value(QueryTimelineResponse {
                        reference_range: None,
                        segments: Vec::new(),
                        versions: version.clone(),
                        truncated: false,
                    })
                    .expect("timeline JSON"),
                )
                .await;
        }
        let query = self.core.request(Op::QueryPendingAway).await;
        self.core
            .ok(
                query,
                serde_json::to_value(QueryPendingAwayResponse {
                    ranges: vec![
                        PendingAway {
                            start_utc_ms: 0,
                            end_utc_ms: 900_000,
                            duration_ms: 900_000,
                        },
                        PendingAway {
                            start_utc_ms: 1_800_000,
                            end_utc_ms: 2_700_000,
                            duration_ms: 900_000,
                        },
                    ],
                    versions: version,
                    truncated: false,
                })
                .expect("away JSON"),
            )
            .await;
        if let DayVersionScope::Documents { verified } = scope {
            let query = self.core.request(Op::QueryDay).await;
            let _: quartz_protocol::QueryDayRequest =
                serde_json::from_value(query.body.clone()).expect("final version check");
            self.core
                .ok(query, empty_day_body(reference_range, &verified.0))
                .await;
        }
    }
    async fn finish_fresh(&mut self, first: Request) {
        self.core
            .ok(
                first,
                serde_json::json!({"reference_range":null,"applications":[],"documents":[],"metrics":{"active_ms":7000,"focus_ratio":0.0,"distract_ratio":0.0,"unclassified_ms":7000,"longest_focus_ms":0,"switches_per_hour":0.0},"hours":[{"start_utc_ms":0,"end_utc_ms":86400000,"local_start":"1970-01-01T00:00:00+00:00[UTC]","categories":[]}],"versions":"day-v2","truncated":false}),
            )
            .await;
        for _ in [TimelineDetail::Category, TimelineDetail::Raw] {
            let query = self.core.request(Op::QueryTimeline).await;
            self.core
                .ok(
                    query,
                    serde_json::json!({"segments":[],"versions":"range-v2","truncated":false}),
                )
                .await;
        }
        let pending = self.core.request(Op::QueryPendingAway).await;
        self.core
            .ok(
                pending,
                serde_json::json!({"ranges":[],"versions":"day-v2","truncated":false}),
            )
            .await;
    }
    async fn loaded(&mut self) -> Result<DayData, LoadDataError> {
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
async fn single_response_day_queries_load_and_use_raw_projection_for_lanes() {
    let mut harness = DayHarness::start().await;
    let first = harness.start_load(DayGeneration::default().next()).await;
    harness.finish(first).await;
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
    let _: quartz_protocol::QueryDayRequest =
        serde_json::from_value(first.body.clone()).expect("fresh day");
    harness.finish(first).await;
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
                LoadDataErrorCode::Local(LocalFailureCode::StaleExhausted)
            }
            kind => LoadDataErrorCode::Protocol(kind),
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
    harness.finish(first).await;
    harness.loaded().await.expect("first day");
    let next = harness.core.request(Op::QueryDay).await;
    harness.finish(next).await;
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
    harness.finish(query).await;
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
        harness.finish(first).await;
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
            LoadDataErrorCode::Protocol(quartz_protocol::ErrorKind::Busy)
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
        harness.finish(query).await;
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
    harness.finish(query).await;
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
    harness.finish(first).await;
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
    harness.finish(query).await;
    harness.loaded().await.expect("settled view");
    harness.ui.stop().await;
}

#[tokio::test]
async fn seven_day_ranking_scope_loads_with_single_day_timeline_and_pending_versions() {
    let mut harness = DayHarness::start().await;
    let first = harness.start_load(DayGeneration::default().next()).await;
    harness
        .finish_scope(
            first,
            DayVersionScope::Documents {
                verified: QueryVersions("ranking-v1".into()),
            },
        )
        .await;
    let data = harness.loaded().await.expect("day with document scope");
    assert_eq!(data.applications.len(), 2);
    harness.ui.stop().await;
}
#[tokio::test]
async fn changed_seven_day_inputs_restart_the_whole_day_load() {
    let mut harness = DayHarness::start().await;
    let first = harness.start_load(DayGeneration::default().next()).await;
    harness
        .finish_scope(
            first,
            DayVersionScope::Documents {
                verified: QueryVersions("ranking-v2".into()),
            },
        )
        .await;
    let retry = harness.core.request(Op::QueryDay).await;
    harness
        .finish_scope(
            retry,
            DayVersionScope::Documents {
                verified: QueryVersions("ranking-v1".into()),
            },
        )
        .await;
    harness.loaded().await.expect("fresh ranking scope");
    harness.ui.stop().await;
}

#[tokio::test]
async fn hiding_removes_registered_and_inflight_day_subscriptions_across_reopens() {
    enum SubscriptionStage { Registered, InFlight }
    for stage in [SubscriptionStage::Registered, SubscriptionStage::InFlight] {
        let mut harness = DayHarness::start().await;
        let mut generation = DayGeneration::default().next();
        for cycle in 0..2 {
            if cycle > 0 {
                harness.core.event(Topic::Control, serde_json::json!({"op":"ui.show"})).await;
                loop { if matches!(harness.ui.events.recv().await, Some(LinkEvent::Control(ControlOp::UiShow))) { break; } }
            }
            harness.ui.link.load_day(harness.request(), generation, DayLoad::Immediate).await.expect("load");
            let subscribe = harness.core.request(Op::Subscribe).await;
            let delayed = match stage {
                SubscriptionStage::Registered => {
                    harness.core.ok(subscribe, serde_json::json!({})).await;
                    Some(harness.core.request(Op::QueryDay).await)
                }
                SubscriptionStage::InFlight => Some(subscribe),
            };
            harness.ui.link.hidden().expect("hide");
            let remove = harness.core.request(Op::Unsubscribe).await;
            let topics: quartz_protocol::Unsubscribe = serde_json::from_value(remove.body.clone()).expect("unsubscribe");
            assert_eq!(topics.topics, [Topic::Day(harness.date)]);
            harness.core.ok(remove, serde_json::json!({})).await;
            let hidden = harness.core.request(Op::UiHidden).await;
            harness.core.ok(hidden, serde_json::json!({"accepted":true})).await;
            if let Some(delayed) = delayed { harness.core.ok(delayed, serde_json::json!({})).await; }
            assert!(tokio::time::timeout(Duration::from_millis(25), harness.core.peer.receive()).await.is_err(), "late subscription acknowledgement must not restart the hidden loader");
            harness.date = harness.date.yesterday().expect("previous date");
            generation = generation.next();
        }
        harness.ui.stop().await;
    }
}

#[tokio::test]
async fn first_geometry_request_is_immediate_even_when_marked_settled() {
    let mut harness = DayHarness::start().await;
    tokio::time::pause();
    harness.ui.link.load_day(harness.request(), DayGeneration::default().next(), DayLoad::Settled).await.expect("geometry");
    harness.core.subscribe(vec![Topic::Day(harness.date)]).await;
    let query = tokio::time::timeout(Duration::from_millis(1), harness.core.request(Op::QueryDay)).await.expect("first query starts without the 180 ms debounce");
    tokio::time::resume();
    harness.finish(query).await;
    harness.loaded().await.expect("first day");
    harness.ui.stop().await;
}

#[tokio::test]
async fn view_refinements_reuse_aggregates_and_day_events_invalidate_them() {
    let mut harness = DayHarness::start().await;
    let generation = DayGeneration::default().next();
    let query = harness.start_load(generation).await;
    harness.finish(query).await;
    let initial = harness.loaded().await.expect("initial day");
    let mut request = harness.request();
    request.view = DayView::Range(UtcRange { start: 1000, end: 1_800_000 });
    harness.ui.link.load_day(request, generation.next(), DayLoad::Settled).await.expect("refinement");
    let check = harness.core.request(Op::QueryDay).await;
    harness.core.ok(check, empty_day_body(None, "day-v1")).await;
    for detail in [TimelineDetail::Category, TimelineDetail::Raw] {
        let query = harness.core.request(Op::QueryTimeline).await;
        let parameters: QueryTimelineRequest = serde_json::from_value(query.body.clone()).expect("timeline");
        assert_eq!(parameters.detail, detail);
        assert_eq!(parameters.start_utc_ms, 1000);
        harness.core.ok(query, serde_json::json!({"segments":[],"versions":"range-v1","truncated":false})).await;
    }
    let check = harness.core.request(Op::QueryDay).await;
    harness.core.ok(check, empty_day_body(None, "day-v1")).await;
    let refined = harness.loaded().await.expect("refined day");
    assert_eq!(refined.metrics, initial.metrics);
    assert_eq!(refined.hours, initial.hours);
    assert_eq!(refined.applications, initial.applications);
    assert_eq!(refined.pending, initial.pending);
    assert!(tokio::time::timeout(Duration::from_millis(25), harness.core.peer.receive()).await.is_err(), "a refinement uses two timeline requests and two aggregate version checks, without reloading away ranges");
    harness.core.event(Topic::Day(harness.date), serde_json::to_value(quartz_protocol::DayChanged { date: harness.date, versions: QueryVersions("day-v2".into()) }).expect("event")).await;
    let query = harness.core.request(Op::QueryDay).await;
    harness.finish(query).await;
    harness.loaded().await.expect("fresh aggregates after invalidation");
    harness.ui.stop().await;
}

#[tokio::test]
async fn failed_hidden_subscription_cleanup_closes_the_connection_to_release_topics() {
    let mut harness = DayHarness::start().await;
    let _delayed = harness.start_load(DayGeneration::default().next()).await;
    harness.ui.link.hidden().expect("hide");
    let remove = harness.core.request(Op::Unsubscribe).await;
    harness.core.reject(remove, quartz_protocol::ErrorKind::Busy).await;
    loop {
        match harness.ui.events.recv().await.expect("event") {
            LinkEvent::Disconnected => break,
            LinkEvent::DayLoading { .. } => {},
            event => panic!("unexpected event {event:?}"),
        }
    }
    harness.ui.stop().await;
}

#[tokio::test]
async fn hidden_cleanup_waits_for_capacity_without_losing_the_hidden_notice() {
    let mut harness = DayHarness::start().await;
    let delayed = harness.start_load(DayGeneration::default().next()).await;
    let mut calls = Vec::new();
    // Keep one slot for the hide command's unsubscribe, with a day query already outstanding.
    for _ in 0..BRIDGE_CAPACITY - 2 {
        let link = harness.ui.link.clone();
        calls.push(tokio::spawn(async move { link.request(quartz_protocol::UiOpenRequest {}).await }));
    }
    let mut held = Vec::new();
    for _ in 0..BRIDGE_CAPACITY - 2 { held.push(harness.core.request(Op::UiOpen).await); }
    harness.ui.link.hidden().expect("hide");
    let remove = harness.core.request(Op::Unsubscribe).await;
    assert!(tokio::time::timeout(Duration::from_millis(25), harness.core.peer.receive()).await.is_err(), "the hidden notice waits until there is capacity");
    harness.core.ok(remove, serde_json::json!({})).await;
    // Bound the lifecycle-notice wait after freeing a request slot.
    const HIDDEN_NOTICE_DEADLINE: Duration = Duration::from_secs(1);
    let hidden = tokio::time::timeout(HIDDEN_NOTICE_DEADLINE, harness.core.request(Op::UiHidden))
        .await.expect("the hidden notice is sent as soon as capacity is available");
    harness.core.ok(hidden, serde_json::json!({"accepted":true})).await;
    for request in held { harness.core.ok(request, serde_json::json!({"accepted":true})).await; }
    harness.core.ok(delayed, serde_json::json!({})).await;
    for call in calls { call.await.expect("request task").expect("request response"); }
    harness.ui.stop().await;
}

#[tokio::test]
async fn followup_full_refresh_bypasses_cached_aggregates() {
    let mut harness = DayHarness::start().await;
    let generation = DayGeneration::default().next();
    let query = harness.start_load(generation).await;
    harness.finish(query).await;
    harness.loaded().await.expect("initial");
    let mut request = harness.request();
    request.view = DayView::Range(UtcRange { start: 1000, end: 1_800_000 });
    harness.ui.link.load_day(request, generation.next(), DayLoad::Immediate).await.expect("explicit refresh");
    let query = harness.core.request(Op::QueryDay).await;
    harness.finish(query).await;
    harness.loaded().await.expect("full refresh");
    harness.ui.stop().await;
}
#[tokio::test]
async fn followup_refinement_checks_versions_without_waiting_for_a_day_event() {
    let mut harness = DayHarness::start().await;
    let generation = DayGeneration::default().next();
    let query = harness.start_load(generation).await;
    harness.finish(query).await;
    harness.loaded().await.expect("initial");
    let mut request = harness.request();
    request.view = DayView::Range(UtcRange { start: 1000, end: 1_800_000 });
    harness.ui.link.load_day(request, generation.next(), DayLoad::Settled).await.expect("refinement");
    let check = harness.core.request(Op::QueryDay).await;
    harness.core.ok(check, empty_day_body(None, "day-v2")).await;
    let fresh = harness.core.request(Op::QueryDay).await;
    harness.finish_fresh(fresh).await;
    harness.loaded().await.expect("changed version triggers complete reload");
    harness.ui.stop().await;
}
#[tokio::test]
async fn followup_seven_day_rankings_are_not_reused_on_refinement() {
    let mut harness = DayHarness::start().await;
    let generation = DayGeneration::default().next();
    let first = harness.start_load(generation).await;
    harness.finish_scope(first, DayVersionScope::Documents { verified: QueryVersions("ranking-v1".into()) }).await;
    harness.loaded().await.expect("seven-day ranking");
    let mut request = harness.request();
    request.view = DayView::Range(UtcRange { start: 1000, end: 1_800_000 });
    harness.ui.link.load_day(request, generation.next(), DayLoad::Settled).await.expect("refinement");
    let first = harness.core.request(Op::QueryDay).await;
    harness.finish_scope(first, DayVersionScope::Documents { verified: QueryVersions("ranking-v1".into()) }).await;
    harness.loaded().await.expect("full seven-day data");
    harness.ui.stop().await;
}
#[tokio::test]
async fn followup_late_load_stays_hidden_until_a_show_control() {
    let mut harness = DayHarness::start().await;
    let generation = DayGeneration::default().next();
    let first = harness.start_load(generation).await;
    harness.finish(first).await;
    harness.loaded().await.expect("initial");
    harness.ui.link.hidden().expect("hide");
    let remove = harness.core.request(Op::Unsubscribe).await;
    harness.core.ok(remove, serde_json::json!({})).await;
    let hidden = harness.core.request(Op::UiHidden).await;
    harness.core.ok(hidden, serde_json::json!({"accepted":true})).await;
    harness.ui.link.load_day(harness.request(), generation.next(), DayLoad::Immediate).await.expect("late task intent");
    assert!(tokio::time::timeout(Duration::from_millis(25), harness.core.peer.receive()).await.is_err(), "hidden workers ignore late query tasks");
    harness.core.event(Topic::Control, serde_json::json!({"op":"ui.show"})).await;
    loop { if matches!(harness.ui.events.recv().await, Some(LinkEvent::Control(ControlOp::UiShow))) { break; } }
    harness.ui.link.load_day(harness.request(), generation.next().next(), DayLoad::Immediate).await.expect("new window intent");
    harness.core.subscribe(vec![Topic::Day(harness.date)]).await;
    let first = harness.core.request(Op::QueryDay).await;
    harness.finish(first).await;
    // A control event is also delivered to the UI receiver.
    loop { if matches!(harness.ui.events.recv().await, Some(LinkEvent::DayLoaded { result: Ok(_), .. })) { break; } }
    harness.ui.stop().await;
}
#[tokio::test]
async fn followup_date_switch_retires_old_loader_before_subscribe_answers() {
    let mut harness = DayHarness::start().await;
    let generation = DayGeneration::default().next();
    let delayed = harness.start_load(generation).await;
    let mut request = harness.request();
    request.date = request.date.yesterday().expect("date");
    harness.ui.link.load_day(request, generation.next(), DayLoad::Immediate).await.expect("date switch");
    let remove = harness.core.request(Op::Unsubscribe).await;
    harness.core.ok(remove, serde_json::json!({})).await;
    let subscribe = harness.core.request(Op::Subscribe).await;
    harness.core.event(Topic::Day(harness.date), serde_json::json!({"date":harness.date,"versions":"changed"})).await;
    harness.core.ok(delayed, empty_day_body(None, "day-v1")).await;
    assert!(tokio::time::timeout(Duration::from_millis(25), harness.core.peer.receive()).await.is_err(), "old date cannot query while waiting for the new subscription");
    harness.core.reject(subscribe, quartz_protocol::ErrorKind::Busy).await;
    harness.loaded().await.expect_err("subscription busy");
    harness.core.event(Topic::Day(harness.date), serde_json::json!({"date":harness.date,"versions":"changed-again"})).await;
    assert!(tokio::time::timeout(Duration::from_millis(25), harness.core.peer.receive()).await.is_err());
    harness.ui.stop().await;
}

#[tokio::test]
async fn followup_refinement_rechecks_versions_after_reading_new_timeline_segments() {
    let mut harness = DayHarness::start().await;
    let generation = DayGeneration::default().next();
    let first = harness.start_load(generation).await;
    harness.finish(first).await;
    harness.loaded().await.expect("initial");
    let mut request = harness.request();
    request.view = DayView::Range(UtcRange { start: 1000, end: 1_800_000 });
    harness.ui.link.load_day(request, generation.next(), DayLoad::Settled).await.expect("refinement");
    let before = harness.core.request(Op::QueryDay).await;
    harness.core.ok(before, empty_day_body(None, "day-v1")).await;
    for _ in [TimelineDetail::Category, TimelineDetail::Raw] {
        let query = harness.core.request(Op::QueryTimeline).await;
        harness.core.ok(query, serde_json::json!({"segments":[],"versions":"range-v2","truncated":false})).await;
    }
    let after = harness.core.request(Op::QueryDay).await;
    harness.core.ok(after, empty_day_body(None, "day-v2")).await;
    let first = harness.core.request(Op::QueryDay).await;
    harness.finish_fresh(first).await;
    let day = harness.loaded().await.expect("coherent fresh version");
    assert_eq!(day.metrics.active_ms, 7000);
    assert!(day.pending.is_empty());
    harness.ui.stop().await;
}
#[tokio::test]
async fn followup_successful_local_annotation_refreshes_without_a_day_event() {
    let mut harness = DayHarness::start().await;
    let generation = DayGeneration::default().next();
    let first = harness.start_load(generation).await;
    harness.finish(first).await;
    harness.loaded().await.expect("initial");
    let id = quartz_protocol::AnnotationId("abcdef00-0000-4000-8000-000000000001".parse().expect("UUID"));
    let annotation = AwayAction::Rest.request(UtcRange { start: 0, end: 900_000 }, id);
    let entity = annotation.entities.first().expect("note").clone();
    let link = harness.ui.link.clone();
    let call = tokio::spawn(async move { link.request(annotation).await });
    let write = harness.core.request(Op::AnnotationsCreate).await;
    harness.core.ok(write, serde_json::json!({"entities":[{"uuid":entity.uuid,"kind":entity.kind,"start_utc_ms":entity.values.start_utc_ms,"end_utc_ms":entity.values.end_utc_ms,"category":entity.values.category,"label_preset":entity.values.label_preset,"label":entity.values.label,"note":entity.values.note,"created_utc_ms":0,"updated_utc_ms":0}]})).await;
    call.await.expect("write task").expect("write success");
    // A cached WholeDay refresh must also query aggregates; no day event has been sent.
    const REFRESH_DEADLINE: Duration = Duration::from_secs(1);
    let first = tokio::time::timeout(REFRESH_DEADLINE, harness.core.request(Op::QueryDay)).await.expect("local mutation invalidates the loader");
    // The latest loaded request is WholeDay, so its full-day timeline versions match the day.
    harness.finish(first).await;
    harness.loaded().await.expect("mutation refresh");
    harness.ui.stop().await;
}
