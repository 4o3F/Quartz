use super::pipeline::DayAggregation;
use super::*;
use input::{
    Availability, CaptureLevel, ClockSegment, FactId, FactSpan, Foreground, ForegroundKind,
    InputRow,
};
use quartz_domain::{
    capture::StopReason,
    facts::{ApplicationId, ApplicationRef, ClockSegmentId},
};
use quartz_protocol::{Activity, QueryStopReason};

fn span(id: i64, start: i64, end: i64) -> FactSpan {
    FactSpan {
        id: FactId(id),
        segment: ClockSegmentId(1),
        start_utc_ms: start,
        end_utc_ms: end,
        start_mono_ms: start,
        end_mono_ms: end,
    }
}
fn clock(id: i64, utc: i64, mono: i64, duration: i64) -> InputRow {
    InputRow::Clock(ClockSegment {
        id: ClockSegmentId(id),
        start_utc_ms: utc,
        end_utc_ms: utc + duration,
        start_mono_ms: mono,
        end_mono_ms: mono + duration,
    })
}
fn window(id: i64, start: i64, end: i64) -> InputRow {
    InputRow::Foreground(Foreground::new(
        span(id, start, end),
        ForegroundKind::Window,
        CaptureLevel::Full,
        quartz_domain::facts::ForegroundContent {
            window_instance: Some(quartz_domain::facts::WindowInstanceId(id)),
            window_class: None,
            app: Some(
                ApplicationRef {
                    id: ApplicationId(id),
                    identity_key: r"exe:c:ixture\editor.exe".into(),
                    exe_name: Some("editor.exe".into()),
                    display_name: "Editor".into(),
                    aumid: None,
                }
                .into(),
            ),
            title: Some("document".into()),
            title_id: Some(quartz_domain::facts::TitleId(1)),
        },
    ))
}
pub(super) fn edit_foreground(
    row: &mut Foreground,
    level: CaptureLevel,
    edit: impl FnOnce(&mut quartz_domain::facts::ForegroundContent),
) {
    let mut content = quartz_domain::facts::ForegroundContent {
        window_instance: row.visible_window_instance(),
        window_class: row.visible_window_class().map(str::to_owned),
        app: row.visible_app().cloned(),
        title: row.visible_title().map(str::to_owned),
        title_id: row.visible_title_id(),
    };
    edit(&mut content);
    *row = Foreground::new(row.span.clone(), row.kind.clone(), level, content);
}
fn derive(rows: Vec<InputRow>, range: QueryRange) -> pipeline::DerivedSegments {
    let mut pipeline = Pipeline::new(
        range,
        super::classification::Classifier::new(
            quartz_domain::rules::registry::RuleRegistry::from_state(Default::default())
                .expect("empty rules")
                .snapshots()
                .load(),
            Vec::new(),
        ),
    );
    for row in rows {
        pipeline.ingest(row).expect("valid input row");
    }
    pipeline.finish_input();
    while !pipeline.advance() {}
    pipeline.into_segments()
}
#[test]
fn idle_masks_window_and_overlapping_stop_reasons_slice_boundaries() {
    let rows = vec![
        clock(1, 0, 0, 100),
        window(1, 0, 100),
        InputRow::Availability {
            span: span(2, 20, 40),
            state: Availability::Idle,
        },
        InputRow::Availability {
            span: span(3, 50, 80),
            state: Availability::Stopped(StopReason::Locked),
        },
        InputRow::Availability {
            span: span(4, 60, 90),
            state: Availability::Stopped(StopReason::Paused),
        },
    ];
    let segments = derive(rows, QueryRange { start: 0, end: 100 });
    assert_eq!(
        segments
            .segments
            .iter()
            .map(|segment| segments.project(segment).duration_ms)
            .sum::<i64>(),
        100
    );
    let idle = segments
        .segments
        .iter()
        .find(|segment| segments.project(segment).activity == Activity::Idle)
        .expect("idle slice");
    assert_eq!(idle.interval_id, Some(FactId(1)));
    assert_eq!(segments.project(idle).start_utc_ms, 20);
    assert_eq!(segments.project(idle).end_utc_ms, 40);
    assert!(segments.segments.iter().any(|segment| {
        segments.project(segment).activity
            == Activity::Stopped {
                reasons: vec![QueryStopReason::Locked, QueryStopReason::Paused],
            }
    }));
    let calendar = DayCalendar::new(
        "1970-01-01".parse().expect("date"),
        crate::query::calendar::DayStartOffset::MIDNIGHT,
    )
    .expect("calendar");
    let aggregate = DayAggregation::new(calendar, &segments);
    assert_eq!(aggregate.metrics.active_ms, 40);
    assert_eq!(aggregate.metrics.unclassified_ms, 40);
}
#[test]
fn active_union_does_not_double_count_overlapping_windows_and_clock_utc_repeats() {
    let mut second = window(3, 100, 200);
    if let InputRow::Foreground(row) = &mut second {
        row.span.segment = ClockSegmentId(2);
        row.span.start_utc_ms -= 100;
        row.span.end_utc_ms -= 100;
    }
    let rows = vec![
        clock(1, 0, 0, 100),
        clock(2, 0, 100, 100),
        window(1, 0, 100),
        window(2, 20, 80),
        second,
    ];
    let segments = derive(rows, QueryRange { start: 0, end: 100 });
    assert_eq!(
        segments
            .segments
            .iter()
            .filter(|segment| segments.project(segment).activity == Activity::Active)
            .map(|segment| segments.project(segment).duration_ms)
            .sum::<i64>(),
        200
    );
    assert!(
        segments
            .segments
            .iter()
            .all(|segment| { segments.project(segment).clock_segment_id.is_some() })
    );
}
#[test]
fn ordinary_day_hours_cover_the_logical_range() {
    let day = DayCalendar::new(
        "2025-01-02".parse().expect("date"),
        crate::query::calendar::DayStartOffset::MIDNIGHT,
    )
    .expect("day");
    assert_eq!(day.hours.len(), 24);
    assert_eq!(
        day.hours.first().expect("hour").start_utc_ms,
        day.range.start
    );
    assert_eq!(day.hours.last().expect("hour").end_utc_ms, day.range.end);
    assert!(
        day.hours
            .windows(2)
            .all(|pair| pair[0].end_utc_ms == pair[1].start_utc_ms)
    );
    let shifted = DayCalendar::new(
        "2025-01-02".parse().expect("date"),
        crate::query::calendar::DayStartOffset::try_from(240).expect("offset"),
    )
    .expect("shifted day");
    assert_eq!(shifted.range.start - day.range.start, 14_400_000);
}
#[test]
fn timeline_merge_is_strictly_subpixel_and_respects_projection_and_activity() {
    let segments = derive(
        vec![
            clock(1, 0, 0, 30),
            window(1, 0, 10),
            window(2, 10, 20),
            window(3, 20, 30),
        ],
        QueryRange { start: 0, end: 30 },
    );
    let mut category = TimelineProjection::default();
    let mut app = TimelineProjection::default();
    for segment in &segments.segments {
        category.push(
            segment,
            &segments.facts,
            quartz_protocol::TimelineDetail::Category,
            30,
        );
        app.push(
            segment,
            &segments.facts,
            quartz_protocol::TimelineDetail::App,
            30,
        );
    }
    assert_eq!(
        category.into_segments().len(),
        2,
        "equal to one pixel is retained"
    );
    assert_eq!(
        app.into_segments().len(),
        3,
        "different apps remain distinct"
    );
}

#[test]
fn timeline_merge_keeps_window_instances_and_raw_title_boundaries() {
    use quartz_domain::facts::WindowInstanceId;
    use quartz_protocol::{TimelineDetail, WindowInstanceRef};
    struct Case {
        second_window: WindowInstanceId,
        expected_app_segments: usize,
    }
    for case in [
        Case {
            second_window: WindowInstanceId(1),
            expected_app_segments: 1,
        },
        Case {
            second_window: WindowInstanceId(2),
            expected_app_segments: 2,
        },
    ] {
        let first = window(1, 0, 10);
        let mut second = window(2, 10, 20);
        let InputRow::Foreground(first_row) = &first else {
            panic!("foreground")
        };
        let InputRow::Foreground(second_row) = &mut second else {
            panic!("foreground")
        };
        edit_foreground(second_row, CaptureLevel::Full, |content| {
            content.app = first_row.visible_app().cloned();
            content.window_instance = Some(case.second_window);
        });
        let segments = derive(
            vec![clock(1, 0, 0, 20), first, second],
            QueryRange { start: 0, end: 20 },
        );
        for detail in [
            TimelineDetail::Category,
            TimelineDetail::App,
            TimelineDetail::Raw,
        ] {
            let mut projection = TimelineProjection::default();
            for segment in &segments.segments {
                projection.push(segment, &segments.facts, detail, 100);
            }
            let result = projection.into_segments();
            let expected = match detail {
                TimelineDetail::Category => 1,
                TimelineDetail::App | TimelineDetail::Raw => case.expected_app_segments,
            };
            assert_eq!(
                result.len(),
                expected,
                "detail {detail:?} window {:?}",
                case.second_window
            );
            assert_eq!(
                result
                    .iter()
                    .map(|segment| segment.duration_ms)
                    .sum::<i64>(),
                20
            );
            let first = result.first().expect("first segment");
            match detail {
                TimelineDetail::Category => assert!(first.window_instance.is_none()),
                TimelineDetail::App | TimelineDetail::Raw => assert_eq!(
                    first.window_instance,
                    Some(WindowInstanceRef::from_stored_id(
                        quartz_protocol::facts::WindowInstanceId(1)
                    ))
                ),
            }
            assert_eq!(first.title.is_some(), detail == TimelineDetail::Raw);
        }
    }
    let first = window(1, 0, 10);
    let mut second = window(2, 10, 20);
    let InputRow::Foreground(first_row) = &first else {
        panic!("foreground")
    };
    let InputRow::Foreground(second_row) = &mut second else {
        panic!("foreground")
    };
    edit_foreground(second_row, CaptureLevel::Full, |content| {
        content.app = first_row.visible_app().cloned();
        content.window_instance = first_row.visible_window_instance();
        content.title = Some("another title".into());
    });
    let segments = derive(
        vec![clock(1, 0, 0, 20), first, second],
        QueryRange { start: 0, end: 20 },
    );
    let mut projection = TimelineProjection::default();
    for segment in &segments.segments {
        projection.push(segment, &segments.facts, TimelineDetail::Raw, 100);
    }
    assert_eq!(
        projection.into_segments().len(),
        2,
        "raw preserves title boundaries in the same window"
    );
}

