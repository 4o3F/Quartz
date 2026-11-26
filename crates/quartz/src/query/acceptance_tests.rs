//! M4 acceptance tests (A1/A2): cross-view consistency, extraction cache parity,
//! and subscribed day-version invalidation.

use super::{
    QueryJob, QueryRequest, QueryResponse, calendar,
    classification::{Classifier, ExtractedTitle, QueryCategoryRef},
    preview::{PreviewJob, PreviewRequest},
    tests::{categories, classified_window, edit_foreground},
    versions::{PublishedVersions, VersionChannel, VersionPort, VersionPublisher},
};
use crate::{
    persistence::{CheckpointCollection, Persistence, dispatch::WriteRequest},
    rules::{registry::RegistryShell, write::RuleMutation},
    store::{
        Store,
        capture::test_support::{Fixture as CaptureFixture, metadata},
        query::test_support::QueryFixture,
    },
};
use jiff::tz::TimeZone;
use quartz_domain::{
    capture::{
        AppIdentity, CaptureCore, CaptureEvent, CaptureInput, Foreground as CaptureForeground,
        IdlePolicy, IdleSample, LastInput, MetadataSource, MonoInstant, ObservationSource,
        PrivacyContext, ProcessId, SessionStatus, StartedCapture, StopChange, StopReason,
        Timestamp, Title, UtcInstant, WindowFacts, WindowHandle,
    },
    facts::{ApplicationId, CaptureLevel, Foreground, TitleId},
    rules::{BUILTIN_CLASSIFICATION_REVISION, Field, FieldSnapshot, registry::RuleSnapshot},
};
use quartz_protocol as wire;
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
    sync::Arc,
    time::Duration,
};

type DocumentKey = (wire::ApplicationId, String, Option<String>, Option<String>);

fn ts(base_utc_ms: i64, offset_ms: u64) -> Timestamp {
    let delta = i64::try_from(offset_ms).expect("offset fits i64");
    Timestamp {
        utc: UtcInstant::try_from(base_utc_ms + delta).expect("valid UTC timestamp"),
        mono: MonoInstant::from_millis(offset_ms),
    }
}

fn window_event(
    handle: u64,
    exe_name: &str,
    display_name: &str,
    title: &str,
    started: UtcInstant,
) -> CaptureEvent {
    CaptureEvent::Foreground {
        source: ObservationSource::Review,
        foreground: CaptureForeground::Window {
            facts: Box::new(WindowFacts {
                handle: WindowHandle::new(handle),
                process: ProcessId::new(u32::try_from(handle).expect("pid fits u32")),
                process_created_utc: Some(started),
                class: "AcceptanceWindowClass".into(),
                app: AppIdentity::Exe(format!(r"c:\fixture\{exe_name}")),
                metadata_source: MetadataSource::Process,
                exe_name: Some(exe_name.into()),
                display_name: display_name.into(),
                title: Title::readable(title),
            }),
            privacy: PrivacyContext::default(),
        },
    }
}

fn run_query_in_zone(
    store: &mut Store,
    request: QueryRequest,
    rules: Arc<RuleSnapshot>,
    zone: TimeZone,
) -> QueryResponse {
    let mut reader = store.take_query_reader();
    let completed = QueryJob::start_in_zone(&mut reader, request, rules, zone)
        .expect("start query")
        .run_measured()
        .expect("complete query");
    store.return_query_reader(completed.reader);
    completed.response
}

fn run_preview(
    store: &mut Store,
    registry: &RegistryShell,
    body: serde_json::Value,
) -> wire::RulesPreviewResponse {
    let request = PreviewRequest::decode(body).expect("decode preview request");
    let mut reader = store.take_query_reader();
    let mut job = PreviewJob::start(&mut reader, request, registry).expect("start preview");
    let response = loop {
        match job.advance().expect("advance preview") {
            Some(QueryResponse::Preview(response)) => break response,
            Some(other) => panic!("expected preview response, got {other:?}"),
            None => {}
        }
    };
    store.return_query_reader(job.finish().expect("finish preview"));
    response
}

fn is_active_wire(activity: &wire::Activity) -> bool {
    matches!(
        activity,
        wire::Activity::Active
            | wire::Activity::ActiveOverride { .. }
            | wire::Activity::OfflineActivity { .. }
    )
}

