//! Value inputs, transactional write descriptions and sanitized shell effects.

use super::{
    BatchId, BrowserContext, CapturedFields, Deadline, ForegroundGeneration, IdleException,
    IdleRecord, IdleSample, MonoInstant, ObservationId, PrivacyContext, ProcessId,
    ReadLastInputError, RowId, RunId, SchedulerKind, SegmentId, SessionKey, TimeRange, Timestamp,
    UtcInstant, WindowFacts, WindowId, WindowInstance, WriteBatch,
};
use crate::rules::registry::RuleSnapshot;
use std::{collections::BTreeSet, sync::Arc};

/// A stop notification only changes its own reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StopReason {
    /// The operating-system session is locked.
    Locked,
    /// The operating-system session is disconnected.
    Disconnected,
    /// The computer is suspended.
    Suspended,
    /// The user paused foreground capture.
    Paused,
}
impl StopReason {
    pub(super) fn end_reason(self) -> IntervalEndReason {
        match self {
            Self::Locked => IntervalEndReason::Locked,
            Self::Disconnected => IntervalEndReason::Disconnected,
            Self::Suspended => IntervalEndReason::Suspended,
            Self::Paused => IntervalEndReason::Paused,
        }
    }
}
/// A reason's transition, rather than a boolean positional parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopChange {
    /// Adds this notification's own stop reason.
    Begin(StopReason),
    /// Removes this notification's own stop reason.
    End(StopReason),
}
/// Session-query and notification-registration result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// Query and notification registration both succeeded.
    Ready(BTreeSet<StopReason>),
    /// Remain stopped and ask the shell to retry registration and querying.
    Unavailable,
}
/// Reason for an explicit no-foreground fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoForegroundReason {
    /// The foreground window is absent.
    Empty,
    /// The desktop cannot currently be observed.
    UnknownDesktop,
}
/// Resolved foreground observation; unknown desktop is a fact rather than an error.
#[derive(Debug, Clone)]
pub enum Foreground {
    /// A shell-resolved foreground window observation.
    Window {
        /// Shell-resolved window identity and capped title facts.
        facts: Box<WindowFacts>,
        /// Privacy signals captured with this observation.
        privacy: PrivacyContext,
    },
    /// No observable foreground is available.
    None(NoForegroundReason),
}
/// Whether a reading came from a hook or a fresh system review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationSource {
    /// An event-triggered window or title snapshot.
    Hook,
    /// A freshly queried foreground snapshot.
    Review,
}
/// Exact interval-end ontology from capture design section 4.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalEndReason {
    /// The foreground window instance changed.
    Switch,
    /// The capped foreground title changed.
    Title,
    /// No observable foreground window remains.
    NoForeground,
    /// The operating-system session is locked.
    Locked,
    /// The computer is suspended.
    Suspended,
    /// The user paused foreground capture.
    Paused,
    /// The operating-system session is disconnected.
    Disconnected,
    /// The core is shutting down normally.
    Shutdown,
    /// Recovery settled the row at its durable confirmation.
    CrashRecovered,
    /// The UTC-to-monotonic mapping changed.
    ClockJump,
    /// A UTC midnight divides the row into storage pieces.
    MidnightSplit,
    /// The effective capture policy changed.
    PolicyChanged,
    /// Capture stopped producing trustworthy foreground facts.
    CaptureGap,
}
/// Shell notifications that require a new UTC-to-monotonic clock mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockChange {
    /// The shell received an explicit system time-change notification.
    TimeChange,
    /// The shell observed resume from suspension.
    Resume,
}
/// Why the enclosing capture run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEndReason {
    /// The core is shutting down normally.
    Shutdown,
    /// The user explicitly exited the core.
    UserExit,
    /// Recovery settled the row at its durable confirmation.
    CrashRecovered,
    /// The core exited for an application update.
    Update,
}
/// Clock-segment boundary trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockEndReason {
    /// The shell received an explicit system time-change notification.
    TimeChange,
    /// The shell observed resume from suspension.
    Resume,
    /// A review exceeded the configured clock-drift tolerance.
    Drift,
    /// The enclosing capture run ended.
    RunEnd,
    /// Recovery settled the row at its durable confirmation.
    CrashRecovered,
}
/// Availability reason, including idle as an independent mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AvailabilityReason {
    /// Input inactivity exceeded the configured threshold.
    Idle,
    /// One independent stop reason masks this range.
    Stopped(StopReason),
}
/// Availability settlement reason; exceptions map to the store's separate exception column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilityEndReason {
    /// The availability reason no longer applies.
    Resumed,
    /// The core is shutting down normally.
    Shutdown,
    /// Recovery settled the row at its durable confirmation.
    CrashRecovered,
    /// The UTC-to-monotonic mapping changed.
    ClockJump,
    /// A UTC midnight divides the row into storage pieces.
    MidnightSplit,
    /// The effective capture policy changed.
    PolicyChanged,
    /// An idle exception ended the idle mask.
    Exception(IdleException),
}
/// Unknown coverage never carries app, window, title or domain fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapReason {
    /// The observation queue or foreground review lost trustworthy coverage.
    EventOverload,
    /// The write-command queue exceeded its bounded policy.
    WriteQueueOverload,
    /// Database failure exhausted the retained write buffer.
    DatabaseError,
}
/// Segment-local coverage carried by an atomic transaction, including clock-boundary transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentRange {
    /// Clock segment for this part of the transaction.
    pub segment: SegmentId,
    /// Half-open covered range.
    pub range: TimeRange,
}
/// A closed range of untrusted capture coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureGap {
    /// Clock segment containing the complete range.
    pub segment: SegmentId,
    /// Half-open unknown range.
    pub range: TimeRange,
    /// Why capture became untrusted.
    pub reason: GapReason,
}
/// Sanitized foreground fact that can leave the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntervalFact {
    /// Allowed fields and optional allowed instance identity.
    Window {
        /// Logical identity of the observed window instance.
        instance: Option<WindowId>,
        /// Foreground fields after privacy actions have been applied.
        fields: CapturedFields,
    },
    /// Known absence of an observable foreground.
    NoForeground(NoForegroundReason),
}
/// Open foreground row with core-assigned stable keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenInterval {
    /// Logical row key, also the run-local sequence number.
    pub id: RowId,
    /// Clock segment of this piece.
    pub segment: SegmentId,
    /// Midnight pieces retain this key.
    pub session: SessionKey,
    /// Inclusive boundary.
    pub start: Timestamp,
    /// Fields after privacy evaluation.
    pub fact: IntervalFact,
}
/// Open availability row, independently of foreground facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAvailability {
    /// Logical row key.
    pub id: RowId,
    /// Clock segment of this piece.
    pub segment: SegmentId,
    /// Availability mask reason.
    pub reason: AvailabilityReason,
    /// Inclusive boundary.
    pub start: Timestamp,
    /// Idle evidence; absent on stop reasons.
    pub idle: Option<IdleRecord>,
}
/// All open-row confirmation targets in a single transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// Capture run to confirm.
    pub run: RunId,
    /// Clock segment to confirm.
    pub segment: SegmentId,
    /// Foreground row, when tracking or recording no foreground.
    pub interval: Option<RowId>,
    /// Every open availability row.
    pub availability: Vec<RowId>,
    /// Confirmation point.
    pub at: Timestamp,
    /// Latest allowed transaction commit time, reserving one second for batching.
    pub commit_by_mono: MonoInstant,
    /// M5 extends the confirmation target set with correlated open web rows (09 §M5).
    pub web_extension: WebRowExtension,
}
/// Database-independent write descriptions; one step must remain indivisible when batching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureWrite {
    /// Starts the shell-assigned run; installation/session metadata belongs to the shell.
    OpenRun {
        /// Owning capture run.
        run: RunId,
        /// Paired clock readings at this boundary.
        at: Timestamp,
    },
    /// Settles the run and clears its confirmation columns.
    CloseRun {
        /// Owning capture run.
        run: RunId,
        /// Paired clock readings at this boundary.
        at: Timestamp,
        /// Typed reason for this transition.
        reason: RunEndReason,
    },
    /// Starts a new UTC-to-monotonic mapping.
    OpenClockSegment {
        /// Clock segment containing this range.
        segment: SegmentId,
        /// Paired clock readings at this boundary.
        at: Timestamp,
    },
    /// Settles the old mapping at its own projected UTC endpoint.
    CloseClockSegment {
        /// Clock segment containing this range.
        segment: SegmentId,
        /// Paired clock readings at this boundary.
        at: Timestamp,
        /// Typed reason for this transition.
        reason: ClockEndReason,
    },
    /// Upserts an allowed instance and updates last_seen at this observation.
    ObserveWindow {
        /// Logical identity of the observed window instance.
        instance: WindowInstance,
        /// Paired clock readings at this boundary.
        at: Timestamp,
    },
    /// Retires an allowed instance.
    RetireWindow {
        /// Logical identity of the observed window instance.
        instance: WindowId,
        /// Paired clock readings at this boundary.
        at: Timestamp,
    },
    /// Opens a sanitized foreground or no-foreground row.
    OpenInterval(OpenInterval),
    /// Closes a row and clears confirmation columns; duration uses monotonic endpoints.
    CloseInterval {
        /// Logical row key assigned by the capture allocator.
        id: RowId,
        /// Paired clock readings at this boundary.
        at: Timestamp,
        /// Typed reason for this transition.
        reason: IntervalEndReason,
    },
    /// Opens an availability mask row.
    OpenAvailability(OpenAvailability),
    /// Closes an availability row, splitting at UTC midnight as required.
    CloseAvailability {
        /// Logical row key assigned by the capture allocator.
        id: RowId,
        /// Paired clock readings at this boundary.
        at: Timestamp,
        /// Typed reason for this transition.
        reason: AvailabilityEndReason,
    },
    /// Atomically advances all open rows and their run/segment.
    Checkpoint(Checkpoint),
    /// Settles persisted open rows in a segment at the start of a lost write range.
    /// D1b resolves currently persisted rows, clears confirmation fields and uses capture_gap.
    SettlePersistedRows {
        /// Owning capture run.
        run: RunId,
        /// Clock segment containing this range.
        segment: SegmentId,
        /// Paired clock readings at this boundary.
        at: Timestamp,
    },
    /// Saves or clears a policy deadline in the same transaction as its capture boundary.
    SetDeadline {
        /// Owner of this registered policy deadline.
        owner: SchedulerKind,
        /// Paired clock readings at this boundary.
        at: UtcInstant,
    },
    /// Persists the complete timed policy in the same transaction as its deadline.
    SetTimedState(super::TimedState),
    /// Deletes a complete persisted policy on cancellation or expiration.
    ClearTimedPolicy(super::TimedPolicyKind),
    /// Clears a persisted scheduler deadline.
    ClearDeadline {
        /// Owner of this registered policy deadline.
        owner: SchedulerKind,
    },
    /// Stores an identity-free unknown range.
    RecordGap(CaptureGap),
}
impl CaptureWrite {
    /// Borrows the confirmation point carried by this write, when present.
    pub fn checkpoint(&self) -> Option<&Checkpoint> {
        match self {
            Self::Checkpoint(point) => Some(point),
            Self::OpenRun { .. }
            | Self::CloseRun { .. }
            | Self::OpenClockSegment { .. }
            | Self::CloseClockSegment { .. }
            | Self::ObserveWindow { .. }
            | Self::RetireWindow { .. }
            | Self::OpenInterval(_)
            | Self::CloseInterval { .. }
            | Self::OpenAvailability(_)
            | Self::CloseAvailability { .. }
            | Self::SettlePersistedRows { .. }
            | Self::SetDeadline { .. }
            | Self::SetTimedState(_)
            | Self::ClearTimedPolicy(_)
            | Self::ClearDeadline { .. }
            | Self::RecordGap(_) => None,
        }
    }
}
/// Sanitized tray/subscription state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureState {
    /// A browser title is withheld until correlated privacy context or timeout.
    AwaitingBrowserContext,
    /// Current sanitized foreground row.
    Tracking(IntervalFact),
    /// Stop reasons may overlap; readiness can also prevent tracking.
    Stopped {
        /// Independent stop reasons currently masking foreground capture.
        reasons: BTreeSet<StopReason>,
        /// Whether session querying and registration have completed.
        session: SessionReadiness,
    },
    /// No observable foreground, or waiting for a fresh review.
    NoForeground(NoForegroundReason),
    /// Capture is untrusted until a successful foreground review.
    Gap(GapReason),
    /// Run has settled and ignores subsequent observations.
    Ended,
}
/// Commands for platform and persistence shells, containing no unfiltered title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEffect {
    /// Correlate browser privacy context to exactly this withheld observation.
    AwaitBrowserContext {
        /// Correlation key of exactly one withheld foreground snapshot.
        observation: ObservationId,
        /// Foreground hook generation associated with this reading.
        generation: ForegroundGeneration,
    },
    /// Main thread must check this generation before replacing hooks.
    RebindHooks {
        /// Foreground hook generation associated with this reading.
        generation: ForegroundGeneration,
        /// Typed process binding or explicit hook removal.
        target: HookTarget,
    },
    /// Request a fresh session query and successful notification registration.
    QuerySession,
    /// Request a current foreground and title snapshot.
    ReviewForeground,
    /// Discard queued hook events made untrustworthy by main-queue overload.
    DiscardQueuedObservations,
    /// Arm exactly one timer, or cancel it when empty.
    ScheduleTimer(Timer),
    /// Run a last-input query at the adaptive deadline.
    CheckIdle,
    /// Re-evaluate timed policy and supply a fresh observation.
    PolicyExpired(super::TimedPolicyKind),
    /// Sanitized current state for subscriptions and tray rendering.
    StateChanged(CaptureState),
    /// Review detected a missed foreground/title notification.
    MissedEvent,
    /// Idle input was invalid; retain the prior mask and retry.
    IdleSampleFailed(ReadLastInputError),
}
/// One work unit's value input and retained immutable rule snapshot.
#[derive(Debug)]
pub struct CaptureInput {
    /// Clock readings sampled by the shell for this work unit.
    pub at: Timestamp,
    /// One retained snapshot; the core never loads a snapshot cell.
    pub rules: Arc<RuleSnapshot>,
    /// System fact, control transition or persistence acknowledgement.
    pub event: CaptureEvent,
}
/// Capture inputs contain no callbacks, handles, connection or channel objects.
#[derive(Debug)]
pub enum CaptureEvent {
    /// Startup/retry session result; must precede opening any interval.
    Session(SessionStatus),
    /// Foreground-generation-tagged resolved observation.
    Foreground {
        /// Foreground hook generation associated with this reading.
        generation: ForegroundGeneration,
        /// Origin of this foreground snapshot.
        source: ObservationSource,
        /// Resolved foreground facts for this work unit.
        foreground: Foreground,
    },
    /// Context for one withheld snapshot; late replies cannot recover discarded titles.
    PrivacyContext {
        /// Correlation key of exactly one withheld foreground snapshot.
        observation: ObservationId,
        /// Browser facts correlated to this withheld observation.
        browser: BrowserContext,
    },
    /// Matching logical window was destroyed.
    Destroy(WindowId),
    /// Native destroy notification from the currently bound process hook.
    DestroyWindow {
        /// Generation checked before retiring the native handle.
        generation: ForegroundGeneration,
        /// Native handle whose live instance should be retired.
        handle: super::WindowHandle,
    },
    /// One stop reason began or ended.
    Stop(StopChange),
    /// An explicit clock boundary, including resume.
    ClockChanged(ClockChange),
    /// One-shot timer fired; all currently due owners are processed.
    Wake,
    /// Pure idle check inputs.
    Idle(IdleSample),
    /// Rule, privacy-mode or exception change; fresh facts are supplied when available.
    PolicyChanged(PolicyObservation),
    /// Register a persisted or elapsed-time policy deadline.
    RegisterDeadline {
        /// Owner of this registered policy deadline.
        owner: SchedulerKind,
        /// Deadline in its explicit clock domain.
        deadline: Deadline,
    },
    /// Install fresh timed policy evidence and persist its value and deadline.
    SetTimedState(super::TimedState),
    /// Restore policy and its original grant start; browser audible needs fresh evidence.
    RestoreTimedState(super::TimedState),
    /// Cancel a complete persisted policy.
    CancelTimedPolicy(super::TimedPolicyKind),
    /// Cancel a scheduler-only deadline.
    CancelDeadline(SchedulerKind),
    /// Main-thread queue lost events.
    EventOverload,
    /// Persistence shell discarded this range; no queued observations may fill it.
    PersistenceGap {
        /// Reason already used by the backlog's recorded loss ranges.
        reason: GapReason,
        /// End of the last backlog loss range in the current segment. The core covers its tail until review.
        lost_through: Timestamp,
    },
    /// Whole transaction batch successfully committed in order; IDs require no callback.
    Committed(BatchId),
    /// A coalesced or discarded batch no longer awaits persistence.
    BatchDiscarded(BatchId),
    /// Immediate settlement and run end.
    Shutdown,
}
/// An indivisible transaction plan plus sanitized shell effects.
#[derive(Debug, Default)]
#[must_use]
pub struct CaptureStep {
    /// Assigned only for nonempty transaction plans.
    pub batch: Option<BatchId>,
    /// Keep this complete list in order in one transaction.
    pub writes: Vec<CaptureWrite>,
    /// Ranges that become unknown if this complete transaction is discarded.
    pub coverage: Vec<SegmentRange>,
    /// Platform actions and state notifications.
    pub effects: Vec<CaptureEffect>,
    /// Frequent title changes permit larger batching; they never discard boundaries.
    pub title_storm: TitleActivity,
}

