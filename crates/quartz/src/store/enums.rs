//! Stable integer encodings shared with migration enum_labels.

use diesel::{
    deserialize::FromSqlRow,
    deserialize::{self, FromSql},
    expression::AsExpression,
    serialize::{self, IsNull, Output, ToSql},
    sql_types::Integer,
    sqlite::{Sqlite, SqliteValue},
};

use num_enum::{IntoPrimitive, TryFromPrimitive};
use strum::{IntoStaticStr, VariantArray};

#[derive(Debug, snafu::Snafu)]
#[snafu(display("unknown {enum_name} code {code}"))]
pub(super) struct DecodeEnumError {
    pub(super) enum_name: &'static str,
    pub(super) code: i32,
}

/// Column context attached to unknown integer codes.
pub(super) trait EnumColumn {
    const COLUMN: &'static str;
}

/// Stable encodings and labels derived from each enum's explicit variants.
pub(super) trait IntegerCode:
    EnumColumn + Copy + Into<i32> + TryFrom<i32> + VariantArray + Into<&'static str> + 'static
{
    fn code(self) -> i32 {
        self.into()
    }
    fn label(self) -> &'static str {
        self.into()
    }
    fn write_sql(self, out: &mut Output<'_, '_, Sqlite>) -> serialize::Result {
        out.set_value(self.code());
        Ok(IsNull::No)
    }
    fn read_sql(value: SqliteValue<'_, '_, '_>) -> deserialize::Result<Self> {
        let code = <i32 as FromSql<Integer, Sqlite>>::from_sql(value)?;
        Self::try_from(code).map_err(|_| {
            DecodeEnumError {
                enum_name: Self::COLUMN,
                code,
            }
            .into()
        })
    }
}
impl<T> IntegerCode for T where
    T: EnumColumn + Copy + Into<i32> + TryFrom<i32> + VariantArray + Into<&'static str> + 'static
{
}

// Diesel's foreign traits require an implementation for each concrete enum type.
macro_rules! impl_integer_sql {
    ($($name:ty),+ $(,)?) => {
        $(
            impl ToSql<Integer, Sqlite> for $name {
                fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Sqlite>) -> serialize::Result {
                    self.write_sql(out)
                }
            }
            impl FromSql<Integer, Sqlite> for $name {
                fn from_sql(value: SqliteValue<'_, '_, '_>) -> deserialize::Result<Self> {
                    Self::read_sql(value)
                }
            }
        )+
    };
}

/// Why a capture run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum RunEndReason {
    Shutdown = 1,
    UserExit = 2,
    /// The next startup closed the run at its last confirmed point.
    CrashRecovered = 3,
    Update = 4,
}

impl EnumColumn for RunEndReason {
    const COLUMN: &'static str = "capture_runs.end_reason";
}

/// Why a UTC-to-monotonic clock segment ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum ClockEndReason {
    TimeChange = 1,
    Resume = 2,
    /// The UTC-to-monotonic mapping drifted.
    Drift = 3,
    /// The enclosing capture run ended.
    RunEnd = 4,
    /// Recovery closed the segment at its last confirmed point.
    CrashRecovered = 5,
}

impl EnumColumn for ClockEndReason {
    const COLUMN: &'static str = "clock_segments.end_reason";
}

/// Identity used to distinguish an application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum ApplicationKind {
    /// Identified by its executable path.
    Exe = 1,
    /// Identified by its Windows application user model ID.
    Aumid = 2,
    /// Identified by its window class when application identity is unavailable.
    Unresolved = 3,
}

impl EnumColumn for ApplicationKind {
    const COLUMN: &'static str = "applications.kind";
}

/// Whether a foreground interval represents a window or no foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum IntervalKind {
    Window = 1,
    NoForeground = 2,
}

impl EnumColumn for IntervalKind {
    const COLUMN: &'static str = "intervals.kind";
}

/// Why an interval has no foreground window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum NoForegroundReason {
    /// There is no foreground window.
    Empty = 1,
    /// The foreground desktop cannot be identified.
    UnknownDesktop = 2,
}

impl EnumColumn for NoForegroundReason {
    const COLUMN: &'static str = "intervals.no_foreground_reason";
}

/// Title availability shared by foreground and browser records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum TitleStatus {
    /// Title text was read successfully.
    Ok = 1,
    /// The title is empty.
    Empty = 2,
    /// Reading the title failed.
    Unreadable = 3,
    /// The title exceeded the 4096 UTF-16-unit limit.
    Truncated = 4,
    /// Privacy policy discarded the title.
    Redacted = 5,
    /// Retention cleanup removed the title.
    Purged = 6,
}

