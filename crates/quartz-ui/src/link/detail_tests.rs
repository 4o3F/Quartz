// Isolated in-process pipe tests. No application windows or production endpoints.
use super::*;
use crate::{
    format::{Formatter, Msg},
    model::live_details::LiveDetails,
};
use quartz_i18n::Language;
use quartz_protocol::{
    self as wire, ApplicationId, ApplicationRef, CategoryRef, CategorySource, DetailEntity,
    EntityClassificationRule, EntityDailyDuration, QueryEntityDetailRequest,
    QueryEntityDetailResponse, QuerySegmentDetailRequest, QuerySegmentDetailResponse,
    QueryVersions, TimelineSegment,
};

const QUIET_PERIOD: Duration = Duration::from_millis(25);
struct DetailHarness {
    ui: Harness,
    core: Core,
    date: wire::LocalDate,
}
impl DetailHarness {
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
            date: "2026-11-21".parse().expect("date"),
        }
    }
    fn request(&self, target: DetailTarget) -> DetailRequest {
        DetailRequest {
            date: self.date,
            target,
        }
    }
    async fn select(&mut self, target: DetailTarget, generation: DetailGeneration) {
        self.ui
            .link
            .select_details(Some(self.request(target)), generation)
            .await
            .expect("select detail");
    }
    fn category() -> CategoryRef {
        CategoryRef::Classified {
            category: wire::Category {
                id: wire::CategoryId(7),
                uuid: wire::CategoryUuid("builtin-development".into()),
                builtin_key: Some(wire::BuiltinCategory::Development),
                name: None,
                color: "cat.8".into(),
                tendency: wire::CategoryTendency::Focus,
            }
            .into(),
        }
    }
    fn rule() -> EntityClassificationRule {
        EntityClassificationRule {
            rule: "builtin.classification/editor@1"
                .parse()
                .expect("rule identity"),
            label: Some("Editor".into()),
            label_key: None,
            category: Self::category(),
        }
    }
    fn interval(source: CategorySource, start: i64) -> TimelineSegment {
        TimelineSegment {
            extraction: None,
            document_ref: None,
            start_utc_ms: start,
            end_utc_ms: start + 100,
            duration_ms: 75,
            clock_segment_id: Some(wire::ClockSegmentId(1)),
            activity: wire::Activity::Active,
            category: Self::category(),
            category_source: source,
            app: Some(ApplicationRef {
                id: ApplicationId(3),
                exe_name: Some("editor.exe".into()),
                display_name: "Editor".into(),
                aumid: None,
            }),
            window_instance: Some(wire::WindowInstanceRef::from_stored_id(
                wire::facts::WindowInstanceId(33),
            )),
            title: Some("retained title".into()),
        }
    }
    async fn entity(&mut self, request: Request) {
        let query: QueryEntityDetailRequest =
            serde_json::from_value(request.body.clone()).expect("entity parameters");
        assert_eq!(query.local_date, self.date);
        let daily = (0..wire::ENTITY_DETAIL_DAYS)
            .map(|index| EntityDailyDuration {
                local_date: self
                    .date
                    .checked_sub(jiff::Span::new().days(
                        i64::try_from(wire::ENTITY_DETAIL_DAYS - index - 1).expect("seven days"),
                    ))
                    .expect("history date"),
                active_ms: 1000,
            })
            .collect();
        let mut second_rule = Self::rule();
        second_rule.rule = "builtin.classification/learning@2"
            .parse()
            .expect("another rule");
        second_rule.label = None;
        second_rule.label_key = Some("category.builtin.learning".into());
        self.core
            .ok(
                request,
                serde_json::to_value(QueryEntityDetailResponse {
                    extraction: Vec::new(),
                    daily,
                    titles: Vec::new(),
                    rules: vec![Self::rule(), second_rule],
                    versions: QueryVersions("entity-v1".into()),
                    truncated: false,
                })
                .expect("entity JSON"),
            )
            .await;
    }
    async fn loaded(&mut self, expected: DetailGeneration) -> Result<DetailData, LoadDataError> {
        loop {
            match self.ui.events.recv().await.expect("detail event") {
                LinkEvent::DetailLoading { generation }
                | LinkEvent::DetailProgress { generation, .. } => {
                    assert!(expected.accepts(generation))
                }
                LinkEvent::DetailLoaded { generation, result } => {
                    assert!(expected.accepts(generation));
                    return result;
                }
                LinkEvent::Day(_) => {}
                event => panic!("unexpected event {event:?}"),
            }
        }
    }
}
#[tokio::test]
async fn single_response_intervals_and_entity_rules_map_labels_and_absent_titles() {
    let mut harness = DetailHarness::start().await;
    let generation = DetailGeneration::default().next();
    let instance = wire::WindowInstanceRef::from_stored_id(wire::facts::WindowInstanceId(33));
    harness
        .select(
            DetailTarget::Segment {
                range: UtcRange { start: 0, end: 300 },
                app: Some(ApplicationId(3)),
                window_instance: Some(instance),
            },
            generation,
        )
        .await;
    let query = harness.core.request(Op::QuerySegmentDetail).await;
    let parameters: QuerySegmentDetailRequest =
        serde_json::from_value(query.body.clone()).expect("segment parameters");
    assert_eq!(parameters.app, Some(ApplicationId(3)));
    assert_eq!(parameters.window_instance, Some(instance));
    let annotation = wire::AnnotationId(
        "abcdef00-0000-4000-8000-000000000001"
            .parse()
            .expect("annotation UUID"),
    );
    let mut manual = DetailHarness::interval(CategorySource::Annotation { annotation }, 100);
    manual.title = None;
    let mut unclassified = DetailHarness::interval(CategorySource::Unclassified, 200);
    unclassified.category = CategoryRef::Unclassified;
    unclassified.title = None;
    harness
        .core
        .ok(
            query,
            serde_json::to_value(QuerySegmentDetailResponse {
                reference_range: None,
                intervals: vec![
                    DetailHarness::interval(
                        CategorySource::Rule {
                            rule: DetailHarness::rule().rule,
                        },
                        0,
                    ),
                    manual,
                    unclassified,
                ],
                versions: QueryVersions("segments-v1".into()),
                truncated: false,
            })
            .expect("segment response"),
        )
        .await;
    let entity = harness.core.request(Op::QueryEntityDetail).await;
    harness.entity(entity).await;
    let mapped =
        LiveDetails::from_protocol(harness.loaded(generation).await.expect("loaded detail"))
            .expect("mapped detail");
    assert_eq!(mapped.intervals.len(), 3);
    let formatter = Formatter::new(Language::SimplifiedChinese);
    let first = mapped.intervals.first().expect("first interval");
    assert_eq!(first.duration, Duration::from_millis(75));
    assert_eq!(formatter.detail_source(&first.source), "规则：Editor");
    assert_eq!(formatter.category_name(&first.category.name), "开发");
    let manual = mapped.intervals.get(1).expect("manual interval");
    assert_eq!(
        formatter.detail_source(&manual.source),
        formatter.text(Msg::TooltipManual)
    );
    assert!(manual.title.is_none());
    let last = mapped.intervals.last().expect("unclassified interval");
    assert_eq!(formatter.detail_source(&last.source), "未分类");
    assert_eq!(formatter.category_name(&last.category.name), "未分类");
    let entity = mapped.entity.expect("app detail");
    assert_eq!(entity.daily.len(), wire::ENTITY_DETAIL_DAYS);
    assert!(entity.titles.is_empty());
    assert_eq!(entity.rules.len(), 2);
    assert_eq!(
        formatter.rule_label(&entity.rules.last().expect("second rule").label),
        "学习"
    );
    harness.ui.stop().await;
}
#[tokio::test]
async fn ranking_selection_requests_only_entity_detail_and_cancels_the_older_load() {
    let mut harness = DetailHarness::start().await;
    let old = DetailGeneration::default().next();
    harness
        .select(DetailTarget::App(ApplicationId(3)), old)
        .await;
    let delayed = harness.core.request(Op::QueryEntityDetail).await;
    // Drain the old loading event before making the new selection current.
    assert!(matches!(
        harness.ui.events.recv().await,
        Some(LinkEvent::DetailLoading { .. })
    ));
    let current = old.next();
    harness
        .select(DetailTarget::App(ApplicationId(4)), current)
        .await;
    let query = harness.core.request(Op::QueryEntityDetail).await;
    assert_eq!(
        serde_json::from_value::<QueryEntityDetailRequest>(query.body.clone())
            .expect("new app")
            .entity,
        DetailEntity::App {
            id: ApplicationId(4)
        }
    );
    harness.entity(query).await;
    assert_eq!(
        harness
            .loaded(current)
            .await
            .expect("current detail")
            .request
            .app(),
        Some(ApplicationId(4))
    );
    harness.core.ok(delayed, serde_json::json!({})).await;
    assert!(
        tokio::time::timeout(QUIET_PERIOD, harness.ui.events.recv())
            .await
            .is_err(),
        "cancelled selection has no completion"
    );
    harness.ui.stop().await;
}
#[tokio::test]
async fn day_events_refresh_the_open_panel_once_while_a_load_is_in_flight() {
    let mut harness = DetailHarness::start().await;
    let link = harness.ui.link.clone();
    let date = harness.date;
    let subscribed = tokio::spawn(async move { link.subscribe(vec![Topic::Day(date)]).await });
    harness.core.subscribe(vec![Topic::Day(date)]).await;
    subscribed
        .await
        .expect("subscription task")
        .expect("subscribe day");
    let generation = DetailGeneration::default().next();
    harness
        .select(DetailTarget::App(ApplicationId(3)), generation)
        .await;
    let first = harness.core.request(Op::QueryEntityDetail).await;
    const EVENT_BURST: usize = 3;
    for _ in 0..EVENT_BURST {
        harness
            .core
            .event(
                Topic::Day(date),
                serde_json::to_value(wire::DayChanged {
                    date,
                    versions: QueryVersions("changed".into()),
                })
                .expect("day event"),
            )
            .await;
    }
    harness.entity(first).await;
    harness.loaded(generation).await.expect("first detail");
    let next = harness.core.request(Op::QueryEntityDetail).await;
    harness.entity(next).await;
    harness.loaded(generation).await.expect("coalesced refresh");
    assert!(
        tokio::time::timeout(QUIET_PERIOD, harness.core.peer.receive())
            .await
            .is_err(),
        "no extra refresh"
    );
    harness
        .ui
        .link
        .select_details(None, generation.next())
        .await
        .expect("close panel");
    harness.ui.stop().await;
}
#[tokio::test]
async fn empty_segment_range_stays_empty_without_entity_queries() {
    let mut harness = DetailHarness::start().await;
    let generation = DetailGeneration::default().next();
    harness
        .select(
            DetailTarget::Segment {
                range: UtcRange { start: 0, end: 100 },
                app: None,
                window_instance: None,
            },
            generation,
        )
        .await;
    let first = harness.core.request(Op::QuerySegmentDetail).await;
    let query: QuerySegmentDetailRequest =
        serde_json::from_value(first.body.clone()).expect("segment request");
    assert!(query.app.is_none());
    assert!(query.window_instance.is_none());
    harness
        .core
        .ok(
            first,
            serde_json::to_value(QuerySegmentDetailResponse {
                reference_range: None,
                intervals: Vec::new(),
                versions: QueryVersions("new".into()),
                truncated: false,
            })
            .expect("empty segment range"),
        )
        .await;
    let data = harness.loaded(generation).await.expect("fresh detail");
    assert!(data.intervals.is_empty());
    assert!(data.entities.is_empty());
    let mapped = LiveDetails::from_protocol(data).expect("empty model");
    assert!(mapped.intervals.is_empty());
    assert!(mapped.entity.is_none());
    assert!(
        tokio::time::timeout(QUIET_PERIOD, harness.core.peer.receive())
            .await
            .is_err(),
        "empty range does not trigger entity queries"
    );
    harness.ui.stop().await;
}
#[tokio::test]
async fn closing_cancels_loading_and_older_intents_cannot_replace_a_newer_selection() {
    let mut harness = DetailHarness::start().await;
    let older = DetailGeneration::default().next();
    let current = older.next();
    harness
        .select(DetailTarget::App(ApplicationId(4)), current)
        .await;
    let delayed = harness.core.request(Op::QueryEntityDetail).await;
    assert!(matches!(
        harness.ui.events.recv().await,
        Some(LinkEvent::DetailLoading { .. })
    ));
    harness
        .select(DetailTarget::App(ApplicationId(3)), older)
        .await;
    assert!(
        tokio::time::timeout(QUIET_PERIOD, harness.core.peer.receive())
            .await
            .is_err(),
        "older intent must not replace the current selection"
    );
    harness
        .ui
        .link
        .select_details(None, current.next())
        .await
        .expect("close panel");
    harness.core.ok(delayed, serde_json::json!({})).await;
    assert!(
        tokio::time::timeout(QUIET_PERIOD, harness.ui.events.recv())
            .await
            .is_err(),
        "closing cancels the result"
    );
    harness
        .core
        .event(
            Topic::Day(harness.date),
            serde_json::to_value(wire::DayChanged {
                date: harness.date,
                versions: QueryVersions("after-close".into()),
            })
            .expect("day event"),
        )
        .await;
    assert!(matches!(
        harness.ui.events.recv().await,
        Some(LinkEvent::Day(_))
    ));
    assert!(
        tokio::time::timeout(QUIET_PERIOD, harness.core.peer.receive())
            .await
            .is_err(),
        "a closed panel does not refresh"
    );
    harness.ui.stop().await;
}
#[tokio::test]
async fn busy_detail_is_a_typed_failure_until_an_explicit_retry() {
    let mut harness = DetailHarness::start().await;
    let generation = DetailGeneration::default().next();
    harness
        .select(DetailTarget::App(ApplicationId(3)), generation)
        .await;
    let first = harness.core.request(Op::QueryEntityDetail).await;
    harness.core.reject(first, wire::ErrorKind::Busy).await;
    assert_eq!(
        harness
            .loaded(generation)
            .await
            .expect_err("busy detail")
            .code(),
        LoadDataErrorCode::Protocol(wire::ErrorKind::Busy)
    );
    assert!(
        tokio::time::timeout(QUIET_PERIOD, harness.core.peer.receive())
            .await
            .is_err(),
        "failure does not poll"
    );
    harness
        .select(DetailTarget::App(ApplicationId(3)), generation.next())
        .await;
    let retry = harness.core.request(Op::QueryEntityDetail).await;
    harness.entity(retry).await;
    harness
        .loaded(generation.next())
        .await
        .expect("retry succeeds");
    harness.ui.stop().await;
}
#[tokio::test]
async fn document_selection_requests_document_entity_detail_and_drops_late_previous_response() {
    let mut harness = DetailHarness::start().await;
    let reference_range = wire::ReferenceRange {
        start_utc_ms: 0,
        end_utc_ms: 86_400_000,
    };
    let doc_old = wire::DocumentRef::from_encoded("doc:old".into());
    let doc_new = wire::DocumentRef::from_encoded("doc:new".into());
    let old = DetailGeneration::default().next();
    harness
        .select(
            DetailTarget::Document {
                document_ref: doc_old.clone(),
                reference_range,
            },
            old,
        )
        .await;
    let delayed = harness.core.request(Op::QueryEntityDetail).await;
    let old_query: QueryEntityDetailRequest =
        serde_json::from_value(delayed.body.clone()).expect("old document query");
    assert_eq!(
        old_query.entity,
        DetailEntity::Document {
            document_ref: doc_old
        }
    );
    assert_eq!(old_query.reference_range, Some(reference_range));
    assert!(matches!(
        harness.ui.events.recv().await,
        Some(LinkEvent::DetailLoading { .. })
    ));

    let current = old.next();
    harness
        .select(
            DetailTarget::Document {
                document_ref: doc_new.clone(),
                reference_range,
            },
            current,
        )
        .await;
    let query = harness.core.request(Op::QueryEntityDetail).await;
    let new_query: QueryEntityDetailRequest =
        serde_json::from_value(query.body.clone()).expect("new document query");
    assert_eq!(
        new_query.entity,
        DetailEntity::Document {
            document_ref: doc_new.clone()
        }
    );
    assert_eq!(new_query.reference_range, Some(reference_range));
    harness.entity(query).await;
    let loaded = harness.loaded(current).await.expect("current document");
    assert_eq!(
        loaded.request.target,
        DetailTarget::Document {
            document_ref: doc_new,
            reference_range
        }
    );
    let mapped = LiveDetails::from_protocol(loaded).expect("mapped document detail");
    assert!(mapped.intervals.is_empty());
    assert!(mapped.entity.is_some());

    harness.core.ok(delayed, serde_json::json!({})).await;
    assert!(
        tokio::time::timeout(QUIET_PERIOD, harness.ui.events.recv())
            .await
            .is_err(),
        "late response for previous document subject must be ignored"
    );
    harness.ui.stop().await;
}

