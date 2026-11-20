//! Local-calendar-day capture inspection for acceptance tooling.

use crate::{
    data_directory::DataDirectory,
    store::{
        ReadOnlyStore,
        facts::{Fact, FactEnd, FactKind, ReadFactsError},
    },
};
use clap::ValueEnum;
use jiff::{civil::Date, tz::TimeZone};
use snafu::{OptionExt, ResultExt, Snafu};
use std::io::{self, Write};

/// Common columns have a fixed position in the text record documented on `DumpDay::write`.
const COMMON_FIELDS: [&str; 12] = [
    "kind",
    "id",
    "run_id",
    "segment_id",
    "start_utc_ms",
    "end_utc_ms",
    "confirmed_utc_ms",
    "start_mono_ms",
    "end_mono_ms",
    "confirmed_mono_ms",
    "duration_ms",
    "end_reason",
];
/// Fixed ASCII positions in the YYYY-MM-DD input format (i18n 10 §6.1).
const DATE_BYTES: usize = 10;
const YEAR_SEPARATOR: usize = 4;
const MONTH_SEPARATOR: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub(crate) enum DumpFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Snafu)]
pub(crate) enum DumpDayError {
    #[snafu(display("could not resolve production data directory: {source}"))]
    Directory {
        source: crate::data_directory::ResolveDataDirectoryError,
    },
    #[snafu(display("could not interpret date or local time zone: {source}"))]
    Date { source: jiff::Error },
    #[snafu(display("serialized fact must be an object"))]
    InvalidShape,
    #[snafu(display("date must use YYYY-MM-DD"))]
    DateFormat,
    #[snafu(display("could not open capture database: {source}"))]
    Open {
        source: crate::store::OpenStoreError,
    },
    #[snafu(display("{source}"))]
    Read { source: ReadFactsError },
    #[snafu(display("could not write dump output: {source}"))]
    Output { source: io::Error },
    #[snafu(display("could not serialize capture fact: {source}"))]
    Json { source: serde_json::Error },
}