impl EnumColumn for TitleStatus {
    const COLUMN: &'static str = "title_status";
}

/// Privacy level retained for a foreground interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum CaptureLevel {
    /// Retain application identity and title.
    Full = 1,
    /// Retain application identity without the title.
    TitleHidden = 2,
    /// Retain only time bounds and the privacy rule snapshot.
    Excluded = 3,
}

impl EnumColumn for CaptureLevel {
    const COLUMN: &'static str = "intervals.capture_level";
}

/// Boundary that closed a foreground interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum IntervalEndReason {
    /// The foreground window changed.
    Switch = 1,
    /// The foreground title changed.
    Title = 2,
    NoForeground = 3,
    Locked = 4,
    Suspended = 5,
    Paused = 6,
    Disconnected = 7,
    Shutdown = 8,
    /// Recovery closed the interval at its last confirmed point.
    CrashRecovered = 9,
    /// The clock mapping changed.
    ClockJump = 10,
    /// A local midnight split preserves the shared session key.
    MidnightSplit = 11,
    /// The effective capture policy changed.
    PolicyChanged = 12,
    /// Capture stopped producing trustworthy facts.
    CaptureGap = 13,
}

impl EnumColumn for IntervalEndReason {
    const COLUMN: &'static str = "intervals.end_reason";
}

/// State recorded as an availability interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum AvailabilityReason {
    Idle = 1,
    Locked = 2,
    Disconnected = 3,
    Suspended = 4,
    Paused = 5,
}

impl EnumColumn for AvailabilityReason {
    const COLUMN: &'static str = "availability_intervals.reason";
}

/// Boundary that closed an availability interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum AvailabilityEndReason {
    /// The recorded availability state ended.
    Resumed = 1,
    Shutdown = 2,
    /// Recovery closed the interval at its last confirmed point.
    CrashRecovered = 3,
    /// The clock mapping changed.
    ClockJump = 4,
    /// A local midnight split ended this interval.
    MidnightSplit = 5,
    /// The effective availability policy changed.
    PolicyChanged = 6,
}

impl EnumColumn for AvailabilityEndReason {
    const COLUMN: &'static str = "availability_intervals.end_reason";
}

/// Exception that ended an idle interval early.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum IdleException {
    /// The user enabled keep-alive timing.
    KeepAlive = 1,
    /// The foreground application is exempt from idle detection.
    AppExempt = 2,
    /// Audible playback suppresses idle detection.
    Audible = 3,
}

impl EnumColumn for IdleException {
    const COLUMN: &'static str = "availability_intervals.exception";
}

/// Boundary that closed a browser interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum WebEndReason {
    /// The browser reported a new state.
    BrowserState = 1,
    /// The linked foreground interval ended.
    ForegroundEnd = 2,
    /// The browser stream disconnected.
    Disconnected = 3,
    /// A browser capture gap interrupted the interval.
    Gap = 4,
    /// The effective web privacy policy changed.
    PolicyChanged = 5,
    /// Recovery closed the interval at its last confirmed point.
    CrashRecovered = 6,
    Shutdown = 7,
}

impl EnumColumn for WebEndReason {
    const COLUMN: &'static str = "web_intervals.end_reason";
}

/// Confidence of a browser interval's foreground association.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum LinkStatus {
    /// The browser interval has an unambiguous foreground association.
    Linked = 1,
    /// The foreground association is ambiguous.
    Ambiguous = 2,
}

impl EnumColumn for LinkStatus {
    const COLUMN: &'static str = "web_intervals.link_status";
}

/// Privacy level retained for a stored browser interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum WebLevel {
    /// Retain domain and tab title.
    Full = 1,
    /// Retain the domain and redact the tab title.
    DomainOnly = 2,
}

impl EnumColumn for WebLevel {
    const COLUMN: &'static str = "web_intervals.web_level";
}

/// Durable result used to acknowledge a browser message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum IngestOutcome {
    /// The message produced stored browser intervals.
    Stored = 1,
    /// The message did not overlap a foreground interval.
    DiscardedNoOverlap = 2,
    /// Privacy policy discarded the message.
    DiscardedPrivacy = 3,
    /// The message fell in a capture gap.
    DiscardedGap = 4,
    /// A deletion barrier discarded the late message.
    DiscardedBarrier = 5,
}

impl EnumColumn for IngestOutcome {
    const COLUMN: &'static str = "ingest_receipts.outcome";
}