#[test]
fn window_identity_obeys_privacy_and_is_absent_from_non_foreground_states() {
    use quartz_protocol::TimelineDetail;
    for level in [
        CaptureLevel::Full,
        CaptureLevel::TitleHidden,
        CaptureLevel::Excluded,
    ] {
        let mut foreground = window(1, 0, 40);
        let InputRow::Foreground(row) = &mut foreground else {
            panic!("foreground")
        };
        edit_foreground(row, level, |_| {});
        assert_eq!(
            row.visible_window_instance().is_some(),
            level != CaptureLevel::Excluded
        );
        let segments = derive(
            vec![
                clock(1, 0, 0, 40),
                foreground,
                InputRow::Availability {
                    span: span(2, 10, 20),
                    state: Availability::Idle,
                },
                InputRow::Availability {
                    span: span(3, 20, 30),
                    state: Availability::Stopped(StopReason::Locked),
                },
            ],
            QueryRange { start: 0, end: 40 },
        );
        for detail in [
            TimelineDetail::Category,
            TimelineDetail::App,
            TimelineDetail::Raw,
        ] {
            for segment in &segments.segments {
                let projected = TimelineProjection::project(segment, &segments.facts, detail);
                let expected = detail != TimelineDetail::Category
                    && level != CaptureLevel::Excluded
                    && projected.activity == Activity::Active;
                assert_eq!(
                    projected.window_instance.is_some(),
                    expected,
                    "level {level:?}, detail {detail:?}, activity {:?}",
                    projected.activity
                );
                assert_eq!(
                    projected.title.is_some(),
                    detail == TimelineDetail::Raw && level == CaptureLevel::Full
                );
            }
        }
    }
    let mut row = window(1, 0, 10);
    let InputRow::Foreground(foreground) = &mut row else {
        panic!("foreground")
    };
    foreground.kind = ForegroundKind::NoForeground;
    let segments = derive(
        vec![clock(1, 0, 0, 10), row],
        QueryRange { start: 0, end: 10 },
    );
    assert!(segments.display(0).window_instance.is_none());
}

#[test]
fn gaps_excluded_and_no_foreground_preserve_all_state_boundaries() {
    let mut excluded = window(5, 40, 60);
    if let InputRow::Foreground(row) = &mut excluded {
        edit_foreground(row, CaptureLevel::Excluded, |_| {});
    }
    let mut no_foreground = window(6, 60, 80);
    if let InputRow::Foreground(row) = &mut no_foreground {
        row.kind = ForegroundKind::NoForeground;
        edit_foreground(row, CaptureLevel::Full, |content| {
            content.app = None;
            content.title = None;
        });
    }
    let rows = vec![
        clock(1, 0, 0, 100),
        window(1, 0, 100),
        InputRow::Gap {
            span: span(2, 20, 40),
            reason: quartz_domain::capture::GapReason::DatabaseError,
        },
        excluded,
        no_foreground,
    ];
    let segments = derive(rows, QueryRange { start: 10, end: 90 });
    assert_eq!(segments.segments.len(), 5);
    assert_eq!(segments.display(0).start_utc_ms, 10);
    assert!(matches!(segments.display(1).activity, Activity::Gap { .. }));
    assert_eq!(segments.display(2).activity, Activity::Excluded);
    assert!(segments.display(2).app.is_none());
    assert!(segments.display(2).title.is_none());
    assert_eq!(segments.display(3).activity, Activity::NoForeground);
    assert_eq!(segments.display(4).end_utc_ms, 90);
}
#[test]
fn zero_monotonic_sleep_retains_its_stopped_wall_range() {
    let mut sleeping = span(1, 0, 100);
    sleeping.end_mono_ms = 0;
    let rows = vec![
        InputRow::Clock(ClockSegment {
            id: ClockSegmentId(1),
            start_utc_ms: 0,
            end_utc_ms: 100,
            start_mono_ms: 0,
            end_mono_ms: 0,
        }),
        InputRow::Availability {
            span: sleeping,
            state: Availability::Stopped(StopReason::Suspended),
        },
    ];
    let segments = derive(rows, QueryRange { start: 10, end: 90 });
    assert_eq!(segments.segments.len(), 1);
    assert_eq!(segments.display(0).start_utc_ms, 10);
    assert_eq!(segments.display(0).end_utc_ms, 90);
    assert_eq!(segments.display(0).duration_ms, 0);
    assert_eq!(
        segments.display(0).activity,
        Activity::Stopped {
            reasons: vec![QueryStopReason::Suspended]
        }
    );
}
#[test]
fn timeline_merge_never_crosses_activity_gaps_or_clock_segments() {
    let mut segments = derive(
        vec![
            clock(1, 0, 0, 30),
            window(1, 0, 30),
            InputRow::Availability {
                span: span(2, 10, 20),
                state: Availability::Idle,
            },
        ],
        QueryRange { start: 0, end: 30 },
    );
    let mut projection = TimelineProjection::default();
    for segment in &segments.segments {
        projection.push(
            segment,
            &segments.facts,
            quartz_protocol::TimelineDetail::Category,
            100,
        );
    }
    assert_eq!(projection.into_segments().len(), 3);
    segments.segments[1].activity = pipeline::Activity::Active;
    segments.segments[1].span.start_utc_ms += 1;
    segments.segments[2].clock_segment_id = Some(ClockSegmentId(2));
    let mut projection = TimelineProjection::default();
    for segment in &segments.segments {
        projection.push(
            segment,
            &segments.facts,
            quartz_protocol::TimelineDetail::Category,
            100,
        );
    }
    assert_eq!(projection.into_segments().len(), 3);
}

#[test]
fn newly_opened_zero_length_clock_and_foreground_do_not_fail_the_query() {
    let segments = derive(
        vec![clock(1, 10, 10, 0), window(1, 10, 10)],
        QueryRange { start: 0, end: 20 },
    );
    assert_eq!(segments.segments.len(), 1);
    assert_eq!(
        segments.display(0).activity,
        Activity::Stopped {
            reasons: vec![QueryStopReason::CoreOffline]
        }
    );
    assert_eq!(segments.display(0).duration_ms, 20);
}

pub(super) fn categories() -> Vec<quartz_domain::categories::Category> {
    use quartz_domain::categories::{BuiltinCategory, Category, CategoryId};
    BuiltinCategory::ALL
        .into_iter()
        .enumerate()
        .map(|(index, builtin)| Category {
            id: CategoryId(i64::try_from(index + 1).expect("category id")),
            uuid: builtin.uuid(),
            builtin_key: Some(builtin),
            name: None,
            color: builtin.color(),
            tendency: builtin.tendency(),
        })
        .collect()
}
pub(super) fn classifier(overlays: &[&str]) -> classification::Classifier {
    use quartz_domain::rules::{
        self, ParseContext, ReferenceCatalog, RuleOrigin, RuleSet,
        registry::{RegistryCommand, RegistryState, RuleRegistry},
    };
    let mut references = ReferenceCatalog::default();
    for builtin in quartz_domain::categories::BuiltinCategory::ALL {
        references.insert_category(&builtin.uuid().0, Some(builtin.key()), None);
    }
    let mut registry = RuleRegistry::from_state(RegistryState {
        references,
        ..Default::default()
    })
    .expect("registry");
    struct BuiltinSet {
        source: &'static str,
        revision: u32,
    }
    for builtin in [
        BuiltinSet {
            source: rules::BUILTIN_CLASSIFICATION,
            revision: rules::BUILTIN_CLASSIFICATION_REVISION,
        },
        BuiltinSet {
            source: rules::BUILTIN_EXTRACTION,
            revision: rules::BUILTIN_EXTRACTION_REVISION,
        },
    ] {
        let set = RuleSet::parse(
            "builtin.toml",
            builtin.source,
            ParseContext {
                origin: RuleOrigin::Builtin,
                references: &registry.state().references,
            },
        )
        .expect("builtin");
        let decision = registry
            .decide(RegistryCommand::InstallBuiltin {
                set,
                revision: builtin.revision,
            })
            .into_parts();
        decision.reply.expect("install decision");
        registry
            .committed(decision.outcome.expect("install write"))
            .expect("commit builtin");
    }
    for text in overlays {
        let set = RuleSet::parse(
            "overlay.toml",
            text,
            ParseContext {
                origin: RuleOrigin::User,
                references: &registry.state().references,
            },
        )
        .expect("overlay");
        let decision = registry
            .decide(RegistryCommand::Apply {
                key: set.header.key.parse().expect("key"),
                file: "overlay.toml".into(),
                text: (*text).into(),
                base_version: 0,
            })
            .into_parts();
        decision.reply.expect("apply overlay");
        registry
            .committed(decision.outcome.expect("overlay write"))
            .expect("commit overlay");
    }
    classification::Classifier::new(registry.snapshots().load(), categories())
}
pub(super) fn classified_window(id: i64, start: i64, end: i64, exe: &str) -> InputRow {
    let mut input = window(id, start, end);
    if let InputRow::Foreground(row) = &mut input {
        edit_foreground(row, CaptureLevel::Full, |content| {
            let app = std::sync::Arc::make_mut(content.app.as_mut().expect("app"));
            app.exe_name = Some(exe.into());
            app.identity_key = format!(r"exe:c:\fixture\{exe}");
        });
    }
    input
}
fn derive_classified(
    rows: Vec<InputRow>,
    range: QueryRange,
    classifier: classification::Classifier,
) -> pipeline::DerivedSegments {
    let mut pipeline = Pipeline::new(range, classifier);
    for row in rows {
        pipeline.ingest(row).expect("row");
    }
    pipeline.finish_input();
    while !pipeline.advance() {}
    pipeline.into_segments()
}
#[test]
fn classification_uses_application_fields_and_respects_capture_levels_and_child_order() {
    use quartz_protocol::{BuiltinCategory, CategoryRef, CategorySource};
    const TITLE_RULE: &str = r"format='quartz-rules/1'
[set]
key='user.titles'
kind='classification'
name='Titles'
parent='builtin.classification'
[[rule]]
id='read'
label='Reading title'
category='builtin:learning'
priority=60
when=[{field='title.raw',op='equals',value='document'}]
";
    let mut rows = vec![
        clock(1, 0, 0, 60),
        classified_window(1, 0, 10, "code.exe"),
        classified_window(2, 10, 20, "unknown.exe"),
        classified_window(3, 20, 30, "code.exe"),
        classified_window(4, 30, 40, "unknown.exe"),
        classified_window(4, 40, 50, "unknown.exe"),
        classified_window(5, 50, 60, "code.exe"),
    ];
    if let InputRow::Foreground(row) = &mut rows[3] {
        edit_foreground(row, CaptureLevel::Excluded, |_| {});
    }
    if let InputRow::Foreground(row) = &mut rows[4] {
        edit_foreground(row, CaptureLevel::TitleHidden, |_| {});
    }
    if let InputRow::Foreground(row) = &mut rows[6] {
        edit_foreground(row, CaptureLevel::TitleHidden, |_| {});
    }
    let segments = derive_classified(
        rows.clone(),
        QueryRange { start: 0, end: 60 },
        classifier(&[]),
    );
    assert!(
        matches!(&segments.display(0).category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Development))
    );
    assert!(
        matches!(&segments.display(0).category_source, CategorySource::Rule { rule } if rule.as_str() == "builtin.classification/vscode@2")
    );
    assert_eq!(segments.display(1).category, CategoryRef::Unclassified);
    assert_eq!(segments.display(2).category, CategoryRef::Unclassified);
    assert_eq!(segments.display(2).activity, Activity::Excluded);
    let titled = derive_classified(
        rows,
        QueryRange { start: 0, end: 60 },
        classifier(&[TITLE_RULE]),
    );
    assert_eq!(
        titled.display(3).category,
        CategoryRef::Unclassified,
        "hidden title cannot match"
    );
    assert!(
        matches!(&titled.display(4).category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Learning)),
        "same app/title ids at full capture have a separate cache entry"
    );
    assert!(
        matches!(&titled.display(5).category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Development)),
        "application rules still apply to TitleHidden"
    );
    let mut title_only = window(1, 0, 10);
    if let InputRow::Foreground(row) = &mut title_only {
        edit_foreground(row, row.capture_level(), |content| content.app = None);
    }
    let title_only = derive_classified(
        vec![clock(1, 0, 0, 10), title_only],
        QueryRange { start: 0, end: 10 },
        classifier(&[TITLE_RULE]),
    );
    assert!(
        matches!(&title_only.display(0).category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Learning)),
        "a window can match its available title without application metadata"
    );
    const CHILD: &str = r"format='quartz-rules/1'