#[tokio::test]
async fn entity_detail_progress_reaches_active_generation_and_stops_after_superseding_selection() {
    let mut harness = DetailHarness::start().await;
    let first_gen = DetailGeneration::default().next();
    harness
        .select(DetailTarget::App(ApplicationId(3)), first_gen)
        .await;
    let first_req = harness.core.request(Op::QueryEntityDetail).await;
    let first_id = first_req.id;
    assert!(matches!(
        harness.ui.events.recv().await,
        Some(LinkEvent::DetailLoading { generation }) if generation == first_gen
    ));

    harness.core.progress(first_id, 2, 7).await;
    assert!(matches!(
        harness.ui.events.recv().await,
        Some(LinkEvent::DetailProgress {
            generation,
            progress: QueryProgress { done: 2, total: 7 },
        }) if generation == first_gen
    ));

    let second_gen = first_gen.next();
    harness
        .select(DetailTarget::App(ApplicationId(4)), second_gen)
        .await;
    let second_req = harness.core.request(Op::QueryEntityDetail).await;
    let second_id = second_req.id;
    assert!(matches!(
        harness.ui.events.recv().await,
        Some(LinkEvent::DetailLoading { generation }) if generation == second_gen
    ));

    harness.core.progress(first_id, 5, 7).await;
    harness.core.progress(second_id, 1, 7).await;
    assert!(matches!(
        harness.ui.events.recv().await,
        Some(LinkEvent::DetailProgress {
            generation,
            progress: QueryProgress { done: 1, total: 7 },
        }) if generation == second_gen
    ));

    harness.entity(second_req).await;
    harness.loaded(second_gen).await.expect("second detail");
    harness.core.ok(first_req, serde_json::json!({})).await;
    harness.ui.stop().await;
}