impl CaptureStep {
    /// Separates the atomic database batch from shell effects without losing coverage metadata.
    pub fn into_parts(self, run: RunId) -> CaptureParts {
        let batch = self.batch.map(|id| WriteBatch {
            id,
            run,
            coverage: self.coverage,
            writes: self.writes,
            title_storm: self.title_storm,
        });
        CaptureParts {
            batch,
            effects: self.effects,
        }
    }
}

/// Shell timer command with an explicit cancellation operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// Arm one timer at the nearest deadline.
    Arm(MonoInstant),
    /// Cancel the current timer.
    Cancel,
}
/// Whether session querying and notification registration have completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionReadiness {
    /// Session observations are trustworthy.
    Ready,
    /// Query or registration is still pending.
    AwaitingSession,
}
/// Write-batching hint derived from observed title activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TitleActivity {
    /// Ordinary title activity.
    #[default]
    Ordinary,
    /// More than the design threshold within its rolling window.
    Storm,
}
/// Marker documenting the later browser-row confirmation integration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WebRowExtension {
    /// Browser rows are added by M5.
    #[default]
    PendingM5,
}
/// Result of separating a capture transaction and its shell effects.
#[derive(Debug)]
#[must_use]
pub struct CaptureParts {
    /// Atomic transaction, absent for an effects-only step.
    pub batch: Option<WriteBatch>,
    /// Shell notifications and commands.
    pub effects: Vec<CaptureEffect>,
}

/// Hook binding target, including explicit hook removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTarget {
    /// Bind to the foreground process.
    Process(ProcessId),
    /// Remove the previous process hooks.
    Unbind,
}
/// Facts available at a capture-policy boundary.
#[derive(Debug)]
pub enum PolicyObservation {
    /// Re-evaluate retained fields and request a fresh reading.
    Refresh,
    /// Evaluate a fresh shell observation.
    Observed(Foreground),
}
