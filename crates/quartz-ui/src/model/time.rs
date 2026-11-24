//! Time of day and spans on a logical day.

use std::{fmt, time::Duration};

use snafu::Snafu;

/// Milliseconds in one second.
pub(crate) const MILLIS_PER_SECOND: u32 = 1000;
/// Seconds in one minute.
pub(crate) const SECONDS_PER_MINUTE: u32 = 60;
/// Minutes in one hour.
pub(crate) const MINUTES_PER_HOUR: u32 = 60;
/// Seconds in one hour.
pub(crate) const SECONDS_PER_HOUR: u32 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR;
/// Hours in one logical day (03 section 1).
pub(crate) const HOURS_PER_DAY: u32 = 24;

/// Elapsed milliseconds from the logical day start; live days can span 23 or 25 hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub(crate) struct TimeOfDay(u32);

impl TimeOfDay {
    /// Start of the day.
    pub(crate) const START: Self = Self(0);
    /// End of the day (24:00).
    pub(crate) const END: Self = Self(HOURS_PER_DAY * SECONDS_PER_HOUR * MILLIS_PER_SECOND);

    /// Builds a time from hours and minutes.
    ///
    /// # Panics
    /// Panics when the time lies outside 00:00 to 24:00 or `minutes` is 60 or more. Meant for
    /// literals, where the check runs at compile time.
    #[must_use]
    #[cfg_attr(
        all(not(test), not(feature = "preview")),
        expect(dead_code, reason = "used by preview fixtures and their model tests")
    )]
    pub(crate) const fn hm(hours: u32, minutes: u32) -> Self {
        assert!(minutes < MINUTES_PER_HOUR, "minutes must be below 60");
        assert!(
            hours < HOURS_PER_DAY || (hours == HOURS_PER_DAY && minutes == 0),
            "time of day must lie within 00:00 to 24:00"
        );
        Self((hours * SECONDS_PER_HOUR + minutes * SECONDS_PER_MINUTE) * MILLIS_PER_SECOND)
    }

    /// Builds a time from seconds since the start of the day; `None` after 24:00.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn from_seconds(seconds: u32) -> Option<Self> {
        if seconds <= Self::END.seconds() {
            Some(Self(seconds * MILLIS_PER_SECOND))
        } else {
            None
        }
    }

    /// Seconds since the start of the day.
    #[must_use]
    pub(crate) const fn seconds(self) -> u32 {
        self.0 / MILLIS_PER_SECOND
    }

    pub(crate) const fn from_elapsed_millis(millis: u32) -> Self {
        Self(millis)
    }
    pub(crate) const fn millis(self) -> u32 {
        self.0
    }

    /// Whole hours.
    #[must_use]
    pub(crate) const fn hour(self) -> u32 {
        self.seconds() / SECONDS_PER_HOUR
    }

    /// Minutes within the hour.
    #[must_use]
    pub(crate) const fn minute(self) -> u32 {
        self.seconds() / SECONDS_PER_MINUTE % MINUTES_PER_HOUR
    }

    /// Moves earlier; the start of the day is the earliest result.
    #[must_use]
    pub(crate) const fn earlier_by_minutes(self, minutes: u32) -> Self {
        Self(
            self.0
                .saturating_sub(minutes.saturating_mul(SECONDS_PER_MINUTE * MILLIS_PER_SECOND)),
        )
    }

    /// Moves later; the end of the day is the latest result.
    #[must_use]
    pub(crate) const fn later_by_minutes(self, minutes: u32) -> Self {
        let seconds = self
            .0
            .saturating_add(minutes.saturating_mul(SECONDS_PER_MINUTE * MILLIS_PER_SECOND));
        if seconds > Self::END.0 {
            Self::END
        } else {
            Self(seconds)
        }
    }
}

/// A span would end before it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(display("span ends at {end} before it starts at {start}"))]
pub(crate) struct BuildTimeSpanError {
    start: TimeOfDay,
    end: TimeOfDay,
}

/// A half-open span `[start, end)` with `start <= end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TimeSpan {
    start: TimeOfDay,
    end: TimeOfDay,
}

impl TimeSpan {
    /// The whole logical day.
    pub(crate) const DAY: Self = Self::new(TimeOfDay::START, TimeOfDay::END);

    /// Builds a span.
    ///
    /// # Errors
    /// Returns [`BuildTimeSpanError`] when `end` is before `start`.
    pub(crate) const fn try_new(
        start: TimeOfDay,
        end: TimeOfDay,
    ) -> Result<Self, BuildTimeSpanError> {
        if end.0 < start.0 {
            Err(BuildTimeSpanError { start, end })
        } else {
            Ok(Self { start, end })
        }
    }

