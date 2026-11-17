//! Derived values of a [`Day`]: totals, shares, streaks, hourly stacks, tracks and rankings.

use std::{collections::BTreeMap, time::Duration};

use super::{
    AppId, BuiltinCategory, CategoryId, CategoryName, Day, DayBar, Details, DetailsSubject, Glyph,
    IntervalCard, Lane, MainBlock, RankRow, RankingKind, Segment, StateBlock, StateKind, Tendency,
    TimeOfDay, TimeSpan, TitleRow,
};

/// A fraction of active time between zero and one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Share(f32);

impl Share {
    /// `part` divided by `total`; zero when `total` is zero.
    #[must_use]
    pub fn of(part: Duration, total: Duration) -> Self {
        if total.is_zero() {
            Self(0.)
        } else {
            Self(part.as_secs_f32() / total.as_secs_f32())
        }
    }

    /// The fraction itself.
    #[must_use]
    pub const fn fraction(self) -> f32 {
        self.0
    }

    /// Whole percent, rounded to nearest.
    #[must_use]
    pub fn percent(self) -> u32 {
        // A share lies in 0..=1, so the rounded percent always fits.
        (self.0 * 100.).round() as u32
    }
}

/// Summary row of the status card (07 section 3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DaySummary {
    pub active: Duration,
    pub focus: Share,
    pub distraction: Share,
    pub unclassified: Duration,
    pub longest_focus_streak: Duration,
    pub switches_per_hour: f32,
}

/// Active time in one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategoryTotal {
    pub category: CategoryId,
    pub total: Duration,
}

/// One bar of the hourly stacked chart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HourlyBar {
    pub hour: u32,
    pub parts: Vec<(CategoryId, Duration)>,
}

impl HourlyBar {
    /// Sum of all parts.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.parts.iter().map(|(_, total)| *total).sum()
    }
}

impl Day {
    fn ordered_segments(&self) -> Vec<&Segment> {
        let mut segments: Vec<&Segment> = self.segments.iter().collect();
        segments.sort_by_key(|segment| segment.span.start);
        segments
    }

    /// Total foreground time.
    #[must_use]
    pub fn active_total(&self) -> Duration {
        self.segments
            .iter()
            .map(|segment| segment.span.duration())
            .sum()
    }

    /// Active time per category, longest first, without empty categories.
    #[must_use]
    pub fn category_totals(&self) -> Vec<CategoryTotal> {
        let mut totals: Vec<CategoryTotal> = self
            .categories
            .iter()
            .map(|category| CategoryTotal {
                category: category.id,
                total: self
                    .segments
                    .iter()
                    .filter(|segment| segment.category == category.id)
                    .map(|segment| segment.span.duration())
                    .sum(),
            })
            .filter(|entry| !entry.total.is_zero())
            .collect();
        totals.sort_by_key(|entry| std::cmp::Reverse(entry.total));
        totals
    }

    /// Status card values.
    #[must_use]
    pub fn summary(&self) -> DaySummary {
        let active = self.active_total();
        let tendency_total = |wanted: Tendency| -> Duration {
            self.segments
                .iter()
                .filter(|segment| self.category(segment.category).tendency == wanted)
                .map(|segment| segment.span.duration())
                .sum()
        };
        let unclassified = self
            .segments
            .iter()
            .filter(|segment| {
                self.category(segment.category).name
                    == CategoryName::Builtin(BuiltinCategory::Uncategorized)
            })
            .map(|segment| segment.span.duration())
            .sum();
        DaySummary {
            active,
            focus: Share::of(tendency_total(Tendency::Focus), active),
            distraction: Share::of(tendency_total(Tendency::Distraction), active),
            unclassified,
            longest_focus_streak: self.longest_focus_streak(),
            switches_per_hour: self.switches_per_hour(active),
        }
    }

    /// Longest run of back-to-back focus segments.
    fn longest_focus_streak(&self) -> Duration {
        let mut longest = Duration::ZERO;
        let mut current = Duration::ZERO;
        let mut previous_end: Option<TimeOfDay> = None;
        for segment in self.ordered_segments() {
            let focus = self.category(segment.category).tendency == Tendency::Focus;
            let contiguous = previous_end == Some(segment.span.start);
            if !focus {
                current = Duration::ZERO;
            } else {
                if !contiguous {
                    current = Duration::ZERO;
                }
                current += segment.span.duration();
                longest = longest.max(current);
            }
            previous_end = Some(segment.span.end);
        }
        longest
    }

    fn switches_per_hour(&self, active: Duration) -> f32 {
        let segments = self.ordered_segments();
        let switches = segments
            .windows(2)
            .filter(|pair| matches!(pair, [a, b] if a.app != b.app))
            .count();
        let hours = active.as_secs_f32() / 3600.;
        if hours <= 0. {
            0.
        } else {
            switches as f32 / hours
        }
    }

