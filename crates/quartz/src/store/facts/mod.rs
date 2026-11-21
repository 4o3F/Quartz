//! Shared capture read values and typed stored row details.

mod consistency;
mod dump;
mod read;
mod simulation;

pub(crate) use simulation::PrepareClassificationWorkloadError;

use super::{
    enums::{self, IntegerCode},
    identifiers::{ColumnName, TableName},
    schema::{
        applications, availability_intervals, capture_gaps, capture_runs, clock_segments,
        intervals, timed_policies, titles, web_intervals, window_instances,
    },
};

/// Ten tables receive generated capture data or expose the M5 web-row placeholder (03 §2).
const GENERATED_TABLE_COUNT: usize = 10;

#[derive(Debug, snafu::Snafu)]
pub(crate) enum ReadFactsError {
    #[snafu(display("could not read capture facts: {source}"))]
    Query { source: diesel::result::Error },
    #[snafu(display("missing non-null {kind} primary key"))]
    MissingId { kind: FactKind },
    #[snafu(display("missing {column} in {kind} {id}"))]
    MissingColumn {
        kind: FactKind,
        id: i64,
        column: ColumnName,
    },
    #[snafu(display("invalid monotonic endpoint state in {kind} {id}"))]
    InvalidMonotonicEnd { kind: FactKind, id: i64 },
    #[snafu(display("invalid endpoint state in {kind} {id}"))]
    InvalidEnd { kind: FactKind, id: i64 },
    #[snafu(display("invalid reason for coverage in {kind}"))]
    InvalidCoverage { kind: FactKind },
    #[snafu(display("incomplete closed-run extent"))]
    InvalidExtent,
    #[snafu(display("duration conservation failed: {detail}"))]
    Inconsistent { detail: String },
}
impl From<diesel::result::Error> for ReadFactsError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Query { source }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum FactKind {
    Run,
    ClockSegment,
    Foreground,
    NoForeground,
    Availability,
    CaptureGap,
    TimedPolicy,
}
impl FactKind {
    fn start_mono_column(self) -> Result<ColumnName, ReadFactsError> {
        match self {
            Self::ClockSegment => Ok(ColumnName::of(clock_segments::start_mono_ms)),
            Self::Foreground | Self::NoForeground => Ok(ColumnName::of(intervals::start_mono_ms)),
            Self::Availability => Ok(ColumnName::of(availability_intervals::start_mono_ms)),
            Self::CaptureGap => Ok(ColumnName::of(capture_gaps::start_mono_ms)),
            Self::Run | Self::TimedPolicy => Err(ReadFactsError::InvalidCoverage { kind: self }),
        }
    }
    pub(crate) fn label(self) -> &'static str {
        self.into()
    }
}
impl std::fmt::Display for FactKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