/// Contribution of a category to focus and distraction totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum CategoryTendency {
    Focus = 1,
    Neutral = 2,
    Distraction = 3,
}

impl EnumColumn for CategoryTendency {
    const COLUMN: &'static str = "categories.tendency";
}

/// Rule family evaluated by a rule-set tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum RuleSetKind {
    Classification = 1,
    Extraction = 2,
    Privacy = 3,
    /// Precedent rules belonging to a precedent chain.
    Precedent = 4,
    /// Policy checks evaluated by the policy subsystem.
    PolicyCheck = 5,
}

impl EnumColumn for RuleSetKind {
    const COLUMN: &'static str = "rule_sets.kind";
}

/// Whether a rule set is built in or supplied by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum RuleSetSource {
    Builtin = 1,
    User = 2,
}

impl EnumColumn for RuleSetSource {
    const COLUMN: &'static str = "rule_sets.source";
}

/// User annotation applied over a time range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum AnnotationKind {
    /// Override the computed category.
    CategoryOverride = 1,
    /// Add activity where no effective foreground activity exists.
    OfflineActivity = 2,
    /// Restore allowed activity masked by idle detection.
    ActiveOverride = 3,
    /// Record dismissal of an away period without changing duration.
    DismissedAway = 4,
    Note = 5,
}

impl EnumColumn for AnnotationKind {
    const COLUMN: &'static str = "annotations.kind";
}

/// Fact source whose per-day version invalidates cached results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum DataSource {
    Intervals = 1,
    /// Availability intervals.
    Availability = 2,
    /// Browser intervals.
    Web = 3,
    Annotations = 4,
    /// Capture gaps.
    Gaps = 5,
}

impl EnumColumn for DataSource {
    const COLUMN: &'static str = "data_day_versions.source";
}

/// Failure that left a capture interval without trustworthy facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[derive(IntoPrimitive, TryFromPrimitive, VariantArray, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum GapReason {
    /// The event input exceeded its capacity.
    EventOverload = 1,
    /// The database write queue exceeded its capacity.
    WriteQueueOverload = 2,
    DatabaseError = 3,
}

impl EnumColumn for GapReason {
    const COLUMN: &'static str = "capture_gaps.reason";
}

/// Persisted policy identities; scheduler-only slots have no database code.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    AsExpression,
    FromSqlRow,
    IntoPrimitive,
    TryFromPrimitive,
    VariantArray,
    IntoStaticStr,
)]
#[diesel(sql_type = Integer)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum TimedPolicyKind {
    Pause = 1,
    Privacy = 2,
    KeepTiming = 3,
    AppExemption = 4,
    Audible = 5,
}
impl EnumColumn for TimedPolicyKind {
    const COLUMN: &'static str = "timed_policies.kind";
}
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    AsExpression,
    FromSqlRow,
    IntoPrimitive,
    TryFromPrimitive,
    VariantArray,
    IntoStaticStr,
)]
#[diesel(sql_type = Integer)]
#[strum(serialize_all = "snake_case")]
#[repr(i32)]
pub(super) enum PrivacyMode {
    Normal = 1,
    Private = 2,
    NoTitles = 3,
}
impl EnumColumn for PrivacyMode {
    const COLUMN: &'static str = "timed_policies.privacy_mode";
}
impl From<quartz_domain::capture::TimedPolicyKind> for TimedPolicyKind {
    fn from(kind: quartz_domain::capture::TimedPolicyKind) -> Self {
        use quartz_domain::capture::TimedPolicyKind as Kind;
        match kind {
            Kind::Pause => Self::Pause,
            Kind::Privacy => Self::Privacy,
            Kind::KeepTiming => Self::KeepTiming,
            Kind::AppExemption => Self::AppExemption,
            Kind::Audible => Self::Audible,
        }
    }
}
impl From<PrivacyMode> for quartz_domain::capture::PrivacyMode {
    fn from(mode: PrivacyMode) -> Self {
        match mode {
            PrivacyMode::Normal => Self::Normal,
            PrivacyMode::Private => Self::Private,
            PrivacyMode::NoTitles => Self::NoTitles,
        }
    }
}
impl From<quartz_domain::capture::PrivacyMode> for PrivacyMode {
    fn from(mode: quartz_domain::capture::PrivacyMode) -> Self {
        use quartz_domain::capture::PrivacyMode as Mode;
        match mode {
            Mode::Normal => Self::Normal,
            Mode::Private => Self::Private,
            Mode::NoTitles => Self::NoTitles,
        }
    }
}

