//! Everything the Today page shows for one logical day, validated once when it is built.

mod live;
use live::LiveDay;
#[cfg(windows)]
pub(crate) use live::PendingAwaySpan;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use jiff::civil::Date;
use snafu::{Snafu, ensure};

use super::{
    application::{AppId, AppInfo},
    category::{Category, CategoryId},
    ranking::{RankRow, RankingKind, Rankings},
    rule::{Rule, RuleId},
    segment::{ClassificationSource, Segment, SegmentId},
    state::{StateBlock, StateKind},
    summary::{CategoryTotal, DaySummary, HourlyBar},
    time::{TimeOfDay, TimeSpan},
    track::{Lane, MainBlock},
};

/// Days in the 7-day chart of the details sidebar, including the day shown.
pub(crate) const WEEK_DAYS: usize = 7;
/// Days of history before the day shown.
pub(crate) const EARLIER_DAYS: usize = WEEK_DAYS - 1;

/// Active time of one application on the days before the day shown, oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct EarlierDays(pub [Duration; EARLIER_DAYS]);

/// The raw facts of one day, before [`Day::new`] validates them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    all(not(test), not(feature = "preview")),
    expect(dead_code, reason = "used by preview fixtures and their model tests")
)]
pub(crate) struct DayParts {
    pub(crate) date: Date,
    /// Categories in list order, which is also the stacking order of charts.
    pub(crate) categories: Vec<Category>,
    pub(crate) apps: Vec<AppInfo>,
    pub(crate) segments: Vec<Segment>,
    pub(crate) states: Vec<StateBlock>,
    /// Earlier days of every application in `apps`.
    pub(crate) history: BTreeMap<AppId, EarlierDays>,
    /// Every rule a segment names.
    pub(crate) rules: Vec<Rule>,
}

/// The facts of a day contradict each other.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
pub(crate) enum BuildDayError {
    #[snafu(display("category {id:?} is listed twice"))]
    DuplicateCategory { id: CategoryId },
    #[snafu(display("application {id:?} is listed twice"))]
    DuplicateApp { id: AppId },
    #[snafu(display("segment {id:?} is listed twice"))]
    DuplicateSegment { id: SegmentId },
    #[snafu(display("rule {id:?} is listed twice"))]
    DuplicateRule { id: RuleId },
    #[snafu(display("segment {segment:?} names unknown category {category:?}"))]
    UnknownCategory {
        segment: SegmentId,
        category: CategoryId,
    },
    #[snafu(display("segment {segment:?} names unknown application {app:?}"))]
    UnknownApp { segment: SegmentId, app: AppId },
    #[snafu(display("segment {segment:?} names unknown rule {rule:?}"))]
    UnknownRule { segment: SegmentId, rule: RuleId },
    #[snafu(display("idle span at {start} names unknown category {category:?}"))]
    UnknownIdleCategory {
        start: TimeOfDay,
        category: CategoryId,
    },
    #[snafu(display("segment {segment:?} covers no time"))]
    EmptySegment { segment: SegmentId },
    #[snafu(display("state span at {start} covers no time"))]
    EmptyState { start: TimeOfDay },
    #[snafu(display("spans {first} and {second} overlap"))]
    OverlappingSpans { first: TimeSpan, second: TimeSpan },
    #[snafu(display("application {app:?} has no history"))]
    MissingHistory { app: AppId },
    #[snafu(display("history names unknown application {app:?}"))]
    UnknownHistoryApp { app: AppId },
}

/// Values derived from the facts once, so rendering only reads them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Derived {
    activity_range: Option<TimeSpan>,
    summary: DaySummary,
    category_totals: Vec<CategoryTotal>,
    hourly: Vec<HourlyBar>,
    main_track: Vec<MainBlock>,
    lanes: Vec<Lane>,
    rankings: Rankings,
}

/// The model's origin and loading state are explicit; only live days carry core display metadata.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DaySource {
    #[cfg_attr(
        all(not(test), not(feature = "preview")),
        expect(dead_code, reason = "used by preview fixtures and their model tests")
    )]
    Fixture,
    Loading,
    Live(Box<LiveDay>),
}