    /// First to last activity of the day.
    #[must_use]
    pub fn activity_range(&self) -> TimeSpan {
        let starts = self
            .segments
            .iter()
            .map(|segment| segment.span.start)
            .chain(self.states.iter().map(|state| state.span.start));
        let ends = self
            .segments
            .iter()
            .map(|segment| segment.span.end)
            .chain(self.states.iter().map(|state| state.span.end));
        match (starts.min(), ends.max()) {
            (Some(start), Some(end)) => TimeSpan::new(start, end),
            _ => TimeSpan::new(TimeOfDay::hm(9, 0), TimeOfDay::hm(18, 0)),
        }
    }

    /// Category main track with adjacent same-category activity merged (07 section 4.2).
    #[must_use]
    pub fn main_track(&self) -> Vec<MainBlock> {
        let mut items: Vec<MainBlock> = self
            .segments
            .iter()
            .map(|segment| MainBlock::Active {
                span: segment.span,
                category: segment.category,
            })
            .chain(self.states.iter().cloned().map(MainBlock::State))
            .collect();
        items.sort_by_key(|item| item.span().start);
        let mut merged: Vec<MainBlock> = Vec::with_capacity(items.len());
        for item in items {
            if let (
                Some(MainBlock::Active {
                    span: last_span,
                    category: last_category,
                }),
                MainBlock::Active { span, category },
            ) = (merged.last_mut(), &item)
                && *last_category == *category
                && last_span.end == span.start
            {
                last_span.end = span.end;
                continue;
            }
            merged.push(item);
        }
        merged
    }

    /// Application swimlanes, longest application first.
    #[must_use]
    pub fn lanes(&self) -> Vec<Lane> {
        let mut totals: BTreeMap<AppId, Duration> = BTreeMap::new();
        for segment in &self.segments {
            *totals.entry(segment.app).or_default() += segment.span.duration();
        }
        let mut apps: Vec<(AppId, Duration)> = totals.into_iter().collect();
        apps.sort_by_key(|(_, total)| std::cmp::Reverse(*total));
        apps.into_iter()
            .map(|(app, _)| Lane {
                app,
                segments: self
                    .ordered_segments()
                    .into_iter()
                    .filter(|segment| segment.app == app)
                    .map(|segment| segment.id)
                    .collect(),
            })
            .collect()
    }

    /// Hourly stacks from the first to the last active hour.
    #[must_use]
    pub fn hourly(&self) -> Vec<HourlyBar> {
        let range = self.activity_range();
        (range.start.hour()..=range.end.earlier_by_minutes(1).hour())
            .map(|hour| {
                let window = TimeSpan::new(TimeOfDay::hm(hour, 0), TimeOfDay::hm(hour + 1, 0));
                let parts = self
                    .categories
                    .iter()
                    .filter_map(|category| {
                        let total: Duration = self
                            .segments
                            .iter()
                            .filter(|segment| segment.category == category.id)
                            .map(|segment| segment.span.overlap(window))
                            .sum();
                        (!total.is_zero()).then_some((category.id, total))
                    })
                    .collect();
                HourlyBar { hour, parts }
            })
            .collect()
    }

    /// The first idle span that still needs a label.
    #[must_use]
    pub fn pending_away(&self) -> Option<&StateBlock> {
        self.states
            .iter()
            .find(|state| state.kind == StateKind::Idle && state.pending_away)
    }

    /// Details sidebar content for a clicked interval or ranking row.
    #[must_use]
    pub fn details(&self, subject: DetailsSubject) -> Option<Details> {
        let (app_id, interval) = match subject {
            DetailsSubject::Segment(id) => {
                let segment = self.segment(id)?;
                (
                    segment.app,
                    Some(IntervalCard {
                        span: segment.span,
                        title: segment.title.clone(),
                        category: segment.category,
                        source: segment.source.clone(),
                        merged_count: segment.merged_count,
                    }),
                )
            }
            DetailsSubject::App(id) => (id, None),
        };
        let app_segments: Vec<&Segment> = self
            .ordered_segments()
            .into_iter()
            .filter(|segment| segment.app == app_id)
            .collect();
        let category = interval
            .as_ref()
            .map(|card| card.category)
            .or_else(|| {
                self.category_totals_of(&app_segments)
                    .into_iter()
                    .max_by_key(|entry| entry.total)
                    .map(|entry| entry.category)
            })
            .unwrap_or(super::FALLBACK_CATEGORY.id);
        let today: Duration = app_segments
            .iter()
            .map(|segment| segment.span.duration())
            .sum();
        let previous = self
            .week_totals
            .iter()
            .find(|week| week.app == app_id)
            .map_or([Duration::ZERO; 6], |week| week.previous_days);
        let mut week: Vec<DayBar> = previous
            .iter()
            .enumerate()
            .map(|(index, total)| DayBar {
                weekday: self.date.weekday.back(6 - index),
                is_today: false,
                total: *total,
            })
            .collect();
        week.push(DayBar {
            weekday: self.date.weekday,
            is_today: true,
            total: today,
        });
        let mut titles: BTreeMap<&str, Duration> = BTreeMap::new();
        for segment in &app_segments {
            *titles.entry(segment.title.as_str()).or_default() += segment.span.duration();
        }
        let mut titles: Vec<TitleRow> = titles
            .into_iter()
            .map(|(title, total)| TitleRow {
                title: title.to_owned(),
                total,
            })
            .collect();
        titles.sort_by_key(|row| std::cmp::Reverse(row.total));
        titles.truncate(4);
        Some(Details {
            app: self.app(app_id).clone(),
            category,
            interval,
            week,
            titles,
            explanation: self
                .explanations
                .iter()
                .find(|explanation| explanation.app == app_id)
                .cloned(),
        })
    }

