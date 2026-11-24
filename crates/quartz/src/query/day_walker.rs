//! Shared logical-day traversal for snapshot scans with a fresh pipeline per day.

use super::{
    RunQueryError,
    calendar::{DayCalendar, DayStartOffset},
    input::QueryRange,
};
use jiff::tz::TimeZone;
use std::collections::VecDeque;

/// Retains only day boundaries while the snapshot scans one day at a time.
#[derive(Debug)]
pub(super) struct DayWalker {
    pub(super) range: QueryRange,
    pub(super) time_zone: String,
    pub(super) offset: DayStartOffset,
    days: VecDeque<LogicalDay>,
    pub(super) date: quartz_protocol::LocalDate,
}
#[derive(Debug)]
struct LogicalDay {
    date: quartz_protocol::LocalDate,
    range: QueryRange,
}
impl DayWalker {
    pub(super) fn new(
        start_date: quartz_protocol::LocalDate,
        end_date: quartz_protocol::LocalDate,
        offset: DayStartOffset,
        zone: TimeZone,
    ) -> Result<Self, RunQueryError> {
        Self::new_encoding_zone(start_date, end_date, offset, zone, |zone| {
            jiff::fmt::temporal::DateTimePrinter::new().time_zone_to_string(zone)
        })
    }
    fn new_encoding_zone(
        start_date: quartz_protocol::LocalDate,
        end_date: quartz_protocol::LocalDate,
        offset: DayStartOffset,
        zone: TimeZone,
        encode: impl FnOnce(&TimeZone) -> Result<String, jiff::Error>,
    ) -> Result<Self, RunQueryError> {
        if start_date >= end_date {
            return Err(RunQueryError::InvalidRangeRequest);
        }
        let mut date = start_date;
        let mut days = VecDeque::new();
        while date < end_date {
            if days.len() == quartz_protocol::RANGE_MAX_DAYS {
                return Err(RunQueryError::InvalidRangeRequest);
            }
            days.push_back(LogicalDay {
                date,
                range: DayCalendar::range_in_zone(date, offset, zone.clone())
                    .map_err(|_| RunQueryError::InvalidRangeRequest)?,
            });
            date = date
                .tomorrow()
                .map_err(|_| RunQueryError::InvalidRangeRequest)?;
        }
        let range = QueryRange {
            start: days.front().expect("validated nonempty dates").range.start,
            end: days.back().expect("validated nonempty dates").range.end,
        };
        Ok(Self {
            range,
            time_zone: encode(&zone)
                .map_err(|source| RunQueryError::EncodeTimeZoneError { source })?,
            offset,
            days,
            date: start_date,
        })
    }
    pub(super) fn next_range(&mut self) -> Option<QueryRange> {
        let day = self.days.pop_front()?;
        self.date = day.date;
        Some(day.range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zone_encoding_failure_is_internal_for_valid_dates() {
        let error = DayWalker::new_encoding_zone(
            "2025-01-02".parse().expect("start"),
            "2025-01-03".parse().expect("end"),
            DayStartOffset::MIDNIGHT,
            TimeZone::UTC,
            |_| Err(TimeZone::get("Invalid/InjectedFailure").expect_err("injected encoding error")),
        )
        .expect_err("unencodable time zone");
        assert_eq!(error.kind(), quartz_protocol::ErrorKind::Internal);
        assert!(matches!(error, RunQueryError::EncodeTimeZoneError { .. }));
    }
    #[test]
    fn posix_timezone_has_a_stable_encodable_identifier() {
        let name = "EST5EDT,M3.2.0,M11.1.0";
        let walker = DayWalker::new(
            "2025-01-02".parse().expect("start"),
            "2025-01-03".parse().expect("end"),
            DayStartOffset::MIDNIGHT,
            TimeZone::posix(name).expect("POSIX zone"),
        )
        .expect("encodable POSIX zone");
        assert_eq!(walker.time_zone, name);
    }
    #[test]
    fn validates_logical_dates_and_follows_dst_with_offsets() {
        struct Case {
            start: &'static str,
            end: &'static str,
            offset: i64,
            hours: i64,
        }
        let zone = TimeZone::get("America/Los_Angeles").expect("zone");
        for case in [
            Case {
                start: "2026-03-08",
                end: "2026-03-09",
                offset: 0,
                hours: 23,
            },
            Case {
                start: "2026-11-01",
                end: "2026-11-02",
                offset: 0,
                hours: 25,
            },
            Case {
                start: "2026-03-07",
                end: "2026-03-09",
                offset: 240,
                hours: 47,
            },
            Case {
                start: "2026-10-31",
                end: "2026-11-02",
                offset: 240,
                hours: 49,
            },
            Case {
                start: "2025-10-31",
                end: "2026-11-01",
                offset: 240,
                hours: 366 * 24 + 1,
            },
        ] {
            let mut walker = DayWalker::new(
                case.start.parse().expect("start"),
                case.end.parse().expect("end"),
                DayStartOffset::try_from(case.offset).expect("offset"),
                zone.clone(),
            )
            .expect("dates");
            assert_eq!(walker.time_zone, "America/Los_Angeles");
            assert_eq!(
                walker.range.end - walker.range.start,
                case.hours * 3_600_000
            );
            let mut boundary = walker.range.start;
            while let Some(day) = walker.next_range() {
                assert_eq!(day.start, boundary);
                let local = jiff::Timestamp::from_millisecond(day.start)
                    .expect("UTC")
                    .to_zoned_datetime(zone.clone());
                assert_eq!(
                    i64::from(local.hour()) * 60 + i64::from(local.minute()),
                    case.offset
                );
                boundary = day.end;
            }
            assert_eq!(boundary, walker.range.end);
        }
        for end in ["2024-01-01", "2023-12-31", "2025-01-02"] {
            assert_eq!(
                DayWalker::new(
                    "2024-01-01".parse().expect("start"),
                    end.parse().expect("end"),
                    DayStartOffset::MIDNIGHT,
                    zone.clone()
                )
                .expect_err("invalid dates")
                .kind(),
                quartz_protocol::ErrorKind::BadRequest
            );
        }
    }
}