[set]
key='user.child'
kind='classification'
name='Child'
parent='builtin.classification'
[[rule]]
id='code'
label='Treat editor as communication'
category='builtin:communication'
priority=50
when=[{field='app.exe_name',op='equals',value='code.exe'}]
";
    let child = derive_classified(
        vec![clock(1, 0, 0, 10), classified_window(1, 0, 10, "code.exe")],
        QueryRange { start: 0, end: 10 },
        classifier(&[CHILD]),
    );
    assert!(
        matches!(&child.display(0).category_source, CategorySource::Rule { rule } if rule.as_str() == "user.child/code@1"),
        "deeper rule wins equal priority"
    );
}
#[test]
fn classification_preserves_title_dependent_overlays_across_fact_slices() {
    use quartz_protocol::{BuiltinCategory, CategoryRef, CategorySource};
    const TITLE_RULE: &str = r"format='quartz-rules/1'
[set]
key='user.titles'
kind='classification'
name='Titles'
parent='builtin.classification'
[[rule]]
id='read'
label='Reading title'
category='builtin:learning'
priority=60
when=[{field='title.raw',op='equals',value='document'}]
";
    let mut changed_title = classified_window(2, 10, 20, "code.exe");
    if let InputRow::Foreground(row) = &mut changed_title {
        edit_foreground(row, CaptureLevel::Full, |content| {
            std::sync::Arc::make_mut(content.app.as_mut().expect("application")).id =
                ApplicationId(1);
            content.title_id = Some(quartz_domain::facts::TitleId(2));
            content.title = Some("source code".into());
        });
    }
    let segments = derive_classified(
        vec![
            clock(1, 0, 0, 20),
            classified_window(1, 0, 10, "code.exe"),
            changed_title,
            InputRow::Availability {
                span: span(3, 2, 4),
                state: Availability::Idle,
            },
        ],
        QueryRange { start: 0, end: 20 },
        classifier(&[TITLE_RULE]),
    );
    assert_eq!(segments.segments.len(), 4);
    for segment in &segments.segments[..3] {
        assert!(matches!(
            &segments.project(segment).category,
            CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Learning)
        ));
        assert!(matches!(
            &segments.project(segment).category_source,
            CategorySource::Rule { rule } if rule.as_str() == "user.titles/read@1"
        ));
    }
    assert!(
        matches!(
            &segments.display(3).category_source,
            CategorySource::Rule { rule } if rule.as_str() == "builtin.classification/vscode@2"
        ),
        "a new title for the same application must be evaluated against the overlay"
    );
}

#[test]
fn day_tendencies_and_focus_continuity_and_category_projection_use_classification() {
    let mut rows = vec![
        clock(1, 0, 0, 110),
        classified_window(1, 0, 20, "code.exe"),
        classified_window(2, 20, 40, "winword.exe"),
        classified_window(3, 40, 50, "vlc.exe"),
        classified_window(4, 50, 60, "unknown.exe"),
        classified_window(5, 60, 100, "code.exe"),
        classified_window(6, 100, 110, "explorer.exe"),
    ];
    rows.push(InputRow::Availability {
        span: span(7, 70, 80),
        state: Availability::Idle,
    });
    let segments = derive_classified(rows, QueryRange { start: 0, end: 110 }, classifier(&[]));
    let calendar = test_day_calendar();
    let aggregate = DayAggregation::new(calendar, &segments);
    assert_eq!(aggregate.metrics.active_ms, 100);
    assert_eq!(aggregate.metrics.focus_ratio, 0.7);
    assert_eq!(aggregate.metrics.distract_ratio, 0.1);
    assert_eq!(aggregate.metrics.unclassified_ms, 10);
    assert_eq!(
        aggregate.metrics.longest_focus_ms, 40,
        "different focus categories stay continuous; idle breaks the later run"
    );
    assert_eq!(
        aggregate.calendar.hours[0]
            .categories
            .iter()
            .map(|bucket| bucket.duration_ms)
            .sum::<i64>(),
        100
    );
    let split = derive_classified(
        vec![
            clock(1, 0, 0, 20),
            classified_window(1, 0, 10, "code.exe"),
            classified_window(2, 10, 20, "devenv.exe"),
        ],
        QueryRange { start: 0, end: 20 },
        classifier(&[]),
    );
    let mut projection = TimelineProjection::default();
    for segment in &split.segments {
        projection.push(
            segment,
            &split.facts,
            quartz_protocol::TimelineDetail::Category,
            30,
        );
    }
    let merged = projection.into_segments();
    assert_eq!(
        merged.len(),
        1,
        "category detail merges across apps and matching rules"
    );
    assert_eq!(
        merged[0].category_source,
        quartz_protocol::CategorySource::Mixed
    );
    assert_eq!(merged[0].duration_ms, 20);
    let mut changed_metadata = split.clone();
    changed_metadata
        .facts
        .rename_category(&changed_metadata.segments[1], "Same category identity");
    let mut identity_projection = TimelineProjection::default();
    for segment in &changed_metadata.segments {
        identity_projection.push(
            segment,
            &changed_metadata.facts,
            quartz_protocol::TimelineDetail::Category,
            30,
        );
    }
    assert_eq!(
        identity_projection.into_segments().len(),
        1,
        "category identity controls merging"
    );
    let mut separated = split;
    separated.segments[1].clock_segment_id = Some(ClockSegmentId(2));
    let aggregate = DayAggregation::new(test_day_calendar(), &separated);
    assert_eq!(
        aggregate.metrics.longest_focus_ms, 10,
        "clock boundary breaks focus continuity"
    );
}

fn test_day_calendar() -> DayCalendar {
    let mut calendar = DayCalendar::new(
        "1970-01-01".parse().expect("date"),
        crate::query::calendar::DayStartOffset::MIDNIGHT,
    )
    .expect("calendar");
    let offset = calendar.range.start;
    calendar.range.start -= offset;
    calendar.range.end -= offset;
    for hour in &mut calendar.hours {
        hour.start_utc_ms -= offset;
        hour.end_utc_ms -= offset;
    }
    calendar
}

#[test]
fn read_errors_day_offsets_and_missing_readers_keep_internal_protocol_codes() {
    for source in [
        ReadQueryError::InvalidSettings,
        ReadQueryError::InvalidFacts,
    ] {
        let error = RunQueryError::from(source);
        assert!(matches!(error, RunQueryError::Read { .. }));
        assert_eq!(error.kind(), quartz_protocol::ErrorKind::Internal);
    }
    let error = RunQueryError::from(
        crate::query::calendar::DayStartOffset::try_from(-1).expect_err("invalid offset"),
    );
    assert!(matches!(error, RunQueryError::InvalidDayOffset));
    assert_eq!(error.kind(), quartz_protocol::ErrorKind::Internal);
    let request = QueryRequest::Day(QueryDayRequest {
        local_date: "2025-01-02".parse().expect("date"),
        cursor: None,
    });
    let rules = quartz_domain::rules::registry::RuleRegistry::from_state(Default::default())
        .expect("empty rules")
        .snapshots()
        .load();
    let error = match QueryJob::start(&mut None, request, rules) {
        Err(error) => error,
        Ok(_) => panic!("a query requires a reader"),
    };
    assert!(matches!(error, RunQueryError::ReaderUnavailable));
    assert_eq!(error.kind(), quartz_protocol::ErrorKind::Internal);
}

