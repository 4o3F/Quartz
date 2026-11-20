use super::pipeline::DayAggregation;
use super::*;
use input::{
    Availability, CaptureLevel, ClockSegment, FactId, FactSpan, Foreground, ForegroundKind,
    InputRow,
};
use quartz_protocol::{Activity, ApplicationId, ApplicationRef, ClockSegmentId, QueryStopReason};

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
        app: Some(ApplicationRef {
            id: ApplicationId(id),
            exe_name: Some("editor.exe".into()),
            display_name: "Editor".into(),
            aumid: None,
        }),
        title: Some("document".into()),
    })
}
fn derive(rows: Vec<InputRow>, range: QueryRange) -> Vec<pipeline::DerivedSegment> {
    let mut pipeline = Pipeline::new(range);
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
            state: Availability::Stopped(QueryStopReason::Locked),
        },
        InputRow::Availability {
            span: span(4, 60, 90),
            state: Availability::Stopped(QueryStopReason::Paused),
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
            reason: quartz_protocol::QueryGapReason::DatabaseError,
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
            state: Availability::Stopped(QueryStopReason::Suspended),
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
    segments[2].display.clock_segment_id = Some(ClockSegmentId(2));
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
