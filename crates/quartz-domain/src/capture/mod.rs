//! Pure capture decisions. Each step is one indivisible database transaction batch.
//!
//! The shell supplies clock readings, resolved system facts and one rule snapshot per
//! work unit. Logical row keys are scoped by run and resolved to database IDs by the
//! store. Observation state advances before persistence; the shell must retain whole
//! steps in [`WriteBacklog`] until acknowledged and report any discarded range as a gap.

mod backlog;
mod components;
mod core;
mod deadlines;
mod foreground;
mod idle;
mod ids;
pub mod limits;
mod model;
mod phase;
mod privacy;
mod recovery;
mod time;
mod window;

pub use backlog::{BacklogFailure, BacklogOutcome, StoredGap, WriteBacklog, WriteBatch};
pub use core::{CaptureCore, StartCaptureError, StartedCapture, StepCaptureError};
pub use deadlines::{Deadline, DeadlineKind, DeadlineScheduler};
pub use idle::{
    ExceptionGrant, IdleDecision, IdleException, IdlePolicy, IdleRecord, IdleSample, IdleStatus,
    LastInput, ReadLastInputError,
};
pub use ids::{BatchId, ObservationId, RowId, RunId, SegmentId, SessionKey, WindowId};
pub use privacy::{
    BrowserContext, CaptureLevel, CapturedFields, PrivacyContext, PrivacyExplanation, PrivacyLabel,
    PrivacyMode, SystemSignal,
};
pub use recovery::{ConfirmedAvailability, ConfirmedInterval, ConfirmedSegment, RecoverableRun};
pub use time::{AdvanceTimeError, MonoInstant, TimeRange, Timestamp, UtcInstant};
pub use window::{
    AppIdentity, ForegroundGeneration, InstanceGeneration, ProcessId, Title, TitleStatus,
    WindowFacts, WindowHandle, WindowInstance, WindowInstances,
};

pub use model::{
    AvailabilityEndReason, AvailabilityReason, CaptureEffect, CaptureEvent, CaptureGap,
    CaptureInput, CaptureParts, CaptureState, CaptureStep, CaptureWrite, Checkpoint, ClockChange,
    ClockEndReason, Foreground, GapReason, HookTarget, IntervalEndReason, IntervalFact,
    NoForegroundReason, ObservationSource, OpenAvailability, OpenInterval, PolicyObservation,
    RunEndReason, SegmentRange, SessionReadiness, SessionStatus, StopChange, StopReason, Timer,
    TitleActivity, WebRowExtension,
};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CaptureCore>();
    assert_send_sync::<CaptureInput>();
    assert_send_sync::<CaptureStep>();
    assert_send_sync::<WriteBacklog>();
};

#[cfg(test)]
mod tests;