impl pipeline::DerivedSegments {
    fn project(&self, segment: &pipeline::DerivedSegment) -> quartz_protocol::TimelineSegment {
        TimelineProjection::project(segment, &self.facts, quartz_protocol::TimelineDetail::Raw)
    }
    fn display(&self, index: usize) -> quartz_protocol::TimelineSegment {
        TimelineProjection::project(
            &self.segments[index],
            &self.facts,
            quartz_protocol::TimelineDetail::Raw,
        )
    }
}

fn annotation(
    id: u32,
    kind: quartz_protocol::AnnotationKind,
    start: i64,
    end: i64,
    category: Option<i64>,
) -> InputRow {
    InputRow::Annotation(input::QueryAnnotation {
        id: quartz_protocol::AnnotationId(
            format!("abcdef00-0000-4000-8000-{id:012x}")
                .parse()
                .expect("annotation UUID"),
        ),
        kind,
        range: QueryRange { start, end },
        category: category.map(quartz_domain::categories::CategoryId),
    })
}

#[test]
fn annotation_categories_restore_allowed_idle_and_preserve_ineligible_slices() {
    use quartz_protocol::{AnnotationKind, BuiltinCategory, CategoryRef, CategorySource};
    let mut excluded = classified_window(2, 60, 80, "code.exe");
    if let InputRow::Foreground(row) = &mut excluded {
        edit_foreground(row, CaptureLevel::Excluded, |_| {});
    }
    let no_foreground = InputRow::Foreground(Foreground::new(
        span(3, 80, 100),
        ForegroundKind::NoForeground,
        CaptureLevel::Full,
        Default::default(),
    ));
    let segments = derive_classified(
        vec![
            clock(1, 0, 0, 100),
            classified_window(1, 0, 60, "code.exe"),
            excluded,
            no_foreground,
            InputRow::Availability {
                span: span(4, 20, 100),
                state: Availability::Idle,
            },
            annotation(1, AnnotationKind::CategoryOverride, 0, 100, Some(4)),
            annotation(2, AnnotationKind::ActiveOverride, 40, 100, Some(2)),
        ],
        QueryRange { start: 0, end: 100 },
        classifier(&[]),
    );
    let active = segments.display(0);
    assert_eq!(active.duration_ms, 20);
    assert_eq!(active.activity, Activity::Active);
    assert!(matches!(
        active.category_source,
        CategorySource::Annotation { .. }
    ));
    assert!(
        matches!(active.category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Entertainment))
    );
    let idle = segments.display(1);
    assert_eq!(idle.activity, Activity::Idle);
    assert!(matches!(idle.category_source, CategorySource::Rule { .. }));
    assert!(
        matches!(idle.category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Development))
    );
    let restored = segments.display(2);
    assert!(matches!(restored.activity, Activity::ActiveOverride { .. }));
    assert!(
        matches!(restored.category_source, CategorySource::Annotation { annotation } if annotation.0.to_string().ends_with("000000000001"))
    );
    assert!(
        matches!(restored.category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Entertainment))
    );
    assert_eq!(segments.display(3).activity, Activity::Excluded);
    assert_eq!(
        segments.display(3).category_source,
        CategorySource::Unclassified
    );
    assert_eq!(segments.display(4).activity, Activity::NoForeground);
    let day = DayAggregation::new(
        DayCalendar::new(
            "1970-01-01".parse().expect("date"),
            crate::query::calendar::DayStartOffset::MIDNIGHT,
        )
        .expect("day"),
        &segments,
    );
    assert_eq!(day.metrics.active_ms, 40);
    assert_eq!(day.metrics.distract_ratio, 1.0);
}

#[test]
fn active_override_uses_optional_category_or_rule_and_only_restores_idle() {
    use quartz_protocol::{AnnotationKind, BuiltinCategory, CategoryRef};
    let segments = derive_classified(
        vec![
            clock(1, 0, 0, 80),
            classified_window(1, 0, 80, "code.exe"),
            InputRow::Availability {
                span: span(2, 0, 80),
                state: Availability::Idle,
            },
            InputRow::Availability {
                span: span(3, 60, 80),
                state: Availability::Stopped(StopReason::Locked),
            },
            annotation(1, AnnotationKind::ActiveOverride, 0, 20, None),
            annotation(2, AnnotationKind::ActiveOverride, 20, 80, Some(2)),
        ],
        QueryRange { start: 0, end: 80 },
        classifier(&[]),
    );
    assert!(
        matches!(segments.display(0).category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Development))
    );
    assert!(
        matches!(segments.display(1).category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Learning))
    );
    assert!(matches!(
        segments.display(2).activity,
        Activity::Stopped { .. }
    ));
    let day = DayAggregation::new(
        DayCalendar::new(
            "1970-01-01".parse().expect("date"),
            crate::query::calendar::DayStartOffset::MIDNIGHT,
        )
        .expect("day"),
        &segments,
    );
    assert_eq!(day.metrics.active_ms, 60);
    assert_eq!(day.metrics.longest_focus_ms, 60);
}

#[test]
fn offline_activity_fills_idle_stopped_gap_no_foreground_and_core_offline_as_a_union() {
    use quartz_protocol::{AnnotationKind, CategoryRef};
    let segments = derive_classified(
        vec![
            clock(1, 0, 0, 100),
            classified_window(1, 0, 40, "code.exe"),
            InputRow::Availability {
                span: span(2, 20, 40),
                state: Availability::Idle,
            },
            InputRow::Availability {
                span: span(3, 40, 60),
                state: Availability::Stopped(StopReason::Locked),
            },
            InputRow::Gap {
                span: span(4, 60, 80),
                reason: quartz_domain::capture::GapReason::DatabaseError,
            },
            annotation(1, AnnotationKind::OfflineActivity, 10, 120, Some(2)),
            annotation(2, AnnotationKind::ActiveOverride, 30, 40, None),
            annotation(3, AnnotationKind::OfflineActivity, 120, 130, None),
        ],
        QueryRange { start: 0, end: 140 },
        classifier(&[]),
    );
    for segment in &segments.segments {
        let display = segments.project(segment);
        match display.start_utc_ms {
            0 | 10 => assert_eq!(display.activity, Activity::Active),
            30 => assert!(matches!(display.activity, Activity::ActiveOverride { .. })),
            130 => assert!(
                matches!(display.activity, Activity::Stopped { reasons } if reasons == vec![QueryStopReason::CoreOffline])
            ),
            _ => {
                assert!(matches!(display.activity, Activity::OfflineActivity { .. }));
                assert!(display.app.is_none());
                assert!(display.title.is_none());
            }
        }
    }
    assert_eq!(
        segments
            .segments
            .iter()
            .filter(|segment| matches!(segment.activity, pipeline::Activity::OfflineActivity(_)))
            .map(|segment| segment.span.duration_ms)
            .sum::<i64>(),
        100
    );
    let last_activity = segments
        .segments
        .iter()
        .find(|segment| segment.span.start_utc_ms == 120)
        .expect("unclassified offline");
    assert_eq!(
        segments.project(last_activity).category,
        CategoryRef::Unclassified
    );
    let day = DayAggregation::new(
        DayCalendar::new(
            "1970-01-01".parse().expect("date"),
            crate::query::calendar::DayStartOffset::MIDNIGHT,
        )
        .expect("day"),
        &segments,
    );
    assert_eq!(day.metrics.active_ms, 130);
    assert_eq!(day.metrics.unclassified_ms, 10);
    assert_eq!(day.metrics.longest_focus_ms, 100);
    assert_eq!(day.ranking[0].active_ms, 30);
    assert_eq!(day.metrics.switches_per_hour, 4.0 * 3_600_000.0 / 130.0);
    assert_eq!(
        day.calendar
            .hours
            .iter()
            .flat_map(|hour| &hour.categories)
            .map(|category| category.duration_ms)
            .sum::<i64>(),
        130
    );
}

#[test]
fn notes_and_dismissals_only_split_boundaries_and_annotation_sources_never_merge() {
    use quartz_protocol::AnnotationKind;
    let rows = vec![
        clock(1, 0, 0, 100),
        classified_window(1, 0, 100, "code.exe"),
    ];
    let original = derive_classified(
        rows.clone(),
        QueryRange { start: 0, end: 100 },
        classifier(&[]),
    );
    let mut annotated = rows;
    annotated.extend([
        annotation(1, AnnotationKind::Note, 10, 60, None),
        annotation(2, AnnotationKind::DismissedAway, 40, 80, None),
    ]);
    let segments = derive_classified(
        annotated.clone(),
        QueryRange { start: 0, end: 100 },
        classifier(&[]),
    );
    let calendar = DayCalendar::new(
        "1970-01-01".parse().expect("date"),
        crate::query::calendar::DayStartOffset::MIDNIGHT,
    )
    .expect("day");
    assert_eq!(
        DayAggregation::new(calendar.clone(), &original).metrics,
        DayAggregation::new(calendar, &segments).metrics
    );
    assert!(
        segments
            .segments
            .iter()
            .all(|segment| segments.project(segment).category == original.display(0).category)
    );
    annotated.extend([
        annotation(3, AnnotationKind::CategoryOverride, 0, 50, Some(2)),
        annotation(4, AnnotationKind::CategoryOverride, 50, 100, Some(2)),
    ]);
    let segments = derive_classified(
        annotated,
        QueryRange { start: 0, end: 100 },
        classifier(&[]),
    );
    let mut projection = TimelineProjection::default();
    for segment in &segments.segments {
        projection.push(
            segment,
            &segments.facts,
            quartz_protocol::TimelineDetail::Category,
            1000,
        );
    }
    let projected = projection.into_segments();
    assert_eq!(projected.len(), 2);
    assert_ne!(projected[0].category_source, projected[1].category_source);
}

