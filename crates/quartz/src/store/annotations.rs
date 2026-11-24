//! Atomic annotation writes, validation, clipping, and transaction-local day invalidations.

mod clipping;

use super::{
    Store, WriteTx,
    enums::{self, DataSource},
    rows::AnnotationsRow,
    schema::{annotations, categories},
    versions::BumpVersionError,
};
use crate::query::MAX_QUERY_RANGE_MS;
use clipping::{Remainders, SplitIdentity};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl, SelectableHelper};
use quartz_domain::{
    capture::{UtcInstant, limits::UTC_DAY_MS},
    facts::QueryRange,
};
use quartz_protocol::{
    Annotation, AnnotationCategory, AnnotationId, AnnotationKind, AnnotationValues,
    AnnotationsCreateRequest, AnnotationsDeleteRequest, AnnotationsResponse,
    AnnotationsUpdateRequest,
};
use std::collections::BTreeSet;

/// A maximum-length range can intersect one additional UTC date.
const MAX_ANNOTATION_REQUEST_DAYS: i64 = MAX_QUERY_RANGE_MS / UTC_DAY_MS + 1;
/// Bounds old/new version increments, including clipping remainders, in one annotation write.
pub(super) const MAX_ANNOTATION_DAY_WRITES: i64 = 4096;

struct AnnotationWork {
    days: i64,
    limit: i64,
}
impl AnnotationWork {
    fn request() -> Self {
        Self {
            days: 0,
            limit: MAX_ANNOTATION_REQUEST_DAYS,
        }
    }
    fn mutations() -> Self {
        Self {
            days: 0,
            limit: MAX_ANNOTATION_DAY_WRITES,
        }
    }
    fn include_values(
        &mut self,
        values: &AnnotationValues,
    ) -> Result<(), ExecuteAnnotationWritesError> {
        self.include_range(QueryRange {
            start: values.start_utc_ms,
            end: values.end_utc_ms,
        })
    }
    fn include_range(&mut self, range: QueryRange) -> Result<(), ExecuteAnnotationWritesError> {
        range
            .end
            .checked_sub(range.start)
            .filter(|duration| *duration > 0 && *duration <= MAX_QUERY_RANGE_MS)
            .ok_or(ExecuteAnnotationWritesError::InvalidRange)?;
        UtcInstant::try_from(range.start)
            .map_err(|_| ExecuteAnnotationWritesError::InvalidRange)?;
        UtcInstant::try_from(range.end).map_err(|_| ExecuteAnnotationWritesError::InvalidRange)?;
        let days = super::versions::UtcDays::covering(range);
        self.days += days.end().0 - days.start().0 + 1;
        if self.days > self.limit {
            return Err(ExecuteAnnotationWritesError::InvalidRange);
        }
        Ok(())
    }
}

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum ExecuteAnnotationWritesError {
    #[snafu(display("annotation range is invalid or overlaps another submitted range"))]
    InvalidRange,
    #[snafu(display("annotation fields do not satisfy the kind's requirements"))]
    InvalidFields,
    #[snafu(display("annotation category is missing or archived"))]
    InvalidCategory,
    #[snafu(display("annotation to update does not exist"))]
    MissingAnnotation,
    #[snafu(display("annotation kind cannot change"))]
    ChangeKind,
    #[snafu(display("annotation UUID occurs more than once in the request"))]
    DuplicateIdentity,
    #[snafu(display("could not write annotations"))]
    Database { source: diesel::result::Error },
    #[snafu(display("could not invalidate annotation days"))]
    Version { source: BumpVersionError },
    #[snafu(display("invalid persisted annotation identity"))]
    Identity {
        source: quartz_protocol::ParseUuidError,
    },
}
impl From<diesel::result::Error> for ExecuteAnnotationWritesError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}

struct PlannedAnnotation {
    before: Option<AnnotationsRow>,
    after: AnnotationsRow,
    mutation: Mutation,
}
#[derive(PartialEq, Eq)]
enum Mutation {
    Keep,
    Write,
}

