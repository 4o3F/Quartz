//! Application dictionary and window instance writes.

use super::{Capture, CaptureColumn, DbRowId, DecodeCaptureRowError, ExecuteCaptureBatchError};
use crate::store::{
    enums,
    rows::{ApplicationsRow, WindowInstancesRow},
    schema::{applications, settings, window_instances},
};
use diesel::prelude::{AsChangeset, ExpressionMethods, QueryDsl, RunQueryDsl};
use diesel::{NullableExpressionMethods, OptionalExtension};
use quartz_domain::capture::{
    AppIdentity, MetadataSource, RunId, Timestamp, WindowId, WindowInstance,
};

#[derive(AsChangeset)]
#[diesel(table_name = applications, treat_none_as_null = true)]
struct ApplicationMetadataUpdate<'metadata> {
    display_name: &'metadata str,
    exe_name: Option<&'metadata str>,
}

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
                .map(|at| CaptureColumn::new(window_instances::process_created_utc_ms).utc(at))
                .transpose()?,
            hwnd: CaptureColumn::new(window_instances::hwnd).integer(instance.handle.get())?,
            generation: CaptureColumn::new(window_instances::generation)
                .integer(instance.generation.get())?,
            retired_utc_ms: None,
            window_class: Some(instance.class.clone()),
            first_seen_utc_ms: CaptureColumn::new(window_instances::first_seen_utc_ms)
                .utc(instance.first_seen.utc)?,
            last_seen_utc_ms: CaptureColumn::new(window_instances::last_seen_utc_ms).utc(at.utc)?,
        })
    }
    pub(super) fn retire_window(
        &mut self,
        run: RunId,
        instance: WindowId,
        at: Timestamp,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let id = self.window(run, instance)?;
        let context = id.context();
        diesel::update(window_instances::table.filter(window_instances::id.eq(id.get())))
            .set((
                window_instances::retired_utc_ms.eq(context
                    .field(window_instances::retired_utc_ms)
                    .utc(at.utc)?),
                window_instances::last_seen_utc_ms.eq(context
                    .field(window_instances::last_seen_utc_ms)
                    .utc(at.utc)?),
            ))
            .execute(self.connection)?;
        self.run_keys(run).windows.remove(&instance);
        Ok(())
    }
    fn bump_metadata_version(&mut self) -> Result<(), ExecuteCaptureBatchError> {
        let value = settings::table
            .filter(settings::key.eq("metadata_version"))
            .select(settings::value)
            .first::<String>(self.connection)
            .optional()?;
        let version: i64 = match value {
            Some(value) => serde_json::from_str(&value)
                .map_err(|_| ExecuteCaptureBatchError::MetadataVersion)?,
            None => 0,
        };
        let version = version
            .checked_add(1)
            .filter(|version| *version > 0)
            .ok_or(ExecuteCaptureBatchError::MetadataVersion)?;
        diesel::insert_into(settings::table)
            .values((
                settings::key.eq("metadata_version"),
                settings::value.eq(version.to_string()),
            ))
            .on_conflict(settings::key)
            .do_update()
            .set(settings::value.eq(version.to_string()))
            .execute(self.connection)?;
        self.keys.metadata_version = Some(version);
        Ok(())
    }
    fn application(
        &mut self,
        instance: &WindowInstance,
        at: Timestamp,
    ) -> Result<DbRowId, ExecuteCaptureBatchError> {
        let row = ApplicationsRow::observed(instance, at)?;
        let previous = applications::table
            .filter(applications::identity_key.eq(&row.identity_key))
            .select((applications::display_name, applications::exe_name))
            .first::<PreviousMetadata>(self.connection)
            .optional()?;
        if matches!(instance.metadata_source, MetadataSource::Process)
            && previous.is_some_and(|previous| {
                previous.display_name != row.display_name || previous.exe_name != row.exe_name
            })
        {
            self.bump_metadata_version()?;
        }
        let insert = diesel::insert_into(applications::table)
            .values(&row)
            .on_conflict(applications::identity_key);
        let result = match instance.metadata_source {
            MetadataSource::Process => insert
                .do_update()
                .set(ApplicationMetadataUpdate {
                    display_name: &row.display_name,
                    exe_name: row.exe_name.as_deref(),
                })
                .returning(applications::id.assume_not_null())
                .get_result(self.connection),
            MetadataSource::Frame | MetadataSource::Unavailable => insert
                .do_update()
                .set(applications::identity_key.eq(&row.identity_key))
                .returning(applications::id.assume_not_null())
                .get_result(self.connection),
        };
        result
            .map(DbRowId::new)
            .map_err(ExecuteCaptureBatchError::from)
    }
}
impl ApplicationsRow {
    pub(super) fn into_identity(self) -> Result<AppIdentity, DecodeCaptureRowError> {
        let context = super::RowContext::new(self.id);
        Ok(match self.kind {
            enums::ApplicationKind::Exe => {
                AppIdentity::Exe(context.required(self.exe_path, applications::exe_path)?)
            }
            enums::ApplicationKind::Aumid => {
                AppIdentity::Aumid(context.required(self.aumid, applications::aumid)?)
            }
            enums::ApplicationKind::Unresolved => AppIdentity::Unresolved(
                self.identity_key
                    .strip_prefix("unresolved:")
                    .ok_or(DecodeCaptureRowError::ApplicationIdentity {
                        field: context.field(applications::identity_key),
                    })?
                    .to_owned(),
            ),
        })
    }

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
            first_seen_utc_ms: CaptureColumn::new(applications::first_seen_utc_ms).utc(at.utc)?,
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

#[derive(diesel::Queryable)]
struct PreviousMetadata {
    display_name: String,
    exe_name: Option<String>,
}