#[test]
fn pending_away_uses_idle_availability_even_without_a_window_and_subtracts_stops() {
    const MINUTE_MS: i64 = 60_000;
    let segments = derive(
        vec![
            clock(1, 0, 0, 90 * MINUTE_MS),
            InputRow::Availability {
                span: span(1, 0, 90 * MINUTE_MS),
                state: Availability::Idle,
            },
            InputRow::Availability {
                span: span(2, 30 * MINUTE_MS, 60 * MINUTE_MS),
                state: Availability::Stopped(StopReason::Locked),
            },
            InputRow::Gap {
                span: span(3, 10 * MINUTE_MS, 20 * MINUTE_MS),
                reason: quartz_domain::capture::GapReason::DatabaseError,
            },
        ],
        QueryRange {
            start: 0,
            end: 90 * MINUTE_MS,
        },
    );
    let mut pending = pending_away::PendingAwayAccumulator::default();
    for segment in &segments.segments {
        pending.push(segment);
    }
    assert_eq!(
        pending.finish(),
        vec![
            quartz_protocol::PendingAway {
                start_utc_ms: 0,
                end_utc_ms: 30 * MINUTE_MS,
                duration_ms: 30 * MINUTE_MS
            },
            quartz_protocol::PendingAway {
                start_utc_ms: 60 * MINUTE_MS,
                end_utc_ms: 90 * MINUTE_MS,
                duration_ms: 30 * MINUTE_MS
            },
        ]
    );
}

#[test]
fn segment_details_keep_all_activity_states_without_display_merging() {
    use quartz_protocol::AnnotationKind;
    let mut excluded = window(2, 70, 80);
    if let InputRow::Foreground(row) = &mut excluded {
        edit_foreground(row, CaptureLevel::Excluded, |_| {});
    }
    let derived = derive(
        vec![
            clock(1, 0, 0, 100),
            window(1, 0, 70),
            excluded,
            InputRow::Foreground(Foreground::new(
                span(3, 80, 100),
                ForegroundKind::NoForeground,
                CaptureLevel::Full,
                Default::default(),
            )),
            InputRow::Availability {
                span: span(4, 10, 40),
                state: Availability::Idle,
            },
            InputRow::Availability {
                span: span(5, 40, 60),
                state: Availability::Stopped(StopReason::Locked),
            },
            annotation(1, AnnotationKind::ActiveOverride, 20, 30, None),
            annotation(2, AnnotationKind::OfflineActivity, 50, 60, None),
        ],
        QueryRange { start: 0, end: 100 },
    );
    let request = QuerySegmentDetailRequest {
        start_utc_ms: 0,
        end_utc_ms: 100,
        app: None,
        window_instance: None,
        cursor: None,
    };
    let mut projection = details::SegmentDetailProjection::default();
    for segment in &derived.segments {
        projection.push(segment, &derived.facts, &request);
    }
    let intervals = projection.finish();
    assert_eq!(
        intervals
            .iter()
            .map(|interval| &interval.activity)
            .collect::<Vec<_>>(),
        vec![
            &Activity::Active,
            &Activity::Idle,
            &Activity::ActiveOverride {
                annotation: quartz_protocol::AnnotationId(
                    "abcdef00-0000-4000-8000-000000000001"
                        .parse()
                        .expect("UUID")
                )
            },
            &Activity::Idle,
            &Activity::Stopped {
                reasons: vec![QueryStopReason::Locked]
            },
            &Activity::OfflineActivity {
                annotation: quartz_protocol::AnnotationId(
                    "abcdef00-0000-4000-8000-000000000002"
                        .parse()
                        .expect("UUID")
                )
            },
            &Activity::Active,
            &Activity::Excluded,
            &Activity::NoForeground,
        ]
    );
}
#[test]
fn entity_details_use_seven_offset_days_across_dst_and_rank_titles() {
    let date: quartz_protocol::LocalDate = "2026-03-10".parse().expect("ending date");
    let offset = calendar::DayStartOffset::try_from(240).expect("04:00 offset");
    let zone = jiff::tz::TimeZone::get("America/Los_Angeles").expect("zone");
    let calendar =
        details::EntityCalendar::in_zone(date, offset, zone.clone()).expect("seven days");
    let range = calendar.range;
    let first: quartz_protocol::LocalDate = "2026-03-04".parse().expect("first date");
    let mut rows = vec![clock(1, range.start, range.start, range.end - range.start)];
    let mut day = first;
    let mut expected = Vec::new();
    for index in 0..quartz_protocol::ENTITY_DETAIL_DAYS {
        let boundary = DayCalendar::in_zone(day, offset, zone.clone()).expect("day");
        let mut row = window(1, boundary.range.start, boundary.range.end);
        if let InputRow::Foreground(foreground) = &mut row {
            edit_foreground(foreground, CaptureLevel::Full, |content| {
                content.title = Some(format!("title-{index}"));
                content.title_id = Some(quartz_domain::facts::TitleId(
                    i64::try_from(index).expect("title id"),
                ));
            });
        }
        rows.push(row);
        expected.push(quartz_protocol::EntityDailyDuration {
            local_date: day,
            active_ms: boundary.range.end - boundary.range.start,
        });
        day = day.tomorrow().expect("next date");
    }
    let derived = derive(rows, range);
    let mut accumulator = details::EntityDetailAccumulator::new(
        details::EntitySelection::App {
            id: quartz_protocol::ApplicationId(1),
        },
        calendar,
    );
    for segment in &derived.segments {
        accumulator.push(segment, &derived.facts);
    }
    let response = accumulator.finish(QueryVersions("test".into()), PagePosition::First);
    let daily = response.daily.expect("first-page daily totals");
    let titles = response.titles.expect("first-page top titles");
    assert_eq!(daily, expected);
    assert_eq!(daily[3].active_ms, 23 * 60 * 60 * 1000);
    assert_eq!(titles.len(), quartz_protocol::ENTITY_DETAIL_TOP_TITLES);
    assert_eq!(
        titles
            .iter()
            .map(|title| title.title.as_str())
            .collect::<Vec<_>>(),
        vec!["title-0", "title-1", "title-2", "title-4", "title-5"]
    );
    assert!(
        titles
            .windows(2)
            .all(|pair| pair[0].duration_ms >= pair[1].duration_ms)
    );
}

#[test]
fn entity_daily_duration_matches_day_queries_at_integer_clipping_boundaries() {
    let date = "2026-03-10".parse().expect("date");
    let zone = jiff::tz::TimeZone::get("America/Los_Angeles").expect("zone");
    let offset = calendar::DayStartOffset::try_from(240).expect("offset");
    let calendar = details::EntityCalendar::in_zone(date, offset, zone.clone()).expect("week");
    let range = calendar.range;
    let first = DayCalendar::in_zone("2026-03-04".parse().expect("date"), offset, zone.clone())
        .expect("first day");
    let boundary = first.range.end;
    let mut foreground = window(1, boundary - 40, boundary + 60);
    if let InputRow::Foreground(row) = &mut foreground {
        row.span.end_mono_ms = row.span.start_mono_ms + 3;
    }
    let rows = vec![
        clock(1, range.start, range.start, range.end - range.start),
        foreground,
        annotation(
            1,
            quartz_protocol::AnnotationKind::Note,
            boundary - 20,
            boundary + 60,
            None,
        ),
    ];
    let derived = derive(rows.clone(), range);
    let mut accumulator = details::EntityDetailAccumulator::new(
        details::EntitySelection::App {
            id: quartz_protocol::ApplicationId(1),
        },
        calendar,
    );
    for segment in &derived.segments {
        accumulator.push(segment, &derived.facts);
    }
    let response = accumulator.finish(QueryVersions("test".into()), PagePosition::First);
    let daily = response.daily.expect("first-page daily totals");
    for daily in &daily {
        let calendar = DayCalendar::in_zone(daily.local_date, offset, zone.clone()).expect("day");
        let segments = derive(rows.clone(), calendar.range);
        let day = DayAggregation::new(calendar, &segments);
        assert_eq!(daily.active_ms, day.metrics.active_ms);
    }
    assert_eq!(daily[0].active_ms, 1);
    assert_eq!(daily[1].active_ms, 2);
}

#[test]
fn extraction_fields_classify_before_slicing_and_respect_missing_titles() {
    const EXTRACT: &str = "format='quartz-rules/1'\n[set]\nkey='user.extract'\nkind='extraction'\nname='Extract'\nparent='builtin.extraction'\n[[rule]]\nid='document'\nlabel='Named fields'\napp=[{field='app.exe_name',op='equals',value='unknown.exe'}]\npattern='^(?P<document>document) - (?P<project>Quartz) - (?P<profile>Work)$'";
    struct FieldCase {
        field: &'static str,
        value: &'static str,
    }
    for case in [
        FieldCase {
            field: "title.normalized",
            value: "document - Quartz - Work",
        },
        FieldCase {
            field: "doc.document",
            value: "document",
        },
        FieldCase {
            field: "doc.project",
            value: "Quartz",
        },
        FieldCase {
            field: "doc.profile",
            value: "Work",
        },
    ] {
        let rule = format!(
            "format='quartz-rules/1'\n[set]\nkey='user.extracted'\nkind='classification'\nname='Extracted fields'\nparent='builtin.classification'\n[[rule]]\nid='read'\nlabel='Read extracted field'\ncategory='builtin:learning'\nwhen=[{{field='{}',op='equals',value='{}'}}]",
            case.field, case.value
        );
        let mut rows = vec![clock(1, 0, 0, 40)];
        for id in 1..=4 {
            let mut input = classified_window(id, (id - 1) * 10, id * 10, "unknown.exe");
            if let InputRow::Foreground(row) = &mut input {
                let level = match id {
                    2 => CaptureLevel::TitleHidden,
                    3 => CaptureLevel::Excluded,
                    _ => CaptureLevel::Full,
                };
                edit_foreground(row, level, |content| {
                    content.title = (id != 4).then(|| "document - Quartz - Work".into());
                    content.title_id = (id != 4).then_some(quartz_domain::facts::TitleId(1));
                });
            }
            rows.push(input);
        }
        let segments = derive_classified(
            rows.clone(),
            QueryRange { start: 0, end: 40 },
            classifier(&[EXTRACT, &rule]),
        );
        assert!(
            matches!(
                segments.display(0).category,
                quartz_protocol::CategoryRef::Classified { .. }
            ),
            "{}",
            case.field
        );
        for index in 1..4 {
            assert_eq!(
                segments.display(index).category,
                quartz_protocol::CategoryRef::Unclassified,
                "{}",
                case.field
            );
        }
        let negated = rule.replace("op='equals'", "negate=true,op='equals'");
        let segments = derive_classified(
            rows,
            QueryRange { start: 0, end: 40 },
            classifier(&[EXTRACT, &negated]),
        );
        assert_eq!(
            segments.display(0).category,
            quartz_protocol::CategoryRef::Unclassified
        );
        for index in [1, 3] {
            assert!(
                matches!(
                    segments.display(index).category,
                    quartz_protocol::CategoryRef::Classified { .. }
                ),
                "{}",
                case.field
            );
        }
        assert_eq!(
            segments.display(2).category,
            quartz_protocol::CategoryRef::Unclassified
        );
        assert_eq!(
            segments
                .segments
                .iter()
                .map(|segment| segments.project(segment).duration_ms)
                .sum::<i64>(),
            40
        );
    }
}

