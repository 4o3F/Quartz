//! Application dictionary and window instance writes.

use super::{Capture, CaptureColumn, DbRowId, ExecuteCaptureBatchError};
use crate::store::{
    enums,
    rows::{ApplicationsRow, WindowInstancesRow},
    schema::{applications, window_instances},
};
use diesel::NullableExpressionMethods;
use diesel::prelude::{ExpressionMethods, QueryDsl, RunQueryDsl};
use quartz_domain::capture::{AppIdentity, RunId, Timestamp, WindowId, WindowInstance};

impl Capture<'_> {
    pub(super) fn observe_window(
        &mut self,
        run: RunId,
        instance: &WindowInstance,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let app_id = self.application(instance, at)?;
        let row = self.window_row(run, instance, at, app_id)?;
        let id = diesel::insert_into(window_instances::table)
            .values(&row)
            .on_conflict((
                window_instances::run_id,
                window_instances::hwnd,
                window_instances::generation,
            ))
            .do_update()
            .set(window_instances::last_seen_utc_ms.eq(row.last_seen_utc_ms))
            .returning(window_instances::id.assume_not_null())
            .get_result(self.connection)?;
        self.run_keys(run)
            .windows
            .insert(instance.id, DbRowId::new(id));
        Ok(())
    }
    fn window_row(
        &self,
        run: RunId,
        instance: &WindowInstance,
        at: Timestamp,
        app_id: DbRowId,
    ) -> Result<WindowInstancesRow, ExecuteCaptureBatchError> {
        Ok(WindowInstancesRow {
            id: None,
            run_id: Self::run_id(run)?.get(),
            app_id: app_id.get(),
            pid: i64::from(instance.process.get()),
            process_created_utc_ms: instance
                .process_created_utc
                .map(|at| CaptureColumn::new("window_instances", "process_created_utc_ms").utc(at))
                .transpose()?,
            hwnd: CaptureColumn::new("window_instances", "hwnd").integer(instance.handle.get())?,
            generation: CaptureColumn::new("window_instances", "generation")
                .integer(instance.generation.get())?,
            retired_utc_ms: None,
            window_class: Some(instance.class.clone()),
            first_seen_utc_ms: CaptureColumn::new("window_instances", "first_seen_utc_ms")
                .utc(instance.first_seen.utc)?,
            last_seen_utc_ms: CaptureColumn::new("window_instances", "last_seen_utc_ms")
                .utc(at.utc)?,
        })
    }
    pub(super) fn retire_window(
        &mut self,
        run: RunId,
        instance: WindowId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let id = self.window(run, instance)?;
        let context = id.context("window_instances");
        diesel::update(window_instances::table.filter(window_instances::id.eq(id.get())))
            .set((
                window_instances::retired_utc_ms.eq(context.field("retired_utc_ms").utc(at.utc)?),
                window_instances::last_seen_utc_ms
                    .eq(context.field("last_seen_utc_ms").utc(at.utc)?),
            ))
            .execute(self.connection)?;
        self.run_keys(run).windows.remove(&instance);
        Ok(())
    }
    fn application(
        &mut self,
        instance: &WindowInstance,
        at: Timestamp,
    ) -> Result<DbRowId, ExecuteCaptureBatchError> {
        let row = ApplicationsRow::observed(instance, at)?;
        diesel::insert_into(applications::table)
            .values(&row)
            .on_conflict(applications::identity_key)
            .do_update()
            .set((
                applications::display_name.eq(&row.display_name),
                applications::exe_name.eq(&row.exe_name),
            ))
            .returning(applications::id.assume_not_null())
            .get_result(self.connection)
            .map(DbRowId::new)
            .map_err(ExecuteCaptureBatchError::from)
    }
}
impl ApplicationsRow {
    fn observed(
        instance: &WindowInstance,
        at: Timestamp,
    ) -> Result<Self, ExecuteCaptureBatchError> {
        let mut row = Self {
            id: None,
            identity_key: instance.app.key(),
            kind: enums::ApplicationKind::Unresolved,
            exe_path: None,
            aumid: None,
            exe_name: instance.exe_name.clone(),
            display_name: instance.display_name.clone(),
            first_seen_utc_ms: CaptureColumn::new("applications", "first_seen_utc_ms")
                .utc(at.utc)?,
        };
        match &instance.app {
            AppIdentity::Exe(path) => {
                row.kind = enums::ApplicationKind::Exe;
                row.exe_path = Some(path.clone());
            }
            AppIdentity::Aumid(id) => {
                row.kind = enums::ApplicationKind::Aumid;
                row.aumid = Some(id.clone());
            }
            AppIdentity::Unresolved(_) => {}
        }
        Ok(row)
    }
}
