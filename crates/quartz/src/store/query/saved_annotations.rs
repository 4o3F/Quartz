//! Stored editing values read independently of activity projection.

use super::{ReadQueryError, ReadSnapshot};
use crate::store::{
    rows::AnnotationsRow,
    schema::{annotations, categories},
};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, JoinOnDsl, NullableExpressionMethods,
    OptionalExtension, QueryDsl, RunQueryDsl, SelectableHelper,
};
use quartz_domain::facts::QueryRange;
use quartz_protocol::{AnnotationCategory, AnnotationId, SavedAnnotation};

#[derive(Debug, Default)]
pub(crate) struct AnnotationScan {
    after: Option<AnnotationOrder>,
}

#[derive(Debug)]
struct AnnotationOrder {
    start_utc_ms: i64,
    end_utc_ms: i64,
    uuid: String,
}

#[derive(diesel::Queryable)]
struct SavedAnnotationRow {
    annotation: AnnotationsRow,
    category_uuid: Option<String>,
}
impl ReadSnapshot {
    /// Reads one complete entity per turn so capture writes can run between rows.
    pub(crate) fn read_saved_annotation(
        &mut self,
        range: QueryRange,
        scan: &mut AnnotationScan,
    ) -> Result<Option<SavedAnnotation>, ReadQueryError> {
        let mut query = annotations::table
            .left_join(categories::table.on(annotations::category_id.eq(categories::id)))
            .filter(annotations::start_utc_ms.lt(range.end))
            .filter(annotations::end_utc_ms.gt(range.start))
            .into_boxed();
        if let Some(after) = &scan.after {
            query = query.filter(
                annotations::start_utc_ms
                    .gt(after.start_utc_ms)
                    .or(annotations::start_utc_ms.eq(after.start_utc_ms).and(
                        annotations::end_utc_ms
                            .gt(after.end_utc_ms)
                            .or(annotations::end_utc_ms
                                .eq(after.end_utc_ms)
                                .and(annotations::uuid.gt(&after.uuid))),
                    )),
            );
        }
        let Some(row) = query
            .order(annotations::start_utc_ms)
            .then_order_by(annotations::end_utc_ms)
            .then_order_by(annotations::uuid)
            .select((AnnotationsRow::as_select(), categories::uuid.nullable()))
            .first::<SavedAnnotationRow>(self.connection())
            .optional()?
        else {
            return Ok(None);
        };
        let SavedAnnotationRow {
            annotation: row,
            category_uuid,
        } = row;
        if row.category_id.is_some() && category_uuid.is_none() {
            return Err(ReadQueryError::InvalidCategories);
        }
        let category = category_uuid
            .map(|uuid| {
                Ok::<_, ReadQueryError>(AnnotationCategory::Uuid {
                    uuid: uuid
                        .parse()
                        .map_err(|source| ReadQueryError::AnnotationIdentity { source })?,
                })
            })
            .transpose()?;
        scan.after = Some(AnnotationOrder {
            start_utc_ms: row.start_utc_ms,
            end_utc_ms: row.end_utc_ms,
            uuid: row.uuid.clone(),
        });
        Ok(Some(SavedAnnotation {
            uuid: AnnotationId(
                row.uuid
                    .parse()
                    .map_err(|source| ReadQueryError::AnnotationIdentity { source })?,
            ),
            kind: row.kind.into(),
            start_utc_ms: row.start_utc_ms,
            end_utc_ms: row.end_utc_ms,
            category,
            label_preset: row.label_preset.map(Into::into),
            label: row.label,
            note: row.note,
            created_utc_ms: row.created_utc_ms,
            updated_utc_ms: row.updated_utc_ms,
        }))
    }
}
