//! Logical-day ranges and hourly buckets in the core's current system time zone.

use super::{RunQueryError, input::QueryRange};
use jiff::{ToSpan, tz::TimeZone};
use quartz_protocol::{CategoryDuration, CategoryRef, HourStack, LocalDate};

use crate::store::versions::{DEFAULT_DAY_START_OFFSET_MIN, MAX_DAY_START_OFFSET_MIN};

#[derive(Debug, Clone)]
pub(crate) struct DayCalendar {
    pub range: QueryRange,
    pub hours: Vec<HourStack>,
}
impl DayCalendar {
    pub(crate) fn new(date: LocalDate, offset: i64) -> Result<Self, RunQueryError> {
        if !(DEFAULT_DAY_START_OFFSET_MIN..=MAX_DAY_START_OFFSET_MIN).contains(&offset) {
            return Err(RunQueryError::InvalidSettings);
        }
        let zone = TimeZone::system();
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