#[rustfmt::skip]
impl_integer_sql!(TimedPolicyKind, PrivacyMode, RunEndReason, ClockEndReason, ApplicationKind, IntervalKind, NoForegroundReason, TitleStatus, CaptureLevel, IntervalEndReason, AvailabilityReason, AvailabilityEndReason, IdleException, WebEndReason, LinkStatus, WebLevel, IngestOutcome, CategoryTendency, RuleSetKind, RuleSetSource, AnnotationKind, DataSource, GapReason);

impl From<quartz_domain::capture::RunEndReason> for RunEndReason {
    fn from(value: quartz_domain::capture::RunEndReason) -> Self {
        match value {
            quartz_domain::capture::RunEndReason::Shutdown => Self::Shutdown,
            quartz_domain::capture::RunEndReason::UserExit => Self::UserExit,
            quartz_domain::capture::RunEndReason::CrashRecovered => Self::CrashRecovered,
            quartz_domain::capture::RunEndReason::Update => Self::Update,
        }
    }
}

impl From<quartz_domain::capture::ClockEndReason> for ClockEndReason {
    fn from(value: quartz_domain::capture::ClockEndReason) -> Self {
        match value {
            quartz_domain::capture::ClockEndReason::TimeChange => Self::TimeChange,
            quartz_domain::capture::ClockEndReason::Resume => Self::Resume,
            quartz_domain::capture::ClockEndReason::Drift => Self::Drift,
            quartz_domain::capture::ClockEndReason::RunEnd => Self::RunEnd,
            quartz_domain::capture::ClockEndReason::CrashRecovered => Self::CrashRecovered,
        }
    }
}

impl From<quartz_domain::capture::IntervalEndReason> for IntervalEndReason {
    fn from(value: quartz_domain::capture::IntervalEndReason) -> Self {
        match value {
            quartz_domain::capture::IntervalEndReason::Switch => Self::Switch,
            quartz_domain::capture::IntervalEndReason::Title => Self::Title,
            quartz_domain::capture::IntervalEndReason::NoForeground => Self::NoForeground,
            quartz_domain::capture::IntervalEndReason::Locked => Self::Locked,
            quartz_domain::capture::IntervalEndReason::Suspended => Self::Suspended,
            quartz_domain::capture::IntervalEndReason::Paused => Self::Paused,
            quartz_domain::capture::IntervalEndReason::Disconnected => Self::Disconnected,
            quartz_domain::capture::IntervalEndReason::Shutdown => Self::Shutdown,
            quartz_domain::capture::IntervalEndReason::CrashRecovered => Self::CrashRecovered,
            quartz_domain::capture::IntervalEndReason::ClockJump => Self::ClockJump,
            quartz_domain::capture::IntervalEndReason::MidnightSplit => Self::MidnightSplit,
            quartz_domain::capture::IntervalEndReason::PolicyChanged => Self::PolicyChanged,
            quartz_domain::capture::IntervalEndReason::CaptureGap => Self::CaptureGap,
        }
    }
}

impl From<quartz_domain::capture::GapReason> for GapReason {
    fn from(value: quartz_domain::capture::GapReason) -> Self {
        match value {
            quartz_domain::capture::GapReason::EventOverload => Self::EventOverload,
            quartz_domain::capture::GapReason::WriteQueueOverload => Self::WriteQueueOverload,
            quartz_domain::capture::GapReason::DatabaseError => Self::DatabaseError,
        }
    }
}

impl From<quartz_domain::capture::TitleStatus> for TitleStatus {
    fn from(value: quartz_domain::capture::TitleStatus) -> Self {
        match value {
            quartz_domain::capture::TitleStatus::Ok => Self::Ok,
            quartz_domain::capture::TitleStatus::Empty => Self::Empty,
            quartz_domain::capture::TitleStatus::Unreadable => Self::Unreadable,
            quartz_domain::capture::TitleStatus::Truncated => Self::Truncated,
            quartz_domain::capture::TitleStatus::Redacted => Self::Redacted,
        }
    }
}

impl From<quartz_domain::capture::CaptureLevel> for CaptureLevel {
    fn from(value: quartz_domain::capture::CaptureLevel) -> Self {
        match value {
            quartz_domain::capture::CaptureLevel::Full => Self::Full,
            quartz_domain::capture::CaptureLevel::TitleHidden => Self::TitleHidden,
            quartz_domain::capture::CaptureLevel::Excluded => Self::Excluded,
        }
    }
}