    /// Builds a span from bounds known to be ordered.
    ///
    /// # Panics
    /// Panics when `end` is before `start`. Meant for literals and for bounds ordered by
    /// construction; use [`TimeSpan::try_new`] for anything else.
    #[must_use]
    pub(crate) const fn new(start: TimeOfDay, end: TimeOfDay) -> Self {
        match Self::try_new(start, end) {
            Ok(span) => span,
            Err(_) => panic!("span ends before it starts"),
        }
    }

    /// Inclusive start.
    #[must_use]
    pub(crate) const fn start(self) -> TimeOfDay {
        self.start
    }

    /// Exclusive end.
    #[must_use]
    pub(crate) const fn end(self) -> TimeOfDay {
        self.end
    }

    /// Whether the span covers no time.
    #[must_use]
    pub(crate) const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    /// Length of the span.
    #[must_use]
    pub(crate) fn duration(self) -> Duration {
        Duration::from_millis(u64::from(self.end.0 - self.start.0))
    }

    /// The common part of two spans; `None` when they share no time.
    #[must_use]
    pub(crate) fn intersection(self, other: Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then_some(Self { start, end })
    }

    /// Time shared with `other`, zero when disjoint.
    #[must_use]
    #[cfg_attr(
        all(not(test), not(feature = "preview")),
        expect(dead_code, reason = "used by preview fixtures and their model tests")
    )]
    pub(crate) fn overlap(self, other: Self) -> Duration {
        self.intersection(other)
            .map_or(Duration::ZERO, Self::duration)
    }

    /// The same start with a later end.
    ///
    /// # Panics
    /// Panics when `end` is before the current start.
    #[must_use]
    #[cfg_attr(
        all(not(test), not(feature = "preview")),
        expect(dead_code, reason = "used by preview fixtures and their model tests")
    )]
    pub(crate) const fn extended_to(self, end: TimeOfDay) -> Self {
        Self::new(self.start, end)
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:02}:{:02}", self.hour(), self.minute())
    }
}

impl fmt::Display for TimeSpan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} – {}", self.start, self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_duration_and_overlap() {
        let a = TimeSpan::new(TimeOfDay::hm(9, 0), TimeOfDay::hm(10, 30));
        let b = TimeSpan::new(TimeOfDay::hm(10, 0), TimeOfDay::hm(11, 0));
        assert_eq!(a.duration(), Duration::from_secs(5400));
        assert_eq!(a.overlap(b), Duration::from_secs(1800));
        let later = TimeSpan::new(TimeOfDay::hm(12, 0), TimeOfDay::hm(13, 0));
        assert_eq!(b.overlap(later), Duration::ZERO);
        assert_eq!(b.intersection(later), None);
    }

    #[test]
    fn inverted_span_is_rejected() {
        let error = TimeSpan::try_new(TimeOfDay::hm(10, 0), TimeOfDay::hm(9, 0))
            .expect_err("end before start");
        assert_eq!(
            error.to_string(),
            "span ends at 09:00 before it starts at 10:00"
        );
        assert!(
            TimeSpan::try_new(TimeOfDay::hm(9, 0), TimeOfDay::hm(9, 0))
                .is_ok_and(TimeSpan::is_empty)
        );
    }

    #[test]
    #[should_panic(expected = "time of day must lie within 00:00 to 24:00")]
    fn hour_past_the_day_panics() {
        let _ = TimeOfDay::hm(25, 0);
    }

    #[test]
    fn moving_saturates_at_day_bounds() {
        assert_eq!(
            TimeOfDay::hm(0, 10).earlier_by_minutes(15),
            TimeOfDay::START
        );
        assert_eq!(TimeOfDay::hm(23, 50).later_by_minutes(15), TimeOfDay::END);
        assert_eq!(TimeOfDay::END.later_by_minutes(u32::MAX), TimeOfDay::END);
        assert_eq!(TimeOfDay::from_seconds(TimeOfDay::END.seconds() + 1), None);
        assert_eq!(TimeOfDay::END.to_string(), "24:00");
    }

    #[test]
    fn extending_keeps_the_start() {
        let span = TimeSpan::new(TimeOfDay::hm(9, 0), TimeOfDay::hm(9, 30));
        let longer = span.extended_to(TimeOfDay::hm(10, 0));
        assert_eq!(
            (longer.start(), longer.end()),
            (span.start(), TimeOfDay::hm(10, 0))
        );
    }
}
