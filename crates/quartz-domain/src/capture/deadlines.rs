//! One-shot deadline selection without timers or clock reads.

use super::time::{AdvanceTimeError, MonoInstant, Timestamp, UtcInstant};
use std::collections::BTreeMap;

/// Owner and purpose of a replaceable deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchedulerKind {
    /// Combined confirmation and foreground review wakeup.
    Maintenance,
    /// Adaptive last-input check.
    Idle,
    /// Browser privacy context wait limit.
    PrivacyContext,
    /// UTC midnight foreground boundary.
    Midnight,
}

/// A scheduler slot or a persisted policy's expiration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeadlineKind {
    /// Derived or temporary capture scheduling.
    Scheduler(SchedulerKind),
    /// Expiration owned by a complete persisted policy.
    Policy(super::TimedPolicyKind),
}
impl From<SchedulerKind> for DeadlineKind {
    fn from(kind: SchedulerKind) -> Self {
        Self::Scheduler(kind)
    }
}
impl From<super::TimedPolicyKind> for DeadlineKind {
    fn from(kind: super::TimedPolicyKind) -> Self {
        Self::Policy(kind)
    }
}

/// Deadline clock domain; persisted policy deadlines use UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deadline {
    /// Wall-clock deadline, recalculated after a clock change.
    Utc(UtcInstant),
    /// Unbiased monotonic deadline for elapsed-time work.
    Mono(MonoInstant),
}
impl Deadline {
    /// Resolves an absolute deadline to a monotonic timer target, clamping expired work to now.
    pub fn mono_at(self, now: Timestamp) -> Result<MonoInstant, AdvanceTimeError> {
        match self {
            Self::Mono(at) => Ok(at.max(now.mono)),
            Self::Utc(at) if at <= now.utc => Ok(now.mono),
            Self::Utc(at) => now.mono.checked_add_rounded_up(at.duration_since(now.utc)?),
        }
    }
    fn expired(self, now: Timestamp) -> bool {
        match self {
            Self::Utc(at) => at <= now.utc,
            Self::Mono(at) => at <= now.mono,
        }
    }
}

/// Bounded-by-owner deadline collection; an empty collection produces no wakeup.
#[derive(Debug, Clone, Default)]
pub struct DeadlineScheduler(BTreeMap<DeadlineKind, Deadline>);
impl DeadlineScheduler {
    /// Replaces the deadline owned by this kind.
    pub fn register(&mut self, owner: impl Into<DeadlineKind>, deadline: Deadline) {
        self.0.insert(owner.into(), deadline);
    }
    /// Removes an owner's deadline.
    pub fn cancel(&mut self, owner: impl Into<DeadlineKind>) {
        self.0.remove(&owner.into());
    }
    /// Returns only the nearest timer target, resolving UTC against the supplied clock pair.
    pub fn next(&self, now: Timestamp) -> Result<Option<MonoInstant>, AdvanceTimeError> {
        self.0
            .values()
            .map(|deadline| deadline.mono_at(now))
            .collect::<Result<Vec<_>, _>>()
            .map(|deadlines| deadlines.into_iter().min())
    }
    /// Removes all due owners, including deadlines already expired when loaded at startup.
    #[must_use]
    pub fn take_due(&mut self, now: Timestamp) -> Vec<DeadlineKind> {
        let due: Vec<_> = self
            .0
            .iter()
            .filter(|(_, deadline)| deadline.expired(now))
            .map(|(owner, _)| *owner)
            .collect();
        for owner in &due {
            self.0.remove(owner);
        }
        due
    }
}