impl From<quartz_domain::capture::NoForegroundReason> for NoForegroundReason {
    fn from(value: quartz_domain::capture::NoForegroundReason) -> Self {
        match value {
            quartz_domain::capture::NoForegroundReason::Empty => Self::Empty,
            quartz_domain::capture::NoForegroundReason::UnknownDesktop => Self::UnknownDesktop,
        }
    }
}

impl From<quartz_domain::capture::IdleException> for IdleException {
    fn from(value: quartz_domain::capture::IdleException) -> Self {
        match value {
            quartz_domain::capture::IdleException::KeepTiming => Self::KeepAlive,
            quartz_domain::capture::IdleException::AppExempt => Self::AppExempt,
            quartz_domain::capture::IdleException::Audible => Self::Audible,
        }
    }
}

impl From<quartz_domain::capture::AvailabilityReason> for AvailabilityReason {
    fn from(value: quartz_domain::capture::AvailabilityReason) -> Self {
        use quartz_domain::capture::{AvailabilityReason as Reason, StopReason};
        match value {
            Reason::Idle => Self::Idle,
            Reason::Stopped(reason) => match reason {
                StopReason::Locked => Self::Locked,
                StopReason::Disconnected => Self::Disconnected,
                StopReason::Suspended => Self::Suspended,
                StopReason::Paused => Self::Paused,
            },
        }
    }
}
impl From<quartz_domain::capture::AvailabilityEndReason> for AvailabilityEndReason {
    fn from(value: quartz_domain::capture::AvailabilityEndReason) -> Self {
        use quartz_domain::capture::AvailabilityEndReason as Reason;
        match value {
            Reason::Resumed | Reason::Exception(_) => Self::Resumed,
            Reason::Shutdown => Self::Shutdown,
            Reason::CrashRecovered => Self::CrashRecovered,
            Reason::ClockJump => Self::ClockJump,
            Reason::MidnightSplit => Self::MidnightSplit,
            Reason::PolicyChanged => Self::PolicyChanged,
        }
    }
}

impl From<NoForegroundReason> for quartz_domain::capture::NoForegroundReason {
    fn from(value: NoForegroundReason) -> Self {
        match value {
            NoForegroundReason::Empty => Self::Empty,
            NoForegroundReason::UnknownDesktop => Self::UnknownDesktop,
        }
    }
}
impl From<CaptureLevel> for quartz_domain::capture::CaptureLevel {
    fn from(value: CaptureLevel) -> Self {
        match value {
            CaptureLevel::Full => Self::Full,
            CaptureLevel::TitleHidden => Self::TitleHidden,
            CaptureLevel::Excluded => Self::Excluded,
        }
    }
}
impl From<AvailabilityReason> for quartz_domain::capture::AvailabilityReason {
    fn from(value: AvailabilityReason) -> Self {
        use quartz_domain::capture::StopReason;
        match value {
            AvailabilityReason::Idle => Self::Idle,
            AvailabilityReason::Locked => Self::Stopped(StopReason::Locked),
            AvailabilityReason::Disconnected => Self::Stopped(StopReason::Disconnected),
            AvailabilityReason::Suspended => Self::Stopped(StopReason::Suspended),
            AvailabilityReason::Paused => Self::Stopped(StopReason::Paused),
        }
    }
}
#[derive(Debug, snafu::Snafu)]
#[snafu(display("title status {code} has no capture representation"))]
pub(crate) struct ConvertTitleStatusError {
    pub(super) code: i32,
}
impl TryFrom<TitleStatus> for quartz_domain::capture::TitleStatus {
    type Error = ConvertTitleStatusError;
    fn try_from(value: TitleStatus) -> Result<Self, Self::Error> {
        match value {
            TitleStatus::Ok => Ok(Self::Ok),
            TitleStatus::Empty => Ok(Self::Empty),
            TitleStatus::Unreadable => Ok(Self::Unreadable),
            TitleStatus::Truncated => Ok(Self::Truncated),
            TitleStatus::Redacted => Ok(Self::Redacted),
            TitleStatus::Purged => Err(ConvertTitleStatusError { code: value.code() }),
        }
    }
}
impl IdleException {
    pub(super) fn from_ending(
        reason: quartz_domain::capture::AvailabilityEndReason,
    ) -> Option<Self> {
        use quartz_domain::capture::AvailabilityEndReason as Reason;
        match reason {
            Reason::Exception(exception) => Some(exception.into()),
            Reason::Resumed
            | Reason::Shutdown
            | Reason::CrashRecovered
            | Reason::ClockJump
            | Reason::MidnightSplit
            | Reason::PolicyChanged => None,
        }
    }
}
