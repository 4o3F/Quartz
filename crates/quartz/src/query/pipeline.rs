//! Pure, resumable boundary sweep and day/timeline aggregation.

use super::{
    calendar::DayCalendar,
    input::{
        Availability, CaptureLevel, ClockSegment, FactId, ForegroundKind, InputRow, QueryRange,
    },
};
use quartz_protocol::{
    Activity, ApplicationId, ApplicationRank, CategoryRef, CategorySource, DayMetrics,
    QueryStopReason, TimelineDetail, TimelineSegment,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Bounds each compute turn independently of the size of the query range.
pub(crate) use crate::store::query::READ_BATCH_ROWS as QUERY_BATCH_ROWS;
use quartz_domain::facts::ClockSegmentId;
const HOUR_MS: f64 = 3_600_000.0;
#[derive(Debug, snafu::Snafu)]
pub(crate) enum DeriveSegmentsError {
    #[snafu(display("capture row has no enclosing clock segment"))]
    MissingClock,
    #[snafu(display("capture row has invalid segment-local boundaries"))]
    InvalidSpan,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ClockOrder {
    start: i64,
    id: ClockSegmentId,
}

#[derive(Debug, Clone)]
pub(crate) struct DerivedSegment {
    pub display: TimelineSegment,
    pub interval_id: Option<FactId>,
    pub capture_level: Option<CaptureLevel>,
}
#[derive(Debug)]
enum BoundaryEdge {
    Start,
    End,
}
#[derive(Debug)]
struct Boundary {
    at: i64,
    entering: Vec<usize>,
    leaving: Vec<usize>,
}
#[derive(Debug)]
struct Sweep {
    clock: ClockSegment,
    boundaries: BTreeMap<i64, Boundary>,
    active: BTreeSet<usize>,
    previous: Option<i64>,
}
impl Sweep {
    fn insert(&mut self, at: i64, row: usize, edge: BoundaryEdge) {
        let boundary = self.boundaries.entry(at).or_insert_with(|| Boundary {
            at,
            entering: Vec::new(),
            leaving: Vec::new(),
        });
        match edge {
            BoundaryEdge::Start => boundary.entering.push(row),
            BoundaryEdge::End => boundary.leaving.push(row),
        }
    }
}
#[derive(Debug)]
pub(crate) struct Pipeline {
    range: QueryRange,
    rows: Vec<InputRow>,
    clocks: BTreeMap<ClockSegmentId, Sweep>,
    clock_order: BTreeSet<ClockOrder>,
    segments: Vec<DerivedSegment>,
    coverage: BTreeMap<i64, i64>,
    offline: VecDeque<QueryRange>,
}
impl Pipeline {
    pub(crate) fn new(range: QueryRange) -> Self {
        Self {
            range,
            rows: Vec::new(),
            clocks: BTreeMap::new(),
            clock_order: BTreeSet::new(),
            segments: Vec::new(),
            coverage: BTreeMap::new(),
            offline: VecDeque::new(),
        }
    }
    pub(crate) fn ingest(&mut self, row: InputRow) -> Result<(), DeriveSegmentsError> {
        if let InputRow::Clock(clock) = row {
            if clock.end_utc_ms < clock.start_utc_ms || clock.end_mono_ms < clock.start_mono_ms {
                return Err(DeriveSegmentsError::InvalidSpan);
            }
            let start = self.range.start.max(clock.start_utc_ms);
            let end = self.range.end.min(clock.end_utc_ms);
            if end <= start {
                return Ok(());
            }
            let mut boundaries = BTreeMap::new();
            for at in [start, end] {
                boundaries.insert(
                    at,
                    Boundary {
                        at,
                        entering: Vec::new(),
                        leaving: Vec::new(),
                    },
                );
            }
            self.coverage
                .entry(clock.start_utc_ms.max(self.range.start))
                .and_modify(|until| *until = (*until).max(clock.end_utc_ms.min(self.range.end)))
                .or_insert(clock.end_utc_ms.min(self.range.end));
            self.clock_order.insert(ClockOrder {
                start: clock.start_utc_ms,
                id: clock.id,
            });
            self.clocks.insert(
                clock.id,
                Sweep {
                    clock,
                    boundaries,
                    active: BTreeSet::new(),
                    previous: None,
                },
            );
            return Ok(());
        }
        if let Some(span) = row.span() {
            if span.end_mono_ms < span.start_mono_ms || span.end_utc_ms < span.start_utc_ms {
                return Err(DeriveSegmentsError::InvalidSpan);
            }
            if span.end_utc_ms == span.start_utc_ms {
                return Ok(());
            }
            let sweep = self
                .clocks
                .get_mut(&span.segment)
                .ok_or(DeriveSegmentsError::MissingClock)?;
            let start = span
                .start_utc_ms
                .max(sweep.clock.start_utc_ms)
                .max(self.range.start);
            let end = span
                .end_utc_ms
                .min(sweep.clock.end_utc_ms)
                .min(self.range.end);
            if end <= start {
                return Ok(());
            }
            let index = self.rows.len();
            sweep.insert(start, index, BoundaryEdge::Start);
            sweep.insert(end, index, BoundaryEdge::End);
            self.rows.push(row);
        }
        Ok(())
    }
    pub(crate) fn finish_input(&mut self) {
        let mut at = self.range.start;
        for (&start, &end) in &self.coverage {
            if start > at {
                self.offline.push_back(QueryRange {
                    start: at,
                    end: start,
                });
            }
            at = at.max(end);
        }
        if at < self.range.end {
            self.offline.push_back(QueryRange {
                start: at,
                end: self.range.end,
            });
        }
    }
    /// Processes a bounded number of boundaries; returns true after the last turn.
    pub(crate) fn advance(&mut self) -> bool {
        for _ in 0..QUERY_BATCH_ROWS {
            let order = self.clock_order.first().copied();
            if let Some(range) = self
                .offline
                .pop_front_if(|range| order.is_none_or(|clock| range.start < clock.start))
            {
                self.segments.push(DerivedSegment {
                    display: TimelineSegment {
                        start_utc_ms: range.start,
                        end_utc_ms: range.end,
                        duration_ms: range.end - range.start,
                        clock_segment_id: None,
                        activity: Activity::Stopped {
                            reasons: vec![QueryStopReason::CoreOffline],
                        },
                        category: CategoryRef::Unclassified,
                        category_source: CategorySource::Unclassified,
                        app: None,
                        title: None,
                    },
                    interval_id: None,
                    capture_level: None,
                });
                continue;
            }
            let Some(order) = order else {
                return true;
            };
            let id = order.id;
            let sweep = self.clocks.get_mut(&id).expect("selected clock exists");
            let Some(entry) = sweep.boundaries.pop_first() else {
                self.clocks.remove(&id);
                self.clock_order.remove(&order);
                continue;
            };
            let boundary = entry.1;
            if let Some(start) = sweep.previous
                && boundary.at > start
            {
                self.segments
                    .push(Self::derive(&self.rows, sweep, start, boundary.at));
            }
            for index in boundary.leaving {
                sweep.active.remove(&index);
            }
            for index in boundary.entering {
                sweep.active.insert(index);
            }
            sweep.previous = Some(boundary.at);
        }
        false
    }
    fn derive(rows: &[InputRow], sweep: &Sweep, start: i64, end: i64) -> DerivedSegment {
        let mut foreground = None;
        let mut idle = false;
        let mut reasons = BTreeSet::new();
        let mut gap = None;
        let mut stopped_span = None;
        let mut gap_span = None;
        for &index in &sweep.active {
            match &rows[index] {
                InputRow::Foreground(row) => {
                    if foreground
                        .is_none_or(|old: &super::input::Foreground| old.span.id < row.span.id)
                    {
                        foreground = Some(row);
                    }
                }
                InputRow::Availability {
                    state: Availability::Idle,
                    ..
                } => idle = true,
                InputRow::Availability {
                    state: Availability::Stopped(reason),
                    span,
                } => {
                    reasons.insert(match reason {
                        quartz_domain::capture::StopReason::Locked => QueryStopReason::Locked,
                        quartz_domain::capture::StopReason::Disconnected => {
                            QueryStopReason::Disconnected
                        }
                        quartz_domain::capture::StopReason::Suspended => QueryStopReason::Suspended,
                        quartz_domain::capture::StopReason::Paused => QueryStopReason::Paused,
                    });
                    stopped_span = Some(span);
                }
                InputRow::Gap { reason, span } => {
                    gap = Some(*reason);
                    gap_span = Some(span);
                }
                InputRow::Clock(_) => unreachable!("clocks are not fact rows"),
            }
        }
        // TODO(M2-D): Apply annotation category and activity overrides before aggregation.
        let activity = if let Some(reason) = gap {
            Activity::Gap {
                reason: match reason {
                    quartz_domain::capture::GapReason::EventOverload => {
                        quartz_protocol::QueryGapReason::EventOverload
                    }
                    quartz_domain::capture::GapReason::WriteQueueOverload => {
                        quartz_protocol::QueryGapReason::WriteQueueOverload
                    }
                    quartz_domain::capture::GapReason::DatabaseError => {
                        quartz_protocol::QueryGapReason::DatabaseError
                    }
                },
            }
        } else if !reasons.is_empty() {
            Activity::Stopped {
                reasons: reasons.into_iter().collect(),
            }
        } else {
            match foreground {
                Some(row) if row.level == CaptureLevel::Excluded => Activity::Excluded,
                None => Activity::NoForeground,
                Some(row) if matches!(row.kind, ForegroundKind::NoForeground) => {
                    Activity::NoForeground
                }
                Some(_) if idle => Activity::Idle,
                Some(_) => Activity::Active,
            }
        };
        let range = QueryRange { start, end };
        let duration_ms = match gap_span
            .or(stopped_span)
            .or_else(|| foreground.map(|row| &row.span))
        {
            Some(span) => span.duration_within(range),
            None => sweep.clock.duration_within(range),
        };
        let start_utc_ms = start;
        let end_utc_ms = end;
        DerivedSegment {
            display: TimelineSegment {
                start_utc_ms,
                end_utc_ms,
                duration_ms,
                clock_segment_id: Some(quartz_protocol::ClockSegmentId(sweep.clock.id.0)),
                activity,
                category: CategoryRef::Unclassified,
                category_source: CategorySource::Unclassified,
                app: foreground
                    .filter(|row| row.level != CaptureLevel::Excluded)
                    .and_then(|row| row.app.as_ref())
                    .map(|app| quartz_protocol::ApplicationRef {
                        id: ApplicationId(app.id.0),
                        exe_name: app.exe_name.clone(),
                        display_name: app.display_name.clone(),
                        aumid: app.aumid.clone(),
                    }),
                title: foreground
                    .filter(|row| row.level == CaptureLevel::Full)
                    .and_then(|row| row.title.clone()),
            },
            interval_id: foreground.map(|row| row.span.id),
            capture_level: foreground.map(|row| row.level),
        }
    }
    pub(crate) fn into_segments(self) -> Vec<DerivedSegment> {
        self.segments
    }
}
#[derive(Debug)]
pub(crate) struct DayAggregation {
    pub metrics: DayMetrics,
    pub calendar: DayCalendar,
    pub ranking: Vec<ApplicationRank>,
}
#[cfg(test)]
impl DayAggregation {
    pub(crate) fn new(calendar: DayCalendar, segments: &[DerivedSegment]) -> Self {
        let mut builder = DayAccumulator::new(calendar);
        for segment in segments {
            builder.push(segment);
        }
        builder.finish()
    }
}
#[derive(Debug)]
struct PreviousActive {
    segment: Option<quartz_protocol::ClockSegmentId>,
    end: i64,
    app: Option<ApplicationId>,
}
#[derive(Debug)]
pub(crate) struct DayAccumulator {
    calendar: DayCalendar,
    active_ms: i64,
    switches: u32,
    ranking: BTreeMap<ApplicationId, ApplicationRank>,
    previous: Option<PreviousActive>,
}
impl DayAccumulator {
    pub(crate) fn new(calendar: DayCalendar) -> Self {
        Self {
            calendar,
            active_ms: 0,
            switches: 0,
            ranking: BTreeMap::new(),
            previous: None,
        }
    }
    pub(crate) fn push(&mut self, segment: &DerivedSegment) {
        let current = &segment.display;
        if current.activity != Activity::Active {
            self.previous = None;
            return;
        }
        if current.duration_ms == 0 {
            return;
        }
        self.active_ms += current.duration_ms;
        if let Some(app) = &current.app {
            self.ranking
                .entry(app.id)
                .or_insert_with(|| ApplicationRank {
                    app: app.clone(),
                    active_ms: 0,
                })
                .active_ms += current.duration_ms;
        }
        let app = current.app.as_ref().map(|app| app.id);
        if self.previous.as_ref().is_some_and(|old| {
            old.segment == current.clock_segment_id
                && old.end == current.start_utc_ms
                && old.app != app
        }) {
            self.switches += 1;
        }
        self.previous = Some(PreviousActive {
            segment: current.clock_segment_id,
            end: current.end_utc_ms,
            app,
        });
        for hour in &mut self.calendar.hours {
            let range = QueryRange {
                start: current.start_utc_ms.max(hour.start_utc_ms),
                end: current.end_utc_ms.min(hour.end_utc_ms),
            };
            if range.end > range.start {
                let duration_ms = segment.duration_within(range);
                match hour
                    .categories
                    .iter_mut()
                    .find(|bucket| bucket.category == current.category)
                {
                    Some(bucket) => bucket.duration_ms += duration_ms,
                    None => hour.categories.push(quartz_protocol::CategoryDuration {
                        category: current.category,
                        duration_ms,
                    }),
                }
            }
        }
    }
    pub(crate) fn finish(self) -> DayAggregation {
        let mut ranking: Vec<_> = self.ranking.into_values().collect();
        ranking.sort_by(|left, right| {
            right
                .active_ms
                .cmp(&left.active_ms)
                .then(left.app.id.cmp(&right.app.id))
        });
        DayAggregation {
            metrics: DayMetrics {
                active_ms: self.active_ms,
                focus_ratio: 0.0,
                distract_ratio: 0.0,
                unclassified_ms: self.active_ms,
                longest_focus_ms: 0,
                switches_per_hour: if self.active_ms == 0 {
                    0.0
                } else {
                    f64::from(self.switches) * HOUR_MS
                        / num_traits::ToPrimitive::to_f64(&self.active_ms)
                            .expect("i64 duration fits f64")
                },
            },
            calendar: self.calendar,
            ranking,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct TimelineProjection {
    segments: Vec<TimelineSegment>,
    last_interval: Option<FactId>,
}
impl TimelineProjection {
    pub(crate) fn push(&mut self, segment: &DerivedSegment, detail: TimelineDetail, pixel_ms: u64) {
        let mut current = segment.display.clone();
        if segment.capture_level != Some(CaptureLevel::Full) {
            current.title = None;
        }
        if segment.capture_level == Some(CaptureLevel::Excluded) {
            current.app = None;
        }
        match detail {
            TimelineDetail::Category => {
                current.app = None;
                current.title = None;
            }
            TimelineDetail::App => current.title = None,
            TimelineDetail::Raw => {}
        }
        if let Some(last) = self.segments.last_mut()
            && (detail != TimelineDetail::Raw || self.last_interval == segment.interval_id)
            && last.end_utc_ms == current.start_utc_ms
            && last.clock_segment_id == current.clock_segment_id
            && last.activity == current.activity
            && last.category == current.category
            && last.category_source == current.category_source
            && last.app == current.app
            && last.title == current.title
            && u64::try_from(current.end_utc_ms - last.start_utc_ms)
                .is_ok_and(|width| width < pixel_ms)
        {
            last.end_utc_ms = current.end_utc_ms;
            last.duration_ms = last
                .duration_ms
                .checked_add(current.duration_ms)
                .expect("adjacent slices stay within one clock segment");
            return;
        }
        self.last_interval = segment.interval_id;
        self.segments.push(current);
    }
    pub(crate) fn into_segments(self) -> Vec<TimelineSegment> {
        self.segments
    }
}
