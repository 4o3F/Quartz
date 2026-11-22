//! Content of the details sidebar (07 section 8).

use std::{cmp::Reverse, collections::BTreeMap, num::NonZeroU32, time::Duration};

use jiff::civil::Weekday;

use super::{
    application::AppId,
    category::CategoryId,
    day::{Day, WEEK_DAYS},
    rule::Explanation,
    segment::{ClassificationSource, Segment, SegmentId},
    summary::{CategoryTotal, total},
    time::TimeSpan,
};

/// What the details sidebar is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DetailsSubject {
    Segment(SegmentId),
    App(AppId),
}

/// The selected-interval card of the details sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct IntervalCard {
    pub(crate) span: TimeSpan,
    pub(crate) title: String,
    pub(crate) category: CategoryId,
    pub(crate) source: ClassificationSource,
    pub(crate) merged: Option<NonZeroU32>,
}

/// The day one bar of the 7-day chart stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BarDay {
    Earlier(Weekday),
    Today,
}

/// One bar of the 7-day chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DayBar {
    pub(crate) day: BarDay,
    pub(crate) total: Duration,
}

/// A common title row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TitleRow {
    pub(crate) title: String,
    pub(crate) total: Duration,
}

/// Content of the details sidebar (07 section 8).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Details {
    pub(crate) app: AppId,
    /// The interval's category, or the category with most of the application's time.
    pub(crate) category: CategoryId,
    pub(crate) interval: Option<IntervalCard>,
    /// Oldest day first; the last bar is the day shown.
    pub(crate) week: [DayBar; WEEK_DAYS],
    pub(crate) titles: Vec<TitleRow>,
    /// The rule behind the interval, or behind the application's longest classified segment;
    /// `None` when no rule matched.
    pub(crate) explanation: Option<Explanation>,
}

impl Day {
    /// Details sidebar content for a clicked interval or ranking row; `None` when the subject is
    /// not part of this day.
    #[must_use]
    pub(crate) fn details(&self, subject: DetailsSubject) -> Option<Details> {
        if !matches!(self.source(), super::DaySource::Fixture) {
            return None;
        }
        let (app, selected) = match subject {
            DetailsSubject::Segment(id) => {
                let segment = self.segment(id)?;
                (segment.app, Some(segment))
            }
            DetailsSubject::App(id) => (self.has_app(id).then_some(id)?, None),
        };
        let app_segments: Vec<&Segment> = self
            .segments
            .iter()
            .filter(|segment| segment.app == app)
            .collect();
        let category = match selected {
            Some(segment) => segment.category,
            None => {
                self.totals_by_category(app_segments.iter().map(|segment| CategoryTotal {
                    category: segment.category,
                    total: segment.duration(),
                }))
                .into_iter()
                .max_by_key(|entry| entry.total)?
                .category
            }
        };
        let explained = match selected {
            Some(segment) => Some(segment),
            None => app_segments
                .iter()
                .filter(|segment| matches!(segment.source, ClassificationSource::Rule { .. }))
                .max_by_key(|segment| segment.duration())
                .copied(),
        };
        Some(Details {
            app,
            category,
            interval: selected.map(|segment| IntervalCard {
                span: segment.span,
                title: segment.title.clone(),
                category: segment.category,
                source: segment.source.clone(),
                merged: segment.merged,
            }),
            week: self.week_bars(app, total(app_segments.iter().map(|s| s.duration()))),
            titles: common_titles(&app_segments),
            explanation: explained.and_then(|segment| self.explanation(segment)),
        })
    }

    /// The rule that classified `segment`; `None` when no rule did.
    fn explanation(&self, segment: &Segment) -> Option<Explanation> {
        match &segment.source {
            ClassificationSource::Rule { rule, actual } => Some(Explanation {
                rule: self.rule(*rule).clone(),
                actual: actual.clone(),
            }),
            ClassificationSource::Manual
            | ClassificationSource::Unmatched
            | ClassificationSource::ProtocolRule(_)
            | ClassificationSource::Mixed => None,
        }
    }

