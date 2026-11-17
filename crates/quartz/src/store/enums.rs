//! Stable integer encodings shared with migration enum_labels.

use diesel::{
    deserialize::FromSqlRow,
    deserialize::{self, FromSql},
    expression::AsExpression,
    serialize::{self, IsNull, Output, ToSql},
    sql_types::Integer,
    sqlite::{Sqlite, SqliteValue},
};

#[derive(Debug, snafu::Snafu)]
#[snafu(display("unknown {enum_name} code {code}"))]
pub(super) struct DecodeEnumError {
    pub(super) enum_name: &'static str,
    pub(super) code: i32,
}

/// Stable codes and diagnostic labels for one database enum column.
pub(super) trait IntegerCode: Copy + 'static {
    const COLUMN: &'static str;
    const ALL: &'static [Self];

    fn code(self) -> i32;

    fn from_code(code: i32) -> Option<Self> {
        Self::ALL.iter().copied().find(|value| value.code() == code)
    }

    fn label(self) -> &'static str;
}

fn to_sql_code<T: IntegerCode>(value: T, out: &mut Output<'_, '_, Sqlite>) -> serialize::Result {
    out.set_value(value.code());
    Ok(IsNull::No)
}

fn from_sql_code<T: IntegerCode>(value: SqliteValue<'_, '_, '_>) -> deserialize::Result<T> {
    let code = <i32 as FromSql<Integer, Sqlite>>::from_sql(value)?;
    T::from_code(code).ok_or_else(|| {
        DecodeEnumError {
            enum_name: T::COLUMN,
            code,
        }
        .into()
    })
}

// Diesel's foreign traits require an implementation for each concrete enum type.
macro_rules! impl_integer_sql {
    ($($name:ty),+ $(,)?) => {
        $(
            impl ToSql<Integer, Sqlite> for $name {
                fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Sqlite>) -> serialize::Result {
                    to_sql_code(*self, out)
                }
            }
            impl FromSql<Integer, Sqlite> for $name {
                fn from_sql(value: SqliteValue<'_, '_, '_>) -> deserialize::Result<Self> {
                    from_sql_code(value)
                }
            }
        )+
    };
}

/// Why a capture run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum RunEndReason {
    Shutdown = 1,
    UserExit = 2,
    /// The next startup closed the run at its last confirmed point.
    CrashRecovered = 3,
    Update = 4,
}

impl IntegerCode for RunEndReason {
    const COLUMN: &'static str = "capture_runs.end_reason";
    const ALL: &'static [Self] = &[
        Self::Shutdown,
        Self::UserExit,
        Self::CrashRecovered,
        Self::Update,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::UserExit => "user_exit",
            Self::CrashRecovered => "crash_recovered",
            Self::Update => "update",
        }
    }
}

/// Why a UTC-to-monotonic clock segment ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
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

impl IntegerCode for ClockEndReason {
    const COLUMN: &'static str = "clock_segments.end_reason";
    const ALL: &'static [Self] = &[
        Self::TimeChange,
        Self::Resume,
        Self::Drift,
        Self::RunEnd,
        Self::CrashRecovered,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::TimeChange => "time_change",
            Self::Resume => "resume",
            Self::Drift => "drift",
            Self::RunEnd => "run_end",
            Self::CrashRecovered => "crash_recovered",
        }
    }
}

/// Identity used to distinguish an application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum ApplicationKind {
    /// Identified by its executable path.
    Exe = 1,
    /// Identified by its Windows application user model ID.
    Aumid = 2,
    /// Identified by its window class when application identity is unavailable.
    Unresolved = 3,
}

impl IntegerCode for ApplicationKind {
    const COLUMN: &'static str = "applications.kind";
    const ALL: &'static [Self] = &[Self::Exe, Self::Aumid, Self::Unresolved];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Exe => "exe",
            Self::Aumid => "aumid",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Whether a foreground interval represents a window or no foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum IntervalKind {
    Window = 1,
    NoForeground = 2,
}

impl IntegerCode for IntervalKind {
    const COLUMN: &'static str = "intervals.kind";
    const ALL: &'static [Self] = &[Self::Window, Self::NoForeground];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::NoForeground => "no_foreground",
        }
    }
}

/// Why an interval has no foreground window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum NoForegroundReason {
    /// There is no foreground window.
    Empty = 1,
    /// The foreground desktop cannot be identified.
    UnknownDesktop = 2,
}

impl IntegerCode for NoForegroundReason {
    const COLUMN: &'static str = "intervals.no_foreground_reason";
    const ALL: &'static [Self] = &[Self::Empty, Self::UnknownDesktop];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::UnknownDesktop => "unknown_desktop",
        }
    }
}

/// Title availability shared by foreground and browser records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
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