/// Everything the Today page shows for one logical day. Every id inside refers to an item of the
/// same day, and no two spans overlap.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Day {
    pub(super) date: Date,
    pub(super) categories: Vec<Category>,
    pub(super) apps: Vec<AppInfo>,
    /// Ordered by start.
    pub(super) segments: Vec<Segment>,
    pub(super) segment_index: BTreeMap<SegmentId, usize>,
    /// Ordered by start.
    pub(super) states: Vec<StateBlock>,
    pub(super) history: BTreeMap<AppId, EarlierDays>,
    pub(super) rules: Vec<Rule>,
    derived: Derived,
    source: DaySource,
}

impl Day {
    /// Validates the facts of a day and derives what the Today page shows.
    ///
    /// # Errors
    /// Returns [`BuildDayError`] for duplicate ids, references to items that are not part of the
    /// day, empty or overlapping spans, and applications without history.
    #[cfg_attr(
        all(not(test), not(feature = "preview")),
        expect(dead_code, reason = "used by preview fixtures and their model tests")
    )]
    pub(crate) fn new(parts: DayParts) -> Result<Self, BuildDayError> {
        let DayParts {
            date,
            categories,
            apps,
            mut segments,
            mut states,
            history,
            rules,
        } = parts;
        let category_ids = unique(categories.iter().map(|category| category.id), |id| {
            BuildDayError::DuplicateCategory { id }
        })?;
        let app_ids = unique(apps.iter().map(|app| app.id), |id| {
            BuildDayError::DuplicateApp { id }
        })?;
        let rule_ids = unique(rules.iter().map(|rule| rule.id), |id| {
            BuildDayError::DuplicateRule { id }
        })?;
        unique(segments.iter().map(|segment| segment.id), |id| {
            BuildDayError::DuplicateSegment { id }
        })?;
        for segment in &segments {
            validate_segment(segment, &category_ids, &app_ids, &rule_ids)?;
        }
        for state in &states {
            ensure!(
                !state.span.is_empty(),
                EmptyStateSnafu {
                    start: state.span.start()
                }
            );
            if let StateKind::Idle {
                preceding: Some(category),
                ..
            } = state.kind
            {
                ensure!(
                    category_ids.contains(&category),
                    UnknownIdleCategorySnafu {
                        start: state.span.start(),
                        category
                    }
                );
            }
        }
        for app in &app_ids {
            ensure!(history.contains_key(app), MissingHistorySnafu { app: *app });
        }
        if let Some(app) = history.keys().find(|app| !app_ids.contains(app)) {
            return UnknownHistoryAppSnafu { app: *app }.fail();
        }
        segments.sort_by_key(|segment| segment.span.start());
        states.sort_by_key(|state| state.span.start());
        ensure_disjoint(&segments, &states)?;
        let segment_index = segments
            .iter()
            .enumerate()
            .map(|(index, segment)| (segment.id, index))
            .collect();
        let mut day = Self {
            date,
            categories,
            apps,
            segments,
            segment_index,
            states,
            history,
            rules,
            derived: Derived::default(),
            source: DaySource::Fixture,
        };
        day.derived = Derived {
            activity_range: day.compute_activity_range(),
            summary: day.compute_summary(),
            category_totals: day.compute_category_totals(),
            hourly: day.compute_hourly(),
            main_track: day.compute_main_track(),
            lanes: day.compute_lanes(),
            rankings: day.compute_rankings(),
        };
        Ok(day)
    }

    pub(crate) fn source(&self) -> &DaySource {
        &self.source
    }

    /// The calendar date of the logical day.
    #[must_use]
    pub(crate) const fn date(&self) -> Date {
        self.date
    }

    /// The category with `id`.
    ///
    /// # Panics
    /// Panics when `id` is not a category of this day; [`Day::new`] validates every category id
    /// stored in the day.
    #[must_use]
    pub(crate) fn category(&self, id: CategoryId) -> &Category {
        self.categories
            .iter()
            .find(|category| category.id == id)
            .unwrap_or_else(|| panic!("category {id:?} is validated by Day::new"))
    }

    /// The application with `id`.
    ///
    /// # Panics
    /// Panics when `id` is not an application of this day; [`Day::new`] validates every
    /// application id stored in the day.
    #[must_use]
    pub(crate) fn app(&self, id: AppId) -> &AppInfo {
        self.apps
            .iter()
            .find(|app| app.id == id)
            .unwrap_or_else(|| panic!("application {id:?} is validated by Day::new"))
    }

    /// The rule with `id`.
    ///
    /// # Panics
    /// Panics when `id` is not a rule of this day; [`Day::new`] validates every rule id stored in
    /// the day.
    #[must_use]
    pub(crate) fn rule(&self, id: RuleId) -> &Rule {
        self.rules
            .iter()
            .find(|rule| rule.id == id)
            .unwrap_or_else(|| panic!("rule {id:?} is validated by Day::new"))
    }

    /// The segment with `id`; `None` when it is not part of this day.
    #[must_use]
    pub(crate) fn segment(&self, id: SegmentId) -> Option<&Segment> {
        self.segment_index
            .get(&id)
            .and_then(|index| self.segments.get(*index))
    }

    /// Whether `id` is an application of this day.
    #[must_use]
    pub(crate) fn has_app(&self, id: AppId) -> bool {
        self.apps.iter().any(|app| app.id == id)
    }

    /// Active time of `app` on the days before this one.
    ///
    /// # Panics
    /// Panics when `app` is not part of this day; [`Day::new`] validates that every application
    /// has history.
    #[must_use]
    pub(crate) fn earlier_days(&self, app: AppId) -> EarlierDays {
        *self
            .history
            .get(&app)
            .unwrap_or_else(|| panic!("application {app:?} history is validated by Day::new"))
    }

    /// First to last activity of the day; `None` for a day without segments or states.
    #[must_use]
    pub(crate) const fn activity_range(&self) -> Option<TimeSpan> {
        self.derived.activity_range
    }

    /// Status card values.
    #[must_use]
    pub(crate) const fn summary(&self) -> &DaySummary {
        &self.derived.summary
    }

    /// Active time per category, longest first, without empty categories.
    #[must_use]
    pub(crate) fn category_totals(&self) -> &[CategoryTotal] {
        &self.derived.category_totals
    }

    /// Hourly stacks from the first to the last active hour; empty without activity.
    #[must_use]
    pub(crate) fn hourly(&self) -> &[HourlyBar] {
        &self.derived.hourly
    }

    /// Category main track with adjacent same-category activity merged (07 section 4.2).
    #[must_use]
    pub(crate) fn main_track(&self) -> &[MainBlock] {
        &self.derived.main_track
    }

    /// The main-track block starting at `start`.
    #[must_use]
    pub(crate) fn main_block_at(&self, start: TimeOfDay) -> Option<&MainBlock> {
        let track = self.main_track();
        if matches!(self.source, DaySource::Live(_)) {
            return track.iter().find(|block| block.span().start() == start);
        }
        let index = track
            .binary_search_by_key(&start, |block| block.span().start())
            .ok()?;
        track.get(index)
    }

    /// Application swimlanes, longest application first.
    #[must_use]
    pub(crate) fn lanes(&self) -> &[Lane] {
        &self.derived.lanes
    }

    /// The segments of a swimlane of this day, in time order.
    ///
    /// # Panics
    /// Panics when `lane` names a segment not belonging to this day; lanes are derived from the
    /// day's own validated segments.
    pub(crate) fn lane_segments<'day>(
        &'day self,
        lane: &'day Lane,
    ) -> impl Iterator<Item = &'day Segment> {
        lane.segments.iter().map(|id| {
            self.segment(*id)
                .unwrap_or_else(|| panic!("lane segment {id:?} is validated by Day::new"))
        })
    }

    /// Ranking rows for a tab, longest first, ties by name.
    #[must_use]
    pub(crate) fn ranking(&self, kind: RankingKind) -> &[RankRow] {
        if let DaySource::Live(live) = &self.source {
            return match kind {
                RankingKind::Apps => &live.apps_ranking,
                RankingKind::Documents => &live.documents_ranking,
                RankingKind::Domains | RankingKind::Mixed => &[],
            };
        }
        self.derived.rankings.get(kind)
    }

    /// Website ranking rows for a live day, longest first; empty for loading or fixture days.
    #[must_use]
    pub(crate) fn websites(&self) -> &[RankRow] {
        match &self.source {
            DaySource::Live(live) => &live.websites_ranking,
            DaySource::Fixture | DaySource::Loading => &[],
        }
    }

    /// The first idle span that still needs a label.
    #[must_use]
    pub(crate) fn pending_away(&self) -> Option<&StateBlock> {
        self.states.iter().find(|state| state.is_pending_away())
    }
}

