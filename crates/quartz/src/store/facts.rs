//! Snapshot reads for offline acceptance commands; all range predicates stay in storage.

use super::{
    ReadOnlyStore, Store,
    enums::{self, IntegerCode},
    rows::{
        AvailabilityIntervalsRow, CaptureGapsRow, CaptureRunsRow, ClockSegmentsRow, IntervalsRow,
        TimedPoliciesRow,
    },
    schema::{
        applications, availability_intervals, capture_gaps, capture_runs, clock_segments,
        intervals, timed_policies, titles, web_intervals, window_instances,
    },
};
use diesel::{
    BoolExpressionMethods, Connection, ExpressionMethods, JoinOnDsl, NullableExpressionMethods,
    QueryDsl, RunQueryDsl, SelectableHelper,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Closed rows are split at UTC midnight (data model 03 §5.1).
pub(crate) const UTC_DAY_MS: i64 = 86_400_000;
/// SQLite's guaranteed minimum bind limit, data model 03 §4.5.
const SQLITE_BIND_LIMIT: usize = 999;
/// Ten tables receive generated capture data or expose the M5 web-row placeholder (03 §2).
const GENERATED_TABLE_COUNT: usize = 10;

#[derive(Debug, diesel::Queryable)]
struct IntervalDetails {
    id: i64,
    display_name: Option<String>,
    exe_path: Option<String>,
    exe_name: Option<String>,
    title: Option<String>,
    hwnd: Option<i64>,
    generation: Option<i64>,
    pid: Option<i64>,
    window_class: Option<String>,
}

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
        column: &'static str,
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CoverageCounts {
    facts: i32,
    stops: i32,
    gaps: i32,
    idle: i32,
}
impl CoverageCounts {
    fn adjust(&mut self, fact: &Fact, delta: i32) -> Result<(), ReadFactsError> {
        let invalid = || ReadFactsError::InvalidCoverage { kind: fact.kind };
        let count = match fact.kind {
            FactKind::Availability => match fact.reason {
                Some(FactReason::Availability(enums::AvailabilityReason::Idle)) => &mut self.idle,
                Some(FactReason::Availability(
                    enums::AvailabilityReason::Locked
                    | enums::AvailabilityReason::Disconnected
                    | enums::AvailabilityReason::Suspended
                    | enums::AvailabilityReason::Paused,
                )) => &mut self.stops,
                None | Some(FactReason::Gap(_)) => return Err(invalid()),
            },
            FactKind::CaptureGap => match fact.reason {
                Some(FactReason::Gap(_)) => &mut self.gaps,
                None | Some(FactReason::Availability(_)) => return Err(invalid()),
            },
            FactKind::Foreground | FactKind::NoForeground => &mut self.facts,
            FactKind::Run | FactKind::ClockSegment | FactKind::TimedPolicy => return Err(invalid()),
        };
        *count += delta;
        Ok(())
    }
    fn apply(&mut self, delta: Self) {
        self.facts += delta.facts;
        self.stops += delta.stops;
        self.gaps += delta.gaps;
        self.idle += delta.idle;
    }
    fn conserved(self) -> bool {
        self.facts + i32::from(self.stops > 0) + self.gaps == 1
            && self.idle <= 1
            && [self.facts, self.stops, self.gaps, self.idle]
                .iter()
                .all(|count| *count >= 0)
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
    pub(crate) fn label(self) -> &'static str {
        self.into()
    }
}
impl std::fmt::Display for FactKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}
impl Serialize for FactKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
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
impl Serialize for FactEnd {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut fields = serializer.serialize_struct("FactEnd", 2)?;
        fields.serialize_field("end_utc_ms", &self.closed())?;
        fields.serialize_field("confirmed_utc_ms", &self.confirmed())?;
        fields.end()
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
impl Serialize for FactEndReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
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
impl Serialize for FactReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

/// Database values, without presentation or local-time conversion.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Fact {
    pub kind: FactKind,
    pub id: i64,
    pub run_id: Option<i64>,
    pub segment_id: Option<i64>,
    pub start_utc_ms: i64,
    #[serde(flatten)]
    pub end: FactEnd,
    pub start_mono_ms: Option<i64>,
    pub end_mono_ms: Option<i64>,
    pub confirmed_mono_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub end_reason: Option<FactEndReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<FactReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_foreground_reason: Option<enums::NoForegroundReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_level: Option<enums::CaptureLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_status: Option<enums::TitleStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<enums::IdleException>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_kind: Option<enums::TimedPolicyKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privacy_mode: Option<enums::PrivacyMode>,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}