    fn category_totals_of(&self, segments: &[&Segment]) -> Vec<CategoryTotal> {
        self.categories
            .iter()
            .map(|category| CategoryTotal {
                category: category.id,
                total: segments
                    .iter()
                    .filter(|segment| segment.category == category.id)
                    .map(|segment| segment.span.duration())
                    .sum(),
            })
            .collect()
    }

    /// Ranking rows for a tab, longest first.
    #[must_use]
    pub fn ranking(&self, kind: RankingKind) -> Vec<RankRow> {
        let mut rows = match kind {
            RankingKind::Mixed => [
                RankingKind::Apps,
                RankingKind::Domains,
                RankingKind::Documents,
            ]
            .into_iter()
            .flat_map(|single| self.ranking(single))
            .collect(),
            single => self.rank_single(single),
        };
        rows.sort_by_key(|row| std::cmp::Reverse(row.total));
        rows
    }

    fn rank_single(&self, kind: RankingKind) -> Vec<RankRow> {
        let mut groups: BTreeMap<String, Vec<&Segment>> = BTreeMap::new();
        for segment in &self.segments {
            let key = match kind {
                RankingKind::Domains => segment.domain.clone(),
                RankingKind::Documents => segment.document.clone(),
                _ => Some(self.app(segment.app).name.clone()),
            };
            if let Some(key) = key {
                groups.entry(key).or_default().push(segment);
            }
        }
        groups
            .into_iter()
            .filter_map(|(name, segments)| {
                let first = segments.first()?;
                let parts = self
                    .categories
                    .iter()
                    .filter_map(|category| {
                        let total: Duration = segments
                            .iter()
                            .filter(|segment| segment.category == category.id)
                            .map(|segment| segment.span.duration())
                            .sum();
                        (!total.is_zero()).then_some((category.id, total))
                    })
                    .collect();
                let glyph = match kind {
                    RankingKind::Domains => Glyph::Website,
                    RankingKind::Documents => Glyph::Document,
                    _ => self.app(first.app).glyph,
                };
                Some(RankRow {
                    name,
                    glyph,
                    total: segments.iter().map(|segment| segment.span.duration()).sum(),
                    parts,
                    subject: DetailsSubject::App(first.app),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_rounds_to_whole_percent() {
        let share = Share::of(Duration::from_secs(271 * 60), Duration::from_secs(425 * 60));
        assert_eq!(share.percent(), 64);
        assert_eq!(Share::of(Duration::ZERO, Duration::ZERO).percent(), 0);
    }

    #[test]
    fn category_totals_and_hourly_sum_to_active_time() {
        let day = crate::app::model::Day::sample();
        let active = day.active_total();
        let totals: Duration = day.category_totals().iter().map(|entry| entry.total).sum();
        let hourly: Duration = day.hourly().iter().map(HourlyBar::total).sum();
        assert_eq!(totals, active);
        assert_eq!(hourly, active);
    }

    #[test]
    fn main_track_merges_same_category_neighbors_only() {
        let day = crate::app::model::Day::sample();
        let track = day.main_track();
        assert!(track.len() < day.segments.len() + day.states.len());
        for pair in track.windows(2) {
            if let [
                MainBlock::Active {
                    span: a,
                    category: ca,
                },
                MainBlock::Active {
                    span: b,
                    category: cb,
                },
            ] = pair
            {
                assert!(ca != cb || a.end != b.start, "unmerged neighbors");
            }
        }
    }

    #[test]
    fn focus_streak_breaks_on_gaps_and_other_categories() {
        let day = crate::app::model::Day::sample();
        let summary = day.summary();
        assert!(summary.longest_focus_streak <= summary.active);
        assert!(summary.longest_focus_streak >= Duration::from_secs(45 * 60));
        assert!(summary.focus.fraction() > summary.distraction.fraction());
    }

    #[test]
    fn rankings_are_sorted_and_cover_active_time() {
        let day = crate::app::model::Day::sample();
        let apps = day.ranking(RankingKind::Apps);
        let total: Duration = apps.iter().map(|row| row.total).sum();
        assert_eq!(total, day.active_total());
        assert!(apps.windows(2).all(|pair| pair[0].total >= pair[1].total));
    }
}