/// Collects `ids`, failing with `duplicate` on the first one seen twice.
#[cfg_attr(
    all(not(test), not(feature = "preview")),
    expect(dead_code, reason = "used by preview fixtures and their model tests")
)]
fn unique<T: Ord + Copy>(
    ids: impl IntoIterator<Item = T>,
    duplicate: impl Fn(T) -> BuildDayError,
) -> Result<BTreeSet<T>, BuildDayError> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(duplicate(id));
        }
    }
    Ok(seen)
}

#[cfg_attr(
    all(not(test), not(feature = "preview")),
    expect(dead_code, reason = "used by preview fixtures and their model tests")
)]
fn validate_segment(
    segment: &Segment,
    category_ids: &BTreeSet<CategoryId>,
    app_ids: &BTreeSet<AppId>,
    rule_ids: &BTreeSet<RuleId>,
) -> Result<(), BuildDayError> {
    let id = segment.id;
    ensure!(!segment.span.is_empty(), EmptySegmentSnafu { segment: id });
    ensure!(
        category_ids.contains(&segment.category),
        UnknownCategorySnafu {
            segment: id,
            category: segment.category
        }
    );
    ensure!(
        app_ids.contains(&segment.app),
        UnknownAppSnafu {
            segment: id,
            app: segment.app
        }
    );
    if let ClassificationSource::Rule { rule, .. } = &segment.source {
        ensure!(
            rule_ids.contains(rule),
            UnknownRuleSnafu {
                segment: id,
                rule: *rule
            }
        );
    }
    Ok(())
}