impl Store {
    pub(crate) fn create_annotations(
        &mut self,
        request: AnnotationsCreateRequest,
        now: UtcInstant,
    ) -> Result<AnnotationsResponse, ExecuteAnnotationWritesError> {
        let mut work = AnnotationWork::request();
        for entity in &request.entities {
            work.include_values(&entity.values)?;
        }
        self.write(|tx| {
            let mut plan = Vec::with_capacity(request.entities.len());
            let mut ids = BTreeSet::new();
            for entity in request.entities {
                if !ids.insert(entity.uuid.clone()) {
                    return Err(ExecuteAnnotationWritesError::DuplicateIdentity);
                }
                if let Some(row) = tx.annotation(&entity.uuid)? {
                    plan.push(PlannedAnnotation {
                        before: None,
                        after: row,
                        mutation: Mutation::Keep,
                    });
                } else {
                    let row = tx.new_annotation(entity.uuid, entity.kind, entity.values, now)?;
                    plan.push(PlannedAnnotation {
                        before: None,
                        after: row,
                        mutation: Mutation::Write,
                    });
                }
            }
            tx.apply_annotations(plan, now)
        })
    }
    pub(crate) fn update_annotations(
        &mut self,
        request: AnnotationsUpdateRequest,
        now: UtcInstant,
    ) -> Result<AnnotationsResponse, ExecuteAnnotationWritesError> {
        let mut work = AnnotationWork::request();
        for entity in &request.entities {
            work.include_values(&entity.values)?;
        }
        self.write(|tx| {
            let mut plan = Vec::with_capacity(request.entities.len());
            let mut ids = BTreeSet::new();
            for entity in request.entities {
                if !ids.insert(entity.uuid.clone()) {
                    return Err(ExecuteAnnotationWritesError::DuplicateIdentity);
                }
                let before = tx
                    .annotation(&entity.uuid)?
                    .ok_or(ExecuteAnnotationWritesError::MissingAnnotation)?;
                let kind = AnnotationKind::from(before.kind);
                if entity.kind.is_some_and(|requested| requested != kind) {
                    return Err(ExecuteAnnotationWritesError::ChangeKind);
                }
                let mut after = tx.new_annotation(entity.uuid, kind, entity.values, now)?;
                after.id = before.id;
                after.created_utc_ms = before.created_utc_ms;
                let mutation = if before.same_content(&after) {
                    after.updated_utc_ms = before.updated_utc_ms;
                    Mutation::Keep
                } else {
                    Mutation::Write
                };
                plan.push(PlannedAnnotation {
                    before: Some(before),
                    after,
                    mutation,
                });
            }
            tx.apply_annotations(plan, now)
        })
    }
    pub(crate) fn delete_annotations(
        &mut self,
        request: AnnotationsDeleteRequest,
    ) -> Result<(), ExecuteAnnotationWritesError> {
        if i64::try_from(request.uuids.len())
            .map_err(|_| ExecuteAnnotationWritesError::InvalidRange)?
            > MAX_ANNOTATION_REQUEST_DAYS
        {
            return Err(ExecuteAnnotationWritesError::InvalidRange);
        }
        self.write(|tx| {
            let mut work = AnnotationWork::mutations();
            let mut rows = Vec::new();
            let mut ids = BTreeSet::new();
            for uuid in request.uuids {
                if !ids.insert(uuid.clone()) {
                    continue;
                }
                if let Some(row) = tx.annotation(&uuid)? {
                    work.include_range(row.range())?;
                    rows.push(row);
                }
            }
            for row in rows {
                tx.remove_annotation(&row)?;
            }
            Ok(())
        })
    }
}
impl WriteTx<'_> {
    fn annotation(
        &mut self,
        uuid: &AnnotationId,
    ) -> Result<Option<AnnotationsRow>, ExecuteAnnotationWritesError> {
        Ok(annotations::table
            .filter(annotations::uuid.eq(uuid.0.to_string()))
            .select(AnnotationsRow::as_select())
            .first(self.connection)
            .optional()?)
    }
    fn new_annotation(
        &mut self,
        uuid: AnnotationId,
        kind: AnnotationKind,
        values: AnnotationValues,
        now: UtcInstant,
    ) -> Result<AnnotationsRow, ExecuteAnnotationWritesError> {
        if values.end_utc_ms <= values.start_utc_ms {
            return Err(ExecuteAnnotationWritesError::InvalidRange);
        }
        let fields_valid = match kind {
            AnnotationKind::CategoryOverride => values.category.is_some(),
            AnnotationKind::OfflineActivity | AnnotationKind::ActiveOverride => true,
            AnnotationKind::DismissedAway => {
                values.category.is_none() && values.label.is_none() && values.note.is_none()
            }
            AnnotationKind::Note => {
                values
                    .label
                    .as_ref()
                    .is_some_and(|text| !text.trim().is_empty())
                    || values
                        .note
                        .as_ref()
                        .is_some_and(|text| !text.trim().is_empty())
            }
        };
        if !fields_valid {
            return Err(ExecuteAnnotationWritesError::InvalidFields);
        }
        let category_id = values
            .category
            .map(|category| self.annotation_category(category))
            .transpose()?;
        let now = now.get().as_millisecond();
        Ok(AnnotationsRow {
            id: None,
            uuid: uuid.0.to_string(),
            kind: kind.into(),
            start_utc_ms: values.start_utc_ms,
            end_utc_ms: values.end_utc_ms,
            category_id,
            label: values.label,
            note: values.note,
            created_utc_ms: now,
            updated_utc_ms: now,
        })
    }
    fn annotation_category(
        &mut self,
        category: AnnotationCategory,
    ) -> Result<i64, ExecuteAnnotationWritesError> {
        let query = categories::table
            .filter(categories::archived.eq(0))
            .into_boxed();
        let query = match category {
            AnnotationCategory::Uuid { uuid } => {
                query.filter(categories::uuid.eq(uuid.to_string()))
            }
            AnnotationCategory::Builtin { key } => {
                query.filter(categories::builtin_key.eq(match key {
                    quartz_protocol::BuiltinCategory::Development => "development",
                    quartz_protocol::BuiltinCategory::Learning => "learning",
                    quartz_protocol::BuiltinCategory::Communication => "communication",
                    quartz_protocol::BuiltinCategory::Entertainment => "entertainment",
                    quartz_protocol::BuiltinCategory::System => "system",
                }))
            }
        };
        query
            .select(categories::id)
            .first::<Option<i64>>(self.connection)
            .optional()?
            .flatten()
            .ok_or(ExecuteAnnotationWritesError::InvalidCategory)
    }
    fn apply_annotations(
        &mut self,
        plan: Vec<PlannedAnnotation>,
        now: UtcInstant,
    ) -> Result<AnnotationsResponse, ExecuteAnnotationWritesError> {
        // Validate the final batch before moving any row, including swaps of two existing ranges.
        for (index, item) in plan.iter().enumerate() {
            if AnnotationKind::from(item.after.kind).clips()
                && plan[..index].iter().any(|other| {
                    other.after.kind == item.after.kind
                        && other.after.start_utc_ms < item.after.end_utc_ms
                        && other.after.end_utc_ms > item.after.start_utc_ms
                })
            {
                return Err(ExecuteAnnotationWritesError::InvalidRange);
            }
        }
        self.check_annotation_work(&plan)?;
        for item in &plan {
            if item.mutation == Mutation::Write
                && let Some(before) = &item.before
            {
                self.remove_annotation(before)?;
            }
        }
        for item in &plan {
            if item.mutation == Mutation::Write {
                self.insert_annotation(&item.after)?;
            }
        }
        // Reserve the final rows' SQLite identities before split remainders allocate rowids.
        for item in &plan {
            if item.mutation == Mutation::Write {
                self.clip_annotations(&item.after, now)?;
            }
        }
        let entities = plan
            .into_iter()
            .map(|item| self.annotation_entity(item.after))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(AnnotationsResponse { entities })
    }
    fn check_annotation_work(
        &mut self,
        plan: &[PlannedAnnotation],
    ) -> Result<(), ExecuteAnnotationWritesError> {
        let mut work = AnnotationWork::mutations();
        for item in plan.iter().filter(|item| item.mutation == Mutation::Write) {
            if let Some(before) = &item.before {
                work.include_range(before.range())?;
            }
            work.include_range(item.after.range())?;
            if !AnnotationKind::from(item.after.kind).clips() {
                continue;
            }
            for old in self.overlapping_annotations(&item.after)? {
                if plan.iter().any(|entry| {
                    entry.mutation == Mutation::Write
                        && entry
                            .before
                            .as_ref()
                            .is_some_and(|before| before.uuid == old.uuid)
                }) {
                    continue;
                }
                work.include_range(old.range())?;
                let remainders = Remainders::subtract(old.range(), item.after.range());
                if let Some(left) = remainders.left {
                    work.include_range(left)?;
                }
                if let Some(right) = remainders.right {
                    work.include_range(right)?;
                }
            }
        }
        Ok(())
    }
    fn overlapping_annotations(
        &mut self,
        writing: &AnnotationsRow,
    ) -> Result<Vec<AnnotationsRow>, ExecuteAnnotationWritesError> {
        let overlaps = annotations::table
            .filter(annotations::kind.eq(writing.kind))
            .filter(annotations::uuid.ne(&writing.uuid))
            .filter(annotations::start_utc_ms.lt(writing.end_utc_ms))
            .filter(annotations::end_utc_ms.gt(writing.start_utc_ms))
            .order(annotations::uuid)
            .select(AnnotationsRow::as_select())
            .limit(MAX_ANNOTATION_DAY_WRITES + 1)
            .load::<AnnotationsRow>(self.connection)?;
        if i64::try_from(overlaps.len()).map_err(|_| ExecuteAnnotationWritesError::InvalidRange)?
            > MAX_ANNOTATION_DAY_WRITES
        {
            return Err(ExecuteAnnotationWritesError::InvalidRange);
        }
        Ok(overlaps)
    }
    fn clip_annotations(
        &mut self,
        writing: &AnnotationsRow,
        now: UtcInstant,
    ) -> Result<(), ExecuteAnnotationWritesError> {
        if !AnnotationKind::from(writing.kind).clips() {
            return Ok(());
        }
        let overlaps = self.overlapping_annotations(writing)?;
        for old in overlaps {
            let remainders = Remainders::subtract(old.range(), writing.range());
            self.remove_annotation(&old)?;
            if let Some(left) = remainders.left {
                let mut row = old.clone();
                row.end_utc_ms = left.end;
                row.updated_utc_ms = now.get().as_millisecond();
                self.insert_annotation(&row)?;
            }
            if let Some(right) = remainders.right {
                let mut row = old.clone();
                row.start_utc_ms = right.start;
                row.updated_utc_ms = now.get().as_millisecond();
                if remainders.left.is_some() {
                    row.id = None;
                    row.uuid = SplitIdentity {
                        old: &old.identity()?,
                        writing: &writing.identity()?,
                        old_range: old.range(),
                        writing_range: writing.range(),
                    }
                    .derive()
                    .0
                    .to_string();
                    row.created_utc_ms = now.get().as_millisecond();
                }
                self.insert_annotation(&row)?;
            }
        }
        Ok(())
    }
    fn insert_annotation(
        &mut self,
        row: &AnnotationsRow,
    ) -> Result<(), ExecuteAnnotationWritesError> {
        diesel::insert_into(annotations::table)
            .values(row)
            .execute(self.connection)?;
        self.bump_annotation(row)
    }
    fn remove_annotation(
        &mut self,
        row: &AnnotationsRow,
    ) -> Result<(), ExecuteAnnotationWritesError> {
        diesel::delete(annotations::table.filter(annotations::uuid.eq(&row.uuid)))
            .execute(self.connection)?;
        self.bump_annotation(row)
    }
    fn bump_annotation(
        &mut self,
        row: &AnnotationsRow,
    ) -> Result<(), ExecuteAnnotationWritesError> {
        self.effects
            .bump_days(self.connection, DataSource::Annotations, row.range())
            .map_err(|source| ExecuteAnnotationWritesError::Version { source })
    }
    fn annotation_entity(
        &mut self,
        row: AnnotationsRow,
    ) -> Result<Annotation, ExecuteAnnotationWritesError> {
        let category = row
            .category_id
            .map(|id| {
                let uuid = categories::table
                    .filter(categories::id.eq(id))
                    .select(categories::uuid)
                    .first::<String>(self.connection)?;
                Ok::<_, ExecuteAnnotationWritesError>(AnnotationCategory::Uuid {
                    uuid: uuid
                        .parse()
                        .map_err(|source| ExecuteAnnotationWritesError::Identity { source })?,
                })
            })
            .transpose()?;
        Ok(Annotation {
            uuid: row.identity()?,
            kind: row.kind.into(),
            values: AnnotationValues {
                start_utc_ms: row.start_utc_ms,
                end_utc_ms: row.end_utc_ms,
                category,
                label: row.label,
                note: row.note,
            },
            created_utc_ms: row.created_utc_ms,
            updated_utc_ms: row.updated_utc_ms,
        })
    }
}
impl AnnotationsRow {
    fn range(&self) -> QueryRange {
        QueryRange {
            start: self.start_utc_ms,
            end: self.end_utc_ms,
        }
    }
    fn identity(&self) -> Result<AnnotationId, ExecuteAnnotationWritesError> {
        self.uuid
            .parse()
            .map(AnnotationId)
            .map_err(|source| ExecuteAnnotationWritesError::Identity { source })
    }
    fn same_content(&self, other: &Self) -> bool {
        self.start_utc_ms == other.start_utc_ms
            && self.end_utc_ms == other.end_utc_ms
            && self.category_id == other.category_id
            && self.label == other.label
            && self.note == other.note
    }
}
impl From<AnnotationKind> for enums::AnnotationKind {
    fn from(kind: AnnotationKind) -> Self {
        match kind {
            AnnotationKind::CategoryOverride => Self::CategoryOverride,
            AnnotationKind::OfflineActivity => Self::OfflineActivity,
            AnnotationKind::ActiveOverride => Self::ActiveOverride,
            AnnotationKind::DismissedAway => Self::DismissedAway,
            AnnotationKind::Note => Self::Note,
        }
    }
}
impl From<enums::AnnotationKind> for AnnotationKind {
    fn from(kind: enums::AnnotationKind) -> Self {
        match kind {
            enums::AnnotationKind::CategoryOverride => Self::CategoryOverride,
            enums::AnnotationKind::OfflineActivity => Self::OfflineActivity,
            enums::AnnotationKind::ActiveOverride => Self::ActiveOverride,
            enums::AnnotationKind::DismissedAway => Self::DismissedAway,
            enums::AnnotationKind::Note => Self::Note,
        }
    }
}

#[cfg(test)]
mod tests;
