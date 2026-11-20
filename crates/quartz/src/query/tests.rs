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
    InputRow::Foreground(Foreground {
        span: span(id, start, end),
        kind: ForegroundKind::Window,
        level: CaptureLevel::Full,
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
    })
}
fn derive(rows: Vec<InputRow>, range: QueryRange) -> Vec<pipeline::DerivedSegment> {
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
            .iter()
            .map(|segment| segment.display.duration_ms)
            .sum::<i64>(),
        100
    );
    let idle = segments
        .iter()
        .find(|segment| segment.display.activity == Activity::Idle)
        .expect("idle slice");
    assert_eq!(idle.interval_id, Some(FactId(1)));
    assert_eq!(idle.display.start_utc_ms, 20);
    assert_eq!(idle.display.end_utc_ms, 40);
    assert!(segments.iter().any(|segment| segment.display.activity
        == Activity::Stopped {
            reasons: vec![QueryStopReason::Locked, QueryStopReason::Paused]
        }));
    let calendar = DayCalendar::new("1970-01-01".parse().expect("date"), 0).expect("calendar");
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
            .iter()
            .filter(|segment| segment.display.activity == Activity::Active)
            .map(|segment| segment.display.duration_ms)
            .sum::<i64>(),
        200
    );
    assert!(
        segments
            .iter()
            .all(|segment| segment.display.clock_segment_id.is_some())
    );
}
#[test]
fn ordinary_day_hours_cover_the_logical_range() {
    let day = DayCalendar::new("2025-01-02".parse().expect("date"), 0).expect("day");
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
    let shifted = DayCalendar::new("2025-01-02".parse().expect("date"), 240).expect("shifted day");
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
    for segment in &segments {
        category.push(segment, quartz_protocol::TimelineDetail::Category, 30);
        app.push(segment, quartz_protocol::TimelineDetail::App, 30);
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
fn gaps_excluded_and_no_foreground_preserve_all_state_boundaries() {
    let mut excluded = window(5, 40, 60);
    if let InputRow::Foreground(row) = &mut excluded {
        row.level = CaptureLevel::Excluded;
    }
    let mut no_foreground = window(6, 60, 80);
    if let InputRow::Foreground(row) = &mut no_foreground {
        row.kind = ForegroundKind::NoForeground;
        row.app = None;
        row.title = None;
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
    assert_eq!(segments.len(), 5);
    assert_eq!(segments[0].display.start_utc_ms, 10);
    assert!(matches!(segments[1].display.activity, Activity::Gap { .. }));
    assert_eq!(segments[2].display.activity, Activity::Excluded);
    assert!(segments[2].display.app.is_none());
    assert!(segments[2].display.title.is_none());
    assert_eq!(segments[3].display.activity, Activity::NoForeground);
    assert_eq!(segments[4].display.end_utc_ms, 90);
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
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].display.start_utc_ms, 10);
    assert_eq!(segments[0].display.end_utc_ms, 90);
    assert_eq!(segments[0].display.duration_ms, 0);
    assert_eq!(
        segments[0].display.activity,
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
    for segment in &segments {
        projection.push(segment, quartz_protocol::TimelineDetail::Category, 100);
    }
    assert_eq!(projection.into_segments().len(), 3);
    segments[1].display.activity = Activity::Active;
    segments[1].display.start_utc_ms += 1;
    segments[2].display.clock_segment_id = Some(quartz_protocol::ClockSegmentId(2));
    let mut projection = TimelineProjection::default();
    for segment in &segments {
        projection.push(segment, quartz_protocol::TimelineDetail::Category, 100);
    }
    assert_eq!(projection.into_segments().len(), 3);
}

#[test]
fn newly_opened_zero_length_clock_and_foreground_do_not_fail_the_query() {
    let segments = derive(
        vec![clock(1, 10, 10, 0), window(1, 10, 10)],
        QueryRange { start: 0, end: 20 },
    );
    assert_eq!(segments.len(), 1);
    assert_eq!(
        segments[0].display.activity,
        Activity::Stopped {
            reasons: vec![QueryStopReason::CoreOffline]
        }
    );
    assert_eq!(segments[0].display.duration_ms, 20);
}

fn categories() -> Vec<quartz_domain::categories::Category> {
    use quartz_domain::categories::{BuiltinCategory, Category, CategoryId};
    BuiltinCategory::ALL
        .into_iter()
        .enumerate()
        .map(|(index, builtin)| Category {
            id: CategoryId(i64::try_from(index + 1).expect("category id")),
            uuid: builtin.uuid(),
            builtin_key: Some(builtin),
            name: None,
            color: builtin.color().into(),
            tendency: builtin.tendency(),
        })
        .collect()
}
fn classifier(overlays: &[&str]) -> classification::Classifier {
    use quartz_domain::rules::{
        self, ParseContext, ReferenceCatalog, RuleOrigin, RuleSet,
        registry::{RegistryCommand, RegistryState, RuleRegistry},
    };
    let mut references = ReferenceCatalog::default();
    for builtin in quartz_domain::categories::BuiltinCategory::ALL {
        references
            .categories
            .insert(format!("builtin:{}", builtin.key()), builtin.uuid().0);
    }
    let mut registry = RuleRegistry::from_state(RegistryState {
        references,
        ..Default::default()
    })
    .expect("registry");
    let set = RuleSet::parse(
        "builtin.toml",
        rules::BUILTIN_CLASSIFICATION,
        ParseContext {
            origin: RuleOrigin::Builtin,
            references: &registry.state().references,
        },
    )
    .expect("builtin");
    let decision = registry
        .decide(RegistryCommand::InstallBuiltin {
            set,
            revision: rules::BUILTIN_CLASSIFICATION_REVISION,
        })
        .into_parts();
    decision.reply.expect("install decision");
    registry
        .committed(decision.outcome.expect("install write"))
        .expect("commit builtin");
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
fn classified_window(id: i64, start: i64, end: i64, exe: &str) -> InputRow {
    let mut input = window(id, start, end);
    if let InputRow::Foreground(row) = &mut input {
        let app = std::sync::Arc::make_mut(row.app.as_mut().expect("app"));
        app.exe_name = Some(exe.into());
        app.identity_key = format!(r"exe:c:\fixture\{exe}");
    }
    input
}
fn derive_classified(
    rows: Vec<InputRow>,
    range: QueryRange,
    classifier: classification::Classifier,
) -> Vec<pipeline::DerivedSegment> {
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
        row.level = CaptureLevel::Excluded;
    }
    if let InputRow::Foreground(row) = &mut rows[4] {
        row.level = CaptureLevel::TitleHidden;
    }
    if let InputRow::Foreground(row) = &mut rows[6] {
        row.level = CaptureLevel::TitleHidden;
    }
    let segments = derive_classified(
        rows.clone(),
        QueryRange { start: 0, end: 60 },
        classifier(&[]),
    );
    assert!(
        matches!(&segments[0].display.category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Development))
    );
    assert!(
        matches!(&segments[0].display.category_source, CategorySource::Rule { rule } if rule.as_str() == "builtin.classification/vscode@1")
    );
    assert_eq!(segments[1].display.category, CategoryRef::Unclassified);
    assert_eq!(segments[2].display.category, CategoryRef::Unclassified);
    assert_eq!(segments[2].display.activity, Activity::Excluded);
    let titled = derive_classified(
        rows,
        QueryRange { start: 0, end: 60 },
        classifier(&[TITLE_RULE]),
    );
    assert_eq!(
        titled[3].display.category,
        CategoryRef::Unclassified,
        "hidden title cannot match"
    );
    assert!(
        matches!(&titled[4].display.category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Learning)),
        "same app/title ids at full capture have a separate cache entry"
    );
    assert!(
        matches!(&titled[5].display.category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Development)),
        "application rules still apply to TitleHidden"
    );
    let mut title_only = window(1, 0, 10);
    if let InputRow::Foreground(row) = &mut title_only {
        row.app = None;
    }
    let title_only = derive_classified(
        vec![clock(1, 0, 0, 10), title_only],
        QueryRange { start: 0, end: 10 },
        classifier(&[TITLE_RULE]),
    );
    assert!(
        matches!(&title_only[0].display.category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Learning)),
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
        matches!(&child[0].display.category_source, CategorySource::Rule { rule } if rule.as_str() == "user.child/code@1"),
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
        std::sync::Arc::make_mut(row.app.as_mut().expect("application")).id = ApplicationId(1);
        row.title_id = Some(quartz_domain::facts::TitleId(2));
        row.title = Some("source code".into());
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
    assert_eq!(segments.len(), 4);
    for segment in &segments[..3] {
        assert!(matches!(
            &segment.display.category,
            CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Learning)
        ));
        assert!(matches!(
            &segment.display.category_source,
            CategorySource::Rule { rule } if rule.as_str() == "user.titles/read@1"
        ));
    }
    assert!(
        matches!(
            &segments[3].display.category_source,
            CategorySource::Rule { rule } if rule.as_str() == "builtin.classification/vscode@1"
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
    for segment in &split {
        projection.push(segment, quartz_protocol::TimelineDetail::Category, 30);
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
    let mut separated = split;
    separated[1].display.clock_segment_id = Some(quartz_protocol::ClockSegmentId(2));
    let aggregate = DayAggregation::new(test_day_calendar(), &separated);
    assert_eq!(
        aggregate.metrics.longest_focus_ms, 10,
        "clock boundary breaks focus continuity"
    );
}

fn test_day_calendar() -> DayCalendar {
    let mut calendar = DayCalendar::new("1970-01-01".parse().expect("date"), 0).expect("calendar");
    let offset = calendar.range.start;
    calendar.range.start -= offset;
    calendar.range.end -= offset;
    for hour in &mut calendar.hours {
        hour.start_utc_ms -= offset;
        hour.end_utc_ms -= offset;
    }
    calendar
}
