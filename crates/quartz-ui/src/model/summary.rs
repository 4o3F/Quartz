//! Status card values, category totals and hourly stacks of a [`Day`].

use std::{collections::BTreeMap, time::Duration};

use super::{
    category::{CategoryId, Tendency},
    day::Day,
    segment::Segment,
    time::{SECONDS_PER_HOUR, TimeOfDay, TimeSpan},
};

/// A part of a whole duration, such as focus time of active time; never more than the whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct Share {
    part: Duration,
    whole: Duration,
}

impl Share {
    /// `part` of `whole`. A zero whole gives a zero share.
    ///
    /// # Panics
    /// Panics when `part` exceeds `whole`; callers pass a sub-total of the whole.
    #[must_use]
    pub(crate) fn of(part: Duration, whole: Duration) -> Self {
        assert!(
            part <= whole,
            "share part {part:?} exceeds the whole {whole:?}"
        );
        Self { part, whole }
    }

    /// The fraction between zero and one, for layout.
    #[must_use]
    pub(crate) fn fraction(self) -> f32 {
        if self.whole.is_zero() {
            0.
        } else {
            self.part.as_secs_f32() / self.whole.as_secs_f32()
        }
    }

    /// Whole percent, rounded half up.
    #[must_use]
    pub(crate) fn percent(self) -> u32 {
        let whole = self.whole.as_millis();
        if whole == 0 {
            return 0;
        }
        let percent = (self.part.as_millis() * 100 + whole / 2) / whole;
        u32::try_from(percent).expect("a share is at most 100 percent")
    }
}

/// Summary row of the status card (07 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct DaySummary {
    /// Total foreground time.
    pub(crate) active: Duration,
    /// Share of active time in focus categories.
    pub(crate) focus: Share,
    /// Share of active time in distraction categories.
    pub(crate) distraction: Share,
    /// Time in the unclassified category.
    pub(crate) unclassified: Duration,
    /// Longest run of back-to-back focus segments.
    pub(crate) longest_focus_streak: Duration,
    /// Changes of foreground application between neighboring segments.
    pub(crate) switches: u32,
}

impl DaySummary {
    /// Application switches per active hour, rounded half up; zero without active time.
    #[must_use]
    pub(crate) fn switches_per_hour(&self) -> u64 {
        let active = self.active.as_secs();
        if active == 0 {
            return 0;
        }
        let per_hour = u64::from(self.switches) * u64::from(SECONDS_PER_HOUR);
        (per_hour + active / 2) / active
    }
}

/// Active time in one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CategoryTotal {
    pub(crate) category: CategoryId,
    pub(crate) total: Duration,
}

/// One bar of the hourly stacked chart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct HourlyBar {
    /// Hour of the day the bar covers.
    pub(crate) hour: u32,
    /// Time per category inside the hour, in category order.
    pub(crate) parts: Vec<CategoryTotal>,
}

impl HourlyBar {
    /// Normal hours use the fixed axis scale; repeated hours keep every category visible.
    pub(crate) fn chart_scale(&self, scale: Duration) -> Duration {
        total(self.parts.iter().map(|part| part.total)).max(scale)
    }
    pub(crate) fn visible_parts(&self) -> impl Iterator<Item = &CategoryTotal> {
        self.parts.iter().filter(|part| !part.total.is_zero())
    }
}

impl CategoryTotal {
    /// Saturates the visual height while retaining the measured duration.
    pub(crate) fn chart_share(self, scale: Duration) -> Share {
        Share::of(self.total.min(scale), scale)
    }
}

/// Sum of durations.
#[must_use]
pub(crate) fn total(durations: impl IntoIterator<Item = Duration>) -> Duration {
    durations.into_iter().sum()
}

impl Day {
    /// Time per category of `parts`, in category order, without empty categories.
    pub(super) fn totals_by_category(
        &self,
        parts: impl IntoIterator<Item = CategoryTotal>,
    ) -> Vec<CategoryTotal> {
        let mut sums: BTreeMap<CategoryId, Duration> = BTreeMap::new();
        for part in parts {
            *sums.entry(part.category).or_default() += part.total;
        }
        self.categories
            .iter()
            .filter_map(|category| {
                let total = sums.get(&category.id).copied()?;
                (!total.is_zero()).then_some(CategoryTotal {
                    category: category.id,
                    total,
                })
            })
            .collect()
    }

