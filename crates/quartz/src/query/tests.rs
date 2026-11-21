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
fn edit_foreground(
    row: &mut Foreground,
    level: CaptureLevel,
    edit: impl FnOnce(&mut quartz_domain::facts::ForegroundContent),
) {
    let mut content = quartz_domain::facts::ForegroundContent {
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
            color: builtin.color(),
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
        matches!(&segments.display(0).category_source, CategorySource::Rule { rule } if rule.as_str() == "builtin.classification/vscode@1")
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
    let error =
        DayCalendar::new("2025-01-02".parse().expect("date"), -1).expect_err("invalid offset");
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
        DayCalendar::new("1970-01-01".parse().expect("date"), 0).expect("day"),
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
        DayCalendar::new("1970-01-01".parse().expect("date"), 0).expect("day"),
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
        DayCalendar::new("1970-01-01".parse().expect("date"), 0).expect("day"),
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
    let calendar = DayCalendar::new("1970-01-01".parse().expect("date"), 0).expect("day");
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
