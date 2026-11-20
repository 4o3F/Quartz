//! Relational timed policy writes and startup restoration.

use super::recovery::LoadCaptureStartupError;
use super::{Capture, CaptureColumn, DecodeCaptureRowError, ExecuteCaptureBatchError};
use crate::store::{
    enums,
    rows::{ApplicationsRow, TimedPoliciesRow},
    schema::{applications, timed_policies},
};
use diesel::{
    ExpressionMethods, NullableExpressionMethods, QueryDsl, RunQueryDsl, SelectableHelper,
};
use quartz_domain::capture::{AppIdentity, TimedPolicy, TimedPolicyKind, TimedState, UtcInstant};

impl Capture<'_> {
    pub(super) fn set_timed_state(
        &mut self,
        state: &TimedState,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let application_id = match &state.policy {
            TimedPolicy::AppExemption(app) | TimedPolicy::Audible(app) => {
                Some(self.policy_application(app, state.started)?)
            }
            _ => None,
        };
        let privacy_mode = match state.policy {
            TimedPolicy::Privacy(mode) => Some(enums::PrivacyMode::from(mode)),
            _ => None,
        };
        let row = TimedPoliciesRow {
            kind: state.owner().into(),
            privacy_mode,
            application_id,
            started_utc_ms: CaptureColumn::new(timed_policies::started_utc_ms)
                .utc(state.started)?,
            until_utc_ms: CaptureColumn::new(timed_policies::until_utc_ms).utc(state.until)?,
        };
        diesel::insert_into(timed_policies::table)
            .values(&row)
            .on_conflict(timed_policies::kind)
            .do_update()
            .set(&row)
            .execute(self.connection)?;
        Ok(())
    }
    pub(super) fn clear_timed_policy(
        &mut self,
        kind: TimedPolicyKind,
    ) -> Result<(), ExecuteCaptureBatchError> {
        diesel::delete(timed_policies::table.find(enums::TimedPolicyKind::from(kind)))
            .execute(self.connection)?;
        Ok(())
    }
    fn policy_application(
        &mut self,
        app: &AppIdentity,
        started: UtcInstant,
    ) -> Result<i64, ExecuteCaptureBatchError> {
        let row = ApplicationsRow::policy_identity(app, started)?;
        diesel::insert_into(applications::table)
            .values(&row)
            .on_conflict(applications::identity_key)
            .do_nothing()
            .execute(self.connection)?;
        applications::table
            .filter(applications::identity_key.eq(&row.identity_key))
            .select(applications::id.assume_not_null())
            .first(self.connection)
            .map_err(ExecuteCaptureBatchError::from)
    }
    pub(super) fn load_timed_states(&mut self) -> Result<Vec<TimedState>, LoadCaptureStartupError> {
        let rows = timed_policies::table
            .order(timed_policies::kind)
            .select(TimedPoliciesRow::as_select())
            .load(self.connection)?;
        rows.into_iter()
            .map(|row| self.decode_timed_state(row))
            .collect()
    }
    fn decode_timed_state(
        &mut self,
        row: TimedPoliciesRow,
    ) -> Result<TimedState, LoadCaptureStartupError> {
        let context = super::RowContext::new(None);
        let policy = match row.kind {
            enums::TimedPolicyKind::Pause => TimedPolicy::Pause,
            enums::TimedPolicyKind::Privacy => TimedPolicy::Privacy(
                context
                    .required(row.privacy_mode, timed_policies::privacy_mode)?
                    .into(),
            ),
            enums::TimedPolicyKind::KeepTiming => TimedPolicy::KeepTiming,
            enums::TimedPolicyKind::AppExemption => {
                TimedPolicy::AppExemption(self.load_policy_application(
                    context.required(row.application_id, timed_policies::application_id)?,
                )?)
            }
            enums::TimedPolicyKind::Audible => TimedPolicy::Audible(self.load_policy_application(
                context.required(row.application_id, timed_policies::application_id)?,
            )?),
        };
        let state = TimedState {
            policy,
            started: context.utc(row.started_utc_ms, timed_policies::started_utc_ms)?,
            until: context.utc(row.until_utc_ms, timed_policies::until_utc_ms)?,
        };
        let normalized =
            state
                .clone()
                .normalized()
                .map_err(|source| DecodeCaptureRowError::Time {
                    field: context.field(timed_policies::until_utc_ms),
                    value: row.until_utc_ms,
                    source,
                })?;
        if state != normalized {
            return Err(DecodeCaptureRowError::ExcessiveTimedGrant.into());
        }
        Ok(state)
    }
    fn load_policy_application(&mut self, id: i64) -> Result<AppIdentity, LoadCaptureStartupError> {
        let row = applications::table
            .filter(applications::id.eq(id))
            .select(ApplicationsRow::as_select())
            .first::<ApplicationsRow>(self.connection)?;
        row.into_identity().map_err(LoadCaptureStartupError::from)
    }
}
impl ApplicationsRow {
    fn policy_identity(
        app: &AppIdentity,
        started: UtcInstant,
    ) -> Result<Self, ExecuteCaptureBatchError> {
        let (kind, exe_path, aumid, display_name) = match app {
            AppIdentity::Exe(path) => (
                enums::ApplicationKind::Exe,
                Some(path.clone()),
                None,
                path.clone(),
            ),
            AppIdentity::Aumid(id) => (
                enums::ApplicationKind::Aumid,
                None,
                Some(id.clone()),
                id.clone(),
            ),
            AppIdentity::Unresolved(class) => (
                enums::ApplicationKind::Unresolved,
                None,
                None,
                class.clone(),
            ),
        };
        Ok(Self {
            id: None,
            identity_key: app.key(),
            kind,
            exe_path,
            aumid,
            exe_name: None,
            display_name,
            first_seen_utc_ms: CaptureColumn::new(applications::first_seen_utc_ms).utc(started)?,
        })
    }
}