fn direct_extract(rules: &RuleSnapshot, row: &Foreground) -> Option<ExtractedTitle> {
    if !Classifier::accepts_extraction(row) {
        return None;
    }
    let fields = FieldSnapshot::from_foreground(
        row,
        rules
            .extraction()
            .fields()
            .chain([Field::RawTitle, Field::DisplayName]),
        None,
    );
    Some(rules.extraction().extract(&fields))
}

fn subscribe_in_zone(
    publisher: &mut VersionPublisher,
    port: &VersionPort,
    store: &mut Store,
    dates: BTreeSet<wire::LocalDate>,
    zone: TimeZone,
) -> PublishedVersions {
    let received = port.submit_days(dates).expect("submit day subscription");
    let command = publisher.requests.try_recv().expect("subscribe command");
    publisher
        .receive_in_zone(store, command, zone)
        .expect("service subscription in zone");
    received
        .blocking_recv()
        .expect("subscription reply")
        .expect("valid subscribed dates")
}

fn publish_in_zone(
    publisher: &mut VersionPublisher,
    port: &VersionPort,
    store: &mut Store,
    zone: TimeZone,
) {
    port.refresh();
    let command = publisher.requests.try_recv().expect("refresh command");
    publisher
        .receive_in_zone(store, command, zone)
        .expect("publish versions in zone");
}