impl IntegerCode for TitleStatus {
    const COLUMN: &'static str = "title_status";
    const ALL: &'static [Self] = &[
        Self::Ok,
        Self::Empty,
        Self::Unreadable,
        Self::Truncated,
        Self::Redacted,
        Self::Purged,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Empty => "empty",
            Self::Unreadable => "unreadable",
            Self::Truncated => "truncated",
            Self::Redacted => "redacted",
            Self::Purged => "purged",
        }
    }
}

/// Privacy level retained for a foreground interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum CaptureLevel {
    /// Retain application identity and title.
    Full = 1,
    /// Retain application identity without the title.
    TitleHidden = 2,
    /// Retain only time bounds and the privacy rule snapshot.
    Excluded = 3,
}

impl IntegerCode for CaptureLevel {
    const COLUMN: &'static str = "intervals.capture_level";
    const ALL: &'static [Self] = &[Self::Full, Self::TitleHidden, Self::Excluded];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::TitleHidden => "title_hidden",
            Self::Excluded => "excluded",
        }
    }
}

/// Boundary that closed a foreground interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
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

impl IntegerCode for IntervalEndReason {
    const COLUMN: &'static str = "intervals.end_reason";
    const ALL: &'static [Self] = &[
        Self::Switch,
        Self::Title,
        Self::NoForeground,
        Self::Locked,
        Self::Suspended,
        Self::Paused,
        Self::Disconnected,
        Self::Shutdown,
        Self::CrashRecovered,
        Self::ClockJump,
        Self::MidnightSplit,
        Self::PolicyChanged,
        Self::CaptureGap,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Switch => "switch",
            Self::Title => "title",
            Self::NoForeground => "no_foreground",
            Self::Locked => "locked",
            Self::Suspended => "suspended",
            Self::Paused => "paused",
            Self::Disconnected => "disconnected",
            Self::Shutdown => "shutdown",
            Self::CrashRecovered => "crash_recovered",
            Self::ClockJump => "clock_jump",
            Self::MidnightSplit => "midnight_split",
            Self::PolicyChanged => "policy_changed",
            Self::CaptureGap => "capture_gap",
        }
    }
}

/// State recorded as an availability interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum AvailabilityReason {
    Idle = 1,
    Locked = 2,
    Disconnected = 3,
    Suspended = 4,
    Paused = 5,
}

impl IntegerCode for AvailabilityReason {
    const COLUMN: &'static str = "availability_intervals.reason";
    const ALL: &'static [Self] = &[
        Self::Idle,
        Self::Locked,
        Self::Disconnected,
        Self::Suspended,
        Self::Paused,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Locked => "locked",
            Self::Disconnected => "disconnected",
            Self::Suspended => "suspended",
            Self::Paused => "paused",
        }
    }
}

/// Boundary that closed an availability interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
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

impl IntegerCode for AvailabilityEndReason {
    const COLUMN: &'static str = "availability_intervals.end_reason";
    const ALL: &'static [Self] = &[
        Self::Resumed,
        Self::Shutdown,
        Self::CrashRecovered,
        Self::ClockJump,
        Self::MidnightSplit,
        Self::PolicyChanged,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Resumed => "resumed",
            Self::Shutdown => "shutdown",
            Self::CrashRecovered => "crash_recovered",
            Self::ClockJump => "clock_jump",
            Self::MidnightSplit => "midnight_split",
            Self::PolicyChanged => "policy_changed",
        }
    }
}

/// Exception that ended an idle interval early.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum IdleException {
    /// The user enabled keep-alive timing.
    KeepAlive = 1,
    /// The foreground application is exempt from idle detection.
    AppExempt = 2,
    /// Audible playback suppresses idle detection.
    Audible = 3,
}

impl IntegerCode for IdleException {
    const COLUMN: &'static str = "availability_intervals.exception";
    const ALL: &'static [Self] = &[Self::KeepAlive, Self::AppExempt, Self::Audible];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::KeepAlive => "keep_alive",
            Self::AppExempt => "app_exempt",
            Self::Audible => "audible",
        }
    }
}

/// Boundary that closed a browser interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
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

impl IntegerCode for WebEndReason {
    const COLUMN: &'static str = "web_intervals.end_reason";
    const ALL: &'static [Self] = &[
        Self::BrowserState,
        Self::ForegroundEnd,
        Self::Disconnected,
        Self::Gap,
        Self::PolicyChanged,
        Self::CrashRecovered,
        Self::Shutdown,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::BrowserState => "browser_state",
            Self::ForegroundEnd => "foreground_end",
            Self::Disconnected => "disconnected",
            Self::Gap => "gap",
            Self::PolicyChanged => "policy_changed",
            Self::CrashRecovered => "crash_recovered",
            Self::Shutdown => "shutdown",
        }
    }
}

/// Confidence of a browser interval's foreground association.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum LinkStatus {
    /// The browser interval has an unambiguous foreground association.
    Linked = 1,
    /// The foreground association is ambiguous.
    Ambiguous = 2,
}

