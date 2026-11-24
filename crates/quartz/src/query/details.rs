//! Read-only detail projections over the shared classified snapshot slices.

use super::{
    RunQueryError,
    calendar::{DayCalendar, DayStartOffset},
    documents::DocumentIdentity,
    input::QueryRange,
    paging::PagePosition,
    pipeline::{DerivedSegment, SegmentFacts, TimelineProjection},
};
use quartz_protocol::{
    Activity, ApplicationId, ClassificationRuleId, ENTITY_DETAIL_DAYS, ENTITY_DETAIL_TOP_TITLES,
    EntityClassificationRule, EntityDailyDuration, EntityTitleDuration, LocalDate,
    QueryEntityDetailResponse, QuerySegmentDetailRequest, QueryVersions, TimelineDetail,
    TimelineSegment,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
struct EntityDay {
    range: QueryRange,
    daily: EntityDailyDuration,
}
#[derive(Debug)]
pub(super) struct EntityCalendar {
    days: Vec<EntityDay>,
    pub(super) range: QueryRange,
}
impl EntityCalendar {
    pub(super) fn in_zone(
        date: LocalDate,
        offset: DayStartOffset,
        zone: jiff::tz::TimeZone,
    ) -> Result<Self, RunQueryError> {
        let mut dates = vec![date];
        while dates.len() < ENTITY_DETAIL_DAYS {
            dates.push(dates.last().expect("ending date").yesterday()?);
        }
        let mut days = Vec::with_capacity(ENTITY_DETAIL_DAYS);
        for local_date in dates.into_iter().rev() {
            let calendar = DayCalendar::in_zone(local_date, offset, zone.clone())?;
            days.push(EntityDay {
                range: calendar.range,
                daily: EntityDailyDuration {
                    local_date,
                    active_ms: 0,
                },
            });
        }
        Ok(Self {
            range: QueryRange {
                start: days.first().expect("seven days").range.start,
                end: days.last().expect("seven days").range.end,
            },
            days,
        })
    }
}

#[derive(Debug, Default)]
pub(super) struct SegmentDetailProjection {
    intervals: Vec<TimelineSegment>,
}
impl SegmentDetailProjection {
    pub(super) fn push(
        &mut self,
        segment: &DerivedSegment,
        facts: &SegmentFacts,
        request: &QuerySegmentDetailRequest,
    ) {
        let interval = TimelineProjection::project(segment, facts, TimelineDetail::Raw);
        if request
            .app
            .is_some_and(|id| interval.app.as_ref().is_none_or(|app| app.id != id))
            || request
                .window_instance
                .is_some_and(|id| interval.window_instance != Some(id))
        {
            return;
        }
        self.intervals.push(interval);
    }
    pub(super) fn finish(self) -> Vec<TimelineSegment> {
        self.intervals
    }
}

#[derive(Debug, Clone)]
pub(super) enum EntitySelection {
    App { id: ApplicationId },
    Document { identity: DocumentIdentity },
}
impl EntitySelection {
    fn matches(&self, interval: &TimelineSegment) -> bool {
        match self {
            Self::App { id } => interval.app.as_ref().is_some_and(|app| app.id == *id),
            Self::Document { identity } => {
                DocumentIdentity::from_segment(interval).as_ref() == Some(identity)
            }
        }
    }
}
#[derive(Debug)]
pub(super) struct EntityDetailAccumulator {
    entity: EntitySelection,
    extraction: BTreeSet<quartz_protocol::EntityExtractionProvenance>,
    calendar: EntityCalendar,
    titles: BTreeMap<String, EntityTitleDuration>,
    rules: BTreeMap<ClassificationRuleId, EntityClassificationRule>,
}
impl EntityDetailAccumulator {
    pub(super) fn new(entity: EntitySelection, calendar: EntityCalendar) -> Self {
        Self {
            entity,
            extraction: BTreeSet::new(),
            calendar,
            titles: BTreeMap::new(),
            rules: BTreeMap::new(),
        }
    }
    pub(super) fn push(&mut self, segment: &DerivedSegment, facts: &SegmentFacts) {
        let interval = TimelineProjection::project(segment, facts, TimelineDetail::Raw);
        if !matches!(
            interval.activity,
            Activity::Active | Activity::ActiveOverride { .. }
        ) || !self.entity.matches(&interval)
        {
            return;
        }
        for day in &mut self.calendar.days {
            let range = QueryRange {
                start: interval.start_utc_ms.max(day.range.start),
                end: interval.end_utc_ms.min(day.range.end),
            };
            if range.start < range.end {
                day.daily.active_ms += facts.captured_duration_within(segment, range);
            }
        }
        if matches!(self.entity, EntitySelection::Document { .. })
            && let Some(extraction) = &interval.extraction
            && let Some(status) = extraction.status
        {
            self.extraction
                .insert(quartz_protocol::EntityExtractionProvenance {
                    extractor_rule: extraction.extractor_rule.clone(),
                    status,
                });
        }
        if let Some(title) = interval.title {
            self.titles
                .entry(title.clone())
                .or_insert(EntityTitleDuration {
                    title,
                    duration_ms: 0,
                })
                .duration_ms += interval.duration_ms;
        }
        if let Some(rule) = facts.rule_details(segment) {
            self.rules.entry(rule.rule.clone()).or_insert(rule);
        }
    }
    pub(super) fn finish(
        self,
        versions: QueryVersions,
        position: PagePosition,
    ) -> QueryEntityDetailResponse {
        let mut titles: Vec<_> = self.titles.into_values().collect();
        titles.sort_by(|left, right| {
            right
                .duration_ms
                .cmp(&left.duration_ms)
                .then(left.title.cmp(&right.title))
        });
        titles.truncate(ENTITY_DETAIL_TOP_TITLES);
        QueryEntityDetailResponse {
            daily: position.is_first().then(|| {
                self.calendar
                    .days
                    .into_iter()
                    .map(|day| day.daily)
                    .collect()
            }),
            titles: position.is_first().then_some(titles),
            rules: self.rules.into_values().collect(),
            extraction: self.extraction.into_iter().collect(),
            versions,
            next_cursor: None,
        }
    }
}