#[test]
fn builtin_manifest_rules_are_narrow_and_user_overrides_win() {
    const OVERRIDE: &str = "format='quartz-rules/1'\n[set]\nkey='user.manifest'\nkind='classification'\nname='Manifest override'\nparent='builtin.classification'\n[[override]]\ntarget='builtin.classification/notepad-cargo-manifest'\nlabel='Read Rust manifests'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='notepad.exe'},{field='doc.document',op='equals',value='Cargo.toml'}]";
    struct ManifestCase {
        title: &'static str,
        exe: &'static str,
        expected: Option<quartz_protocol::BuiltinCategory>,
    }
    for case in [
        ManifestCase {
            title: "Cargo.toml - Notepad",
            exe: "notepad.exe",
            expected: Some(quartz_protocol::BuiltinCategory::Development),
        },
        ManifestCase {
            title: "pyproject.toml - Notepad",
            exe: "notepad.exe",
            expected: Some(quartz_protocol::BuiltinCategory::Development),
        },
        ManifestCase {
            title: "notes.txt - Notepad",
            exe: "notepad.exe",
            expected: None,
        },
        ManifestCase {
            title: "My Cargo.toml notes - Notepad",
            exe: "notepad.exe",
            expected: None,
        },
        ManifestCase {
            title: "Cargo.toml - Notepad",
            exe: "unknown.exe",
            expected: None,
        },
    ] {
        let mut input = classified_window(1, 0, 10, case.exe);
        if let InputRow::Foreground(row) = &mut input {
            edit_foreground(row, CaptureLevel::Full, |content| {
                std::sync::Arc::make_mut(content.app.as_mut().expect("app")).display_name =
                    "Notepad".into();
                content.title = Some(case.title.into());
            });
        }
        let result = derive_classified(
            vec![clock(1, 0, 0, 10), input.clone()],
            QueryRange { start: 0, end: 10 },
            classifier(&[]),
        );
        let category = match result.display(0).category {
            quartz_protocol::CategoryRef::Unclassified => None,
            quartz_protocol::CategoryRef::Classified { category } => category.builtin_key,
        };
        assert_eq!(category, case.expected);
        if case.exe == "notepad.exe" && case.title.starts_with("Cargo.toml") {
            let result = derive_classified(
                vec![clock(1, 0, 0, 10), input],
                QueryRange { start: 0, end: 10 },
                classifier(&[OVERRIDE]),
            );
            assert!(
                matches!(&result.display(0).category_source, quartz_protocol::CategorySource::Rule {rule} if rule.as_str() == "user.manifest/notepad-cargo-manifest@1")
            );
            assert!(
                matches!(&result.display(0).category, quartz_protocol::CategoryRef::Classified {category} if category.builtin_key == Some(quartz_protocol::BuiltinCategory::Learning))
            );
        }
    }
}

#[test]
fn document_app_merge_ignores_dirty_churn_and_preserves_raw_display_and_boundaries() {
    use quartz_domain::facts::{TitleId, WindowInstanceId};
    use quartz_protocol::TimelineDetail;
    let mut first = classified_window(1, 0, 10, "code.exe");
    let mut second = classified_window(2, 10, 20, "code.exe");
    for input in [&mut first, &mut second] {
        let InputRow::Foreground(row) = input else {
            panic!("foreground")
        };
        let title = if row.span.id == FactId(1) {
            "main.rs - Quartz - Visual Studio Code"
        } else {
            "● main.rs - Quartz - Visual Studio Code"
        };
        let id = row.span.id.0;
        edit_foreground(row, CaptureLevel::Full, |content| {
            content.window_instance = Some(WindowInstanceId(1));
            content.title_id = Some(TitleId(id));
            content.title = Some(title.into());
            std::sync::Arc::make_mut(content.app.as_mut().expect("app")).id = ApplicationId(1);
        });
    }
    let mut pipeline = Pipeline::new(QueryRange { start: 0, end: 20 }, classifier(&[]));
    for row in [clock(1, 0, 0, 20), first, second] {
        pipeline.ingest(row).expect("row");
    }
    pipeline.finish_input();
    while !pipeline.advance() {}
    let derived = pipeline.into_segments();
    let mut app = TimelineProjection::default();
    let mut raw = TimelineProjection::default();
    let request = QuerySegmentDetailRequest {
        start_utc_ms: 0,
        end_utc_ms: 20,
        app: None,
        window_instance: None,
        cursor: None,
    };
    let mut detail = details::SegmentDetailProjection::default();
    for segment in &derived.segments {
        app.push(segment, &derived.facts, TimelineDetail::App, 1);
        raw.push(segment, &derived.facts, TimelineDetail::Raw, 100);
        detail.push(segment, &derived.facts, &request);
    }
    let app = app.into_segments();
    let raw = raw.into_segments();
    assert_eq!(app.len(), 1, "document merge precedes pixel merge");
    assert_eq!(app[0].duration_ms, 20);
    let extraction = app[0].extraction.as_ref().expect("visible extraction");
    assert_eq!(extraction.document.as_deref(), Some("main.rs"));
    assert_eq!(extraction.project.as_deref(), Some("Quartz"));
    assert_eq!(extraction.dirty, Some(true));
    assert!(app[0].title.is_none());
    assert_eq!(raw.len(), 2);
    assert_eq!(
        raw,
        detail.finish(),
        "raw detail preserves each original fact and extraction"
    );
    assert_eq!(
        raw[0].extraction.as_ref().expect("first").dirty,
        Some(false)
    );
    assert_eq!(
        raw[1].title.as_deref(),
        Some("● main.rs - Quartz - Visual Studio Code")
    );

    let mut boundaries = derived.clone();
    boundaries.segments[1].clock_segment_id = Some(ClockSegmentId(2));
    let mut app = TimelineProjection::default();
    for segment in &boundaries.segments {
        app.push(segment, &boundaries.facts, TimelineDetail::App, 100);
    }
    assert_eq!(
        app.into_segments().len(),
        2,
        "document merge respects clock segments"
    );
}

#[test]
fn extraction_projection_hides_private_fields_and_preserves_missing_and_empty() {
    use quartz_protocol::{ExtractionStatus, TimelineDetail, TitleExtraction};
    for level in [
        CaptureLevel::Full,
        CaptureLevel::TitleHidden,
        CaptureLevel::Excluded,
    ] {
        for title in [None, Some("")] {
            let mut row = window(1, 0, 10);
            let InputRow::Foreground(foreground) = &mut row else {
                panic!("foreground")
            };
            edit_foreground(foreground, level, |content| {
                content.title = title.map(str::to_owned)
            });
            let derived = derive(
                vec![clock(1, 0, 0, 10), row],
                QueryRange { start: 0, end: 10 },
            );
            let raw = TimelineProjection::project(
                &derived.segments[0],
                &derived.facts,
                TimelineDetail::Raw,
            );
            let extraction = raw.extraction.expect("raw extraction shape");
            assert!(raw.document_ref.is_none());
            if level == CaptureLevel::Full {
                assert_eq!(
                    extraction.status,
                    Some(if title.is_none() {
                        ExtractionStatus::Unknown
                    } else {
                        ExtractionStatus::Empty
                    })
                );
                assert!(extraction.document.is_none());
            } else {
                assert_eq!(extraction, TitleExtraction::default());
            }
            assert!(
                TimelineProjection::project(
                    &derived.segments[0],
                    &derived.facts,
                    TimelineDetail::Category
                )
                .extraction
                .is_none()
            );
        }
    }
}

#[test]
fn app_merge_separates_documents_projects_profiles_case_and_unknown_titles() {
    use quartz_domain::facts::{TitleId, WindowInstanceId};
    use quartz_protocol::TimelineDetail;
    struct Case {
        first: &'static str,
        second: &'static str,
        exe: &'static str,
        second_app: i64,
        expected: usize,
    }
    for case in [
        Case {
            first: "main.rs - Quartz - Visual Studio Code",
            second: "main.rs - Other - Visual Studio Code",
            exe: "code.exe",
            second_app: 1,
            expected: 2,
        },
        Case {
            first: "main.rs - Quartz - Visual Studio Code",
            second: "Main.rs - Quartz - Visual Studio Code",
            exe: "code.exe",
            second_app: 1,
            expected: 2,
        },
        Case {
            first: "café.rs - Quartz - Visual Studio Code",
            second: "cafe\u{301}.rs - Quartz - Visual Studio Code",
            exe: "code.exe",
            second_app: 1,
            expected: 1,
        },
        Case {
            first: "main.rs - Quartz - Visual Studio Code",
            second: "main.rs - Quartz - Visual Studio Code",
            exe: "code.exe",
            second_app: 2,
            expected: 2,
        },
        Case {
            first: "Page - Google Chrome - Personal",
            second: "Page - Google Chrome - Work",
            exe: "chrome.exe",
            second_app: 1,
            expected: 2,
        },
        Case {
            first: "Quartz (Workspace) - Visual Studio Code",
            second: "Other (Workspace) - Visual Studio Code",
            exe: "code.exe",
            second_app: 1,
            expected: 2,
        },
        Case {
            first: "Quartz (Workspace) - Visual Studio Code",
            second: "Quartz (Workspace) - Visual Studio Code",
            exe: "code.exe",
            second_app: 1,
            expected: 1,
        },
    ] {
        let mut first = classified_window(1, 0, 10, case.exe);
        let mut second = classified_window(2, 10, 20, case.exe);
        let InputRow::Foreground(row) = &mut first else {
            panic!("foreground")
        };
        edit_foreground(row, CaptureLevel::Full, |content| {
            content.title = Some(case.first.into())
        });
        let InputRow::Foreground(row) = &mut second else {
            panic!("foreground")
        };
        edit_foreground(row, CaptureLevel::Full, |content| {
            content.title = Some(case.second.into());
            content.title_id = Some(TitleId(2));
            content.window_instance = Some(WindowInstanceId(1));
            std::sync::Arc::make_mut(content.app.as_mut().expect("app")).id =
                ApplicationId(case.second_app);
        });
        let derived = derive_classified(
            vec![clock(1, 0, 0, 20), first, second],
            QueryRange { start: 0, end: 20 },
            classifier(&[]),
        );
        let mut projection = TimelineProjection::default();
        for segment in &derived.segments {
            projection.push(segment, &derived.facts, TimelineDetail::App, 100);
        }
        assert_eq!(
            projection.into_segments().len(),
            case.expected,
            "{} followed by {}",
            case.first,
            case.second
        );
    }
}

