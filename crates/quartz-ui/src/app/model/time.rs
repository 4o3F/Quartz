//! Time of day and spans on a logical day.

use std::time::Duration;

/// Seconds since the start of the logical day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeOfDay(u32);

impl TimeOfDay {
    /// Start of the day.
    pub const START: Self = Self(0);
    /// End of the day (24:00).
    pub const END: Self = Self(24 * 3600);

    /// Builds a time from hours and minutes, clamped to the day.
    #[must_use]
    pub const fn hm(hours: u32, minutes: u32) -> Self {
        let seconds = hours * 3600 + minutes * 60;
        if seconds > Self::END.0 {
            Self::END
        } else {
            Self(seconds)
        }
    }

    /// Seconds since the start of the day.
    #[must_use]
    pub const fn seconds(self) -> u32 {
        self.0
    }

    /// Whole hours.
    #[must_use]
    pub const fn hour(self) -> u32 {
        self.0 / 3600
    }

    /// Minutes within the hour.
    #[must_use]
    pub const fn minute(self) -> u32 {
        self.0 / 60 % 60
    }

    /// Moves earlier, stopping at the start of the day.
    #[must_use]
    pub const fn earlier_by_minutes(self, minutes: u32) -> Self {
        Self(self.0.saturating_sub(minutes * 60))
    }

    /// Moves later, stopping at the end of the day.
    #[must_use]
    pub const fn later_by_minutes(self, minutes: u32) -> Self {
        let seconds = self.0 + minutes * 60;
        if seconds > Self::END.0 {
            Self::END
        } else {
            Self(seconds)
        }
    }

    /// Builds a time from fractional minutes, clamped to the day.
    #[must_use]
    pub fn from_minutes_f32(minutes: f32) -> Self {
        let seconds = (minutes.max(0.) * 60.).round();
        // The clamp keeps the value inside the day, so the cast cannot truncate.
        Self(seconds.min(Self::END.0 as f32) as u32)
    }

    /// Fractional minutes since the start of the day.
    #[must_use]
    pub fn minutes_f32(self) -> f32 {
        self.0 as f32 / 60.
    }
}

/// A half-open span `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSpan {
    pub start: TimeOfDay,
    pub end: TimeOfDay,
}

impl TimeSpan {
    /// Builds a span; an end before the start collapses to an empty span.
    #[must_use]
    pub const fn new(start: TimeOfDay, end: TimeOfDay) -> Self {
        Self {
            start,
            end: if end.0 < start.0 { start } else { end },
        }
    }

    /// Length of the span.
    #[must_use]
    pub fn duration(self) -> Duration {
        Duration::from_secs(u64::from(self.end.0 - self.start.0))
    }

    /// Overlap with `other`, zero when disjoint.
    #[must_use]
    pub fn overlap(self, other: Self) -> Duration {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        Self::new(start, end).duration()
    }
}

impl std::fmt::Display for TimeOfDay {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:02}:{:02}", self.hour(), self.minute())
    }
}
impl std::fmt::Display for TimeSpan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
        assert_eq!(
            b.overlap(TimeSpan::new(TimeOfDay::hm(12, 0), TimeOfDay::hm(13, 0))),
            Duration::ZERO
        );
    }

    #[test]
    fn inverted_span_is_empty() {
        let span = TimeSpan::new(TimeOfDay::hm(10, 0), TimeOfDay::hm(9, 0));
        assert_eq!(span.duration(), Duration::ZERO);
    }
}