impl IntegerCode for LinkStatus {
    const COLUMN: &'static str = "web_intervals.link_status";
    const ALL: &'static [Self] = &[Self::Linked, Self::Ambiguous];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// Privacy level retained for a stored browser interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum WebLevel {
    /// Retain domain and tab title.
    Full = 1,
    /// Retain the domain and redact the tab title.
    DomainOnly = 2,
}

impl IntegerCode for WebLevel {
    const COLUMN: &'static str = "web_intervals.web_level";
    const ALL: &'static [Self] = &[Self::Full, Self::DomainOnly];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::DomainOnly => "domain_only",
        }
    }
}

/// Durable result used to acknowledge a browser message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
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

impl IntegerCode for IngestOutcome {
    const COLUMN: &'static str = "ingest_receipts.outcome";
    const ALL: &'static [Self] = &[
        Self::Stored,
        Self::DiscardedNoOverlap,
        Self::DiscardedPrivacy,
        Self::DiscardedGap,
        Self::DiscardedBarrier,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::DiscardedNoOverlap => "discarded_no_overlap",
            Self::DiscardedPrivacy => "discarded_privacy",
            Self::DiscardedGap => "discarded_gap",
            Self::DiscardedBarrier => "discarded_barrier",
        }
    }
}

/// Contribution of a category to focus and distraction totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum CategoryTendency {
    Focus = 1,
    Neutral = 2,
    Distraction = 3,
}

impl IntegerCode for CategoryTendency {
    const COLUMN: &'static str = "categories.tendency";
    const ALL: &'static [Self] = &[Self::Focus, Self::Neutral, Self::Distraction];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Focus => "focus",
            Self::Neutral => "neutral",
            Self::Distraction => "distraction",
        }
    }
}

/// Rule family evaluated by a rule-set tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
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

impl IntegerCode for RuleSetKind {
    const COLUMN: &'static str = "rule_sets.kind";
    const ALL: &'static [Self] = &[
        Self::Classification,
        Self::Extraction,
        Self::Privacy,
        Self::Precedent,
        Self::PolicyCheck,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Classification => "classification",
            Self::Extraction => "extraction",
            Self::Privacy => "privacy",
            Self::Precedent => "precedent",
            Self::PolicyCheck => "policy_check",
        }
    }
}

/// Whether a rule set is built in or supplied by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum RuleSetSource {
    Builtin = 1,
    User = 2,
}

impl IntegerCode for RuleSetSource {
    const COLUMN: &'static str = "rule_sets.source";
    const ALL: &'static [Self] = &[Self::Builtin, Self::User];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
        }
    }
}

/// User annotation applied over a time range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
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

impl IntegerCode for AnnotationKind {
    const COLUMN: &'static str = "annotations.kind";
    const ALL: &'static [Self] = &[
        Self::CategoryOverride,
        Self::OfflineActivity,
        Self::ActiveOverride,
        Self::DismissedAway,
        Self::Note,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::CategoryOverride => "category_override",
            Self::OfflineActivity => "offline_activity",
            Self::ActiveOverride => "active_override",
            Self::DismissedAway => "dismissed_away",
            Self::Note => "note",
        }
    }
}

/// Fact source whose per-day version invalidates cached results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
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

impl IntegerCode for DataSource {
    const COLUMN: &'static str = "data_day_versions.source";
    const ALL: &'static [Self] = &[
        Self::Intervals,
        Self::Availability,
        Self::Web,
        Self::Annotations,
        Self::Gaps,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::Intervals => "intervals",
            Self::Availability => "availability",
            Self::Web => "web",
            Self::Annotations => "annotations",
            Self::Gaps => "gaps",
        }
    }
}

/// Failure that left a capture interval without trustworthy facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Integer)]
#[repr(i32)]
pub(super) enum GapReason {
    /// The event input exceeded its capacity.
    EventOverload = 1,
    /// The database write queue exceeded its capacity.
    WriteQueueOverload = 2,
    DatabaseError = 3,
}

impl IntegerCode for GapReason {
    const COLUMN: &'static str = "capture_gaps.reason";
    const ALL: &'static [Self] = &[
        Self::EventOverload,
        Self::WriteQueueOverload,
        Self::DatabaseError,
    ];

    fn code(self) -> i32 {
        self as i32
    }

    fn label(self) -> &'static str {
        match self {
            Self::EventOverload => "event_overload",
            Self::WriteQueueOverload => "write_queue_overload",
            Self::DatabaseError => "database_error",
        }
    }
}

impl_integer_sql!(
    RunEndReason,
    ClockEndReason,
    ApplicationKind,
    IntervalKind,
    NoForegroundReason,
    TitleStatus,
    CaptureLevel,
    IntervalEndReason,
    AvailabilityReason,
    AvailabilityEndReason,
    IdleException,
    WebEndReason,
    LinkStatus,
    WebLevel,
    IngestOutcome,
    CategoryTendency,
    RuleSetKind,
    RuleSetSource,
    AnnotationKind,
    DataSource,
    GapReason,
);
