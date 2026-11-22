//! Logical-day ranges and hourly buckets in the core's current system time zone.

use super::{RunQueryError, input::QueryRange};
use jiff::{ToSpan, tz::TimeZone};
use quartz_protocol::{CategoryDuration, CategoryRef, HourStack, LocalDate};

/// Validated local-clock offset defining the start of a logical day (03 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DayStartOffset(i64);
impl DayStartOffset {
    /// Missing settings use local midnight.
    pub(crate) const MIDNIGHT: Self = Self(0);
    /// 03 §1 limits the day start to local noon.
    pub(crate) const MAX_MINUTES: i64 = 720;
    pub(crate) const fn minutes(self) -> i64 {
        self.0
    }
    fn span(self) -> jiff::Span {
        self.0.minutes()
    }
}
#[derive(Debug, snafu::Snafu)]
#[snafu(display("day start offset must be between 0 and 720 minutes"))]
pub(crate) struct ParseDayStartOffsetError;
impl TryFrom<i64> for DayStartOffset {
    type Error = ParseDayStartOffsetError;
    fn try_from(minutes: i64) -> Result<Self, Self::Error> {
        if !(Self::MIDNIGHT.minutes()..=Self::MAX_MINUTES).contains(&minutes) {
            return Err(ParseDayStartOffsetError);
        }
        Ok(Self(minutes))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DayCalendar {
    pub range: QueryRange,
    pub hours: Vec<HourStack>,
}
impl DayCalendar {
    pub(crate) fn current_date(
        now: jiff::Timestamp,
        offset: DayStartOffset,
    ) -> Result<LocalDate, RunQueryError> {
        Self::current_date_in_zone(now, offset, TimeZone::try_system()?)
    }
    pub(crate) fn current_date_in_zone(
        now: jiff::Timestamp,
        offset: DayStartOffset,
        zone: TimeZone,
    ) -> Result<LocalDate, RunQueryError> {
        let date = now.to_zoned_datetime(zone.clone()).date();
        let today = Self::in_zone(date, offset, zone)?;
        Ok(if now.as_millisecond() < today.range.start {
            date.yesterday()?
        } else {
            date
        })
    }
    pub(crate) fn next_start(
        now: jiff::Timestamp,
        offset: DayStartOffset,
    ) -> Result<i64, RunQueryError> {
        Self::next_start_in_zone(now, offset, TimeZone::try_system()?)
    }
    pub(crate) fn next_start_in_zone(
        now: jiff::Timestamp,
        offset: DayStartOffset,
        zone: TimeZone,
    ) -> Result<i64, RunQueryError> {
        let date = now.to_zoned_datetime(zone.clone()).date();
        let today = Self::in_zone(date, offset, zone.clone())?;
        if now.as_millisecond() < today.range.start {
            Ok(Self::in_zone(date.yesterday()?, offset, zone)?.range.end)
        } else {
            Ok(today.range.end)
        }
    }
    pub(crate) fn new(date: LocalDate, offset: DayStartOffset) -> Result<Self, RunQueryError> {
        Self::in_zone(date, offset, TimeZone::try_system()?)
    }
    pub(super) fn in_zone(
        date: LocalDate,
        offset: DayStartOffset,
        zone: TimeZone,
    ) -> Result<Self, RunQueryError> {
        let local_start = date.at(0, 0, 0, 0).checked_add(offset.span())?;
        let local_end = date.tomorrow()?.at(0, 0, 0, 0).checked_add(offset.span())?;
        let mut at = local_start.to_zoned_datetime(zone.clone())?;
        let end = local_end.to_zoned_datetime(zone)?;
        let range = QueryRange {
            start: at.timestamp().as_millisecond(),
            end: end.timestamp().as_millisecond(),
        };
        let mut hours = Vec::new();
        while at < end {
            let next = at.checked_add(1.hours())?.min(end.clone());
            hours.push(HourStack {
                start_utc_ms: at.timestamp().as_millisecond(),
                end_utc_ms: next.timestamp().as_millisecond(),
                local_start: at.to_string(),
                categories: vec![CategoryDuration {
                    category: CategoryRef::Unclassified,
                    duration_ms: 0,
                }],
            });
            at = next;
        }
        Ok(Self { range, hours })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_dates_follow_midnight_offset_boundaries_and_dst() {
        struct Case {
            at: &'static str,
            offset_min: i64,
            date: &'static str,
            next: &'static str,
        }
        let zone = TimeZone::get("America/Los_Angeles").expect("time zone");
        let cases = [
            Case {
                at: "2026-11-21T07:59:59.999Z",
                offset_min: 0,
                date: "2026-11-20",
                next: "2026-11-21T08:00:00Z",
            },
            Case {
                at: "2026-11-21T08:00:00Z",
                offset_min: 0,
                date: "2026-11-21",
                next: "2026-11-22T08:00:00Z",
            },
            Case {
                at: "2026-11-21T07:59:59.999Z",
                offset_min: 240,
                date: "2026-11-20",
                next: "2026-11-21T12:00:00Z",
            },
            Case {
                at: "2026-11-21T08:00:00Z",
                offset_min: 240,
                date: "2026-11-20",
                next: "2026-11-21T12:00:00Z",
            },
            Case {
                at: "2026-11-21T11:59:59.999Z",
                offset_min: 240,
                date: "2026-11-20",
                next: "2026-11-21T12:00:00Z",
            },
            Case {
                at: "2026-11-21T12:00:00Z",
                offset_min: 240,
                date: "2026-11-21",
                next: "2026-11-22T12:00:00Z",
            },
            Case {
                at: "2026-03-08T08:00:00Z",
                offset_min: 0,
                date: "2026-03-08",
                next: "2026-03-09T07:00:00Z",
            },
            Case {
                at: "2026-03-08T10:59:59.999Z",
                offset_min: 240,
                date: "2026-03-07",
                next: "2026-03-08T11:00:00Z",
            },
            Case {
                at: "2026-03-08T11:00:00Z",
                offset_min: 240,
                date: "2026-03-08",
                next: "2026-03-09T11:00:00Z",
            },
            Case {
                at: "2026-11-01T07:00:00Z",
                offset_min: 0,
                date: "2026-11-01",
                next: "2026-11-02T08:00:00Z",
            },
            Case {
                at: "2026-11-01T11:59:59.999Z",
                offset_min: 240,
                date: "2026-10-31",
                next: "2026-11-01T12:00:00Z",
            },
            Case {
                at: "2026-11-01T12:00:00Z",
                offset_min: 240,
                date: "2026-11-01",
                next: "2026-11-02T12:00:00Z",
            },
        ];
        for case in cases {
            let now = case.at.parse().expect("UTC timestamp");
            assert_eq!(
                DayCalendar::current_date_in_zone(
                    now,
                    DayStartOffset::try_from(case.offset_min).expect("case offset"),
                    zone.clone()
                )
                .expect("logical date"),
                case.date.parse::<LocalDate>().expect("date"),
                "at {} offset {}",
                case.at,
                case.offset_min
            );
            assert_eq!(
                DayCalendar::next_start_in_zone(
                    now,
                    DayStartOffset::try_from(case.offset_min).expect("case offset"),
                    zone.clone()
                )
                .expect("deadline"),
                case.next
                    .parse::<jiff::Timestamp>()
                    .expect("next start")
                    .as_millisecond(),
                "at {} offset {}",
                case.at,
                case.offset_min
            );
        }
    }
    #[test]
    fn current_date_changes_at_the_system_logical_day_boundary() {
        let date = "2026-11-21".parse().expect("date");
        let offset = DayStartOffset::try_from(240).expect("offset");
        let calendar = DayCalendar::new(date, offset).expect("logical day");
        let before =
            jiff::Timestamp::from_millisecond(calendar.range.start - 1).expect("before boundary");
        let boundary =
            jiff::Timestamp::from_millisecond(calendar.range.start).expect("at boundary");
        assert_eq!(
            DayCalendar::current_date(before, offset).expect("previous day"),
            date.yesterday().expect("yesterday")
        );
        assert_eq!(
            DayCalendar::current_date(boundary, offset).expect("today"),
            date
        );
    }
    #[test]
    fn tomorrow_uses_current_logical_day_before_and_at_offset_and_across_dst() {
        let zone = TimeZone::get("America/Los_Angeles").expect("time zone");
        let offset = DayStartOffset::try_from(240).expect("offset");
        let before: jiff::Timestamp = "2026-11-21T11:00:00Z".parse().expect("03:00 local");
        let boundary: jiff::Timestamp = "2026-11-21T12:00:00Z".parse().expect("04:00 local");
        let next: jiff::Timestamp = "2026-11-22T12:00:00Z".parse().expect("next 04:00");
        assert_eq!(
            DayCalendar::next_start_in_zone(before, offset, zone.clone()).expect("before offset"),
            boundary.as_millisecond()
        );
        assert_eq!(
            DayCalendar::next_start_in_zone(boundary, offset, zone.clone()).expect("at offset"),
            next.as_millisecond()
        );
        let spring: jiff::Timestamp = "2026-03-08T08:00:00Z".parse().expect("spring midnight");
        let end: jiff::Timestamp = "2026-03-09T07:00:00Z".parse().expect("23-hour day end");
        assert_eq!(
            DayCalendar::next_start_in_zone(spring, DayStartOffset::MIDNIGHT, zone.clone())
                .expect("spring day"),
            end.as_millisecond()
        );
        let fall: jiff::Timestamp = "2026-11-01T07:00:00Z".parse().expect("fall midnight");
        let end: jiff::Timestamp = "2026-11-02T08:00:00Z".parse().expect("25-hour day end");
        assert_eq!(
            DayCalendar::next_start_in_zone(fall, DayStartOffset::MIDNIGHT, zone.clone())
                .expect("fall day"),
            end.as_millisecond()
        );
        assert!(DayStartOffset::try_from(-1).is_err());
    }
}
