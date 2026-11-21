//! Logical-day ranges and hourly buckets in the core's current system time zone.

use super::{RunQueryError, input::QueryRange};
use jiff::{ToSpan, tz::TimeZone};
use quartz_protocol::{CategoryDuration, CategoryRef, HourStack, LocalDate};

use crate::store::settings::{DEFAULT_DAY_START_OFFSET_MIN, MAX_DAY_START_OFFSET_MIN};

#[derive(Debug, Clone)]
pub(crate) struct DayCalendar {
    pub range: QueryRange,
    pub hours: Vec<HourStack>,
}
impl DayCalendar {
    pub(crate) fn next_start(now: jiff::Timestamp, offset: i64) -> Result<i64, RunQueryError> {
        Self::next_start_in_zone(now, offset, TimeZone::system())
    }
    fn next_start_in_zone(
        now: jiff::Timestamp,
        offset: i64,
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
    pub(crate) fn new(date: LocalDate, offset: i64) -> Result<Self, RunQueryError> {
        Self::in_zone(date, offset, TimeZone::system())
    }
    fn in_zone(date: LocalDate, offset: i64, zone: TimeZone) -> Result<Self, RunQueryError> {
        if !(DEFAULT_DAY_START_OFFSET_MIN..=MAX_DAY_START_OFFSET_MIN).contains(&offset) {
            return Err(RunQueryError::InvalidDayOffset);
        }
        let local_start = date.at(0, 0, 0, 0).checked_add(offset.minutes())?;
        let local_end = date
            .tomorrow()?
            .at(0, 0, 0, 0)
            .checked_add(offset.minutes())?;
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
    fn tomorrow_uses_current_logical_day_before_and_at_offset_and_across_dst() {
        let zone = TimeZone::get("America/Los_Angeles").expect("time zone");
        let offset = 240;
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
            DayCalendar::next_start_in_zone(spring, 0, zone.clone()).expect("spring day"),
            end.as_millisecond()
        );
        let fall: jiff::Timestamp = "2026-11-01T07:00:00Z".parse().expect("fall midnight");
        let end: jiff::Timestamp = "2026-11-02T08:00:00Z".parse().expect("25-hour day end");
        assert_eq!(
            DayCalendar::next_start_in_zone(fall, 0, zone.clone()).expect("fall day"),
            end.as_millisecond()
        );
        assert!(DayCalendar::next_start_in_zone(before, -1, zone).is_err());
    }
}