    pub(super) fn compute_category_totals(&self) -> Vec<CategoryTotal> {
        let mut totals =
            self.totals_by_category(self.segments.iter().map(|segment| CategoryTotal {
                category: segment.category,
                total: segment.duration(),
            }));
        totals.sort_by_key(|entry| std::cmp::Reverse(entry.total));
        totals
    }

    pub(super) fn compute_summary(&self) -> DaySummary {
        let active = total(self.segments.iter().map(Segment::duration));
        let tendency_total = |wanted: Tendency| {
            total(
                self.segments
                    .iter()
                    .filter(|segment| self.category(segment.category).tendency == wanted)
                    .map(Segment::duration),
            )
        };
        let unclassified = total(
            self.segments
                .iter()
                .filter(|segment| self.category(segment.category).is_uncategorized())
                .map(Segment::duration),
        );
        DaySummary {
            active,
            focus: Share::of(tendency_total(Tendency::Focus), active),
            distraction: Share::of(tendency_total(Tendency::Distraction), active),
            unclassified,
            longest_focus_streak: self.longest_focus_streak(),
            switches: self.switches(),
        }
    }

    /// Longest run of back-to-back focus segments.
    fn longest_focus_streak(&self) -> Duration {
        let mut longest = Duration::ZERO;
        let mut current = Duration::ZERO;
        let mut previous_end: Option<TimeOfDay> = None;
        for segment in &self.segments {
            let focus = self.category(segment.category).tendency == Tendency::Focus;
            let contiguous = previous_end == Some(segment.span.start());
            if !focus || !contiguous {
                current = Duration::ZERO;
            }
            if focus {
                current += segment.duration();
                longest = longest.max(current);
            }
            previous_end = Some(segment.span.end());
        }
        longest
    }

    fn switches(&self) -> u32 {
        let switches = self
            .segments
            .iter()
            .zip(self.segments.iter().skip(1))
            .filter(|(before, after)| before.app != after.app)
            .count();
        u32::try_from(switches).expect("a day has far fewer than 4 billion segments")
    }

    pub(super) fn compute_activity_range(&self) -> Option<TimeSpan> {
        let spans = || {
            self.segments
                .iter()
                .map(|segment| segment.span)
                .chain(self.states.iter().map(|state| state.span))
        };
        let start = spans().map(TimeSpan::start).min()?;
        let end = spans().map(TimeSpan::end).max()?;
        Some(TimeSpan::new(start, end))
    }

