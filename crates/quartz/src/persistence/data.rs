//! Database-thread execution and logical-date validation for `data.*` operations (04 §6.3).

use super::{
    diagnostics::{DataFailure, WriteFailure},
    dispatch::WriteResponse,
};
use crate::{
    query::{
        RunQueryError,
        calendar::{DayCalendar, DayStartOffset},
        versions::VersionState,
    },
    rules::registry::RegistryShell,
    store::{Store, data::CreateManualBackupError},
};
use jiff::tz::TimeZone;
use quartz_domain::{capture::UtcInstant, facts::QueryRange};
use quartz_protocol::{
    DataDeleteRangeRequest, DataDeleteRangeResponse, ErrorKind, ProtocolError, RANGE_MAX_DAYS,
};

#[derive(Debug)]
pub(crate) enum DataRequest {
    Info,
    Backup,
    Compact,
    DeleteRange(DataDeleteRangeRequest),
}

impl DataRequest {
    pub(super) fn requires_idle_reader(&self) -> bool {
        matches!(self, Self::Compact | Self::DeleteRange(_))
    }

    pub(super) fn execute(
        self,
        store: &mut Store,
        now: UtcInstant,
        registry: &RegistryShell,
    ) -> Result<WriteResponse, ProtocolError> {
        match self {
            Self::Info => store
                .data_info()
                .map(WriteResponse::DataInfo)
                .map_err(|error| WriteFailure::data_info(&error).into_protocol_error()),
            Self::Backup => store
                .create_manual_backup(now)
                .map(WriteResponse::DataBackup)
                .map_err(|error| match error {
                    CreateManualBackupError::AlreadyExists { .. } => {
                        ProtocolError::new(ErrorKind::BackupExists)
                    }
                    other => WriteFailure::data_backup(&other).into_protocol_error(),
                }),
            Self::Compact => store
                .compact()
                .map(WriteResponse::DataCompact)
                .map_err(|error| WriteFailure::data_compact(&error).into_protocol_error()),
            Self::DeleteRange(request) => {
                let zone = RunQueryError::system_time_zone(TimeZone::try_system)
                    .map_err(|_| WriteFailure::Data(DataFailure::TimeZone).into_protocol_error())?;
                execute_delete_range_in_zone(store, request, now, registry, zone)
            }
        }
    }
}

fn execute_delete_range_in_zone(
    store: &mut Store,
    request: DataDeleteRangeRequest,
    now: UtcInstant,
    registry: &RegistryShell,
    zone: TimeZone,
) -> Result<WriteResponse, ProtocolError> {
    let settings = store
        .query_settings()
        .map_err(|_| WriteFailure::Data(DataFailure::ReadVersions).into_protocol_error())?;
    let utc_range = resolve_delete_range(request, now, settings.day_start_offset, &zone)?;
    let summary = store
        .delete_range(utc_range, now)
        .map_err(|error| WriteFailure::data_delete(&error).into_protocol_error())?;
    let days = store
        .range_versions(utc_range)
        .map_err(|_| WriteFailure::Data(DataFailure::ReadVersions).into_protocol_error())?;
    let versions = VersionState {
        days,
        settings,
        rule_hashes: registry.snapshots().load().hashes().clone(),
        time_zone: zone,
    }
    .digest(utc_range)
    .map_err(|_| WriteFailure::Data(DataFailure::DigestVersions).into_protocol_error())?;
    Ok(WriteResponse::DataDeleteRange(DataDeleteRangeResponse {
        deleted_ms: summary.deleted_ms,
        versions,
    }))
}