fn persist_single_fact_run(
    store: &mut Store,
    start_utc_ms: i64,
    duration_ms: u64,
    rules: Arc<RuleSnapshot>,
) {
    let start = ts(start_utc_ms, 0);
    let end = ts(start_utc_ms, duration_ms);
    let opened = Persistence::open(store, metadata()).expect("open persistence for run");
    let run = opened.startup.next_run;
    let mut shell = opened.persistence;
    let StartedCapture { mut core, step } = CaptureCore::start(run, start).expect("start run");
    let accepted = shell.accept(step.into_parts(run), start);
    assert!(accepted.feedback.is_empty());
    let initial = shell.flush(store, start, CheckpointCollection::Skip);
    assert!(initial.failure().is_none(), "{:?}", initial.failure());
    for feedback in initial.feedback {
        let _ = core
            .step(CaptureInput {
                at: start,
                rules: rules.clone(),
                event: feedback,
            })
            .expect("initial feedback");
    }
    let mut persist = |at: Timestamp, event: CaptureEvent| {
        let parts = core
            .step(CaptureInput {
                at,
                rules: rules.clone(),
                event,
            })
            .expect("step event")
            .into_parts(run);
        let accepted = shell.accept(parts, at);
        assert!(accepted.feedback.is_empty());
        let flushed = shell.flush(store, at, CheckpointCollection::Skip);
        assert!(flushed.failure().is_none(), "{:?}", flushed.failure());
        for feedback in flushed.feedback {
            let _ = core
                .step(CaptureInput {
                    at,
                    rules: rules.clone(),
                    event: feedback,
                })
                .expect("flush feedback");
        }
    };
    persist(
        start,
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    persist(
        start,
        window_event(1, "code.exe", "Visual Studio Code", "main.rs", start.utc),
    );
    persist(end, CaptureEvent::Shutdown);
}

#[test]
fn same_facts_and_rules_agree_across_views() {
    const DAY_OFFSET_MIN: i64 = 240;
    const MIN_MS: u64 = 60_000;
    const USER_CLASSIFICATION: &str = "format='quartz-rules/1'\n\
[set]\n\
key='user.acceptance'\n\
kind='classification'\n\
name='Acceptance classification'\n\
parent='builtin.classification'\n\
[[rule]]\n\
id='custom-notes'\n\
label='Custom notes'\n\
category='builtin:learning'\n\
when=[{field='app.exe_name',op='equals',value='custom-notes.exe'}]\n";

    let date: wire::LocalDate = "2025-01-02".parse().expect("fixture date");
    let next_date = date.tomorrow().expect("next date");
    let offset = calendar::DayStartOffset::try_from(DAY_OFFSET_MIN).expect("day offset");
    // PreviewJob::start resolves the host system time zone, so capture that zone once and
    // pass it explicitly to DayCalendar::in_zone and QueryJob::start_in_zone so all five
    // views share identical logical-day boundaries on any host.
    let zone = TimeZone::try_system().expect("system time zone");
    let day_calendar = calendar::DayCalendar::in_zone(date, offset, zone.clone()).expect("day");
    let day_start = day_calendar.range.start;
    let local_midnight = next_date
        .at(0, 0, 0, 0)
        .to_zoned_datetime(zone.clone())
        .expect("local midnight")
        .timestamp()
        .as_millisecond();
    assert!(
        local_midnight > day_start && local_midnight < day_calendar.range.end,
        "local midnight {local_midnight} must lie inside logical day {:?}",
        day_calendar.range
    );
    let midnight_offset = u64::try_from(local_midnight - day_start).expect("midnight offset");

    let start = ts(day_start, 0);
    let mut fixture = CaptureFixture::new(start);
    fixture.store.seed_categories().expect("seed categories");
    fixture
        .store
        .test_day_offset_write(DAY_OFFSET_MIN)
        .expect("write day-start offset");

    fixture.persist(
        start,
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    // [0, 300s): VS Code with a document-extracting title; [210s, 300s) is masked idle (90s).
    fixture.persist(
        start,
        window_event(
            1,
            "code.exe",
            "Visual Studio Code",
            "main.rs - Quartz - Visual Studio Code",
            start.utc,
        ),
    );
    fixture.persist(
        ts(day_start, 4 * MIN_MS),
        CaptureEvent::Idle(IdleSample {
            tick: Duration::from_millis(4 * MIN_MS),
            last_input: LastInput::Tick(30_000),
            policy: IdlePolicy::default(),
            exceptions: Vec::new(),
        }),
    );
    fixture.persist(
        ts(day_start, 5 * MIN_MS),
        CaptureEvent::Idle(IdleSample {
            tick: Duration::from_millis(5 * MIN_MS),
            last_input: LastInput::Tick(u32::try_from(5 * MIN_MS).expect("tick fits u32")),
            policy: IdlePolicy::default(),
            exceptions: Vec::new(),
        }),
    );
    // [300s, 420s): 120s active on app classified by the user rule.
    fixture.persist(
        ts(day_start, 5 * MIN_MS),
        window_event(
            2,
            "custom-notes.exe",
            "Custom Notes",
            "Design Notes - Custom Notes",
            start.utc,
        ),
    );
    // [420s, 540s): 120s active on an unclassified app.
    fixture.persist(
        ts(day_start, 7 * MIN_MS),
        window_event(3, "mystery.exe", "Mystery App", "Scratchpad", start.utc),
    );
    // [540s, 600s): 60s active on Word (builtin Learning + Office extraction).
    fixture.persist(
        ts(day_start, 9 * MIN_MS),
        window_event(
            4,
            "winword.exe",
            "Word",
            "Spec.docx - Microsoft Word",
            start.utc,
        ),
    );
    // Stopped (away) between 10m and 30s before local midnight.
    fixture.persist(
        ts(day_start, 10 * MIN_MS),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    fixture.persist(
        ts(day_start, midnight_offset - 30_000),
        CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
    );
    // [midnight - 30s, midnight + 60s): 90s active on VS Code crossing local midnight.
    fixture.persist(
        ts(day_start, midnight_offset - 30_000),
        window_event(
            1,
            "code.exe",
            "Visual Studio Code",
            "main.rs - Quartz - Visual Studio Code",
            start.utc,
        ),
    );
    fixture.persist(
        ts(day_start, midnight_offset + 60_000),
        CaptureEvent::Shutdown,
    );

    // Category override covering [60s, 120s), strictly inside VS Code's morning active segment.
    fixture
        .store
        .create_annotations(
            wire::AnnotationsCreateRequest {
                entities: vec![wire::CreateAnnotation {
                    uuid: wire::AnnotationId(
                        "abcdef00-0000-4000-8000-000000000001"
                            .parse()
                            .expect("annotation UUID"),
                    ),
                    kind: wire::AnnotationKind::CategoryOverride,
                    values: wire::AnnotationValues {
                        start_utc_ms: day_start + 60_000,
                        end_utc_ms: day_start + 120_000,
                        category: Some(wire::AnnotationCategory::Builtin {
                            key: wire::BuiltinCategory::Communication,
                        }),
                        label_preset: None,
                        label: None,
                        note: None,
                    },
                }],
            },
            start.utc,
        )
        .expect("create category override");

    let mut registry = RegistryShell::open(&mut fixture.store, start.utc).expect("registry");
    RuleMutation::Edit(wire::RuleSetsEditRequest {
        key: "user.acceptance".into(),
        base_version: 0,
        text: USER_CLASSIFICATION.into(),
    })
    .execute(&mut registry, &mut fixture.store, start.utc)
    .expect("apply user classification rule");
    let rules = registry.snapshots().load();

    // 1. query.day
    let QueryResponse::Day(day) = run_query_in_zone(
        &mut fixture.store,
        QueryRequest::Day(wire::QueryDayRequest { local_date: date }),
        rules.clone(),
        zone.clone(),
    ) else {
        panic!("expected QueryResponse::Day");
    };
    assert!(!day.truncated);
    assert_eq!(day.metrics.active_ms, 600_000);
    assert_eq!(day.metrics.unclassified_ms, 120_000);
    assert_eq!(day.applications.len(), 4);

    let mut day_categories: BTreeMap<Option<wire::CategoryId>, i64> = BTreeMap::new();
    let mut day_by_builtin: BTreeMap<Option<&'static str>, i64> = BTreeMap::new();
    let mut post_midnight_ms = 0_i64;
    for hour in &day.hours {
        for bucket in &hour.categories {
            if bucket.duration_ms > 0 {
                *day_categories.entry(bucket.category.id()).or_default() += bucket.duration_ms;
                let key = match &bucket.category {
                    wire::CategoryRef::Unclassified => None,
                    wire::CategoryRef::Classified { category } => {
                        category.builtin_key.map(|builtin| match builtin {
                            wire::BuiltinCategory::Development => "development",
                            wire::BuiltinCategory::Learning => "learning",
                            wire::BuiltinCategory::Communication => "communication",
                            wire::BuiltinCategory::Entertainment => "entertainment",
                            wire::BuiltinCategory::System => "system",
                        })
                    }
                };
                *day_by_builtin.entry(key).or_default() += bucket.duration_ms;
                if hour.start_utc_ms >= local_midnight {
                    post_midnight_ms += bucket.duration_ms;
                }
            }
        }
    }
    assert_eq!(
        post_midnight_ms, 60_000,
        "post-midnight portion of the crossing span stays in the offset logical day"
    );
    assert_eq!(
        day_by_builtin,
        BTreeMap::from([
            (None, 120_000),
            (Some("development"), 240_000),
            (Some("learning"), 180_000),
            (Some("communication"), 60_000),
        ])
    );
    assert_eq!(day_categories.values().sum::<i64>(), day.metrics.active_ms);

    let day_apps: BTreeMap<wire::ApplicationId, i64> = day
        .applications
        .iter()
        .map(|rank| (rank.app.id, rank.active_ms))
        .collect();
    assert_eq!(day_apps.values().sum::<i64>(), day.metrics.active_ms);

    let day_docs: BTreeMap<DocumentKey, i64> = day
        .documents
        .iter()
        .map(|rank| {
            (
                (
                    rank.app,
                    rank.document.clone(),
                    rank.project.clone(),
                    rank.profile.clone(),
                ),
                rank.active_ms,
            )
        })
        .collect();
    assert!(
        day_docs.iter().any(|((_, doc, project, _), ms)| {
            doc == "main.rs" && project.as_deref() == Some("Quartz") && *ms == 300_000
        }),
        "VS Code extracted document must combine morning and midnight-crossing spans: {day_docs:?}"
    );
    assert!(
        day_docs
            .iter()
            .any(|((_, doc, _, _), ms)| doc == "Spec.docx" && *ms == 60_000),
        "Word extracted document missing: {day_docs:?}"
    );

    // 2. query.timeline
    let QueryResponse::Timeline(timeline) = run_query_in_zone(
        &mut fixture.store,
        QueryRequest::Timeline(wire::QueryTimelineRequest {
            start_utc_ms: day_calendar.range.start,
            end_utc_ms: day_calendar.range.end,
            detail: wire::TimelineDetail::App,
            ms_per_pixel: wire::MillisecondsPerPixel(NonZeroU64::MIN),
        }),
        rules.clone(),
        zone.clone(),
    ) else {
        panic!("expected QueryResponse::Timeline");
    };
    assert!(!timeline.truncated);
    let mut timeline_active_ms = 0_i64;
    let mut timeline_idle_ms = 0_i64;
    let mut timeline_annotation_ms = 0_i64;
    let mut timeline_categories: BTreeMap<Option<wire::CategoryId>, i64> = BTreeMap::new();
    let mut timeline_apps: BTreeMap<wire::ApplicationId, i64> = BTreeMap::new();
    let mut timeline_unclassified_apps: BTreeMap<wire::ApplicationId, i64> = BTreeMap::new();
    for segment in &timeline.segments {
        if segment.activity == wire::Activity::Idle {
            timeline_idle_ms += segment.duration_ms;
        }
        if !is_active_wire(&segment.activity) || segment.duration_ms == 0 {
            continue;
        }
        timeline_active_ms += segment.duration_ms;
        if matches!(
            segment.category_source,
            wire::CategorySource::Annotation { .. }
        ) {
            timeline_annotation_ms += segment.duration_ms;
        }
        *timeline_categories
            .entry(segment.category.id())
            .or_default() += segment.duration_ms;
        if let Some(app) = &segment.app {
            *timeline_apps.entry(app.id).or_default() += segment.duration_ms;
            if segment.category == wire::CategoryRef::Unclassified {
                *timeline_unclassified_apps.entry(app.id).or_default() += segment.duration_ms;
            }
        }
    }
    assert_eq!(timeline_idle_ms, 90_000);
    assert_eq!(timeline_annotation_ms, 60_000);
    assert_eq!(timeline_active_ms, day.metrics.active_ms);
    assert_eq!(timeline_categories, day_categories);
    assert_eq!(timeline_apps, day_apps);

    // 3. query.range (single logical day, all three dimensions)
    let QueryResponse::Range(range) = run_query_in_zone(
        &mut fixture.store,
        QueryRequest::Range(wire::QueryRangeRequest {
            start_date: date,
            end_date: next_date,
            bucket: wire::RangeBucket::Day,
            dimensions: vec![
                wire::RangeDimension::Category,
                wire::RangeDimension::App,
                wire::RangeDimension::Document,
            ],
        }),
        rules.clone(),
        zone.clone(),
    ) else {
        panic!("expected QueryResponse::Range");
    };
    assert!(!range.truncated);
    assert_eq!(range.buckets.len(), 1);
    let bucket = &range.buckets[0];
    assert_eq!(bucket.active_ms, day.metrics.active_ms);
    let mut range_categories: BTreeMap<Option<wire::CategoryId>, i64> = BTreeMap::new();
    let mut range_apps: BTreeMap<wire::ApplicationId, i64> = BTreeMap::new();
    let mut range_docs: BTreeMap<DocumentKey, i64> = BTreeMap::new();
    for group in &bucket.groups {
        match (&group.dimension, &group.entity) {
            (
                wire::RangeDimension::Category,
                wire::RangeEntity::Category(wire::CategoryEntity::Category { category }),
            ) => {
                *range_categories.entry(category.id()).or_default() += group.active_ms;
            }
            (
                wire::RangeDimension::App,
                wire::RangeEntity::Work(wire::WorkEntity::App { id, .. }),
            ) => {
                *range_apps.entry(*id).or_default() += group.active_ms;
            }
            (
                wire::RangeDimension::Document,
                wire::RangeEntity::Work(wire::WorkEntity::Document {
                    app,
                    document,
                    project,
                    profile,
                    ..
                }),
            ) => {
                *range_docs
                    .entry((*app, document.clone(), project.clone(), profile.clone()))
                    .or_default() += group.active_ms;
            }
            other => panic!("unexpected range group {other:?}"),
        }
    }
    assert_eq!(range_categories, day_categories);
    assert_eq!(range_apps, day_apps);
    assert_eq!(range_docs, day_docs);

    // 4. query.unclassified
    let QueryResponse::Unclassified(unclassified) = run_query_in_zone(
        &mut fixture.store,
        QueryRequest::Unclassified(wire::QueryUnclassifiedRequest {
            start_date: date,
            end_date: next_date,
            group_by: wire::UnclassifiedGroupBy::App,
            search: None,
        }),
        rules,
        zone,
    ) else {
        panic!("expected QueryResponse::Unclassified");
    };
    assert!(!unclassified.truncated);
    let unclassified_apps: BTreeMap<wire::ApplicationId, i64> = unclassified
        .items
        .iter()
        .map(|item| match &item.entity {
            wire::WorkEntity::App { id, .. } => (*id, item.active_ms),
            other => panic!("expected unclassified app entity, got {other:?}"),
        })
        .collect();
    assert_eq!(unclassified_apps, timeline_unclassified_apps);
    assert_eq!(
        unclassified_apps.values().sum::<i64>(),
        day.metrics.unclassified_ms
    );

    // 5. rules.preview (identity draft: empty user classification set)
    let identity_body = serde_json::json!({
        "mutation": {
            "op": "rule_sets.edit",
            "body": {
                "key": "user.identity",
                "base_version": 0,
                "text": "format='quartz-rules/1'\n[set]\nkey='user.identity'\nkind='classification'\nname='Identity'\n"
            }
        },
        "start_date": date,
        "end_date": next_date,
    });
    let preview = run_preview(&mut fixture.store, &registry, identity_body);
    assert_eq!(preview.totals.before, preview.totals.after);
    assert_eq!(preview.totals.before.active_ms, day.metrics.active_ms);
    let preview_categories: BTreeMap<Option<wire::CategoryId>, i64> = preview
        .totals
        .before
        .categories
        .iter()
        .map(|rank| (rank.category.id(), rank.active_ms))
        .collect();
    assert_eq!(preview_categories, day_categories);
    assert_eq!(preview.document_change_count, 0);
    assert_eq!(preview.further_hidden_count, 0);

    let find_rule = |id: &str| {
        preview
            .rules
            .iter()
            .find(|stats| stats.rule == id)
            .unwrap_or_else(|| panic!("missing preview statistics for {id}"))
    };
    let vscode_id = format!("builtin.classification/vscode@{BUILTIN_CLASSIFICATION_REVISION}");
    let word_id = format!("builtin.classification/word@{BUILTIN_CLASSIFICATION_REVISION}");
    let vscode = find_rule(&vscode_id);
    let custom_notes = find_rule("user.acceptance/custom-notes@1");
    let word = find_rule(&word_id);
    assert_eq!(vscode.matched_ms, 390_000);
    assert_eq!(vscode.effective_ms, 240_000);
    assert_eq!(custom_notes.matched_ms, 120_000);
    assert_eq!(custom_notes.effective_ms, 120_000);
    assert_eq!(word.matched_ms, 60_000);
    assert_eq!(word.effective_ms, 60_000);
    assert!(
        preview
            .rules
            .iter()
            .all(|stats| stats.effective_ms <= stats.matched_ms)
    );
    let total_rule_effective_ms: i64 = preview.rules.iter().map(|stats| stats.effective_ms).sum();
    assert_eq!(
        total_rule_effective_ms + timeline_annotation_ms + day.metrics.unclassified_ms,
        day.metrics.active_ms
    );
}

#[test]
fn extraction_cache_matches_direct_computation() {
    let mut fixture = QueryFixture::new(0);
    let rules = fixture.rules();
    let mut cached = Classifier::new(rules.clone(), categories());

    struct RowSpec {
        app_id: i64,
        exe: &'static str,
        display_name: &'static str,
        title_id: Option<i64>,
        title: Option<&'static str>,
        level: CaptureLevel,
    }
    let specs = [
        RowSpec {
            app_id: 1,
            exe: "code.exe",
            display_name: "Visual Studio Code",
            title_id: Some(1),
            title: Some("main.rs - Quartz - Visual Studio Code"),
            level: CaptureLevel::Full,
        },
        // Exact repeat of (app_id, title_id, level) exercises the extraction cache hit path.
        RowSpec {
            app_id: 1,
            exe: "code.exe",
            display_name: "Visual Studio Code",
            title_id: Some(1),
            title: Some("main.rs - Quartz - Visual Studio Code"),
            level: CaptureLevel::Full,
        },
        RowSpec {
            app_id: 1,
            exe: "code.exe",
            display_name: "Visual Studio Code",
            title_id: Some(2),
            title: Some("● main.rs - Quartz - Visual Studio Code"),
            level: CaptureLevel::Full,
        },
        RowSpec {
            app_id: 2,
            exe: "winword.exe",
            display_name: "Word",
            title_id: Some(3),
            title: Some("Spec.docx - Microsoft Word"),
            level: CaptureLevel::Full,
        },
        RowSpec {
            app_id: 3,
            exe: "chrome.exe",
            display_name: "Google Chrome",
            title_id: Some(4),
            title: Some("Docs - Google Chrome - Work"),
            level: CaptureLevel::Full,
        },
        RowSpec {
            app_id: 4,
            exe: "windowsterminal.exe",
            display_name: "Windows Terminal",
            title_id: Some(5),
            title: Some("nvim Cargo.toml - Windows Terminal"),
            level: CaptureLevel::Full,
        },
        RowSpec {
            app_id: 4,
            exe: "windowsterminal.exe",
            display_name: "Windows Terminal",
            title_id: Some(6),
            title: Some("pwsh"),
            level: CaptureLevel::Full,
        },
        RowSpec {
            app_id: 5,
            exe: "custom-notes.exe",
            display_name: "Custom Notes",
            title_id: Some(7),
            title: Some("Design Notes - Custom Notes"),
            level: CaptureLevel::Full,
        },
        RowSpec {
            app_id: 5,
            exe: "custom-notes.exe",
            display_name: "Custom Notes",
            title_id: Some(8),
            title: Some(""),
            level: CaptureLevel::Full,
        },
        RowSpec {
            app_id: 5,
            exe: "custom-notes.exe",
            display_name: "Custom Notes",
            title_id: None,
            title: None,
            level: CaptureLevel::Full,
        },
        RowSpec {
            app_id: 1,
            exe: "code.exe",
            display_name: "Visual Studio Code",
            title_id: Some(1),
            title: Some("main.rs - Quartz - Visual Studio Code"),
            level: CaptureLevel::TitleHidden,
        },
        RowSpec {
            app_id: 1,
            exe: "code.exe",
            display_name: "Visual Studio Code",
            title_id: Some(1),
            title: Some("main.rs - Quartz - Visual Studio Code"),
            level: CaptureLevel::Excluded,
        },
    ];

    let rows: Vec<Foreground> = specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let id = i64::try_from(index + 1).expect("row id");
            let super::input::InputRow::Foreground(mut row) =
                classified_window(id, id * 10, id * 10 + 10, spec.exe)
            else {
                panic!("classified_window returns Foreground");
            };
            edit_foreground(&mut row, spec.level, |content| {
                let app = Arc::make_mut(content.app.as_mut().expect("app"));
                app.id = ApplicationId(spec.app_id);
                app.display_name = spec.display_name.into();
                content.title_id = spec.title_id.map(TitleId);
                content.title = spec.title.map(str::to_owned);
            });
            row
        })
        .collect();

    let first = cached
        .extract(&rows[0])
        .expect("initial VS Code extraction");
    let repeated = cached.extract(&rows[1]).expect("cached VS Code extraction");
    assert!(
        Arc::ptr_eq(&first, &repeated),
        "repeated (app, title_id, level) must hit the query-scoped extraction cache"
    );
    assert_eq!(
        Some((*first).clone()),
        direct_extract(&rules, &rows[0]),
        "first cached extraction must match direct RuleSnapshot computation"
    );

    for (index, row) in rows.iter().enumerate() {
        let got = cached.extract(row).map(|value| (*value).clone());
        let expected = direct_extract(&rules, row);
        assert_eq!(got, expected, "row {index} mismatch before daily reset");
    }

    cached.reset_day();
    for (index, row) in rows.iter().enumerate() {
        let got = cached.extract(row).map(|value| (*value).clone());
        let expected = direct_extract(&rules, row);
        assert_eq!(got, expected, "row {index} mismatch after daily reset");
    }
}

#[test]
fn each_input_change_invalidates_day_version() {
    // Sub-cases already covered verbatim by F1 tests in query::versions::publisher::tests:
    // - (b) extraction rule change: `extraction_rule_change_republishes_subscribed_days`
    // - (d) annotation create on subscribed date: `date_scoped_fact_and_annotation_changes_leave_unaffected_dates_unchanged`
    // - system time-zone refresh: `timezone_refresh_republishes_subscribed_days_and_updates_published_zone`
    let utc = TimeZone::UTC;
    let date: wire::LocalDate = "2025-01-02".parse().expect("subscribed date");
    let other_date: wire::LocalDate = "2025-01-20".parse().expect("unrelated date");
    let date_ms = date
        .at(10, 0, 0, 0)
        .to_zoned_datetime(utc.clone())
        .expect("date UTC")
        .timestamp()
        .as_millisecond();
    let other_ms = other_date
        .at(10, 0, 0, 0)
        .to_zoned_datetime(utc.clone())
        .expect("other date UTC")
        .timestamp()
        .as_millisecond();
    let now = UtcInstant::try_from(date_ms).expect("UTC instant");

    let mut fixture = QueryFixture::new(0);
    let mut registry = RegistryShell::open(&mut fixture.store, now).expect("registry");
    let VersionChannel {
        mut publisher,
        port,
    } = VersionPublisher::open(&mut fixture.store, registry.snapshots()).expect("publisher");
    // Drain startup pending_effects from seed_categories before subscribing.
    publish_in_zone(&mut publisher, &port, &mut fixture.store, utc.clone());

    let initial = subscribe_in_zone(
        &mut publisher,
        &port,
        &mut fixture.store,
        BTreeSet::from([date]),
        utc.clone(),
    );
    let mut current = initial.days[&date].versions.clone();

    // (a) Classification rule change
    RuleMutation::Edit(wire::RuleSetsEditRequest {
        key: "user.invalidation".into(),
        base_version: 0,
        text: "format='quartz-rules/1'\n\
[set]\n\
key='user.invalidation'\n\
kind='classification'\n\
name='Invalidation test'\n\
parent='builtin.classification'\n\
[[rule]]\n\
id='notes'\n\
label='Notes'\n\
category='builtin:learning'\n\
when=[{field='app.exe_name',op='equals',value='custom-notes.exe'}]\n"
            .into(),
    })
    .execute(&mut registry, &mut fixture.store, now)
    .expect("apply classification rule");
    publish_in_zone(&mut publisher, &port, &mut fixture.store, utc.clone());
    let after_classification = port.updates.borrow().days[&date].versions.clone();
    assert_ne!(
        after_classification, current,
        "classification rule change must invalidate subscribed day"
    );
    current = after_classification;

    // (c) Category rename (metadata)
    let dev = quartz_domain::categories::BuiltinCategory::Development;
    let rename = WriteRequest::decode(
        wire::Op::CategoriesUpdate,
        serde_json::to_value(wire::CategoriesUpdateRequest {
            entity: wire::CategoryEdit {
                uuid: dev.uuid().0.parse().expect("builtin category UUID"),
                parent_uuid: None,
                name: Some("Engineering".into()),
                color: dev.color().to_string(),
                tendency: wire::CategoryTendency::Focus,
            },
        })
        .expect("encode category update"),
    )
    .expect("decode category update");
    rename
        .execute(&mut fixture.store, now, None, &mut registry)
        .expect("rename category");
    publish_in_zone(&mut publisher, &port, &mut fixture.store, utc.clone());
    let after_rename = port.updates.borrow().days[&date].versions.clone();
    assert_ne!(
        after_rename, current,
        "category rename must invalidate subscribed day"
    );
    current = after_rename;

    // (e) New persisted capture fact on the subscribed date
    let rules = registry.snapshots().load();
    persist_single_fact_run(&mut fixture.store, date_ms, 5_000, rules.clone());
    publish_in_zone(&mut publisher, &port, &mut fixture.store, utc.clone());
    let after_same_day_fact = port.updates.borrow().days[&date].versions.clone();
    assert_ne!(
        after_same_day_fact, current,
        "new fact on subscribed date must invalidate subscribed day"
    );
    current = after_same_day_fact;

    // Fact on another date outside the 7-day reference window must NOT change subscribed day version.
    persist_single_fact_run(&mut fixture.store, other_ms, 5_000, rules);
    publish_in_zone(&mut publisher, &port, &mut fixture.store, utc.clone());
    let after_other_day_fact = port.updates.borrow().days[&date].versions.clone();
    assert_eq!(
        after_other_day_fact, current,
        "new fact on another date must not change subscribed day version"
    );

    // (f) Day-start offset setting change
    fixture
        .store
        .test_day_offset_write(240)
        .expect("write day-start offset");
    publish_in_zone(&mut publisher, &port, &mut fixture.store, utc);
    let after_offset = port.updates.borrow().days[&date].versions.clone();
    assert_ne!(
        after_offset, current,
        "day-start offset change must invalidate subscribed day"
    );
}