#[derive(Debug)]
pub(crate) struct DumpDay<'date> {
    date: &'date str,
    format: DumpFormat,
}
impl<'date> DumpDay<'date> {
    pub(crate) fn new(date: &'date str, format: DumpFormat) -> Self {
        Self { date, format }
    }
    pub(crate) fn run(&self) -> Result<(), DumpDayError> {
        let directory = DataDirectory::resolve().context(DirectorySnafu)?;
        let zone = TimeZone::try_system().context(DateSnafu)?;
        let mut store = ReadOnlyStore::open_facts(&directory.database()).context(OpenSnafu)?;
        self.write(&mut store, &zone, &mut io::stdout().lock())
    }
    /// Text uses one chronological record per line: `kind id=N start=LOCAL (UTC_MS) end=LOCAL (UTC_MS)|open (confirmed LOCAL (UTC_MS)) duration_ms=N end_reason=LABEL`, followed by sorted `key=JSON` fields. Strings are JSON escaped, absent scalar values are `null`, and stored titles alone produce a `title` field. Ties sort by kind then database ID. JSON emits the same records as one object per line with numeric UTC milliseconds and explicit local endpoints; there is no header or summary in either format. Rows retain their full stored endpoints rather than being clipped to the day.
    pub(crate) fn write(
        &self,
        store: &mut ReadOnlyStore,
        zone: &TimeZone,
        output: &mut impl Write,
    ) -> Result<(), DumpDayError> {
        snafu::ensure!(
            self.date.len() == DATE_BYTES
                && self.date.as_bytes()[YEAR_SEPARATOR] == b'-'
                && self.date.as_bytes()[MONTH_SEPARATOR] == b'-'
                && self
                    .date
                    .bytes()
                    .enumerate()
                    .all(|(index, byte)| index == YEAR_SEPARATOR
                        || index == MONTH_SEPARATOR
                        || byte.is_ascii_digit()),
            DateFormatSnafu
        );
        let date: Date = self.date.parse().context(DateSnafu)?;
        let from = date
            .to_zoned_datetime(zone.clone())
            .context(DateSnafu)?
            .timestamp()
            .as_millisecond();
        let to = date
            .tomorrow()
            .context(DateSnafu)?
            .to_zoned_datetime(zone.clone())
            .context(DateSnafu)?
            .timestamp()
            .as_millisecond();
        for fact in store.dump_range(from, to).context(ReadSnafu)? {
            self.write_fact(&fact, zone, output)?;
        }
        Ok(())
    }
    fn local(value: i64, zone: &TimeZone) -> Result<String, DumpDayError> {
        Ok(jiff::Timestamp::from_millisecond(value)
            .context(DateSnafu)?
            .to_zoned_datetime(zone.clone())
            .to_string())
    }
    fn write_fact(
        &self,
        fact: &Fact,
        zone: &TimeZone,
        output: &mut impl Write,
    ) -> Result<(), DumpDayError> {
        let start = Self::local(fact.start_utc_ms, zone)?;
        let end = Self::local(fact.endpoint(), zone)?;
        let mut value = serde_json::to_value(fact).context(JsonSnafu)?;
        // Keep nullable columns present on the row kinds that own them.
        match fact.kind {
            FactKind::Foreground | FactKind::NoForeground => {
                value["no_foreground_reason"] = serde_json::json!(fact.no_foreground_reason);
            }
            FactKind::Availability => value["exception"] = serde_json::json!(fact.exception),
            FactKind::TimedPolicy => value["privacy_mode"] = serde_json::json!(fact.privacy_mode),
            FactKind::Run | FactKind::ClockSegment | FactKind::CaptureGap => {}
        }
        match self.format {
            DumpFormat::Json => {
                value["start_local"] = start.into();
                value["end_local"] = if matches!(fact.end, FactEnd::Closed(_)) {
                    end.clone().into()
                } else {
                    serde_json::Value::Null
                };
                value["confirmed_local"] = if matches!(fact.end, FactEnd::Open { .. }) {
                    end.into()
                } else {
                    serde_json::Value::Null
                };
                serde_json::to_writer(&mut *output, &value).context(JsonSnafu)?;
                writeln!(output).context(OutputSnafu)?;
            }
            DumpFormat::Text => {
                write!(
                    output,
                    "{} id={} start={} ({}) end=",
                    fact.kind, fact.id, start, fact.start_utc_ms
                )
                .context(OutputSnafu)?;
                if matches!(fact.end, FactEnd::Closed(_)) {
                    write!(output, "{end} ({})", fact.endpoint()).context(OutputSnafu)?;
                } else {
                    write!(output, "open (confirmed {end} ({}))", fact.endpoint())
                        .context(OutputSnafu)?;
                }
                write!(
                    output,
                    " duration_ms={} end_reason={}",
                    fact.duration_ms
                        .map_or_else(|| "null".into(), |value| value.to_string()),
                    match fact.end_reason {
                        Some(reason) => reason.label(),
                        None => "null",
                    }
                )
                .context(OutputSnafu)?;
                write!(
                    output,
                    " run_id={} segment_id={}",
                    serde_json::json!(fact.run_id),
                    serde_json::json!(fact.segment_id)
                )
                .context(OutputSnafu)?;
                let fields = value.as_object_mut().context(InvalidShapeSnafu)?;
                for key in COMMON_FIELDS {
                    fields.remove(key);
                }
                for (key, value) in fields {
                    write!(output, " {key}={value}").context(OutputSnafu)?;
                }
                writeln!(output).context(OutputSnafu)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        Store,
        capture::{CaptureKeys, RunMetadata, WindowsSessionId},
    };
    use quartz_domain::capture::{
        self as capture, AvailabilityEndReason, AvailabilityReason, CaptureGap, CaptureWrite,
        Checkpoint, ClockEndReason, GapReason, IntervalEndReason, IntervalFact, MonoInstant,
        NoForegroundReason, OpenAvailability, OpenInterval, RowId, RunEndReason, RunId, SegmentId,
        SessionKey, StopReason, TimeRange, TimedPolicy, TimedState, Timestamp, UtcInstant,
    };

    fn at(text: &str) -> Timestamp {
        let utc: jiff::Timestamp = text.parse().expect("fixture timestamp");
        Timestamp {
            utc: UtcInstant::new(utc),
            mono: MonoInstant::try_from(utc.as_millisecond())
                .expect("fixture uses nonnegative monotonic readings"),
        }
    }
    #[test]
    fn dumps_midnight_pieces_open_confirmations_and_local_calendar_day() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let directory = DataDirectory::new(temporary.path());
        let mut store = Store::open(&directory).expect("create migrated database");
        let start = at("2025-01-01T23:59:00Z");
        let midnight = at("2025-01-02T00:00:00Z");
        let one = at("2025-01-02T00:01:00Z");
        let two = at("2025-01-02T00:02:00Z");
        let three = at("2025-01-02T00:03:00Z");
        let end = at("2025-01-02T00:04:00Z");
        let run = RunId::new(1);
        let segment = SegmentId::from_persisted(1);
        let id = RowId::from_persisted(1);
        let mut writes = vec![
            CaptureWrite::OpenRun { run, at: start },
            CaptureWrite::OpenClockSegment { segment, at: start },
            CaptureWrite::OpenInterval(OpenInterval {
                id,
                segment,
                session: SessionKey::from_persisted(1),
                start,
                fact: IntervalFact::Window {
                    app: Some(capture::AppIdentity::Exe(r"c:\test\editor.exe".into())),
                    instance: Some(capture::WindowId::from_persisted(1)),
                    fields: capture::CapturedFields {
                        level: capture::CaptureLevel::TitleHidden,
                        title: None,
                        title_status: capture::TitleStatus::Redacted,
                        privacy: Some(capture::PrivacyExplanation {
                            rule_id: "test@1".into(),
                            label: Some(capture::PrivacyLabel::Key(
                                "privacy.label.title_hidden".into(),
                            )),
                        }),
                        web: quartz_domain::rules::WebAction::None,
                    },
                },
            }),
            CaptureWrite::CloseInterval {
                id,
                at: one,
                reason: IntervalEndReason::NoForeground,
            },
            CaptureWrite::OpenInterval(OpenInterval {
                id: RowId::from_persisted(2),
                segment,
                session: SessionKey::from_persisted(2),
                start: one,
                fact: IntervalFact::Window {
                    app: Some(capture::AppIdentity::Exe(r"c:\test\editor.exe".into())),
                    instance: Some(capture::WindowId::from_persisted(1)),
                    fields: capture::CapturedFields {
                        level: capture::CaptureLevel::Full,
                        title: Some("文档\n\"quoted\"".into()),
                        title_status: capture::TitleStatus::Ok,
                        privacy: None,
                        web: quartz_domain::rules::WebAction::Full,
                    },
                },
            }),
            CaptureWrite::CloseInterval {
                id: RowId::from_persisted(2),
                at: two,
                reason: IntervalEndReason::Locked,
            },
            CaptureWrite::OpenAvailability(OpenAvailability {
                id: RowId::from_persisted(3),
                segment,
                reason: AvailabilityReason::Stopped(StopReason::Locked),
                start: two,
                idle: None,
            }),
            CaptureWrite::CloseAvailability {
                id: RowId::from_persisted(3),
                at: three,
                reason: AvailabilityEndReason::Resumed,
            },
            CaptureWrite::RecordGap(CaptureGap {
                segment,
                range: TimeRange { start: three, end },
                reason: GapReason::EventOverload,
            }),
            CaptureWrite::SetTimedState(TimedState {
                policy: TimedPolicy::KeepTiming,
                started: one.utc,
                until: two.utc,
            }),
            CaptureWrite::CloseClockSegment {
                segment,
                at: end,
                reason: ClockEndReason::RunEnd,
            },
            CaptureWrite::CloseRun {
                run,
                at: end,
                reason: RunEndReason::Shutdown,
            },
        ];
        // Production's capture core emits explicit foreground pieces at UTC midnight.
        let CaptureWrite::OpenInterval(mut second_piece) = writes[2].clone() else {
            panic!("fixture foreground");
        };
        second_piece.id = RowId::from_persisted(4);
        second_piece.start = midnight;
        writes[3] = CaptureWrite::CloseInterval {
            id,
            at: midnight,
            reason: IntervalEndReason::MidnightSplit,
        };
        writes.splice(
            4..4,
            [
                CaptureWrite::OpenInterval(second_piece),
                CaptureWrite::CloseInterval {
                    id: RowId::from_persisted(4),
                    at: one,
                    reason: IntervalEndReason::NoForeground,
                },
            ],
        );
        writes.insert(
            2,
            CaptureWrite::ObserveWindow {
                instance: capture::WindowInstance {
                    id: capture::WindowId::from_persisted(1),
                    handle: capture::WindowHandle::new(42),
                    process: capture::ProcessId::new(7),
                    process_created_utc: Some(start.utc),
                    generation: capture::InstanceGeneration::initial(),
                    class: "TestWindow".into(),
                    app: capture::AppIdentity::Exe(r"c:\test\editor.exe".into()),
                    metadata_source: capture::MetadataSource::Process,
                    exe_name: Some("editor.exe".into()),
                    display_name: "Editor".into(),
                    first_seen: start,
                },
                at: start,
            },
        );
        let metadata = RunMetadata {
            device_id: "test-device".into(),
            win_session_id: WindowsSessionId::new(1),
            app_version: "test".into(),
        };
        let mut keys = CaptureKeys::default();
        store
            .capture_transaction(&mut keys, &metadata, |capture| {
                capture.execute_writes(run, &writes)
            })
            .expect("capture write path");
        let open_run = RunId::new(2);
        let open_start = at("2025-01-02T00:05:00Z");
        let confirmed = at("2025-01-02T00:06:00Z");
        let open_writes = vec![
            CaptureWrite::OpenRun {
                run: open_run,
                at: open_start,
            },
            CaptureWrite::OpenClockSegment {
                segment,
                at: open_start,
            },
            CaptureWrite::OpenInterval(OpenInterval {
                id,
                segment,
                session: SessionKey::from_persisted(1),
                start: open_start,
                fact: IntervalFact::NoForeground(NoForegroundReason::Empty),
            }),
            CaptureWrite::Checkpoint(Checkpoint {
                run: open_run,
                segment,
                interval: Some(id),
                availability: Vec::new(),
                at: confirmed,
                commit_by_mono: confirmed.mono,
            }),
        ];
        store
            .capture_transaction(&mut keys, &metadata, |capture| {
                capture.execute_writes(open_run, &open_writes)
            })
            .expect("open capture rows");
        // Keep the writer alive: reads must work concurrently with the WAL writer.
        let mut reader =
            ReadOnlyStore::open(&directory.database()).expect("read-only WAL snapshot");
        let zone = TimeZone::fixed(jiff::tz::Offset::from_hours(2).expect("fixed test zone"));
        let mut output = Vec::new();
        DumpDay::new("2025-01-02", DumpFormat::Text)
            .write(&mut reader, &zone, &mut output)
            .expect("text dump");
        let text = String::from_utf8(output).expect("UTF-8 output");
        assert!(
            text.contains("start=2025-01-02T01:59:00+02:00"),
            "local endpoint: {text}"
        );
        assert!(
            text.contains(&format!("({})", midnight.utc.get().as_millisecond())),
            "UTC midnight piece"
        );
        for expected in [
            "foreground id=",
            "no_foreground id=",
            "availability id=",
            "capture_gap id=",
            "timed_policy id=",
            "end_reason=midnight_split",
            "capture_level=\"title_hidden\"",
            "reason=\"locked\"",
            "open (confirmed 2025-01-02T02:06:00+02:00",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(
            text.contains("app_display_name=\"Editor\""),
            "application display name"
        );
        assert!(
            text.contains("window_instance_id=1"),
            "stored window identity"
        );
        let mut json = Vec::new();
        DumpDay::new("2025-01-02", DumpFormat::Json)
            .write(&mut reader, &zone, &mut json)
            .expect("JSON dump");
        let rows: Vec<serde_json::Value> = String::from_utf8(json)
            .expect("UTF-8 JSON")
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect();
        let locked = rows
            .iter()
            .find(|row| row["reason"] == "locked")
            .expect("locked availability");
        assert_eq!(locked.get("exception"), Some(&serde_json::Value::Null));
        let policy = rows
            .iter()
            .find(|row| row["kind"] == "timed_policy")
            .expect("timed policy");
        assert_eq!(policy.get("privacy_mode"), Some(&serde_json::Value::Null));
        assert_eq!(
            text.lines().count(),
            rows.len(),
            "escaped titles preserve one record per line"
        );
        assert!(
            rows.iter()
                .any(|row| row["title"] == "文档\n\"quoted\"" && row["hwnd"] == 42),
            "stored Unicode title and window details"
        );
        assert!(
            rows.iter()
                .filter(|row| row["capture_level"] == "title_hidden")
                .all(|row| row.get("title").is_none()),
            "redacted titles are absent"
        );
        assert!(
            rows.windows(2)
                .all(|rows| rows[0]["start_utc_ms"].as_i64() <= rows[1]["start_utc_ms"].as_i64()),
            "chronological order"
        );
        assert!(
            rows.iter().any(
                |row| row["confirmed_utc_ms"] == confirmed.utc.get().as_millisecond()
                    && row["end_utc_ms"].is_null()
            ),
            "open confirmation"
        );
        for date in ["2025-01-01", "2025-01-03"] {
            let mut empty = Vec::new();
            DumpDay::new(date, DumpFormat::Json)
                .write(&mut reader, &zone, &mut empty)
                .expect("adjacent day dump");
            assert!(
                empty.is_empty(),
                "local date {date} must have no intersecting records"
            );
        }
    }
}