impl Fact {
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
            fields: BTreeMap::new(),
        })
    }
    fn details(mut self, fields: BTreeMap<String, Value>) -> Self {
        self.fields.extend(fields);
        self
    }
    fn monotonic_duration(&self) -> Result<i64, ReadFactsError> {
        let start = self.start_mono_ms.ok_or(ReadFactsError::MissingColumn {
            kind: self.kind,
            id: self.id,
            column: "start_mono_ms",
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
#[derive(Debug, diesel::Queryable)]
struct NullableExtent {
    from_utc_ms: Option<i64>,
    to_utc_ms: Option<i64>,
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
    pub name: &'static str,
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
                name: "applications",
                rows: self.applications,
            },
            TableCount {
                name: "availability_intervals",
                rows: self.availability_intervals,
            },
            TableCount {
                name: "capture_gaps",
                rows: self.capture_gaps,
            },
            TableCount {
                name: "capture_runs",
                rows: self.capture_runs,
            },
            TableCount {
                name: "clock_segments",
                rows: self.clock_segments,
            },
            TableCount {
                name: "intervals",
                rows: self.intervals,
            },
            TableCount {
                name: "timed_policies",
                rows: self.timed_policies,
            },
            TableCount {
                name: "titles",
                rows: self.titles,
            },
            TableCount {
                name: "web_intervals",
                rows: self.web_intervals,
            },
            TableCount {
                name: "window_instances",
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
    fn into_rows(self) -> Vec<Fact> {
        self.foreground
            .into_iter()
            .chain(self.availability)
            .chain(self.gaps)
            .collect()
    }
}

impl ReadOnlyStore {
    /// A transaction pins one WAL snapshot across all three tables.
    pub(crate) fn fact_range(&mut self, from: i64, to: i64) -> Result<FactRange, ReadFactsError> {
        self.connection
            .transaction(|connection| Self::load_range(connection, from, to))
    }
    fn load_range(
        connection: &mut diesel::SqliteConnection,
        from: i64,
        to: i64,
    ) -> Result<FactRange, ReadFactsError> {
        // Keep closed and open reads separate: open availability may span many days.
        let mut foreground = Vec::new();
        for rows in [
            intervals::table
                .filter(intervals::start_utc_ms.ge(from.saturating_sub(UTC_DAY_MS)))
                .filter(intervals::start_utc_ms.lt(to))
                .filter(intervals::end_utc_ms.gt(from))
                .select(IntervalsRow::as_select())
                .load::<IntervalsRow>(connection)?,
            intervals::table
                .filter(intervals::end_utc_ms.is_null())
                .filter(intervals::start_utc_ms.lt(to))
                .filter(intervals::confirmed_utc_ms.gt(from))
                .select(IntervalsRow::as_select())
                .load::<IntervalsRow>(connection)?,
        ] {
            for row in rows {
                foreground.push(row.into_fact()?);
            }
        }
        let mut availability = Vec::new();
        for rows in [
            availability_intervals::table
                .filter(availability_intervals::start_utc_ms.ge(from.saturating_sub(UTC_DAY_MS)))
                .filter(availability_intervals::start_utc_ms.lt(to))
                .filter(availability_intervals::end_utc_ms.gt(from))
                .select(AvailabilityIntervalsRow::as_select())
                .load::<AvailabilityIntervalsRow>(connection)?,
            availability_intervals::table
                .filter(availability_intervals::end_utc_ms.is_null())
                .filter(availability_intervals::start_utc_ms.lt(to))
                .filter(availability_intervals::confirmed_utc_ms.gt(from))
                .select(AvailabilityIntervalsRow::as_select())
                .load::<AvailabilityIntervalsRow>(connection)?,
        ] {
            for row in rows {
                availability.push(row.into_fact()?);
            }
        }
        let gaps = capture_gaps::table
            .filter(capture_gaps::start_utc_ms.lt(to))
            .filter(capture_gaps::end_utc_ms.gt(from))
            .select(CaptureGapsRow::as_select())
            .load::<CaptureGapsRow>(connection)?
            .into_iter()
            .map(CaptureGapsRow::into_fact)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FactRange {
            foreground,
            availability,
            gaps,
        })
    }
    /// Adds stored dictionaries and enclosing run/clock/policy rows within the same snapshot.
    pub(crate) fn dump_range(&mut self, from: i64, to: i64) -> Result<Vec<Fact>, ReadFactsError> {
        self.connection.transaction(|connection| {
            let mut range = Self::load_range(connection, from, to)?;
            let ids: Vec<_> = range.foreground.iter().map(|fact| fact.id).collect();
            let mut details = BTreeMap::new();
            for chunk in ids.chunks(SQLITE_BIND_LIMIT) {
                let rows = intervals::table
                    .left_join(applications::table.on(intervals::app_id.eq(applications::id)))
                    .left_join(titles::table.on(intervals::title_id.eq(titles::id)))
                    .left_join(
                        window_instances::table
                            .on(intervals::window_instance_id.eq(window_instances::id)),
                    )
                    .filter(intervals::id.eq_any(chunk))
                    .select((
                        intervals::id.assume_not_null(),
                        applications::display_name.nullable(),
                        applications::exe_path.nullable(),
                        applications::exe_name.nullable(),
                        titles::text.nullable(),
                        window_instances::hwnd.nullable(),
                        window_instances::generation.nullable(),
                        window_instances::pid.nullable(),
                        window_instances::window_class.nullable(),
                    ))
                    .load::<IntervalDetails>(connection)?;
                details.extend(rows.into_iter().map(|row| (row.id, row)));
            }
            for fact in &mut range.foreground {
                if let Some(row) = details.remove(&fact.id) {
                    fact.fields
                        .insert("app_display_name".into(), json!(row.display_name));
                    fact.fields.insert("exe".into(), json!(row.exe_path));
                    fact.fields.insert("exe_name".into(), json!(row.exe_name));
                    fact.fields.insert("hwnd".into(), json!(row.hwnd));
                    fact.fields
                        .insert("window_generation".into(), json!(row.generation));
                    fact.fields.insert("pid".into(), json!(row.pid));
                    fact.fields
                        .insert("window_class".into(), json!(row.window_class));
                    if let Some(title) = row.title {
                        fact.fields.insert("title".into(), json!(title));
                    }
                }
            }
            let mut facts = range.into_rows();
            let runs = capture_runs::table
                .filter(capture_runs::started_utc_ms.lt(to))
                .filter(
                    capture_runs::ended_utc_ms
                        .gt(from)
                        .or(capture_runs::ended_utc_ms
                            .is_null()
                            .and(capture_runs::confirmed_utc_ms.gt(from))),
                )
                .select(CaptureRunsRow::as_select())
                .load::<CaptureRunsRow>(connection)?;
            for row in runs {
                let mut fact = Fact::new(
                    FactKind::Run,
                    row.id,
                    row.started_utc_ms,
                    row.ended_utc_ms,
                    row.confirmed_utc_ms,
                )?;
                fact.run_id = row.id;
                fact.end_reason = row.end_reason.map(FactEndReason::Run);
                facts.push(fact.details(BTreeMap::from([
                    ("device_id".into(), json!(row.device_id)),
                    ("win_session_id".into(), json!(row.win_session_id)),
                    ("app_version".into(), json!(row.app_version)),
                ])));
            }
            let segments = clock_segments::table
                .filter(clock_segments::start_utc_ms.lt(to))
                .filter(
                    clock_segments::end_utc_ms
                        .gt(from)
                        .or(clock_segments::end_utc_ms
                            .is_null()
                            .and(clock_segments::confirmed_utc_ms.gt(from))),
                )
                .select(ClockSegmentsRow::as_select())
                .load::<ClockSegmentsRow>(connection)?;
            for row in segments {
                let mut fact = Fact::new(
                    FactKind::ClockSegment,
                    row.id,
                    row.start_utc_ms,
                    row.end_utc_ms,
                    row.confirmed_utc_ms,
                )?;
                fact.run_id = Some(row.run_id);
                fact.segment_id = row.id;
                fact.start_mono_ms = Some(row.start_mono_ms);
                fact.end_mono_ms = row.end_mono_ms;
                fact.confirmed_mono_ms = row.confirmed_mono_ms;
                fact.duration_ms = Some(fact.monotonic_duration()?);
                fact.end_reason = row.end_reason.map(FactEndReason::Clock);
                facts.push(fact);
            }
            let policies = timed_policies::table
                .filter(timed_policies::started_utc_ms.lt(to))
                .filter(timed_policies::until_utc_ms.gt(from))
                .select(TimedPoliciesRow::as_select())
                .load::<TimedPoliciesRow>(connection)?;
            for row in policies {
                let mut fact = Fact::new(
                    FactKind::TimedPolicy,
                    Some(i64::from(row.kind.code())),
                    row.started_utc_ms,
                    Some(row.until_utc_ms),
                    None,
                )?;
                fact.policy_kind = Some(row.kind);
                fact.privacy_mode = row.privacy_mode;
                fact.fields
                    .insert("application_id".into(), json!(row.application_id));
                facts.push(fact);
            }
            facts.sort_by_key(|fact| (fact.start_utc_ms, fact.kind.label(), fact.id));
            Ok(facts)
        })
    }
    pub(crate) fn extent(&mut self) -> Result<Option<FactExtent>, ReadFactsError> {
        use diesel::dsl::{max, min};
        let extent = capture_runs::table
            .filter(capture_runs::ended_utc_ms.is_not_null())
            .select((
                min(capture_runs::started_utc_ms),
                max(capture_runs::ended_utc_ms),
            ))
            .first::<NullableExtent>(&mut self.connection)?;
        match (extent.from_utc_ms, extent.to_utc_ms) {
            (Some(from_utc_ms), Some(to_utc_ms)) => Ok(Some(FactExtent {
                from_utc_ms,
                to_utc_ms,
            })),
            (None, None) => Ok(None),
            (Some(_), None) | (None, Some(_)) => Err(ReadFactsError::InvalidExtent),
        }
    }
    pub(crate) fn row_counts(&mut self) -> Result<RowCounts, ReadFactsError> {
        Ok(RowCounts {
            capture_runs: capture_runs::table
                .count()
                .get_result(&mut self.connection)?,
            clock_segments: clock_segments::table
                .count()
                .get_result(&mut self.connection)?,
            applications: applications::table
                .count()
                .get_result(&mut self.connection)?,
            window_instances: window_instances::table
                .count()
                .get_result(&mut self.connection)?,
            titles: titles::table.count().get_result(&mut self.connection)?,
            intervals: intervals::table.count().get_result(&mut self.connection)?,
            availability_intervals: availability_intervals::table
                .count()
                .get_result(&mut self.connection)?,
            capture_gaps: capture_gaps::table
                .count()
                .get_result(&mut self.connection)?,
            timed_policies: timed_policies::table
                .count()
                .get_result(&mut self.connection)?,
            web_intervals: web_intervals::table
                .count()
                .get_result(&mut self.connection)?,
        })
    }
    /// Checks complete sampled runs using monotonic coverage and independent idle masks.
    pub(crate) fn check_consistency(&mut self, from: i64, to: i64) -> Result<(), ReadFactsError> {
        let facts = self.dump_range(from, to)?;
        let fail = |detail| ReadFactsError::Inconsistent { detail };
        for run in facts.iter().filter(|row| row.kind == FactKind::Run) {
            if run.start_utc_ms < from
                || run.endpoint() > to
                || matches!(run.end, FactEnd::Open { .. })
            {
                return Err(fail(format!(
                    "run {} is not complete within the check range",
                    run.id
                )));
            }
            let mut segments: Vec<_> = facts
                .iter()
                .filter(|row| row.kind == FactKind::ClockSegment && row.run_id == Some(run.id))
                .collect();
            segments.sort_by_key(|row| (row.start_mono_ms, row.id));
            let Some(first) = segments.first() else {
                return Err(fail(format!("run {} has no segments", run.id)));
            };
            let last = segments.last().expect("nonempty segments");
            if first.start_utc_ms != run.start_utc_ms
                || last.endpoint() != run.endpoint()
                || segments
                    .windows(2)
                    .any(|pair| pair[0].end_mono_ms != pair[1].start_mono_ms)
            {
                return Err(fail(format!(
                    "run {} has discontinuous clock segments",
                    run.id
                )));
            }
        }
        for segment in facts
            .iter()
            .filter(|row| row.kind == FactKind::ClockSegment)
        {
            let left = segment
                .start_mono_ms
                .ok_or_else(|| fail("segment has no monotonic start".into()))?;
            let right = segment
                .end_mono_ms
                .ok_or_else(|| fail("segment has no monotonic end".into()))?;
            if right < left {
                return Err(fail(format!(
                    "segment {} has reversed endpoints",
                    segment.id
                )));
            }
            // Sweep boundaries rather than every pair of spans; stop masks are unioned.
            let mut boundaries = BTreeMap::<i64, CoverageCounts>::new();
            boundaries.entry(left).or_default();
            boundaries.entry(right).or_default();
            for fact in facts.iter().filter(|row| {
                row.segment_id == Some(segment.id) && row.kind != FactKind::ClockSegment
            }) {
                let start = fact
                    .start_mono_ms
                    .ok_or_else(|| fail("fact has no monotonic start".into()))?;
                let end = fact
                    .end_mono_ms
                    .ok_or_else(|| fail("fact has no monotonic end".into()))?;
                if start < left
                    || end > right
                    || end < start
                    || fact.endpoint() < fact.start_utc_ms
                    || fact.duration_ms != Some(end - start)
                {
                    return Err(fail(format!(
                        "invalid endpoints or duration in {} {}",
                        fact.kind, fact.id
                    )));
                }
                if fact.kind != FactKind::CaptureGap
                    && fact.endpoint() - fact.start_utc_ms > UTC_DAY_MS
                {
                    return Err(fail("row exceeds 24 hours".into()));
                }
                boundaries.entry(start).or_default().adjust(fact, 1)?;
                boundaries.entry(end).or_default().adjust(fact, -1)?;
            }
            let mut active = CoverageCounts::default();
            let mut previous = left;
            for (at, delta) in boundaries {
                if at > previous && !active.conserved() {
                    return Err(fail(format!(
                        "segment {} coverage at [{previous}, {at}): {active:?}",
                        segment.id
                    )));
                }
                active.apply(delta);
                previous = at;
            }
            if active != CoverageCounts::default() {
                return Err(fail(format!(
                    "segment {} has unsettled coverage",
                    segment.id
                )));
            }
        }
        Ok(())
    }
}
impl IntervalsRow {
    fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let kind = match self.kind {
            enums::IntervalKind::Window => FactKind::Foreground,
            enums::IntervalKind::NoForeground => FactKind::NoForeground,
        };
        let mut fact = Fact::new(
            kind,
            self.id,
            self.start_utc_ms,
            self.end_utc_ms,
            self.confirmed_utc_ms,
        )?;
        fact.run_id = Some(self.run_id);
        fact.segment_id = Some(self.clock_segment_id);
        fact.start_mono_ms = Some(self.start_mono_ms);
        fact.end_mono_ms = self.end_mono_ms;
        fact.confirmed_mono_ms = self.confirmed_mono_ms;
        fact.duration_ms = Some(match fact.end {
            FactEnd::Closed(_) => self.duration_ms.ok_or(ReadFactsError::MissingColumn {
                kind,
                id: fact.id,
                column: "duration_ms",
            })?,
            FactEnd::Open { .. } => fact.monotonic_duration()?,
        });
        fact.end_reason = self.end_reason.map(FactEndReason::Interval);
        fact.no_foreground_reason = self.no_foreground_reason;
        fact.capture_level = Some(self.capture_level);
        fact.title_status = Some(self.title_status);
        Ok(fact.details(BTreeMap::from([
            ("sequence".into(), json!(self.sequence)),
            ("session_key".into(), json!(self.session_key)),
            ("app_id".into(), json!(self.app_id)),
            ("window_instance_id".into(), json!(self.window_instance_id)),
            ("privacy_rule_id".into(), json!(self.privacy_rule_id)),
            ("privacy_label_key".into(), json!(self.privacy_label_key)),
            ("privacy_label".into(), json!(self.privacy_label)),
        ])))
    }
}
impl AvailabilityIntervalsRow {
    fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let mut fact = Fact::new(
            FactKind::Availability,
            self.id,
            self.start_utc_ms,
            self.end_utc_ms,
            self.confirmed_utc_ms,
        )?;
        fact.run_id = Some(self.run_id);
        fact.segment_id = Some(self.clock_segment_id);
        fact.start_mono_ms = Some(self.start_mono_ms);
        fact.end_mono_ms = self.end_mono_ms;
        fact.confirmed_mono_ms = self.confirmed_mono_ms;
        fact.duration_ms = Some(fact.monotonic_duration()?);
        fact.end_reason = self.end_reason.map(FactEndReason::Availability);
        fact.reason = Some(FactReason::Availability(self.reason));
        fact.exception = self.exception;
        Ok(fact.details(BTreeMap::from([
            ("last_input_utc_ms".into(), json!(self.last_input_utc_ms)),
            ("detected_utc_ms".into(), json!(self.detected_utc_ms)),
            ("threshold_ms".into(), json!(self.threshold_ms)),
            ("policy_version".into(), json!(self.policy_version)),
        ])))
    }
}
impl CaptureGapsRow {
    fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let mut fact = Fact::new(
            FactKind::CaptureGap,
            self.id,
            self.start_utc_ms,
            Some(self.end_utc_ms),
            None,
        )?;
        fact.run_id = Some(self.run_id);
        fact.segment_id = Some(self.clock_segment_id);
        fact.start_mono_ms = Some(self.start_mono_ms);
        fact.end_mono_ms = Some(self.end_mono_ms);
        fact.duration_ms = Some(self.end_mono_ms - self.start_mono_ms);
        fact.reason = Some(FactReason::Gap(self.reason));
        Ok(fact)
    }
}

