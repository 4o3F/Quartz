//! Protocol projections retain core day aggregates and application identities.
use super::{Day, DaySource, Derived};
use crate::{
    link::{DayData, UtcRange},
    model::{
        AppId, AppInfo, AwayLabel, BuiltinCategory, Category, CategoryId, ClassificationSource,
        ClockAnomaly, DaySummary, Glyph, HourlyBar, MainBlock, RankRow, Segment, SegmentId, Share,
        StateBlock, StateKind, Tendency, TimeOfDay, TimeSpan, ranking::Rankings,
        summary::CategoryTotal,
    },
};
use jiff::civil::Date;
use num_traits::ToPrimitive as _;
use quartz_protocol::{self as wire, CategoryRef};
use std::{collections::BTreeMap, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq, snafu::Snafu)]
#[snafu(display("could not map the core day response"))]
pub(crate) struct BuildLiveDayError;
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LiveDay {
    pub(super) ranking: Vec<RankRow>,
    app_ids: BTreeMap<AppId, wire::ApplicationId>,
    bounds: UtcRange,
    pending: Vec<PendingAwaySpan>,
    switches_per_hour: u64,
    zone: jiff::tz::TimeZone,
}
/// A validated pending interval in the model's elapsed day coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingAwaySpan {
    pub(crate) span: TimeSpan,
    pub(crate) duration: Duration,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CategoryKey {
    Unclassified,
    Classified(wire::CategoryId),
}
struct Mapping {
    categories: Vec<Category>,
    category_ids: BTreeMap<CategoryKey, CategoryId>,
    apps: Vec<AppInfo>,
    app_ids: BTreeMap<wire::ApplicationId, AppId>,
    bounds: UtcRange,
}
impl Mapping {
    fn duration(value: i64) -> Result<Duration, BuildLiveDayError> {
        Ok(Duration::from_millis(
            u64::try_from(value).map_err(|_| BuildLiveDayError)?,
        ))
    }
    fn category(&mut self, value: &CategoryRef) -> Result<CategoryId, BuildLiveDayError> {
        let key = match value {
            CategoryRef::Unclassified => CategoryKey::Unclassified,
            CategoryRef::Classified { category } => CategoryKey::Classified(category.id),
        };
        if let Some(id) = self.category_ids.get(&key) {
            return Ok(*id);
        }
        let id =
            CategoryId::new(u16::try_from(self.categories.len()).map_err(|_| BuildLiveDayError)?);
        let category = match value {
            CategoryRef::Unclassified => Category::builtin(id, BuiltinCategory::Uncategorized),
            CategoryRef::Classified { category } => {
                let display = crate::model::live_details::CategoryDisplay::from_protocol(value)
                    .map_err(|_| BuildLiveDayError)?;
                Category {
                    id,
                    name: display.name,
                    color: display.color,
                    tendency: match category.tendency {
                        wire::CategoryTendency::Focus => Tendency::Focus,
                        wire::CategoryTendency::Neutral => Tendency::Neutral,
                        wire::CategoryTendency::Distraction => Tendency::Distraction,
                    },
                }
            }
        };
        self.categories.push(category);
        self.category_ids.insert(key, id);
        Ok(id)
    }
    fn app(&mut self, value: &wire::ApplicationRef) -> Result<AppId, BuildLiveDayError> {
        if let Some(id) = self.app_ids.get(&value.id) {
            return Ok(*id);
        }
        let id = AppId::new(u16::try_from(self.apps.len()).map_err(|_| BuildLiveDayError)?);
        self.apps.push(AppInfo {
            id,
            name: value.display_name.clone(),
            short_name: value.display_name.clone(),
            exe: value.exe_name.clone().unwrap_or_default(),
            glyph: Glyph::Terminal,
        });
        self.app_ids.insert(value.id, id);
        Ok(id)
    }
    fn span(&self, start: i64, end: i64) -> Result<TimeSpan, BuildLiveDayError> {
        let time = |value: i64| -> Result<TimeOfDay, BuildLiveDayError> {
            if value < self.bounds.start || value > self.bounds.end {
                return Err(BuildLiveDayError);
            }
            let elapsed = value
                .checked_sub(self.bounds.start)
                .ok_or(BuildLiveDayError)?;
            let elapsed = u32::try_from(elapsed).map_err(|_| BuildLiveDayError)?;
            Ok(TimeOfDay::from_elapsed_millis(elapsed))
        };
        TimeSpan::try_new(time(start)?, time(end)?).map_err(|_| BuildLiveDayError)
    }
    fn state(activity: &wire::Activity) -> Option<StateKind> {
        match activity {
            wire::Activity::Active
            | wire::Activity::ActiveOverride { .. }
            | wire::Activity::OfflineActivity { .. } => None,
            wire::Activity::Idle => Some(StateKind::Idle {
                preceding: None,
                away: AwayLabel::Settled,
            }),
            wire::Activity::NoForeground => Some(StateKind::NoForeground),
            wire::Activity::Excluded => Some(StateKind::Excluded),
            wire::Activity::Gap { .. } => Some(StateKind::CollectionGap),
            wire::Activity::Stopped { reasons } => {
                Some(if reasons.contains(&wire::QueryStopReason::Locked) {
                    StateKind::Locked
                } else if reasons.contains(&wire::QueryStopReason::Suspended) {
                    StateKind::Sleep
                } else {
                    StateKind::CoreStopped
                })
            }
        }
    }
    fn ratio(value: f64) -> Result<Share, BuildLiveDayError> {
        const RATIO_PRECISION: u64 = 1_000_000;
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(BuildLiveDayError);
        }
        let part = (value * RATIO_PRECISION.to_f64().ok_or(BuildLiveDayError)?)
            .round()
            .to_u64()
            .ok_or(BuildLiveDayError)?;
        Ok(Share::of(
            Duration::from_micros(part),
            Duration::from_micros(RATIO_PRECISION),
        ))
    }
}
impl Day {
    pub(crate) fn empty(date: Date) -> Self {
        Self {
            date,
            categories: Vec::new(),
            apps: Vec::new(),
            segments: Vec::new(),
            segment_index: BTreeMap::new(),
            states: Vec::new(),
            history: BTreeMap::new(),
            rules: Vec::new(),
            derived: Derived::default(),
            source: DaySource::Loading,
        }
    }
    pub(crate) fn from_protocol(data: DayData) -> Result<Self, BuildLiveDayError> {
        let length = data
            .bounds
            .end
            .checked_sub(data.bounds.start)
            .filter(|span| *span > 0)
            .ok_or(BuildLiveDayError)?;
        u32::try_from(length).map_err(|_| BuildLiveDayError)?;
        jiff::Timestamp::from_millisecond(data.bounds.start).map_err(|_| BuildLiveDayError)?;
        jiff::Timestamp::from_millisecond(data.bounds.end).map_err(|_| BuildLiveDayError)?;
        let first: jiff::ZonedDateTime = data
            .hours
            .first()
            .ok_or(BuildLiveDayError)?
            .local_start
            .parse()
            .map_err(|_| BuildLiveDayError)?;
        let zone = first.time_zone().clone();
        let switches_per_hour = data
            .metrics
            .switches_per_hour
            .round()
            .to_u64()
            .ok_or(BuildLiveDayError)?;
        let mut map = Mapping {
            categories: Vec::new(),
            category_ids: BTreeMap::new(),
            apps: Vec::new(),
            app_ids: BTreeMap::new(),
            bounds: data.bounds,
        };
        let pending = data
            .pending
            .iter()
            .map(|range| {
                Ok(PendingAwaySpan {
                    span: map.span(range.start_utc_ms, range.end_utc_ms)?,
                    duration: Mapping::duration(range.duration_ms)?,
                })
            })
            .collect::<Result<Vec<_>, BuildLiveDayError>>()?;
        let mut day = Self::empty(data.date);
        let mut hourly = Vec::new();
        let mut totals: BTreeMap<CategoryId, Duration> = BTreeMap::new();
        for hour in &data.hours {
            let label: jiff::ZonedDateTime =
                hour.local_start.parse().map_err(|_| BuildLiveDayError)?;
            let mut parts = Vec::new();
            for part in &hour.categories {
                let category = map.category(&part.category)?;
                let total = Mapping::duration(part.duration_ms)?;
                *totals.entry(category).or_default() += total;
                parts.push(CategoryTotal { category, total });
            }
            hourly.push(HourlyBar {
                hour: u32::try_from(label.hour()).map_err(|_| BuildLiveDayError)?,
                parts,
            });
        }
        let mut track = Vec::new();
        for segment in &data.categories {
            let span = map.span(segment.start_utc_ms, segment.end_utc_ms)?;
            if let Some(kind) = Mapping::state(&segment.activity) {
                track.push(MainBlock::State(StateBlock {
                    span,
                    kind,
                    clock: ClockAnomaly::None,
                }));
            } else {
                track.push(MainBlock::Active {
                    span,
                    category: map.category(&segment.category)?,
                });
            }
        }
        for raw in &data.raw {
            if !matches!(
                raw.activity,
                wire::Activity::Active | wire::Activity::ActiveOverride { .. }
            ) {
                continue;
            }
            let Some(app) = &raw.app else {
                continue;
            };
            let app = map.app(app)?;
            let category = map.category(&raw.category)?;
            let id =
                SegmentId::new(u32::try_from(day.segments.len()).map_err(|_| BuildLiveDayError)?);
            let source = match &raw.category_source {
                wire::CategorySource::Rule { rule } => {
                    ClassificationSource::ProtocolRule(rule.clone())
                }
                wire::CategorySource::Annotation { .. } => ClassificationSource::Manual,
                wire::CategorySource::Mixed => ClassificationSource::Mixed,
                wire::CategorySource::Unclassified => ClassificationSource::Unmatched,
            };
            day.segment_index.insert(id, day.segments.len());
            day.segments.push(Segment {
                id,
                span: map.span(raw.start_utc_ms, raw.end_utc_ms)?,
                elapsed: Mapping::duration(raw.duration_ms)?,
                app,
                category,
                title: raw.title.clone().unwrap_or_default(),
                domain: None,
                document: None,
                source,
                merged: None,
                window_instance: raw.window_instance,
            });
        }
        let mut ranking = Vec::new();
        for row in &data.applications {
            let app = map.app(&row.app)?;
            ranking.push(RankRow::application(
                app,
                row.app.display_name.clone(),
                Mapping::duration(row.active_ms)?,
            ));
        }
        day.categories = map.categories;
        day.apps = map.apps;
        day.derived = Derived {
            activity_range: day.compute_activity_range(),
            summary: DaySummary {
                active: Mapping::duration(data.metrics.active_ms)?,
                focus: Mapping::ratio(data.metrics.focus_ratio)?,
                distraction: Mapping::ratio(data.metrics.distract_ratio)?,
                unclassified: Mapping::duration(data.metrics.unclassified_ms)?,
                longest_focus_streak: Mapping::duration(data.metrics.longest_focus_ms)?,
                switches: 0,
            },
            category_totals: totals
                .into_iter()
                .map(|(category, total)| CategoryTotal { category, total })
                .collect(),
            hourly,
            main_track: track,
            lanes: day.compute_lanes(),
            rankings: Rankings::default(),
        };
        day.derived.activity_range = day
            .derived
            .main_track
            .iter()
            .filter(|block| matches!(block, MainBlock::Active { .. }))
            .map(MainBlock::span)
            .reduce(|span, next| {
                TimeSpan::new(span.start().min(next.start()), span.end().max(next.end()))
            });
        let mut app_ids = BTreeMap::new();
        for (protocol, local) in map.app_ids {
            app_ids.insert(local, protocol);
        }
        day.source = DaySource::Live(LiveDay {
            ranking,
            app_ids,
            bounds: data.bounds,
            pending,
            switches_per_hour,
            zone,
        });
        Ok(day)
    }
    pub(crate) fn protocol_app(&self, app: AppId) -> Option<wire::ApplicationId> {
        match &self.source {
            DaySource::Live(live) => live.app_ids.get(&app).copied(),
            DaySource::Fixture | DaySource::Loading => None,
        }
    }
    /// Rebase a retained UTC selection onto the current logical-day coordinates.
    pub(crate) fn span_for_utc(&self, range: UtcRange) -> Option<TimeSpan> {
        let DaySource::Live(live) = &self.source else {
            return None;
        };
        if range.start < live.bounds.start || range.end > live.bounds.end {
            return None;
        }
        TimeSpan::try_new(
            TimeOfDay::from_elapsed_millis(
                u32::try_from(range.start.checked_sub(live.bounds.start)?).ok()?,
            ),
            TimeOfDay::from_elapsed_millis(
                u32::try_from(range.end.checked_sub(live.bounds.start)?).ok()?,
            ),
        )
        .ok()
    }
    pub(crate) fn local_clock_at(&self, utc: i64) -> Option<jiff::civil::Time> {
        let DaySource::Live(live) = &self.source else {
            return None;
        };
        Some(
            jiff::Timestamp::from_millisecond(utc)
                .expect("validated detail timestamp")
                .to_zoned_datetime(live.zone.clone())
                .time(),
        )
    }
    pub(crate) fn switches_per_hour(&self) -> u64 {
        match &self.source {
            DaySource::Live(live) => live.switches_per_hour,
            DaySource::Fixture | DaySource::Loading => self.summary().switches_per_hour(),
        }
    }
    pub(crate) fn full_span(&self) -> TimeSpan {
        match &self.source {
            DaySource::Live(live) => TimeSpan::new(
                TimeOfDay::START,
                TimeOfDay::from_elapsed_millis(
                    u32::try_from(live.bounds.end - live.bounds.start)
                        .expect("validated logical day span"),
                ),
            ),
            DaySource::Fixture | DaySource::Loading => TimeSpan::DAY,
        }
    }
    pub(crate) fn pending_ranges(&self) -> &[PendingAwaySpan] {
        match &self.source {
            DaySource::Live(live) => &live.pending,
            DaySource::Fixture | DaySource::Loading => &[],
        }
    }
    /// Converts validated day coordinates back to UTC for query and annotation intents.
    pub(crate) fn utc_range(&self, span: TimeSpan) -> Option<UtcRange> {
        let DaySource::Live(live) = &self.source else {
            return None;
        };
        Some(UtcRange {
            start: live.bounds.start + i64::from(span.start().millis()),
            end: live.bounds.start + i64::from(span.end().millis()),
        })
    }
    pub(crate) fn local_clock(&self, time: TimeOfDay) -> Option<jiff::civil::Time> {
        let DaySource::Live(live) = &self.source else {
            return None;
        };
        Some(
            jiff::Timestamp::from_millisecond(live.bounds.start + i64::from(time.millis()))
                .expect("axis remains within the validated UTC day")
                .to_zoned_datetime(live.zone.clone())
                .time(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Formatter;
    use crate::model::{CategoryColor, PaletteSlot, RankingKind};
    use quartz_i18n::Language;
    fn data() -> DayData {
        let category = wire::CategoryRef::Classified {
            category: wire::Category {
                id: wire::CategoryId(7),
                uuid: wire::CategoryUuid("builtin-development".into()),
                builtin_key: Some(wire::BuiltinCategory::Development),
                name: None,
                color: "cat.8".into(),
                tendency: wire::CategoryTendency::Focus,
            }
            .into(),
        };
        let app = wire::ApplicationRef {
            id: wire::ApplicationId(91),
            exe_name: Some("editor.exe".into()),
            display_name: "Editor".into(),
            aumid: None,
        };
        let page = wire::QueryDayResponse {
            reference_range: None,
            documents: Vec::new(),
            metrics: Some(wire::DayMetrics {
                active_ms: 4000,
                focus_ratio: 0.5,
                distract_ratio: 0.25,
                unclassified_ms: 1000,
                longest_focus_ms: 2000,
                switches_per_hour: 3.5,
            }),
            hours: Some(vec![wire::HourStack {
                start_utc_ms: 0,
                end_utc_ms: 86_400_000,
                local_start: "1970-01-01T00:00:00+00:00[UTC]".into(),
                categories: vec![
                    wire::CategoryDuration {
                        category: category.clone(),
                        duration_ms: 2000,
                    },
                    wire::CategoryDuration {
                        category: wire::CategoryRef::Unclassified,
                        duration_ms: 2000,
                    },
                ],
            }]),
            applications: vec![wire::ApplicationRank {
                app: app.clone(),
                active_ms: 2000,
            }],
            versions: wire::QueryVersions("v1".into()),
            next_cursor: None,
        };
        let page: wire::QueryDayResponse =
            serde_json::from_value(serde_json::to_value(page).expect("protocol JSON"))
                .expect("typed day fixture");
        let annotation = wire::AnnotationId(
            "abcdef00-0000-4000-8000-000000000001"
                .parse()
                .expect("UUID"),
        );
        let first = wire::TimelineSegment {
            extraction: None,
            document_ref: None,
            start_utc_ms: 1,
            end_utc_ms: 2001,
            duration_ms: 2000,
            clock_segment_id: Some(wire::ClockSegmentId(1)),
            activity: wire::Activity::ActiveOverride {
                annotation: annotation.clone(),
            },
            category,
            category_source: wire::CategorySource::Annotation {
                annotation: annotation.clone(),
            },
            app: Some(app),
            title: Some("raw title".into()),
            window_instance: Some(wire::WindowInstanceRef::from_stored_id(
                wire::facts::WindowInstanceId(33),
            )),
        };
        let mut offline = first.clone();
        offline.start_utc_ms = 2001;
        offline.end_utc_ms = 4001;
        offline.activity = wire::Activity::OfflineActivity { annotation };
        offline.category = wire::CategoryRef::Unclassified;
        offline.app = None;
        offline.title = None;
        offline.window_instance = None;
        let timeline = wire::QueryTimelineResponse {
            reference_range: None,
            segments: vec![first, offline],
            versions: wire::QueryVersions("v1".into()),
            next_cursor: None,
        };
        let timeline: wire::QueryTimelineResponse =
            serde_json::from_value(serde_json::to_value(timeline).expect("timeline JSON"))
                .expect("typed timeline fixture");
        DayData {
            date: "1970-01-01".parse().expect("date"),
            bounds: UtcRange {
                start: 0,
                end: 86_400_000,
            },
            visible: UtcRange {
                start: 0,
                end: 86_400_000,
            },
            metrics: page.metrics.expect("metrics"),
            hours: page.hours.expect("hours"),
            applications: page.applications,
            categories: timeline.segments.clone(),
            raw: timeline.segments,
            pending: Vec::new(),
        }
    }
    #[test]
    fn multicolored_rollback_hour_preserves_all_categories_and_exact_labels() {
        let mut data = data();
        let hour = data.hours.first_mut().expect("hour");
        hour.categories
            .first_mut()
            .expect("development")
            .duration_ms = 3_600_000;
        hour.categories
            .get_mut(1)
            .expect("second category")
            .duration_ms = 1_800_000;
        hour.categories.push(wire::CategoryDuration {
            category: wire::CategoryRef::Unclassified,
            duration_ms: 0,
        });
        let day = Day::from_protocol(data).expect("day");
        let bar = day.hourly().first().expect("hour");
        let scale = bar.chart_scale(Duration::from_hours(1));
        assert_eq!(scale, Duration::from_mins(90));
        let shares: Vec<_> = bar
            .visible_parts()
            .map(|part| part.chart_share(scale).fraction())
            .collect();
        assert_eq!(shares.len(), 2);
        assert!((shares.first().expect("first") - 2. / 3.).abs() < f32::EPSILON);
        assert!((shares.get(1).expect("second") - 1. / 3.).abs() < f32::EPSILON);
        assert!((shares.iter().sum::<f32>() - 1.).abs() < f32::EPSILON);
        let formatter = Formatter::new(Language::English);
        let label = formatter.hourly_bar(bar, &day).expect("active hour label");
        assert!(
            label.contains(&formatter.detail_duration(Duration::from_hours(1))),
            "{label}"
        );
        assert!(
            label.contains(&formatter.detail_duration(Duration::from_mins(30))),
            "{label}"
        );
        assert_eq!(
            label.matches(';').count(),
            1,
            "zero placeholders are absent: {label}"
        );
    }
    #[test]
    fn empty_hour_has_no_meter_label_or_visible_parts() {
        let mut data = data();
        for part in &mut data.hours.first_mut().expect("hour").categories {
            part.duration_ms = 0;
        }
        let day = Day::from_protocol(data).expect("empty hour");
        let formatter = Formatter::new(Language::English);
        let bar = day.hourly().first().expect("hour");
        assert!(bar.visible_parts().next().is_none());
        assert_eq!(formatter.hourly_bar(bar, &day), None);
        let bar = HourlyBar {
            hour: 10,
            parts: Vec::new(),
        };
        assert_eq!(formatter.hourly_bar(&bar, &day), None);
    }
    #[test]
    fn partial_hour_keeps_one_hour_chart_scale() {
        let day = Day::from_protocol(data()).expect("partial hour");
        let bar = day.hourly().first().expect("hour");
        assert_eq!(
            bar.chart_scale(Duration::from_hours(1)),
            Duration::from_hours(1)
        );
    }
    #[test]
    fn rollback_hour_saturates_chart_and_preserves_exact_accessible_duration() {
        const HOUR_MS: i64 = 3_600_000;
        let mut data = data();
        let mut first = data.raw.first().expect("foreground").clone();
        first.start_utc_ms = 0;
        first.end_utc_ms = HOUR_MS;
        first.duration_ms = HOUR_MS;
        first.clock_segment_id = Some(wire::ClockSegmentId(1));
        let mut repeated = first.clone();
        repeated.end_utc_ms = HOUR_MS / 2;
        repeated.duration_ms = HOUR_MS / 2;
        repeated.clock_segment_id = Some(wire::ClockSegmentId(2));
        data.raw = vec![first.clone(), repeated];
        data.categories = data.raw.clone();
        let total = data.raw.iter().map(|segment| segment.duration_ms).sum();
        data.metrics.active_ms = total;
        data.hours = vec![wire::HourStack {
            start_utc_ms: 0,
            end_utc_ms: HOUR_MS,
            local_start: "1970-01-01T00:00:00+00:00[UTC]".into(),
            categories: vec![wire::CategoryDuration {
                category: first.category,
                duration_ms: total,
            }],
        }];
        let day = Day::from_protocol(data).expect("rollback day");
        let bar = day.hourly().first().expect("hour");
        let part = bar.parts.first().expect("category");
        assert_eq!(part.chart_share(Duration::from_hours(1)).fraction(), 1.);
        assert_eq!(part.total, Duration::from_mins(90));
        let formatter = Formatter::new(Language::English);
        let label =
            formatter.hourly_category(bar.hour, &day.category(part.category).name, part.total);
        assert!(
            label.contains(&formatter.detail_duration(Duration::from_mins(90))),
            "{label}"
        );
    }
    #[test]
    fn authoritative_aggregates_builtin_names_unclassified_and_annotation_states() {
        let day = Day::from_protocol(data()).expect("mapped day");
        assert_eq!(day.summary().active, Duration::from_millis(4000));
        assert_eq!(day.summary().focus.percent(), 50);
        assert_eq!(day.switches_per_hour(), 4);
        let name = &day.category(CategoryId::new(0)).name;
        assert_eq!(
            Formatter::new(Language::English).category_name(name),
            "Development"
        );
        assert_eq!(
            Formatter::new(Language::SimplifiedChinese).category_name(name),
            "开发"
        );
        assert_eq!(
            day.category(CategoryId::new(0)).color,
            CategoryColor::Palette(PaletteSlot::numbered(8))
        );
        assert!(day.category(CategoryId::new(1)).is_uncategorized());
        assert_eq!(
            day.main_track().len(),
            2,
            "offline activity stays on the main track"
        );
        assert_eq!(
            day.segments.len(),
            1,
            "offline activity has no app swimlane"
        );
        let segment = day.segments.first().expect("captured segment");
        assert_eq!(segment.span.start().millis(), 1);
        assert!(segment.window_instance.is_some());
        assert_eq!(segment.title, "raw title");
        assert!(
            day.detail_request(crate::model::DetailsSubject::Segment(segment.id))
                .is_some()
        );
    }
    #[test]
    fn empty_day_and_complete_application_ranking_are_preserved() {
        let mut data = data();
        let mut second = data.applications.first().expect("app").clone();
        second.app.id = wire::ApplicationId(92);
        second.app.display_name = "Second app".into();
        second.active_ms = 1000;
        data.applications.push(second);
        let day = Day::from_protocol(data.clone()).expect("ranking");
        assert_eq!(day.ranking(RankingKind::Apps).len(), 2);
        assert_eq!(
            day.ranking(RankingKind::Apps).get(1).expect("second").total,
            Duration::from_millis(1000)
        );
        assert_eq!(
            day.ranking(RankingKind::Apps)
                .first()
                .expect("first")
                .dominant_category(),
            None
        );
        data.metrics.active_ms = 0;
        data.metrics.focus_ratio = 0.;
        data.metrics.distract_ratio = 0.;
        data.metrics.unclassified_ms = 0;
        data.metrics.longest_focus_ms = 0;
        data.metrics.switches_per_hour = 0.;
        data.applications.clear();
        data.categories.clear();
        data.raw.clear();
        data.hours.first_mut().expect("hours").categories.clear();
        let empty = Day::from_protocol(data).expect("empty day");
        assert!(empty.summary().active.is_zero());
        assert!(empty.main_track().is_empty());
        assert!(empty.lanes().is_empty());
    }
    #[test]
    fn logical_day_coordinates_keep_dst_and_non_midnight_local_labels() {
        const MILLIS_PER_HOUR: i64 = 3_600_000;
        struct Case {
            start: &'static str,
            hours: u32,
            elapsed_hours: u32,
            expected_hour: i8,
        }
        for case in [
            Case {
                start: "2026-11-01T00:00:00-07:00[America/Los_Angeles]",
                hours: 25,
                elapsed_hours: 2,
                expected_hour: 1,
            },
            Case {
                start: "2026-03-08T00:00:00-08:00[America/Los_Angeles]",
                hours: 23,
                elapsed_hours: 2,
                expected_hour: 3,
            },
            Case {
                start: "2026-11-21T04:00:00-08:00[America/Los_Angeles]",
                hours: 24,
                elapsed_hours: 1,
                expected_hour: 5,
            },
        ] {
            let start: jiff::ZonedDateTime = case.start.parse().expect("core local start");
            let mut input = data();
            input.date = start.date();
            input.bounds = UtcRange {
                start: start.timestamp().as_millisecond(),
                end: start.timestamp().as_millisecond() + i64::from(case.hours) * MILLIS_PER_HOUR,
            };
            input.visible = input.bounds;
            input.categories.clear();
            input.raw.clear();
            input.pending.clear();
            let hour = input.hours.first_mut().expect("hour stack");
            hour.start_utc_ms = input.bounds.start;
            hour.end_utc_ms = input.bounds.end;
            hour.local_start = case.start.into();
            let day = Day::from_protocol(input).expect("calendar mapping");
            assert_eq!(
                day.full_span().duration(),
                Duration::from_secs(u64::from(case.hours) * 3600)
            );
            let elapsed = TimeOfDay::from_elapsed_millis(
                u32::try_from(i64::from(case.elapsed_hours) * MILLIS_PER_HOUR)
                    .expect("day elapsed"),
            );
            assert_eq!(
                day.local_clock(elapsed).expect("local axis time").hour(),
                case.expected_hour
            );
        }
    }
    #[test]
    fn mapped_day_keeps_typed_pending_spans_and_explicit_source_states() {
        let fixture = Day::new(crate::model::fixture::parts()).expect("fixture");
        assert!(matches!(fixture.source(), DaySource::Fixture));
        let mut input = data();
        assert!(matches!(
            Day::empty(input.date).source(),
            DaySource::Loading
        ));
        let expected = UtcRange {
            start: 50,
            end: 250,
        };
        input.pending.push(wire::PendingAway {
            start_utc_ms: expected.start,
            end_utc_ms: expected.end,
            duration_ms: 150,
        });
        let day = Day::from_protocol(input).expect("typed live day");
        assert!(matches!(day.source(), DaySource::Live(_)));
        let pending = day.pending_ranges().first().expect("pending interval");
        assert_eq!(pending.span.start().millis(), 50);
        assert_eq!(pending.span.end().millis(), 250);
        assert_eq!(pending.duration, Duration::from_millis(150));
        assert_eq!(day.utc_range(pending.span), Some(expected));
        assert!(day.history.is_empty());
        assert!(day.rules.is_empty());
    }
    #[test]
    fn detail_intents_keep_protocol_app_and_window_instance_filters() {
        use crate::{link::DetailTarget, model::DetailsSubject};
        let day = Day::from_protocol(data()).expect("live day");
        let segment = day.segments.first().expect("segment");
        let request = day
            .detail_request(DetailsSubject::Segment(segment.id))
            .expect("swimlane intent");
        assert_eq!(
            request.target,
            DetailTarget::Segment {
                range: UtcRange {
                    start: 1,
                    end: 2001
                },
                app: Some(wire::ApplicationId(91)),
                window_instance: segment.window_instance
            }
        );
        assert_eq!(
            day.detail_request(DetailsSubject::App(segment.app))
                .expect("ranking intent")
                .target,
            DetailTarget::App(wire::ApplicationId(91))
        );
        assert_eq!(
            day.span_for_utc(UtcRange {
                start: 1,
                end: 2001
            }),
            Some(segment.span)
        );
        assert!(
            day.span_for_utc(UtcRange {
                start: -1,
                end: 2001
            })
            .is_none()
        );
        let span = day.main_track().first().expect("main block").span();
        assert_eq!(
            day.detail_request(DetailsSubject::MainBlock(span))
                .expect("main intent")
                .target,
            DetailTarget::Segment {
                range: day.utc_range(span).expect("core UTC"),
                app: None,
                window_instance: None
            }
        );
    }
    #[test]
    fn invalid_palette_and_negative_metrics_report_mapping_errors() {
        let mut invalid = data();
        invalid.metrics.active_ms = -1;
        assert!(Day::from_protocol(invalid).is_err());
        let mut invalid = data();
        if let wire::CategoryRef::Classified { category } = &mut invalid
            .hours
            .first_mut()
            .expect("hour")
            .categories
            .first_mut()
            .expect("category")
            .category
        {
            std::sync::Arc::make_mut(category).color = "#invalid".into();
        }
        assert!(Day::from_protocol(invalid).is_err());
    }
}
