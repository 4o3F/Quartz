//! Persistable timed policy and foreground-scoped idle grants.

use super::{
    AdvanceTimeError, AppIdentity, ExceptionGrant, IdleException, PrivacyMode, Timestamp,
    UtcInstant, limits,
};
use std::collections::BTreeMap;

/// Persisted policy identities, independent of scheduler-only slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimedPolicyKind {
    Pause,
    Privacy,
    KeepTiming,
    AppExemption,
    Audible,
}

/// The state whose lifetime is owned by one deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimedPolicy {
    /// User pause.
    Pause,
    /// Session-wide field retention.
    Privacy(PrivacyMode),
    /// Session-wide idle suppression.
    KeepTiming,
    /// Idle suppression only while this application is foreground.
    AppExemption(AppIdentity),
    /// Fresh browser evidence, applicable only to this foreground application.
    Audible(AppIdentity),
}
/// Full state persisted atomically with its expiration and policy boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedState {
    /// Policy value, including the application scope when required.
    pub policy: TimedPolicy,
    /// Original grant start, retained across restart for the two-hour cap.
    pub started: UtcInstant,
    /// Requested expiration; normalized before installation.
    pub until: UtcInstant,
}
impl TimedState {
    /// Returns the unique deadline owner for this policy.
    pub fn owner(&self) -> TimedPolicyKind {
        match self.policy {
            TimedPolicy::Pause => TimedPolicyKind::Pause,
            TimedPolicy::Privacy(_) => TimedPolicyKind::Privacy,
            TimedPolicy::KeepTiming => TimedPolicyKind::KeepTiming,
            TimedPolicy::AppExemption(_) => TimedPolicyKind::AppExemption,
            TimedPolicy::Audible(_) => TimedPolicyKind::Audible,
        }
    }
    /// Checks ordering and applies the single-grant cap from capture 02 §3.2.
    pub fn normalized(mut self) -> Result<Self, AdvanceTimeError> {
        self.until.duration_since(self.started)?;
        if matches!(
            self.policy,
            TimedPolicy::AppExemption(_) | TimedPolicy::Audible(_)
        ) {
            self.until = self
                .until
                .min(self.started.checked_add(limits::EXCEPTION_CAP)?);
        }
        Ok(self)
    }
    fn grant(&self, foreground: Option<&AppIdentity>) -> Option<ExceptionGrant> {
        let kind = match &self.policy {
            TimedPolicy::KeepTiming => IdleException::KeepTiming,
            TimedPolicy::AppExemption(app) if foreground == Some(app) => IdleException::AppExempt,
            TimedPolicy::Audible(app) if foreground == Some(app) => IdleException::Audible,
            _ => return None,
        };
        Some(ExceptionGrant {
            kind,
            started_utc: self.started,
            until_utc: self.until,
        })
    }
}
#[derive(Debug, Clone, Copy)]
pub(super) enum GrantEvidence {
    Live,
    Restored,
}
#[derive(Debug)]
struct ActiveState {
    state: TimedState,
    evidence: GrantEvidence,
}
#[derive(Debug, Default)]
pub(super) struct TimedStates(BTreeMap<TimedPolicyKind, ActiveState>);
impl TimedStates {
    pub(super) fn install(&mut self, state: TimedState, evidence: GrantEvidence) {
        self.0
            .insert(state.owner(), ActiveState { state, evidence });
    }
    pub(super) fn remove(&mut self, owner: TimedPolicyKind) {
        self.0.remove(&owner);
    }
    pub(super) fn privacy(&self, now: Timestamp) -> Option<PrivacyMode> {
        self.0
            .get(&TimedPolicyKind::Privacy)
            .and_then(|active| match active.state.policy {
                TimedPolicy::Privacy(mode)
                    if active.state.started <= now.utc && now.utc < active.state.until =>
                {
                    Some(mode)
                }
                _ => None,
            })
    }
    pub(super) fn grants(
        &self,
        now: Timestamp,
        foreground: Option<&AppIdentity>,
    ) -> Vec<ExceptionGrant> {
        self.0
            .values()
            .filter(|active| {
                active.state.started <= now.utc
                    && now.utc < active.state.until
                    && !matches!(
                        (&active.state.policy, active.evidence),
                        (TimedPolicy::Audible(_), GrantEvidence::Restored)
                    )
            })
            .filter_map(|active| active.state.grant(foreground))
            .collect()
    }
}