#[test]
fn document_merges_keep_category_source_idle_gap_and_utc_boundaries() {
    use quartz_protocol::{AnnotationKind, TimelineDetail};
    for boundary in [
        InputRow::Availability {
            span: span(2, 10, 20),
            state: Availability::Idle,
        },
        InputRow::Gap {
            span: span(2, 10, 20),
            reason: quartz_domain::capture::GapReason::EventOverload,
        },
        annotation(1, AnnotationKind::CategoryOverride, 10, 20, Some(1)),
        annotation(1, AnnotationKind::CategoryOverride, 10, 20, Some(2)),
    ] {
        let derived = derive_classified(
            vec![
                clock(1, 0, 0, 30),
                classified_window(1, 0, 30, "code.exe"),
                boundary,
            ],
            QueryRange { start: 0, end: 30 },
            classifier(&[]),
        );
        for detail in [TimelineDetail::App, TimelineDetail::Raw] {
            let mut projection = TimelineProjection::default();
            for segment in &derived.segments {
                projection.push(segment, &derived.facts, detail, 100);
            }
            assert_eq!(
                projection.into_segments().len(),
                3,
                "boundary survives {detail:?}"
            );
        }
    }
    let mut derived = derive(
        vec![
            clock(1, 0, 0, 30),
            window(1, 0, 30),
            InputRow::Availability {
                span: span(2, 10, 20),
                state: Availability::Idle,
            },
        ],
        QueryRange { start: 0, end: 30 },
    );
    derived.segments[1].activity = pipeline::Activity::Active;
    derived.segments[1].span.start_utc_ms += 1;
    let mut projection = TimelineProjection::default();
    for segment in &derived.segments {
        projection.push(segment, &derived.facts, TimelineDetail::App, 1);
    }
    assert_eq!(
        projection.into_segments().len(),
        2,
        "UTC gap survives document merge"
    );
}

fn unclassified_request(
    group_by: quartz_protocol::UnclassifiedGroupBy,
) -> quartz_protocol::QueryUnclassifiedRequest {
    quartz_protocol::QueryUnclassifiedRequest {
        start_date: "1970-01-01".parse().expect("date"),
        end_date: "1970-01-02".parse().expect("date"),
        group_by,
        search: None,
        cursor: None,
    }
}
fn grouped_unclassified(
    derived: &pipeline::DerivedSegments,
    request: &quartz_protocol::QueryUnclassifiedRequest,
) -> Vec<quartz_protocol::UnclassifiedItem> {
    let mut accumulator = super::unclassified::UnclassifiedAccumulator::new(request);
    for segment in &derived.segments {
        accumulator.push(segment, &derived.facts);
    }
    accumulator
        .finish()
        .into_iter()
        .map(|group| {
            group
                .into_item(
                    quartz_protocol::ReferenceRange {
                        start_utc_ms: 0,
                        end_utc_ms: 1000,
                    },
                    &quartz_protocol::QueryVersions("test".into()),
                )
                .expect("group projection")
        })
        .collect()
}
#[test]
fn unclassified_orders_active_durations_and_identity_ties_and_caps_chronological_samples() {
    use quartz_protocol::{UnclassifiedGroupBy, WorkEntity};
    let mut rows = vec![clock(1, 0, 0, 1000)];
    for index in (0..8).rev() {
        let mut input = classified_window(index + 1, index * 10, index * 10 + 10, "msedge.exe");
        if let InputRow::Foreground(row) = &mut input {
            edit_foreground(row, CaptureLevel::Full, |content| {
                std::sync::Arc::make_mut(content.app.as_mut().expect("app")).id = ApplicationId(2);
            });
        }
        rows.push(input);
    }
    rows.push(classified_window(3, 100, 120, "msedge.exe"));
    rows.push(classified_window(1, 130, 150, "msedge.exe"));
    let derived = derive_classified(
        rows,
        QueryRange {
            start: 0,
            end: 1000,
        },
        classifier(&[]),
    );
    let items = grouped_unclassified(&derived, &unclassified_request(UnclassifiedGroupBy::App));
    assert_eq!(
        items.iter().map(|item| item.active_ms).collect::<Vec<_>>(),
        vec![80, 20, 20]
    );
    let ids: Vec<_> = items
        .iter()
        .map(|item| match item.entity {
            WorkEntity::App { id, .. } => id.0,
            _ => panic!("app group"),
        })
        .collect();
    assert_eq!(ids, vec![2, 1, 3]);
    assert_eq!(
        items[0].sample_ranges.len(),
        quartz_protocol::UNCLASSIFIED_MAX_SAMPLES
    );
    assert_eq!(
        items[0]
            .sample_ranges
            .iter()
            .map(|sample| sample.start_utc_ms)
            .collect::<Vec<_>>(),
        vec![0, 10, 20, 30, 40]
    );
    let documents = grouped_unclassified(
        &derived,
        &unclassified_request(UnclassifiedGroupBy::Document),
    );
    assert_eq!(
        documents.iter().map(|item| item.active_ms).sum::<i64>(),
        120
    );
}
#[test]
fn unclassified_search_folds_visible_names_documents_projects_and_profiles() {
    use quartz_protocol::UnclassifiedGroupBy;
    const EXTRACTOR: &str = "format='quartz-rules/1'\n[set]\nkey='user.search'\nkind='extraction'\nname='Search fields'\n[[rule]]\nid='parts'\nlabel='Parts'\napp=[{field='app.exe_name',op='equals',value='msedge.exe'}]\npattern='\\A(?P<document>[^|]+)\\|(?P<project>[^|]+)\\|(?P<profile>[^|]+)\\z'";
    let mut input = classified_window(1, 0, 100, "msedge.exe");
    if let InputRow::Foreground(row) = &mut input {
        edit_foreground(row, CaptureLevel::Full, |content| {
            content.title = Some("Ｓｔｒａße.rs|ＰＲＯＪＥＣＴ|ＷＯＲＫ".into());
            std::sync::Arc::make_mut(content.app.as_mut().expect("app")).display_name =
                "ＥＤＩＴＯＲ".into();
        });
    }
    let derived = derive_classified(
        vec![clock(1, 0, 0, 100), input],
        QueryRange { start: 0, end: 100 },
        classifier(&[EXTRACTOR]),
    );
    let mut request = unclassified_request(UnclassifiedGroupBy::Document);
    for search in ["STRASSE", "project", "work", "editor", "ＭＳＥＤＧＥ", ""] {
        request.search = Some(search.into());
        assert_eq!(
            grouped_unclassified(&derived, &request).len(),
            1,
            "search {search}"
        );
    }
    request.search = Some("missing".into());
    assert!(grouped_unclassified(&derived, &request).is_empty());
    request.group_by = UnclassifiedGroupBy::App;
    request.search = Some("strasse".into());
    assert!(grouped_unclassified(&derived, &request).is_empty());
}
#[test]
fn unclassified_sample_selection_orders_utc_before_clock_identity() {
    use quartz_protocol::UnclassifiedGroupBy;
    let mut later = classified_window(2, 100, 120, "msedge.exe");
    if let InputRow::Foreground(row) = &mut later {
        row.span.segment = ClockSegmentId(2);
        row.span.start_utc_ms = 0;
        row.span.end_utc_ms = 20;
        edit_foreground(row, CaptureLevel::Full, |content| {
            std::sync::Arc::make_mut(content.app.as_mut().expect("app")).id = ApplicationId(1)
        });
    }
    let derived = derive_classified(
        vec![
            clock(1, 0, 0, 100),
            clock(2, 0, 100, 100),
            classified_window(1, 20, 40, "msedge.exe"),
            later,
        ],
        QueryRange { start: 0, end: 100 },
        classifier(&[]),
    );
    let items = grouped_unclassified(&derived, &unclassified_request(UnclassifiedGroupBy::App));
    assert_eq!(items[0].active_ms, 40);
    assert_eq!(
        items[0]
            .sample_ranges
            .iter()
            .map(|sample| sample.start_utc_ms)
            .collect::<Vec<_>>(),
        vec![0, 20]
    );
    assert_eq!(
        items[0].sample_ranges[0].clock_segment_id,
        Some(quartz_protocol::ClockSegmentId(2))
    );
}

#[test]
fn system_timezone_acquisition_failure_is_internal_for_a_valid_day_request() {
    let mut fixture = crate::store::query::test_support::QueryFixture::new(0);
    let request = fixture.request_day();
    let rules = fixture.rules();
    let mut reader = fixture.store.take_query_reader();
    let error = QueryJob::start_reading_zone(&mut reader, request, rules, || {
        Err(jiff::tz::TimeZone::get("Invalid/InjectedFailure").expect_err("injected zone failure"))
    })
    .expect_err("system zone unavailable");
    assert!(matches!(
        error,
        RunQueryError::ReadSystemTimeZoneError { .. }
    ));
    assert_eq!(error.kind(), ErrorKind::Internal);
    fixture
        .store
        .return_query_reader(reader.expect("reader retained"));
}

#[test]
fn timezone_encoding_error_maps_to_internal() {
    let source =
        jiff::tz::TimeZone::get("Invalid/InjectedFailure").expect_err("injected encoding error");
    assert_eq!(
        RunQueryError::EncodeTimeZoneError { source }.kind(),
        ErrorKind::Internal
    );
}