impl Store {
    /// Closes the reader first so the final WAL writer can checkpoint the simulation file.
    pub(crate) fn finish_simulation(self) {
        let Self {
            _writer: writer,
            _reader: reader,
        } = self;
        drop(reader);
        drop(writer);
    }

    /// Migrates a caller-reserved, empty simulation file without production backups or locks.
    pub(crate) fn create_simulation(path: &std::path::Path) -> Result<Self, super::OpenStoreError> {
        use diesel_migrations::MigrationHarness;
        use snafu::{OptionExt, ResultExt};
        let mut writer =
            diesel::SqliteConnection::establish(path.to_str().context(super::InvalidPathSnafu)?)
                .context(super::ConnectSnafu)?;
        super::raw::ConnectionSql::new(&mut writer)
            .configure_writer()
            .context(super::ConfigureSnafu)?;
        super::migrations::typed(writer.run_pending_migrations(super::migrations::Migrations))
            .context(super::LoadMigrationsSnafu)?;
        Ok(Self {
            _writer: writer,
            _reader: ReadOnlyStore::connect(path)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        data_directory::DataDirectory,
        store::capture::{CaptureKeys, RunMetadata, WindowsSessionId},
    };
    use quartz_domain::capture::{
        self as capture, AvailabilityEndReason, AvailabilityReason, CaptureWrite, ClockEndReason,
        OpenAvailability, RowId, RunEndReason, RunId, SegmentId, StopReason,
    };

    #[test]
    fn decoding_rejects_missing_ids_and_ambiguous_endpoints() {
        let kind = FactKind::Foreground;
        assert!(matches!(
            Fact::new(kind, None, 0, Some(1), None),
            Err(ReadFactsError::MissingId {
                kind: FactKind::Foreground
            })
        ));
        for (end, confirmed) in [(None, None), (Some(1), Some(1))] {
            assert!(matches!(
                Fact::new(kind, Some(1), 0, end, confirmed),
                Err(ReadFactsError::InvalidEnd { id: 1, .. })
            ));
        }
        let closed = Fact::new(kind, Some(1), 0, Some(2), None).expect("closed endpoint");
        let open = Fact::new(kind, Some(1), 0, None, Some(2)).expect("open confirmation");
        assert_eq!(closed.end, FactEnd::Closed(2));
        assert_eq!(open.end, FactEnd::Open { confirmed: 2 });
        let json = serde_json::to_value(open).expect("serialize typed endpoint");
        assert!(json["end_utc_ms"].is_null());
        assert_eq!(json["confirmed_utc_ms"], 2);
        assert_eq!(json["kind"], "foreground");
        assert_eq!(
            serde_json::to_value(enums::AvailabilityReason::Locked).expect("strum label"),
            "locked"
        );
    }

    #[test]
    fn consistency_unions_stops_and_rejects_a_coverage_hole() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        for (name, covered_ms, expected) in
            [("complete", 60_000, Ok(())), ("missing", 30_000, Err(()))]
        {
            let directory = DataDirectory::new(temporary.path().join(name));
            std::fs::create_dir(directory.as_path()).expect("data directory");
            let mut store = Store::open(&directory).expect("database");
            let at = |millis| capture::Timestamp {
                utc: capture::UtcInstant::try_from(millis).expect("UTC time"),
                mono: capture::MonoInstant::try_from(millis)
                    .expect("fixture uses nonnegative monotonic readings"),
            };
            let run = RunId::new(1);
            let segment = SegmentId::from_persisted(1);
            let mut writes = vec![
                CaptureWrite::OpenRun { run, at: at(0) },
                CaptureWrite::OpenClockSegment { segment, at: at(0) },
            ];
            for (logical, reason, start, end) in [
                (
                    1,
                    AvailabilityReason::Stopped(StopReason::Locked),
                    0,
                    covered_ms,
                ),
                (
                    2,
                    AvailabilityReason::Stopped(StopReason::Disconnected),
                    10_000,
                    20_000,
                ),
                (3, AvailabilityReason::Idle, 0, 60_000),
            ] {
                let id = RowId::from_persisted(logical);
                writes.push(CaptureWrite::OpenAvailability(OpenAvailability {
                    id,
                    segment,
                    reason,
                    start: at(start),
                    idle: None,
                }));
                writes.push(CaptureWrite::CloseAvailability {
                    id,
                    at: at(end),
                    reason: AvailabilityEndReason::Resumed,
                });
            }
            writes.extend([
                CaptureWrite::CloseClockSegment {
                    segment,
                    at: at(60_000),
                    reason: ClockEndReason::RunEnd,
                },
                CaptureWrite::CloseRun {
                    run,
                    at: at(60_000),
                    reason: RunEndReason::Shutdown,
                },
            ]);
            let metadata = RunMetadata {
                device_id: "test".into(),
                win_session_id: WindowsSessionId::new(1),
                app_version: "test".into(),
            };
            store
                .capture_transaction(&mut CaptureKeys::default(), &metadata, |capture| {
                    capture.execute_writes(run, &writes)
                })
                .expect("capture writes");
            let result = ReadOnlyStore::open(&directory.database())
                .expect("read-only database")
                .check_consistency(0, 60_000);
            assert_eq!(
                result.map_err(|_| ()),
                expected,
                "{name}: stop masks must be unioned and idle must remain independent"
            );
        }
    }
}
