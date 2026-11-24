//! Main-thread logical date and its next UTC boundary, shared with the query calendar.

use crate::{
    core::sources::ReadClockError,
    query::{
        RunQueryError,
        calendar::{DayCalendar, DayStartOffset},
    },
};
use jiff::{Timestamp, tz::TimeZone};
use quartz_protocol::LocalDate;
use std::time::Duration;

#[derive(Debug)]
pub(crate) struct LogicalDay {
    date: LocalDate,
    next_start_utc_ms: i64,
    offset: DayStartOffset,
    zone: TimeZone,
}
impl LogicalDay {
    pub(crate) fn new(now: Timestamp, offset: DayStartOffset) -> Result<Self, RunQueryError> {
        Self::new_reading_zone(now, offset, TimeZone::try_system)
    }
    fn new_reading_zone(
        now: Timestamp,
        offset: DayStartOffset,
        read: impl FnOnce() -> Result<TimeZone, jiff::Error>,
    ) -> Result<Self, RunQueryError> {
        Self::in_zone(now, offset, RunQueryError::system_time_zone(read)?)
    }
    pub(super) fn in_zone(
        now: Timestamp,
        offset: DayStartOffset,
        zone: TimeZone,
    ) -> Result<Self, RunQueryError> {
        Ok(Self {
            date: DayCalendar::current_date_in_zone(now, offset, zone.clone())?,
            next_start_utc_ms: DayCalendar::next_start_in_zone(now, offset, zone.clone())?,
            offset,
            zone,
        })
    }
    pub(super) fn is_due(&self, now: Timestamp) -> bool {
        now.as_millisecond() >= self.next_start_utc_ms
    }
    pub(super) fn date(&self) -> LocalDate {
        self.date
    }
    pub(super) fn offset(&self) -> DayStartOffset {
        self.offset
    }
    #[cfg(test)]
    pub(super) fn next_start_utc_ms(&self) -> i64 {
        self.next_start_utc_ms
    }
    pub(super) fn advance(&self, now: Timestamp) -> Result<Self, RunQueryError> {
        Self::in_zone(now, self.offset, self.zone.clone())
    }
    pub(super) fn wait(&self, now: Timestamp) -> Duration {
        let remaining = self.next_start_utc_ms.saturating_sub(now.as_millisecond());
        Duration::from_millis(remaining.max(0).unsigned_abs())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RefreshLogicalDay {
    CheckDeadline,
    ResetZone,
    SetOffset(DayStartOffset),
}

#[derive(Debug, snafu::Snafu)]
pub(super) enum RefreshLogicalDayError {
    #[snafu(display("could not read logical-day clock: {source}"))]
    ReadClock { source: ReadClockError },
    #[snafu(display("could not resolve logical day: {source}"))]
    ResolveCalendar { source: RunQueryError },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_day_timezone_acquisition_failure_is_internal() {
        let error =
            LogicalDay::new_reading_zone(Timestamp::UNIX_EPOCH, DayStartOffset::MIDNIGHT, || {
                Err(TimeZone::get("Invalid/InjectedFailure").expect_err("injected zone error"))
            })
            .expect_err("zone unavailable");
        assert_eq!(error.kind(), quartz_protocol::ErrorKind::Internal);
        assert!(matches!(
            error,
            RunQueryError::ReadSystemTimeZoneError { .. }
        ));
    }
}
