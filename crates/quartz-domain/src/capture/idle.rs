//! Adaptive idle checks using supplied tick samples and bounded exceptions.

use std::time::Duration;

use super::{AdvanceTimeError, MonoInstant, Timestamp, UtcInstant, limits};

/// Why an idle interval ended early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleException {
    /// A temporary keep-timing grant suppresses idle.
    KeepTiming,
    /// The current application has a bounded idle exemption.
    AppExempt,
    /// The foreground browser tab has a bounded audible exemption.
    Audible,
}
/// A currently applicable exception; application and audible grants expire within two hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExceptionGrant {
    /// Exception owner.
    pub kind: IdleException,
    /// Start UTC of this single grant.
    pub started_utc: UtcInstant,
    /// Requested expiration UTC.
    pub until_utc: UtcInstant,
}
impl ExceptionGrant {
    /// Applies the two-hour cap to application and audible grants.
    pub fn expires_at(self) -> UtcInstant {
        match self.kind {
            IdleException::KeepTiming => self.until_utc,
            IdleException::AppExempt | IdleException::Audible => self.until_utc.min(
                self.started_utc
                    .checked_add(limits::EXCEPTION_CAP)
                    .expect("validated grant fits UTC range"),
            ),
        }
    }
    /// Tests whether this grant applies at a supplied wall-clock reading.
    pub fn active_at(self, now: Timestamp) -> bool {
        self.started_utc <= now.utc && now.utc < self.expires_at()
    }
}
/// Result of the shell's last-input API call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastInput {
    /// The shell successfully queried a wrapping last-input tick.
    Tick(u32),
    /// The shell could not query last-input information.
    ApiFailed,
}
/// Idle policy snapshot saved on each idle row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdlePolicy {
    /// Inactivity threshold; the default is three minutes.
    pub threshold: Duration,
    /// Statistical interpretation version.
    pub version: u32,
}
impl Default for IdlePolicy {
    fn default() -> Self {
        Self {
            threshold: limits::IDLE_THRESHOLD,
            version: 1,
        }
    }
}
/// Full idle sample; GetTickCount64 includes sleep while unbiased interrupt time does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleSample {
    /// GetTickCount64 sampled alongside the capture timestamp.
    pub tick: Duration,
    /// GetLastInputInfo result.
    pub last_input: LastInput,
    /// Threshold and interpretation version for this check.
    pub policy: IdlePolicy,
    /// Applicable foreground-scoped grants, already selected by the shell.
    pub exceptions: Vec<ExceptionGrant>,
}
/// Saved interpretation of an idle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleRecord {
    /// Last input projected into the current clock segment.
    pub last_input: Timestamp,
    /// Threshold crossing, clamped to the current segment by the capture core.
    pub start: Timestamp,
    /// Time this check detected inactivity.
    pub detected: Timestamp,
    /// Policy used for this transition.
    pub policy: IdlePolicy,
}
/// Explicit API failure or anomalous future tick result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
pub enum ReadLastInputError {
    /// The shell could not obtain the last-input tick.
    #[snafu(display("could not read last input"))]
    ApiFailed,
    /// The wrapping delta is ambiguous or indicates an input tick after now.
    #[snafu(display("last input tick is later than the current tick"))]
    FutureLastInput,
}
/// Mutually exclusive idle outcomes; failure retains the previously open mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleStatus {
    /// Input activity remains below the configured threshold.
    Active,
    /// The threshold has elapsed with no applicable exception.
    Idle(IdleRecord),
    /// A foreground-scoped grant suppresses idle.
    Suppressed(IdleException),
    /// The shell's sample cannot be interpreted reliably.
    Failed(ReadLastInputError),
}
/// Pure idle decision and its next adaptive check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct IdleDecision {
    /// Exactly one sample outcome.
    pub status: IdleStatus,
    /// Next unbiased deadline for an input check.
    pub next_check_mono: MonoInstant,
}
impl IdleDecision {
    /// Returns threshold evidence when this sample is idle.
    #[must_use]
    pub fn idle(self) -> Option<IdleRecord> {
        match self.status {
            IdleStatus::Idle(record) => Some(record),
            _ => None,
        }
    }
    /// Returns the exception that suppressed this sample.
    #[must_use]
    pub fn exception(self) -> Option<IdleException> {
        match self.status {
            IdleStatus::Suppressed(exception) => Some(exception),
            _ => None,
        }
    }
    /// Returns a failed system sample without changing the prior mask.
    #[must_use]
    pub fn error(self) -> Option<ReadLastInputError> {
        match self.status {
            IdleStatus::Failed(error) => Some(error),
            _ => None,
        }
    }
}
impl IdleSample {
    /// Interprets wrapping input ticks and schedules the next threshold or recovery check.
    /// # Errors
    /// Rejects submillisecond inputs and time arithmetic outside the supported range.
    pub fn decide(&self, now: Timestamp) -> Result<IdleDecision, AdvanceTimeError> {
        self.decide_at(now, now)
    }
    /// Evaluates UTC exceptions on the sample and constructs evidence on the segment mapping.
    pub(super) fn decide_at(
        &self,
        observed: Timestamp,
        boundary: Timestamp,
    ) -> Result<IdleDecision, AdvanceTimeError> {
        self.validate(observed)?;
        i64::try_from(boundary.utc)?;
        boundary
            .utc
            .checked_sub(Duration::from_millis(u64::from(u32::MAX)))?;
        Ok(self.decide_validated(observed, boundary))
    }
    pub(super) fn validate(&self, now: Timestamp) -> Result<(), AdvanceTimeError> {
        i64::try_from(now.utc)?;
        now.utc
            .checked_sub(Duration::from_millis(u64::from(u32::MAX)))?;
        MonoInstant::from_millis(0).checked_add(self.tick)?;
        now.mono.checked_add(self.policy.threshold)?;
        now.mono.checked_add(limits::IDLE_POLL)?;
        for grant in &self.exceptions {
            i64::try_from(grant.started_utc)?;
            i64::try_from(grant.until_utc)?;
            if matches!(
                grant.kind,
                IdleException::AppExempt | IdleException::Audible
            ) {
                grant.started_utc.checked_add(limits::EXCEPTION_CAP)?;
            }
            super::Deadline::Utc(grant.until_utc).mono_at(now)?;
        }
        Ok(())
    }
    fn decide_validated(&self, observed: Timestamp, boundary: Timestamp) -> IdleDecision {
        let age = match self.input_age() {
            Ok(age) => age,
            Err(error) => return self.retry(boundary, IdleStatus::Failed(error)),
        };
        if let Some(grant) = self
            .exceptions
            .iter()
            .find(|grant| grant.active_at(observed))
        {
            return self.suppressed(observed, grant.kind);
        }
        if age < self.policy.threshold {
            return IdleDecision {
                status: IdleStatus::Active,
                next_check_mono: boundary.mono.advance(self.policy.threshold - age),
            };
        }
        self.retry(boundary, IdleStatus::Idle(self.record(boundary, age)))
    }
    fn input_age(&self) -> Result<Duration, ReadLastInputError> {
        let tick = match self.last_input {
            LastInput::Tick(tick) => tick,
            LastInput::ApiFailed => return Err(ReadLastInputError::ApiFailed),
        };
        let low =
            u32::try_from(self.tick.as_millis() & u128::from(u32::MAX)).expect("mask fits a u32");
        let age = u64::from(low.wrapping_sub(tick));
        if age > u64::from(u32::MAX / 2) || u128::from(age) > self.tick.as_millis() {
            return Err(ReadLastInputError::FutureLastInput);
        }
        Ok(Duration::from_millis(age))
    }
    fn retry(&self, now: Timestamp, status: IdleStatus) -> IdleDecision {
        IdleDecision {
            status,
            next_check_mono: now.mono.advance(limits::IDLE_POLL),
        }
    }
    fn suppressed(&self, now: Timestamp, exception: IdleException) -> IdleDecision {
        let next = self
            .exceptions
            .iter()
            .filter(|grant| grant.active_at(now))
            .map(|grant| grant.expires_at())
            .min()
            .expect("an active grant was selected");
        IdleDecision {
            status: IdleStatus::Suppressed(exception),
            next_check_mono: now.mono.advance(
                next.duration_since(now.utc)
                    .expect("active grant expires after now"),
            ),
        }
    }
    fn record(&self, now: Timestamp, age: Duration) -> IdleRecord {
        let idle_age = age - self.policy.threshold;
        IdleRecord {
            last_input: Timestamp {
                utc: now
                    .utc
                    .checked_sub(age)
                    .expect("validated idle age fits UTC range"),
                mono: now.mono.retreat_to_origin(age),
            },
            start: Timestamp {
                utc: now
                    .utc
                    .checked_sub(idle_age)
                    .expect("validated idle age fits UTC range"),
                mono: now.mono.retreat_to_origin(idle_age),
            },
            detected: now,
            policy: self.policy,
        }
    }
}
