//! Startup-only default insertion and snapshot category metadata reads.

use super::{
    Store, enums, query::ReadSnapshot, query_error::ReadQueryError, rows::CategoriesRow,
    schema::categories, versions::TxEffects,
};
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl, SelectableHelper};
use quartz_domain::categories::{
    BuiltinCategory, Category, CategoryId, CategoryTendency, CategoryUuid,
};

#[derive(Debug, snafu::Snafu)]
pub(crate) enum SeedCategoriesError {
    #[snafu(display("could not insert default categories: {source}"))]
    Insert { source: diesel::result::Error },
    #[snafu(display("could not update category metadata version: {source}"))]
    Version {
        source: Box<super::capture::ExecuteCaptureBatchError>,
    },
}
impl From<diesel::result::Error> for SeedCategoriesError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Insert { source }
    }
}
impl Store {
    /// Inserts only absent builtin keys, retaining all existing metadata and archived rows.
    pub(crate) fn seed_categories(&mut self) -> Result<(), SeedCategoriesError> {
        let mut effects = TxEffects::default();
        self.writer
            .immediate_transaction::<_, SeedCategoriesError, _>(|connection| {
                let mut inserted = 0;
                for (order, builtin) in BuiltinCategory::ALL.into_iter().enumerate() {
                    inserted += diesel::insert_into(categories::table)
                        .values(CategoriesRow {
                            id: None,
                            uuid: builtin.uuid().0,
                            parent_id: None,
                            builtin_key: Some(builtin.key().into()),
                            name: None,
                            color: builtin.color().into(),
                            tendency: match builtin.tendency() {
                                CategoryTendency::Focus => enums::CategoryTendency::Focus,
                                CategoryTendency::Neutral => enums::CategoryTendency::Neutral,
                                CategoryTendency::Distraction => {
                                    enums::CategoryTendency::Distraction
                                }
                            },
                            sort_order: i64::try_from(order)
                                .expect("five builtin categories fit i64"),
                            archived: 0,
                        })
                        .on_conflict(categories::builtin_key)
                        .do_nothing()
                        .execute(connection)?;
                }
                if inserted > 0 {
                    effects.bump_metadata(connection).map_err(|source| {
                        SeedCategoriesError::Version {
                            source: Box::new(source),
                        }
                    })?;
                }
                Ok(())
            })?;
        self.pending_effects.merge(effects);
        Ok(())
    }
}
impl ReadSnapshot {
    pub(crate) fn categories(&mut self) -> Result<Vec<Category>, ReadQueryError> {
        categories::table
            .filter(categories::archived.eq(0))
            .order((categories::sort_order, categories::id))
            .select(CategoriesRow::as_select())
            .load::<CategoriesRow>(self.connection())?
            .into_iter()
            .map(CategoriesRow::into_category)
            .collect()
    }
}
impl CategoriesRow {
    fn into_category(self) -> Result<Category, ReadQueryError> {
        let builtin_key = self
            .builtin_key
            .map(|key| BuiltinCategory::from_key(&key).ok_or(ReadQueryError::InvalidCategories))
            .transpose()?;
        let valid_color = self.color == "neutral"
            || self.color.strip_prefix("cat.").is_some_and(|slot| {
                slot.parse::<u8>().is_ok_and(|number| {
                    (1..=quartz_domain::categories::PALETTE_SLOT_COUNT).contains(&number)
                        && number.to_string() == slot
                })
            });
        if !valid_color || (self.name.is_none() && builtin_key.is_none()) {
            return Err(ReadQueryError::InvalidCategories);
        }
        Ok(Category {
            id: CategoryId(self.id.ok_or(ReadQueryError::InvalidCategories)?),
            uuid: CategoryUuid(self.uuid),
            builtin_key,
            name: self.name,
            color: self.color,
            tendency: match self.tendency {
                enums::CategoryTendency::Focus => CategoryTendency::Focus,
                enums::CategoryTendency::Neutral => CategoryTendency::Neutral,
                enums::CategoryTendency::Distraction => CategoryTendency::Distraction,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_directory::DataDirectory;

    #[test]
    fn seeding_is_idempotent_preserves_edits_and_bumps_metadata_only_on_insert() {
        let directory = tempfile::tempdir().expect("isolated categories");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("seed categories");
        assert_eq!(store.take_committed_versions().metadata_version, Some(1));
        assert_eq!(
            categories::table
                .count()
                .get_result::<i64>(&mut store.writer)
                .expect("count"),
            5
        );
        diesel::update(categories::table.filter(categories::builtin_key.eq("development")))
            .set((
                categories::name.eq("My work"),
                categories::color.eq("cat.2"),
                categories::tendency.eq(enums::CategoryTendency::Neutral),
                categories::sort_order.eq(42_i64),
                categories::archived.eq(1),
            ))
            .execute(&mut store.writer)
            .expect("user edits");
        store.seed_categories().expect("repeat seed");
        assert_eq!(store.take_committed_versions().metadata_version, None);
        let edited = categories::table
            .filter(categories::builtin_key.eq("development"))
            .select(CategoriesRow::as_select())
            .first::<CategoriesRow>(&mut store.writer)
            .expect("edited row");
        assert_eq!(edited.name.as_deref(), Some("My work"));
        assert_eq!(edited.color, "cat.2");
        assert_eq!(edited.tendency, enums::CategoryTendency::Neutral);
        assert_eq!(edited.sort_order, 42);
        assert_eq!(edited.archived, 1);
        diesel::delete(categories::table.filter(categories::builtin_key.eq("system")))
            .execute(&mut store.writer)
            .expect("remove one default");
        store.seed_categories().expect("insert missing default");
        assert_eq!(store.take_committed_versions().metadata_version, Some(2));
    }
}