    fn week_bars(&self, app: AppId, today: Duration) -> [DayBar; WEEK_DAYS] {
        let earlier = self.earlier_days(app).0;
        let weekday = self.date.weekday();
        std::array::from_fn(|index| match earlier.get(index) {
            Some(total) => DayBar {
                day: BarDay::Earlier(weekday.wrapping_sub(
                    i64::try_from(earlier.len() - index).expect("a week index fits in i64"),
                )),
                total: *total,
            },
            None => DayBar {
                day: BarDay::Today,
                total: today,
            },
        })
    }
}

/// Every title with its time, longest first, ties by title.
fn common_titles(segments: &[&Segment]) -> Vec<TitleRow> {
    let mut titles: BTreeMap<&str, Duration> = BTreeMap::new();
    for segment in segments {
        *titles.entry(segment.title.as_str()).or_default() += segment.duration();
    }
    let mut rows: Vec<TitleRow> = titles
        .into_iter()
        .map(|(title, total)| TitleRow {
            title: title.to_owned(),
            total,
        })
        .collect();
    rows.sort_by_key(|row| Reverse(row.total));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        DayParts, EarlierDays,
        fixture::{BROWSER, EDITOR, EDITOR_RULE, NONE, WORK, day, parts, segment},
    };

    #[test]
    fn week_ends_today_and_derives_earlier_weekdays_from_the_date() {
        let mut parts = parts();
        parts.history.insert(
            EDITOR,
            EarlierDays([1, 2, 3, 4, 5, 6].map(|minutes| Duration::from_secs(minutes * 60))),
        );
        let day = Day::new(DayParts {
            segments: vec![segment(1, EDITOR, WORK, (9, 0), (9, 30))],
            ..parts
        })
        .expect("valid day");
        let details = day
            .details(DetailsSubject::App(EDITOR))
            .expect("editor details");
        let days: Vec<BarDay> = details.week.iter().map(|bar| bar.day).collect();
        assert_eq!(
            days,
            [
                BarDay::Earlier(Weekday::Tuesday),
                BarDay::Earlier(Weekday::Wednesday),
                BarDay::Earlier(Weekday::Thursday),
                BarDay::Earlier(Weekday::Friday),
                BarDay::Earlier(Weekday::Saturday),
                BarDay::Earlier(Weekday::Sunday),
                BarDay::Today,
            ]
        );
        assert_eq!(
            details.week.first().map(|bar| bar.total),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            details.week.last().map(|bar| bar.total),
            Some(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn segment_details_explain_the_segment_and_app_details_the_longest_rule() {
        let mut ruled = segment(1, EDITOR, WORK, (9, 0), (10, 0));
        ruled.source = ClassificationSource::Rule {
            rule: EDITOR_RULE,
            actual: "editor.exe".to_owned(),
        };
        let unmatched = segment(2, EDITOR, NONE, (10, 0), (10, 10));
        let day = day(vec![ruled, unmatched.clone()], vec![]);
        let of_segment = day
            .details(DetailsSubject::Segment(unmatched.id))
            .expect("segment details");
        assert_eq!(of_segment.category, NONE);
        assert_eq!(of_segment.explanation, None);
        assert_eq!(
            of_segment.interval.map(|card| card.span),
            Some(unmatched.span)
        );
        let of_app = day
            .details(DetailsSubject::App(EDITOR))
            .expect("app details");
        assert_eq!(of_app.category, WORK);
        assert_eq!(
            of_app.explanation.map(|explanation| explanation.actual),
            Some("editor.exe".to_owned())
        );
        let titles: Vec<&str> = of_app.titles.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(titles, ["title 1", "title 2"]);
    }

    #[test]
    fn subjects_outside_the_day_have_no_details() {
        let day = day(vec![segment(1, EDITOR, WORK, (9, 0), (9, 30))], vec![]);
        assert_eq!(
            day.details(DetailsSubject::Segment(SegmentId::new(9))),
            None
        );
        assert_eq!(day.details(DetailsSubject::App(AppId::new(9))), None);
        assert_eq!(
            day.details(DetailsSubject::App(BROWSER)),
            None,
            "no time today"
        );
    }
}