/// A persisted row has exactly one authoritative UTC endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactEnd {
    Closed(i64),
    Open { confirmed: i64 },
}
impl FactEnd {
    pub(crate) fn at(self) -> i64 {
        match self {
            Self::Closed(at) => at,
            Self::Open { confirmed } => confirmed,
        }
    }
    pub(crate) fn closed(self) -> Option<i64> {
        match self {
            Self::Closed(at) => Some(at),
            Self::Open { .. } => None,
        }
    }
    pub(crate) fn confirmed(self) -> Option<i64> {
        match self {
            Self::Closed(_) => None,
            Self::Open { confirmed } => Some(confirmed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum FactEndReason {
    Run(enums::RunEndReason),
    Clock(enums::ClockEndReason),
    Interval(enums::IntervalEndReason),
    Availability(enums::AvailabilityEndReason),
}
impl FactEndReason {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Run(reason) => reason.label(),
            Self::Clock(reason) => reason.label(),
            Self::Interval(reason) => reason.label(),
            Self::Availability(reason) => reason.label(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum FactReason {
    Availability(enums::AvailabilityReason),
    Gap(enums::GapReason),
}
impl FactReason {
    fn label(self) -> &'static str {
        match self {
            Self::Availability(reason) => reason.label(),
            Self::Gap(reason) => reason.label(),
        }
    }
}

/// Database values, without presentation or local-time conversion.
#[derive(Debug, Clone)]
pub(crate) struct Fact {
    pub kind: FactKind,
    pub id: i64,
    pub run_id: Option<i64>,
    pub segment_id: Option<i64>,
    pub start_utc_ms: i64,
    pub end: FactEnd,
    pub start_mono_ms: Option<i64>,
    pub end_mono_ms: Option<i64>,
    pub confirmed_mono_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub end_reason: Option<FactEndReason>,
    pub reason: Option<FactReason>,
    pub no_foreground_reason: Option<enums::NoForegroundReason>,
    pub capture_level: Option<enums::CaptureLevel>,
    pub title_status: Option<enums::TitleStatus>,
    pub exception: Option<enums::IdleException>,
    pub policy_kind: Option<enums::TimedPolicyKind>,
    pub privacy_mode: Option<enums::PrivacyMode>,
    details: Option<FactDetails>,
    window: Option<WindowDetails>,
}
impl Fact {
    /// Reuses the existing stored-fact representation for the typed wire record.
    pub(crate) fn record(&self) -> Result<quartz_protocol::facts::FactRecord, serde_json::Error> {
        serde_json::from_value(serde_json::to_value(self)?)
    }

    pub(crate) fn chronological_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.start_utc_ms
            .cmp(&other.start_utc_ms)
            .then_with(|| self.kind.label().cmp(other.kind.label()))
            .then_with(|| self.id.cmp(&other.id))
    }

    fn new(
        kind: FactKind,
        id: Option<i64>,
        start: i64,
        ended: Option<i64>,
        confirmed: Option<i64>,
    ) -> Result<Self, ReadFactsError> {
        let id = id.ok_or(ReadFactsError::MissingId { kind })?;
        let end = match (ended, confirmed) {
            (Some(at), None) => FactEnd::Closed(at),
            (None, Some(confirmed)) => FactEnd::Open { confirmed },
            (None, None) | (Some(_), Some(_)) => {
                return Err(ReadFactsError::InvalidEnd { kind, id });
            }
        };
        Ok(Self {
            kind,
            id,
            run_id: None,
            segment_id: None,
            start_utc_ms: start,
            end,
            start_mono_ms: None,
            end_mono_ms: None,
            confirmed_mono_ms: None,
            duration_ms: Some(end.at() - start),
            end_reason: None,
            reason: None,
            no_foreground_reason: None,
            capture_level: None,
            title_status: None,
            exception: None,
            policy_kind: None,
            privacy_mode: None,
            details: None,
            window: None,
        })
    }
    fn monotonic_duration(&self) -> Result<i64, ReadFactsError> {
        let start = self.start_mono_ms.ok_or(ReadFactsError::MissingColumn {
            kind: self.kind,
            id: self.id,
            column: self.kind.start_mono_column()?,
        })?;
        let end = match (self.end, self.end_mono_ms, self.confirmed_mono_ms) {
            (FactEnd::Closed(_), Some(end), None) => end,
            (FactEnd::Open { .. }, None, Some(confirmed)) => confirmed,
            (FactEnd::Closed(_), None, None | Some(_))
            | (FactEnd::Closed(_), Some(_), Some(_))
            | (FactEnd::Open { .. }, None, None)
            | (FactEnd::Open { .. }, Some(_), None | Some(_)) => {
                return Err(ReadFactsError::InvalidMonotonicEnd {
                    kind: self.kind,
                    id: self.id,
                });
            }
        };
        Ok(end - start)
    }
    pub(crate) fn endpoint(&self) -> i64 {
        self.end.at()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FactExtent {
    pub from_utc_ms: i64,
    pub to_utc_ms: i64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RangeRowCounts {
    pub foreground: usize,
    pub availability: usize,
    pub gaps: usize,
}
impl RangeRowCounts {
    pub(crate) fn total(self) -> usize {
        self.foreground + self.availability + self.gaps
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TableCount {
    pub name: TableName,
    pub rows: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowCounts {
    pub capture_runs: i64,
    pub clock_segments: i64,
    pub applications: i64,
    pub window_instances: i64,
    pub titles: i64,
    pub intervals: i64,
    pub availability_intervals: i64,
    pub capture_gaps: i64,
    pub timed_policies: i64,
    pub web_intervals: i64,
}
impl RowCounts {
    pub(crate) fn fact_rows(self) -> i64 {
        self.intervals + self.availability_intervals + self.capture_gaps
    }
    pub(crate) fn tables(self) -> [TableCount; GENERATED_TABLE_COUNT] {
        [
            TableCount {
                name: TableName::of(applications::table),
                rows: self.applications,
            },
            TableCount {
                name: TableName::of(availability_intervals::table),
                rows: self.availability_intervals,
            },
            TableCount {
                name: TableName::of(capture_gaps::table),
                rows: self.capture_gaps,
            },
            TableCount {
                name: TableName::of(capture_runs::table),
                rows: self.capture_runs,
            },
            TableCount {
                name: TableName::of(clock_segments::table),
                rows: self.clock_segments,
            },
            TableCount {
                name: TableName::of(intervals::table),
                rows: self.intervals,
            },
            TableCount {
                name: TableName::of(timed_policies::table),
                rows: self.timed_policies,
            },
            TableCount {
                name: TableName::of(titles::table),
                rows: self.titles,
            },
            TableCount {
                name: TableName::of(web_intervals::table),
                rows: self.web_intervals,
            },
            TableCount {
                name: TableName::of(window_instances::table),
                rows: self.window_instances,
            },
        ]
    }
}

#[derive(Debug)]
pub(crate) struct FactRange {
    pub foreground: Vec<Fact>,
    pub availability: Vec<Fact>,
    pub gaps: Vec<Fact>,
}
impl FactRange {
    pub(crate) fn counts(&self) -> RangeRowCounts {
        RangeRowCounts {
            foreground: self.foreground.len(),
            availability: self.availability.len(),
            gaps: self.gaps.len(),
        }
    }
}

#[derive(Debug, Clone)]
enum FactDetails {
    Interval {
        sequence: i64,
        session_key: i64,
        app_id: Option<i64>,
        window_instance_id: Option<i64>,
        privacy_rule_id: Option<String>,
        privacy_label_key: Option<String>,
        privacy_label: Option<String>,
    },
    Availability {
        last_input_utc_ms: Option<i64>,
        detected_utc_ms: Option<i64>,
        threshold_ms: Option<i64>,
        policy_version: i64,
    },
    Run {
        device_id: String,
        win_session_id: i64,
        app_version: String,
    },
    TimedPolicy {
        application_id: Option<i64>,
    },
}

#[derive(Debug, Clone)]
struct WindowDetails {
    display_name: Option<String>,
    exe_path: Option<String>,
    exe_name: Option<String>,
    title: Option<String>,
    hwnd: Option<i64>,
    generation: Option<i64>,
    pid: Option<i64>,
    window_class: Option<String>,
}

#[cfg(test)]
mod tests;