fn resolve_delete_range(
    request: DataDeleteRangeRequest,
    now: UtcInstant,
    offset: DayStartOffset,
    zone: &TimeZone,
) -> Result<QueryRange, ProtocolError> {
    let today = DayCalendar::current_date_in_zone(now.get(), offset, zone.clone())
        .map_err(|_| WriteFailure::Data(DataFailure::TimeZone).into_protocol_error())?;
    if request.start_date >= request.end_date || request.end_date > today {
        return Err(ProtocolError::new(ErrorKind::BadRequest));
    }
    let mut cursor = request.start_date;
    let mut day_count: usize = 0;
    let mut last_date = request.start_date;
    while cursor < request.end_date {
        if day_count == RANGE_MAX_DAYS {
            return Err(ProtocolError::new(ErrorKind::BadRequest));
        }
        last_date = cursor;
        day_count += 1;
        cursor = cursor
            .tomorrow()
            .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
    }
    let first = DayCalendar::range_in_zone(request.start_date, offset, zone.clone())
        .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
    let last = if last_date == request.start_date {
        first
    } else {
        DayCalendar::range_in_zone(last_date, offset, zone.clone())
            .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?
    };
    if first.start >= last.end {
        return Err(ProtocolError::new(ErrorKind::BadRequest));
    }
    Ok(QueryRange {
        start: first.start,
        end: last.end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::LocalDate;

    fn date(value: &str) -> LocalDate {
        value.parse().expect("valid test date")
    }

    fn utc_ms(iso: &str) -> UtcInstant {
        let ts: jiff::Timestamp = iso.parse().expect("valid ISO timestamp");
        UtcInstant::new(ts)
    }

    #[test]
    fn delete_range_rejects_today_future_empty_reversed_and_oversize_ranges() {
        let zone = TimeZone::UTC;
        let now = utc_ms("2026-11-27T12:00:00Z"); // today = 2026-11-27

        let rejected = [
            // Empty range
            (date("2026-11-25"), date("2026-11-25")),
            // Reversed range
            (date("2026-11-26"), date("2026-11-25")),
            // Includes today (end_date = tomorrow)
            (date("2026-11-27"), date("2026-11-28")),
            (date("2026-11-25"), date("2026-11-28")),
            // Future range
            (date("2026-11-28"), date("2026-11-29")),
            // Oversize: 367 days (> RANGE_MAX_DAYS = 366)
            (date("2025-11-25"), date("2026-11-27")),
        ];
        for (start_date, end_date) in rejected {
            let err = resolve_delete_range(
                DataDeleteRangeRequest {
                    start_date,
                    end_date,
                },
                now,
                DayStartOffset::MIDNIGHT,
                &zone,
            )
            .expect_err("invalid range must be rejected");
            assert_eq!(err.kind, ErrorKind::BadRequest, "{start_date}..{end_date}");
        }

        // Exactly yesterday [2026-11-26, 2026-11-27) is accepted.
        let yesterday = resolve_delete_range(
            DataDeleteRangeRequest {
                start_date: date("2026-11-26"),
                end_date: date("2026-11-27"),
            },
            now,
            DayStartOffset::MIDNIGHT,
            &zone,
        )
        .expect("finished yesterday accepted");
        assert_eq!(
            yesterday.start,
            utc_ms("2026-11-26T00:00:00Z").get().as_millisecond()
        );
        assert_eq!(
            yesterday.end,
            utc_ms("2026-11-27T00:00:00Z").get().as_millisecond()
        );

        // Exactly RANGE_MAX_DAYS (366 days) ending at today is accepted.
        let max_span = resolve_delete_range(
            DataDeleteRangeRequest {
                start_date: date("2025-11-26"),
                end_date: date("2026-11-27"),
            },
            now,
            DayStartOffset::MIDNIGHT,
            &zone,
        )
        .expect("RANGE_MAX_DAYS accepted");
        assert_eq!(
            max_span.start,
            utc_ms("2025-11-26T00:00:00Z").get().as_millisecond()
        );
        assert_eq!(
            max_span.end,
            utc_ms("2026-11-27T00:00:00Z").get().as_millisecond()
        );

        // Respects day_start_offset: at 03:00 UTC with 04:00 offset, logical today is 2026-11-26.
        let offset = DayStartOffset::try_from(240).expect("offset");
        let early = utc_ms("2026-11-27T03:00:00Z");
        let err = resolve_delete_range(
            DataDeleteRangeRequest {
                start_date: date("2026-11-26"),
                end_date: date("2026-11-27"),
            },
            early,
            offset,
            &zone,
        )
        .expect_err("2026-11-26 is still current logical day before 04:00");
        assert_eq!(err.kind, ErrorKind::BadRequest);
        let finished = resolve_delete_range(
            DataDeleteRangeRequest {
                start_date: date("2026-11-25"),
                end_date: date("2026-11-26"),
            },
            early,
            offset,
            &zone,
        )
        .expect("2026-11-25 is finished");
        assert_eq!(
            finished.start,
            utc_ms("2026-11-25T04:00:00Z").get().as_millisecond()
        );
        assert_eq!(
            finished.end,
            utc_ms("2026-11-26T04:00:00Z").get().as_millisecond()
        );
    }
}