#[test]
fn entity_detail_ignores_rounded_zero_duration_titles() {
    let calendar = details::EntityCalendar::in_zone(
        "2026-03-10".parse().expect("date"),
        calendar::DayStartOffset::MIDNIGHT,
        jiff::tz::TimeZone::UTC,
    )
    .expect("calendar");
    let start = calendar.range.start;
    let mut row = window(1, start, start + 100);
    if let InputRow::Foreground(foreground) = &mut row {
        foreground.span.end_mono_ms = foreground.span.start_mono_ms + 1;
    }
    let derived = derive(
        vec![clock(1, start, start, 100), row],
        QueryRange {
            start,
            end: start + 1,
        },
    );
    assert!(
        derived
            .segments
            .iter()
            .any(|segment| segment.activity.is_active() && segment.span.duration_ms == 0)
    );
    let mut accumulator = details::EntityDetailAccumulator::new(
        details::EntitySelection::App {
            id: quartz_protocol::ApplicationId(1),
        },
        calendar,
    );
    for segment in &derived.segments {
        accumulator.push(segment, &derived.facts);
    }
    let response = accumulator.finish(QueryVersions("test".into()), PagePosition::First);
    assert!(response.titles.expect("titles").is_empty());
    assert!(response.rules.is_empty());
    assert!(response.extraction.is_empty());
}

#[test]
fn rejected_search_identity_is_normalized_once() {
    let derived = derive(
        vec![clock(1, 0, 0, 100), window(1, 0, 100)],
        QueryRange { start: 0, end: 100 },
    );
    let request = quartz_protocol::QueryUnclassifiedRequest {
        start_date: "2025-01-02".parse().expect("start"),
        end_date: "2025-01-03".parse().expect("end"),
        group_by: quartz_protocol::UnclassifiedGroupBy::App,
        search: Some("does not match".into()),
        cursor: None,
    };
    let mut accumulator = UnclassifiedAccumulator::new(&request);
    let segment = derived
        .segments
        .iter()
        .find(|segment| segment.activity.is_active())
        .expect("active");
    accumulator.push(segment, &derived.facts);
    let once = accumulator.search_evaluations.get();
    assert!(once > 0);
    for _ in 0..100 {
        accumulator.push(segment, &derived.facts);
    }
    assert_eq!(accumulator.search_evaluations.get(), once);
    assert!(accumulator.finish().is_empty());
}

#[test]
fn entity_detail_filters_unrelated_activity_before_projection() {
    let calendar = details::EntityCalendar::in_zone(
        "2026-03-10".parse().expect("date"),
        calendar::DayStartOffset::MIDNIGHT,
        jiff::tz::TimeZone::UTC,
    )
    .expect("calendar");
    let start = calendar.range.start;
    let derived = derive(
        vec![clock(1, start, start, 100), window(1, start, start + 100)],
        QueryRange {
            start,
            end: start + 100,
        },
    );
    let mut accumulator = details::EntityDetailAccumulator::new(
        details::EntitySelection::App {
            id: quartz_protocol::ApplicationId(2),
        },
        calendar,
    );
    pipeline::PROJECTED_SEGMENTS.set(0);
    for segment in &derived.segments {
        accumulator.push(segment, &derived.facts);
    }
    assert_eq!(pipeline::PROJECTED_SEGMENTS.get(), 0);
}

#[test]
fn pending_away_builds_no_hour_stacks() {
    use crate::store::query::test_support::{FIXTURE_DATE, QueryFixture};
    let mut fixture = QueryFixture::new(0);
    let request = QueryRequest::PendingAway(quartz_protocol::QueryPendingAwayRequest {
        local_date: FIXTURE_DATE.parse().expect("date"),
        cursor: None,
    });
    calendar::BUILT_HOURS.set(0);
    let response = fixture.run(request).expect("pending away");
    assert!(matches!(response, QueryResponse::PendingAway(_)));
    assert_eq!(calendar::BUILT_HOURS.get(), 0);
}

#[test]
fn range_early_stop_measures_aggregation_and_page_encoding() {
    use crate::store::query::test_support::{FIXTURE_DATE, QueryFixture};
    let mut fixture = QueryFixture::new(900);
    let date: quartz_protocol::LocalDate = FIXTURE_DATE.parse().expect("date");
    let mut job = fixture.start(QueryRequest::Range(quartz_protocol::QueryRangeRequest {
        start_date: date,
        end_date: date.checked_add(jiff::Span::new().days(10)).expect("end"),
        bucket: quartz_protocol::RangeBucket::Day,
        dimensions: vec![
            quartz_protocol::RangeDimension::App,
            quartz_protocol::RangeDimension::Document,
        ],
        cursor: None,
    }));
    loop {
        timing::MEASURED_STAGES.with_borrow_mut(Vec::clear);
        if let Some(response) = job.advance().expect("advance") {
            let QueryResponse::Range(page) = response else {
                panic!("range response")
            };
            assert!(
                page.next_cursor.is_some(),
                "fixture must stop at a full page"
            );
            timing::MEASURED_STAGES.with_borrow(|stages| {
                assert!(
                    stages.ends_with(&[QueryStage::Aggregate, QueryStage::PageEncode]),
                    "final operations must be measured: {stages:?}"
                );
            });
            break;
        }
    }
    fixture
        .store
        .return_query_reader(job.finish().expect("finish"));
}

#[test]
fn document_details_filter_before_projection_with_normalized_optional_fields() {
    use documents::DocumentIdentity;
    const EXTRACTOR: &str = r"format='quartz-rules/1'
[set]
key='user.detail'
kind='extraction'
name='Document detail'
[[rule]]
id='parts'
label='Named fields'
app=[{field='app.exe_name',op='equals',value='unknown.exe'}]
pattern='\A(?P<document>[^|]+)(?:\|(?P<project>[^|]+))?(?:\|(?P<profile>[^|]+))?\z'";
    let date = "2026-03-10".parse().expect("date");
    let zone = jiff::tz::TimeZone::UTC;
    let calendar =
        details::EntityCalendar::in_zone(date, calendar::DayStartOffset::MIDNIGHT, zone.clone())
            .expect("calendar");
    let start = calendar.range.start;
    let mut rows = vec![clock(1, start, start, 30)];
    let mut at = start;
    for (index, title) in ["Cafe\u{301}|Re\u{301}po|Pe\u{301}rson", "Café", "Café|Répo"]
        .into_iter()
        .enumerate()
    {
        let id = i64::try_from(index + 1).expect("fact ID");
        let mut input = classified_window(id, at, at + 10, "unknown.exe");
        if let InputRow::Foreground(row) = &mut input {
            edit_foreground(row, CaptureLevel::Full, |content| {
                content.title = Some(title.into());
                content.title_id = Some(quartz_domain::facts::TitleId(id));
                std::sync::Arc::make_mut(content.app.as_mut().expect("app")).id = ApplicationId(1);
            });
        }
        rows.push(input);
        at += 10;
    }
    let derived = derive_classified(
        rows,
        QueryRange { start, end: at },
        classifier(&[EXTRACTOR]),
    );
    assert_eq!(derived.segments.len(), 3);
    let selected = DocumentIdentity {
        app: quartz_protocol::ApplicationId(1),
        document: "Café".into(),
        project: Some("Répo".into()),
        profile: Some("Pérson".into()),
    };
    let extracted = quartz_domain::rules::Extraction {
        document: Some("Cafe\u{301}".into()),
        project: Some("Re\u{301}po".into()),
        profile: Some("Pe\u{301}rson".into()),
        dirty: false,
        normalized_title: None,
        status: quartz_domain::rules::ExtractionStatus::Extracted,
    };
    assert!(selected.matches_extraction(selected.app, &extracted));
    struct Case {
        identity: DocumentIdentity,
        expected_segments: usize,
    }
    let cases = [
        Case {
            identity: selected.clone(),
            expected_segments: 1,
        },
        Case {
            identity: DocumentIdentity {
                app: quartz_protocol::ApplicationId(2),
                ..selected.clone()
            },
            expected_segments: 0,
        },
        Case {
            identity: DocumentIdentity {
                document: "Other".into(),
                ..selected.clone()
            },
            expected_segments: 0,
        },
        Case {
            identity: DocumentIdentity {
                project: None,
                ..selected.clone()
            },
            expected_segments: 0,
        },
        Case {
            identity: DocumentIdentity {
                profile: None,
                ..selected.clone()
            },
            expected_segments: 1,
        },
        Case {
            identity: DocumentIdentity {
                project: None,
                profile: None,
                ..selected.clone()
            },
            expected_segments: 1,
        },
        Case {
            identity: DocumentIdentity {
                project: Some(String::new()),
                profile: None,
                ..selected
            },
            expected_segments: 0,
        },
    ];
    for case in cases {
        let expected: Vec<_> = derived
            .segments
            .iter()
            .filter(|segment| {
                let projected = TimelineProjection::project(
                    segment,
                    &derived.facts,
                    quartz_protocol::TimelineDetail::Raw,
                );
                segment.activity.is_active()
                    && segment.span.duration_ms > 0
                    && DocumentIdentity::from_segment(&projected).as_ref() == Some(&case.identity)
            })
            .collect();
        assert_eq!(
            expected.len(),
            case.expected_segments,
            "fixture identity {:?}",
            case.identity
        );
        let mut accumulator = details::EntityDetailAccumulator::new(
            details::EntitySelection::Document {
                identity: case.identity.clone(),
            },
            details::EntityCalendar::in_zone(
                date,
                calendar::DayStartOffset::MIDNIGHT,
                zone.clone(),
            )
            .expect("calendar"),
        );
        pipeline::PROJECTED_SEGMENTS.set(0);
        for segment in &derived.segments {
            accumulator.push(segment, &derived.facts);
        }
        assert_eq!(
            pipeline::PROJECTED_SEGMENTS.get(),
            expected.len(),
            "identity {:?}",
            case.identity
        );
        let response = accumulator.finish(QueryVersions("test".into()), PagePosition::First);
        assert_eq!(
            response
                .daily
                .expect("daily")
                .iter()
                .map(|day| day.active_ms)
                .sum::<i64>(),
            expected
                .iter()
                .map(|segment| segment.span.duration_ms)
                .sum::<i64>()
        );
    }
}