/// Fails when any two spans of segments and states share time.
#[cfg_attr(
    all(not(test), not(feature = "preview")),
    expect(dead_code, reason = "used by preview fixtures and their model tests")
)]
fn ensure_disjoint(segments: &[Segment], states: &[StateBlock]) -> Result<(), BuildDayError> {
    let mut spans: Vec<TimeSpan> = segments
        .iter()
        .map(|segment| segment.span)
        .chain(states.iter().map(|state| state.span))
        .collect();
    spans.sort_by_key(|span| span.start());
    let mut previous: Option<TimeSpan> = None;
    for span in spans {
        if let Some(first) = previous {
            ensure!(
                first.end() <= span.start(),
                OverlappingSpansSnafu {
                    first,
                    second: span
                }
            );
        }
        previous = Some(span);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AwayLabel,
        fixture::{BROWSER, EDITOR, EDITOR_RULE, NONE, WORK, day, parts, segment, span, state},
    };

    fn build(segments: Vec<Segment>, states: Vec<StateBlock>) -> Result<Day, BuildDayError> {
        Day::new(DayParts {
            segments,
            states,
            ..parts()
        })
    }

    #[test]
    fn references_outside_the_day_are_rejected() {
        let unknown_category = CategoryId::new(99);
        let mut stray = segment(1, EDITOR, unknown_category, (9, 0), (9, 30));
        assert_eq!(
            build(vec![stray.clone()], vec![]),
            Err(BuildDayError::UnknownCategory {
                segment: stray.id,
                category: unknown_category
            })
        );
        stray.category = WORK;
        stray.app = AppId::new(99);
        assert!(matches!(
            build(vec![stray.clone()], vec![]),
            Err(BuildDayError::UnknownApp { .. })
        ));
        stray.app = EDITOR;
        stray.source = ClassificationSource::Rule {
            rule: RuleId::new(99),
            actual: "editor.exe".to_owned(),
        };
        assert!(matches!(
            build(vec![stray], vec![]),
            Err(BuildDayError::UnknownRule { .. })
        ));
        let idle = state(
            StateKind::Idle {
                preceding: Some(unknown_category),
                away: AwayLabel::Settled,
            },
            (9, 0),
            (9, 30),
        );
        assert!(matches!(
            build(vec![], vec![idle]),
            Err(BuildDayError::UnknownIdleCategory { .. })
        ));
    }

    #[test]
    fn duplicate_ids_empty_and_overlapping_spans_are_rejected() {
        let first = segment(1, EDITOR, WORK, (9, 0), (9, 30));
        assert_eq!(
            build(vec![first.clone(), first.clone()], vec![]),
            Err(BuildDayError::DuplicateSegment { id: first.id })
        );
        assert!(matches!(
            build(vec![segment(2, EDITOR, WORK, (9, 0), (9, 0))], vec![]),
            Err(BuildDayError::EmptySegment { .. })
        ));
        let locked = state(StateKind::Locked, (9, 15), (10, 0));
        assert_eq!(
            build(vec![first], vec![locked]),
            Err(BuildDayError::OverlappingSpans {
                first: span((9, 0), (9, 30)),
                second: span((9, 15), (10, 0)),
            })
        );
    }

    #[test]
    fn every_application_needs_history_and_history_needs_an_application() {
        let mut missing = parts();
        missing.history.remove(&BROWSER);
        assert_eq!(
            Day::new(missing),
            Err(BuildDayError::MissingHistory { app: BROWSER })
        );
        let mut extra = parts();
        extra.history.insert(AppId::new(99), EarlierDays::default());
        assert!(matches!(
            Day::new(extra),
            Err(BuildDayError::UnknownHistoryApp { .. })
        ));
    }

    #[test]
    fn segments_are_ordered_and_looked_up_by_id() {
        let mut later = segment(1, EDITOR, WORK, (10, 0), (11, 0));
        later.source = ClassificationSource::Rule {
            rule: EDITOR_RULE,
            actual: "editor.exe".to_owned(),
        };
        let day = day(
            vec![later, segment(2, BROWSER, NONE, (9, 0), (10, 0))],
            vec![],
        );
        let starts: Vec<TimeOfDay> = day
            .segments
            .iter()
            .map(|segment| segment.span.start())
            .collect();
        assert_eq!(starts, [TimeOfDay::hm(9, 0), TimeOfDay::hm(10, 0)]);
        assert_eq!(
            day.segment(SegmentId::new(1)).map(|segment| segment.app),
            Some(EDITOR)
        );
        assert_eq!(day.segment(SegmentId::new(3)), None);
        assert_eq!(day.rule(EDITOR_RULE).priority, 1);
    }

    #[test]
    fn main_blocks_are_found_by_start() {
        let day = day(
            vec![segment(1, EDITOR, WORK, (9, 0), (10, 0))],
            vec![state(StateKind::Locked, (10, 0), (11, 0))],
        );
        assert!(matches!(
            day.main_block_at(TimeOfDay::hm(10, 0)),
            Some(MainBlock::State(_))
        ));
        assert_eq!(day.main_block_at(TimeOfDay::hm(9, 30)), None);
    }

    #[test]
    fn pending_away_skips_settled_idle_spans() {
        let idle = |away, from, to| {
            state(
                StateKind::Idle {
                    preceding: Some(WORK),
                    away,
                },
                from,
                to,
            )
        };
        let day = day(
            vec![],
            vec![
                idle(AwayLabel::Settled, (9, 0), (9, 30)),
                idle(AwayLabel::Pending, (10, 0), (10, 30)),
            ],
        );
        assert_eq!(
            day.pending_away().map(|state| state.span),
            Some(span((10, 0), (10, 30)))
        );
    }
}