    pub(super) fn compute_hourly(&self) -> Vec<HourlyBar> {
        let Some(range) = self.compute_activity_range() else {
            return Vec::new();
        };
        (range.start().hour()..=range.end().earlier_by_minutes(1).hour())
            .map(|hour| {
                let window = TimeSpan::new(TimeOfDay::hm(hour, 0), TimeOfDay::hm(hour + 1, 0));
                HourlyBar {
                    hour,
                    parts: self.totals_by_category(self.segments.iter().map(|segment| {
                        CategoryTotal {
                            category: segment.category,
                            total: segment.span.overlap(window),
                        }
                    })),
                }
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
        assert_eq!(
            Share::of(Duration::from_secs(1), Duration::from_secs(2)).percent(),
            50
        );
        assert!(
            (Share::of(Duration::from_secs(1), Duration::from_secs(4)).fraction() - 0.25).abs()
                < f32::EPSILON
        );
    }

    #[test]
    #[should_panic(expected = "exceeds the whole")]
    fn share_rejects_a_part_larger_than_the_whole() {
        let _ = Share::of(Duration::from_secs(2), Duration::from_secs(1));
    }

    #[test]
    fn switch_rate_rounds_half_up_and_is_zero_without_activity() {
        let summary = |switches: u32, active_minutes: u64| DaySummary {
            active: Duration::from_secs(active_minutes * 60),
            focus: Share::default(),
            distraction: Share::default(),
            unclassified: Duration::ZERO,
            longest_focus_streak: Duration::ZERO,
            switches,
        };
        assert_eq!(summary(5, 120).switches_per_hour(), 3);
        assert_eq!(summary(4, 120).switches_per_hour(), 2);
        assert_eq!(summary(0, 0).switches_per_hour(), 0);
    }
}

#[cfg(test)]
mod day_tests {
    use std::time::Duration;

    use crate::model::{
        StateKind, TimeSpan,
        fixture::{BROWSER, EDITOR, NONE, PLAY, WORK, day, segment, span, state},
    };

    fn minutes(count: u64) -> Duration {
        Duration::from_secs(count * 60)
    }

    #[test]
    fn empty_day_has_no_range_and_no_hours() {
        let day = day(vec![], vec![]);
        assert_eq!(day.activity_range(), None);
        assert!(day.hourly().is_empty());
        assert_eq!(day.summary().active, Duration::ZERO);
        assert_eq!(day.summary().focus.percent(), 0);
    }

    #[test]
    fn range_covers_segments_and_states() {
        let day = day(
            vec![segment(1, EDITOR, WORK, (9, 0), (9, 30))],
            vec![state(StateKind::Locked, (12, 0), (13, 0))],
        );
        assert_eq!(day.activity_range(), Some(span((9, 0), (13, 0))));
    }

    #[test]
    fn hourly_bars_split_segments_at_hour_boundaries() {
        let day = day(
            vec![
                segment(1, EDITOR, WORK, (9, 40), (10, 20)),
                segment(2, BROWSER, PLAY, (10, 20), (11, 0)),
            ],
            vec![],
        );
        #[derive(Debug, PartialEq, Eq)]
        struct HourTotal {
            hour: u32,
            total: Duration,
        }
        let hours: Vec<HourTotal> = day
            .hourly()
            .iter()
            .map(|bar| HourTotal {
                hour: bar.hour,
                total: bar.parts.iter().map(|part| part.total).sum(),
            })
            .collect();
        assert_eq!(
            hours,
            [
                HourTotal {
                    hour: 9,
                    total: minutes(20)
                },
                HourTotal {
                    hour: 10,
                    total: minutes(60)
                }
            ]
        );
        let ten = day.hourly().get(1).expect("hour 10");
        let categories: Vec<_> = ten.parts.iter().map(|part| part.category).collect();
        assert_eq!(categories, [WORK, PLAY], "category order");
    }

    #[test]
    fn summary_shares_streak_and_switches() {
        let day = day(
            vec![
                segment(1, EDITOR, WORK, (9, 0), (9, 30)),
                segment(2, EDITOR, WORK, (9, 30), (10, 0)),
                segment(3, BROWSER, PLAY, (10, 0), (10, 30)),
                segment(4, EDITOR, WORK, (10, 30), (10, 45)),
                segment(5, BROWSER, NONE, (11, 0), (11, 15)),
            ],
            vec![],
        );
        let summary = day.summary();
        assert_eq!(summary.active, minutes(120));
        assert_eq!(
            summary.focus.percent(),
            63,
            "75 of 120 minutes, rounded half up"
        );
        assert_eq!(summary.distraction.percent(), 25);
        assert_eq!(summary.unclassified, minutes(15));
        assert_eq!(summary.longest_focus_streak, minutes(60));
        assert_eq!(summary.switches, 3);
        let totals: Duration = day.category_totals().iter().map(|entry| entry.total).sum();
        assert_eq!(totals, summary.active);
        assert_eq!(
            day.category_totals().first().map(|entry| entry.category),
            Some(WORK)
        );
    }

    #[test]
    fn a_gap_breaks_the_focus_streak() {
        let day = day(
            vec![
                segment(1, EDITOR, WORK, (9, 0), (9, 30)),
                segment(2, EDITOR, WORK, (9, 40), (10, 0)),
            ],
            vec![],
        );
        assert_eq!(day.summary().longest_focus_streak, minutes(30));
        assert_eq!(day.summary().switches, 0);
        let _: Option<TimeSpan> = day.activity_range();
    }
}
