//! Pure, resumable boundary sweep and day/timeline aggregation.

use super::{
    calendar::DayCalendar,
    classification::{ClassificationId, Classifier, QueryCategoryRef},
    input::{
        Availability, CaptureLevel, ClockSegment, FactId, ForegroundKind, InputRow,
        QueryAnnotation, QueryRange,
    },
};
use quartz_protocol::{
    ApplicationId, ApplicationRank, CategoryRef, DayMetrics, QueryStopReason, TimelineDetail,
    TimelineSegment,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Bounds each compute turn independently of the size of the query range.
pub(crate) use crate::store::query::READ_BATCH_ROWS as QUERY_BATCH_ROWS;
use quartz_domain::{
    capture::{GapReason, StopReason},
    facts::{ClockSegmentId, Foreground, SpanDuration},
};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Activity {
    Active,
    ActiveOverride(quartz_protocol::AnnotationId),
    #[expect(
        clippy::enum_variant_names,
        reason = "03 section 5.1 names this activity OfflineActivity"
    )]
    OfflineActivity(quartz_protocol::AnnotationId),
    Idle,
    NoForeground,
    Excluded,
    Stopped {
        reasons: Vec<StopReason>,
    },
    CoreOffline,
    Gap {
        reason: GapReason,
    },
}
impl Activity {
    fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Active | Self::ActiveOverride(_) | Self::OfflineActivity(_)
        )
    }
}
#[derive(Debug, Clone)]
pub(crate) enum CategorySource {
    Annotation(quartz_protocol::AnnotationId),
    Rule(std::sync::Arc<quartz_domain::rules::ExplanationId>),
    Unclassified,
}
#[derive(Debug, Clone, Copy)]
struct ForegroundIndex(usize);
#[derive(Debug, Clone)]
pub(crate) struct DerivedSegment {
    pub span: SpanDuration,
    pub clock_segment_id: Option<ClockSegmentId>,
    foreground: Option<ForegroundIndex>,
    classification: ClassificationId,
    pub activity: Activity,
    category_source: CategorySource,
    pub interval_id: Option<FactId>,
    pub pending_idle: Option<QueryRange>,
}
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct SegmentFacts {
    rows: Vec<ClassifiedInput>,
    classifier: Classifier,
}
impl SegmentFacts {
    #[cfg(test)]
    pub(super) fn rename_category(&mut self, segment: &DerivedSegment, name: &str) {
        if let CategoryRef::Classified { category } =
            &mut self.classifier.result_mut(segment.classification).category
        {
            std::sync::Arc::make_mut(category).name = Some(name.into());
        }
    }
    fn foreground(&self, segment: &DerivedSegment) -> Option<&Foreground> {
        segment
            .foreground
            .map(|index| match &self.rows[index.0].input {
                InputRow::Foreground(row) => row,
                _ => unreachable!("foreground index references foreground fact"),
            })
    }
    pub(super) fn category(&self, segment: &DerivedSegment) -> &CategoryRef {
        &self.classifier.result(segment.classification).category
    }
    fn app(&self, segment: &DerivedSegment) -> Option<&quartz_domain::facts::ApplicationRef> {
        self.foreground(segment)
            .and_then(|row| row.visible_app().map(std::convert::AsRef::as_ref))
    }
    fn project_app(app: &quartz_domain::facts::ApplicationRef) -> quartz_protocol::ApplicationRef {
        quartz_protocol::ApplicationRef {
            id: ApplicationId(app.id.0),
            exe_name: app.exe_name.clone(),
            display_name: app.display_name.clone(),
            aumid: app.aumid.clone(),
        }
    }
}
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct DerivedSegments {
    pub segments: Vec<DerivedSegment>,
    pub facts: SegmentFacts,
}
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
pub(super) struct ClassifiedInput {
    input: InputRow,
    classification: ClassificationId,
    source: CategorySource,
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
    rows: Vec<ClassifiedInput>,
    annotations: Vec<usize>,
    clocks: BTreeMap<ClockSegmentId, Sweep>,
    clock_order: BTreeSet<ClockOrder>,
    segments: Vec<DerivedSegment>,
    coverage: BTreeMap<i64, i64>,
    offline: VecDeque<QueryRange>,
    classifier: Classifier,
}
impl Pipeline {
    pub(crate) fn new(range: QueryRange, classifier: Classifier) -> Self {
        Self {
            range,
            classifier,
            rows: Vec::new(),
            annotations: Vec::new(),
            clocks: BTreeMap::new(),
            clock_order: BTreeSet::new(),
            segments: Vec::new(),
            coverage: BTreeMap::new(),
            offline: VecDeque::new(),
        }
    }
    pub(super) fn classify_rows(&mut self, rows: Vec<InputRow>) -> Vec<ClassifiedInput> {
        rows.into_iter()
            .map(|input| self.classified(input))
            .collect()
    }
    fn classified(&mut self, input: InputRow) -> ClassifiedInput {
        let classification = self.classify_row(&input);
        let source = self.classifier.result(classification).source.clone();
        ClassifiedInput {
            input,
            classification,
            source,
        }
    }
    fn classify_row(&mut self, row: &InputRow) -> ClassificationId {
        match row {
            InputRow::Foreground(row) => self.classifier.classify(row),
            InputRow::Annotation(annotation) => self.classifier.annotation(annotation),
            _ => ClassificationId::UNCLASSIFIED,
        }
    }
    #[cfg(test)]
    pub(crate) fn ingest(&mut self, input: InputRow) -> Result<(), DeriveSegmentsError> {
        let row = self.classified(input);
        self.ingest_classified(row)
    }
    pub(super) fn ingest_classified(
        &mut self,
        row: ClassifiedInput,
    ) -> Result<(), DeriveSegmentsError> {
        if let InputRow::Clock(clock) = row.input {
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
        if let InputRow::Annotation(annotation) = &row.input {
            if annotation.range.end <= annotation.range.start {
                return Err(DeriveSegmentsError::InvalidSpan);
            }
            let index = self.rows.len();
            for sweep in self.clocks.values_mut() {
                let start = annotation
                    .range
                    .start
                    .max(sweep.clock.start_utc_ms)
                    .max(self.range.start);
                let end = annotation
                    .range
                    .end
                    .min(sweep.clock.end_utc_ms)
                    .min(self.range.end);
                if start < end {
                    sweep.insert(start, index, BoundaryEdge::Start);
                    sweep.insert(end, index, BoundaryEdge::End);
                }
            }
            self.annotations.push(index);
            self.rows.push(row);
            return Ok(());
        }
        if let Some(span) = row.input.span() {
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
    fn offline_segment(&self, range: QueryRange) -> DerivedSegment {
        let mut segment = DerivedSegment {
            span: SpanDuration {
                start_utc_ms: range.start,
                end_utc_ms: range.end,
                duration_ms: range.end - range.start,
            },
            clock_segment_id: None,
            foreground: None,
            classification: ClassificationId::UNCLASSIFIED,
            activity: Activity::CoreOffline,
            category_source: CategorySource::Unclassified,
            interval_id: None,
            pending_idle: None,
        };
        for &index in &self.annotations {
            let row = &self.rows[index];
            if let InputRow::Annotation(annotation) = &row.input
                && annotation.kind == quartz_protocol::AnnotationKind::OfflineActivity
                && annotation.range.start <= range.start
                && annotation.range.end >= range.end
            {
                segment.apply_offline(annotation, row.classification);
            }
        }
        segment
    }
    /// Processes a bounded number of boundaries; returns true after the last turn.
    pub(crate) fn advance(&mut self) -> bool {
        for _ in 0..QUERY_BATCH_ROWS {
            let order = self.clock_order.first().copied();
            if let Some(range) = self
                .offline
                .pop_front_if(|range| order.is_none_or(|clock| range.start < clock.start))
            {
                let boundary = self
                    .annotations
                    .iter()
                    .filter_map(|&index| match &self.rows[index].input {
                        InputRow::Annotation(annotation)
                            if annotation.kind
                                == quartz_protocol::AnnotationKind::OfflineActivity =>
                        {
                            Some(annotation)
                        }
                        _ => None,
                    })
                    .flat_map(|annotation| [annotation.range.start, annotation.range.end])
                    .filter(|at| *at > range.start && *at < range.end)
                    .min();
                let end = boundary.unwrap_or(range.end);
                if end < range.end {
                    self.offline.push_front(QueryRange {
                        start: end,
                        end: range.end,
                    });
                }
                self.segments.push(self.offline_segment(QueryRange {
                    start: range.start,
                    end,
                }));
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
    fn derive(rows: &[ClassifiedInput], sweep: &Sweep, start: i64, end: i64) -> DerivedSegment {
        let mut foreground = None;
        let mut foreground_index = None;
        let mut classification = ClassificationId::UNCLASSIFIED;
        let mut idle = false;
        let mut reasons = BTreeSet::new();
        let mut gap = None;
        let mut stopped_span = None;
        let mut gap_span = None;
        let mut category_override = None;
        let mut active_override = None;
        let mut offline_activity = None;
        let mut annotated_away = None;
        for &index in &sweep.active {
            match &rows[index].input {
                InputRow::Foreground(row) => {
                    if foreground
                        .is_none_or(|old: &super::input::Foreground| old.span.id < row.span.id)
                    {
                        foreground = Some(row);
                        foreground_index = Some(ForegroundIndex(index));
                        classification = rows[index].classification;
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
                    reasons.insert(*reason);
                    stopped_span = Some(span);
                }
                InputRow::Gap { reason, span } => {
                    gap = Some(*reason);
                    gap_span = Some(span);
                }
                InputRow::Annotation(annotation) => {
                    use quartz_protocol::AnnotationKind;
                    match annotation.kind {
                        AnnotationKind::CategoryOverride => category_override = Some(&rows[index]),
                        AnnotationKind::ActiveOverride => active_override = Some(&rows[index]),
                        AnnotationKind::OfflineActivity => offline_activity = Some(&rows[index]),
                        AnnotationKind::Note | AnnotationKind::DismissedAway => {}
                    }
                    if annotation.kind != AnnotationKind::CategoryOverride {
                        annotated_away = Some(annotation.kind);
                    }
                }
                InputRow::Clock(_) => unreachable!("clocks are not fact rows"),
            }
        }
        let activity = if let Some(reason) = gap {
            Activity::Gap { reason }
        } else if !reasons.is_empty() {
            Activity::Stopped {
                reasons: reasons.into_iter().collect(),
            }
        } else {
            match foreground {
                Some(row) if row.capture_level() == CaptureLevel::Excluded => Activity::Excluded,
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
        let mut segment = DerivedSegment {
            span: SpanDuration {
                start_utc_ms: start,
                end_utc_ms: end,
                duration_ms,
            },
            clock_segment_id: Some(sweep.clock.id),
            foreground: foreground_index,
            classification,
            activity,
            category_source: foreground_index.map_or(CategorySource::Unclassified, |index| {
                rows[index.0].source.clone()
            }),
            interval_id: foreground.map(|row| row.span.id),
            pending_idle: (idle && stopped_span.is_none() && annotated_away.is_none())
                .then_some(range),
        };
        if segment.activity == Activity::Idle
            && let Some(row) = active_override
            && let InputRow::Annotation(annotation) = &row.input
        {
            segment.activity = Activity::ActiveOverride(annotation.id.clone());
            if annotation.category.is_some() {
                segment.classification = row.classification;
                segment.category_source = row.source.clone();
            }
        }
        if segment.activity.is_active() {
            if let Some(row) = category_override {
                segment.classification = row.classification;
                segment.category_source = row.source.clone();
            }
        } else if matches!(
            segment.activity,
            Activity::Idle
                | Activity::Stopped { .. }
                | Activity::Gap { .. }
                | Activity::NoForeground
        ) && let Some(row) = offline_activity
            && let InputRow::Annotation(annotation) = &row.input
        {
            segment.apply_offline(annotation, row.classification);
        }
        segment
    }
    pub(crate) fn into_segments(self) -> DerivedSegments {
        DerivedSegments {
            segments: self.segments,
            facts: SegmentFacts {
                rows: self.rows,
                classifier: self.classifier,
            },
        }
    }
}
impl DerivedSegment {
    fn apply_offline(&mut self, annotation: &QueryAnnotation, classification: ClassificationId) {
        self.activity = Activity::OfflineActivity(annotation.id.clone());
        self.classification = classification;
        self.category_source = CategorySource::Annotation(annotation.id.clone());
        self.foreground = None;
        self.interval_id = None;
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
    pub(crate) fn new(calendar: DayCalendar, segments: &DerivedSegments) -> Self {
        let mut builder = DayAccumulator::new(calendar);
        for segment in &segments.segments {
            builder.push(segment, &segments.facts);
        }
        builder.finish()
    }
}
#[derive(Debug, PartialEq, Eq)]
enum ActiveApplication {
    Captured(Option<ApplicationId>),
    Offline(quartz_protocol::AnnotationId),
}
#[derive(Debug)]
struct PreviousActive {
    segment: Option<ClockSegmentId>,
    end: i64,
    app: ActiveApplication,
}
#[derive(Debug)]
pub(crate) struct DayAccumulator {
    calendar: DayCalendar,
    active_ms: i64,
    switches: u32,
    ranking: BTreeMap<ApplicationId, ApplicationRank>,
    previous: Option<PreviousActive>,
    focus_ms: i64,
    distract_ms: i64,
    unclassified_ms: i64,
    focus_run_ms: i64,
    longest_focus_ms: i64,
}
impl DayAccumulator {
    pub(crate) fn new(calendar: DayCalendar) -> Self {
        Self {
            calendar,
            active_ms: 0,
            switches: 0,
            ranking: BTreeMap::new(),
            previous: None,
            focus_ms: 0,
            distract_ms: 0,
            unclassified_ms: 0,
            focus_run_ms: 0,
            longest_focus_ms: 0,
        }
    }
    pub(crate) fn push(&mut self, segment: &DerivedSegment, facts: &SegmentFacts) {
        let current = &segment.span;
        let category = facts.category(segment);
        if !segment.activity.is_active() {
            self.previous = None;
            self.focus_run_ms = 0;
            return;
        }
        if current.duration_ms == 0 {
            return;
        }
        self.active_ms += current.duration_ms;
        let adjacent = self.previous.as_ref().is_some_and(|previous| {
            previous.segment == segment.clock_segment_id && previous.end == current.start_utc_ms
        });
        match category.tendency() {
            quartz_protocol::CategoryTendency::Focus => {
                self.focus_ms += current.duration_ms;
                if !adjacent {
                    self.focus_run_ms = 0;
                }
                self.focus_run_ms += current.duration_ms;
                self.longest_focus_ms = self.longest_focus_ms.max(self.focus_run_ms);
            }
            quartz_protocol::CategoryTendency::Distraction => {
                self.distract_ms += current.duration_ms;
                self.focus_run_ms = 0;
            }
            quartz_protocol::CategoryTendency::Neutral => self.focus_run_ms = 0,
        }
        if *category == CategoryRef::Unclassified {
            self.unclassified_ms += current.duration_ms;
        }
        if let Some(app) = facts.app(segment) {
            let id = ApplicationId(app.id.0);
            self.ranking
                .entry(id)
                .or_insert_with(|| ApplicationRank {
                    app: SegmentFacts::project_app(app),
                    active_ms: 0,
                })
                .active_ms += current.duration_ms;
        }
        let app = match &segment.activity {
            Activity::OfflineActivity(id) => ActiveApplication::Offline(id.clone()),
            _ => ActiveApplication::Captured(facts.app(segment).map(|app| ApplicationId(app.id.0))),
        };
        if self.previous.as_ref().is_some_and(|old| {
            old.segment == segment.clock_segment_id
                && old.end == current.start_utc_ms
                && old.app != app
        }) {
            self.switches += 1;
        }
        self.previous = Some(PreviousActive {
            segment: segment.clock_segment_id,
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
                    .find(|bucket| bucket.category == *category)
                {
                    Some(bucket) => bucket.duration_ms += duration_ms,
                    None => hour.categories.push(quartz_protocol::CategoryDuration {
                        category: category.clone(),
                        duration_ms,
                    }),
                }
            }
        }
    }
    fn ratio(&self, duration_ms: i64) -> f64 {
        use num_traits::ToPrimitive;
        if self.active_ms == 0 {
            0.0
        } else {
            duration_ms.to_f64().expect("duration fits f64")
                / self.active_ms.to_f64().expect("active duration fits f64")
        }
    }
    pub(crate) fn finish(self) -> DayAggregation {
        let focus_ratio = self.ratio(self.focus_ms);
        let distract_ratio = self.ratio(self.distract_ms);
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
                focus_ratio,
                distract_ratio,
                unclassified_ms: self.unclassified_ms,
                longest_focus_ms: self.longest_focus_ms,
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
}
impl TimelineProjection {
    pub(crate) fn project(
        segment: &DerivedSegment,
        facts: &SegmentFacts,
        detail: TimelineDetail,
    ) -> TimelineSegment {
        TimelineSegment {
            start_utc_ms: segment.span.start_utc_ms,
            end_utc_ms: segment.span.end_utc_ms,
            duration_ms: segment.span.duration_ms,
            clock_segment_id: segment
                .clock_segment_id
                .map(|id| quartz_protocol::ClockSegmentId(id.0)),
            activity: match &segment.activity {
                Activity::Active => quartz_protocol::Activity::Active,
                Activity::ActiveOverride(annotation) => quartz_protocol::Activity::ActiveOverride {
                    annotation: annotation.clone(),
                },
                Activity::OfflineActivity(annotation) => {
                    quartz_protocol::Activity::OfflineActivity {
                        annotation: annotation.clone(),
                    }
                }
                Activity::Idle => quartz_protocol::Activity::Idle,
                Activity::NoForeground => quartz_protocol::Activity::NoForeground,
                Activity::Excluded => quartz_protocol::Activity::Excluded,
                Activity::CoreOffline => quartz_protocol::Activity::Stopped {
                    reasons: vec![QueryStopReason::CoreOffline],
                },
                Activity::Stopped { reasons } => quartz_protocol::Activity::Stopped {
                    reasons: reasons
                        .iter()
                        .map(|reason| match reason {
                            StopReason::Locked => QueryStopReason::Locked,
                            StopReason::Disconnected => QueryStopReason::Disconnected,
                            StopReason::Suspended => QueryStopReason::Suspended,
                            StopReason::Paused => QueryStopReason::Paused,
                        })
                        .collect(),
                },
                Activity::Gap { reason } => quartz_protocol::Activity::Gap {
                    reason: match reason {
                        GapReason::EventOverload => quartz_protocol::QueryGapReason::EventOverload,
                        GapReason::WriteQueueOverload => {
                            quartz_protocol::QueryGapReason::WriteQueueOverload
                        }
                        GapReason::DatabaseError => quartz_protocol::QueryGapReason::DatabaseError,
                    },
                },
            },
            category: facts.category(segment).clone(),
            category_source: match &segment.category_source {
                CategorySource::Unclassified => quartz_protocol::CategorySource::Unclassified,
                CategorySource::Annotation(annotation) => {
                    quartz_protocol::CategorySource::Annotation {
                        annotation: annotation.clone(),
                    }
                }
                CategorySource::Rule(rule) => quartz_protocol::CategorySource::Rule {
                    rule: rule
                        .as_str()
                        .parse()
                        .expect("domain revision satisfies wire grammar"),
                },
            },
            app: if detail == TimelineDetail::Category {
                None
            } else {
                facts.app(segment).map(SegmentFacts::project_app)
            },
            window_instance: if detail != TimelineDetail::Category
                && matches!(
                    segment.activity,
                    Activity::Active | Activity::ActiveOverride(_)
                ) {
                facts
                    .foreground(segment)
                    .and_then(Foreground::visible_window_instance)
                    .map(|id| {
                        quartz_protocol::WindowInstanceRef::from_stored_id(
                            quartz_protocol::facts::WindowInstanceId(id.0),
                        )
                    })
            } else {
                None
            },
            title: if detail == TimelineDetail::Raw {
                facts
                    .foreground(segment)
                    .and_then(|row| row.visible_title())
                    .map(str::to_owned)
            } else {
                None
            },
        }
    }
    pub(crate) fn push(
        &mut self,
        segment: &DerivedSegment,
        facts: &SegmentFacts,
        detail: TimelineDetail,
        pixel_ms: u64,
    ) {
        let current = Self::project(segment, facts, detail);
        if let Some(last) = self.segments.last_mut()
            && last.end_utc_ms == current.start_utc_ms
            && last.clock_segment_id == current.clock_segment_id
            && last.activity == current.activity
            && last.category.id() == current.category.id()
            && (last.category_source == current.category_source
                || (detail == TimelineDetail::Category
                    && !matches!(
                        last.category_source,
                        quartz_protocol::CategorySource::Annotation { .. }
                    )
                    && !matches!(
                        current.category_source,
                        quartz_protocol::CategorySource::Annotation { .. }
                    )))
            && last.app == current.app
            && last.window_instance == current.window_instance
            && last.title == current.title
            && u64::try_from(current.end_utc_ms - last.start_utc_ms)
                .is_ok_and(|width| width < pixel_ms)
        {
            if last.category_source != current.category_source {
                last.category_source = quartz_protocol::CategorySource::Mixed;
            }
            last.end_utc_ms = current.end_utc_ms;
            last.duration_ms = last
                .duration_ms
                .checked_add(current.duration_ms)
                .expect("adjacent slices stay within one clock segment");
            return;
        }
        self.segments.push(current);
    }
    pub(crate) fn into_segments(self) -> Vec<TimelineSegment> {
        self.segments
    }
}
