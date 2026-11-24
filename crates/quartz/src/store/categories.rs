//! Category transactions, stable hierarchy reads, and startup default insertion.

use super::{
    Store, WriteTx, enums, query::ReadSnapshot, query_error::ReadQueryError, rows::CategoriesRow,
    schema::categories, versions::BumpVersionError,
};
use diesel::{
    Connection, ExpressionMethods, QueryDsl, RunQueryDsl, SelectableHelper, SqliteConnection,
};
use quartz_domain::categories::{
    ArchiveCategory, BuiltinCategory, Category, CategoryArchiveState, CategoryEntity, CategoryId,
    CategoryName, CategoryTendency, CategoryUuid, CategoryValues, CreateCategory,
    EditCategoryError, IncludeArchivedCategories, ReorderCategories, UpdateCategory,
};

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, snafu::Snafu)]
pub(crate) enum SeedCategoriesError {
    #[snafu(display("could not insert default categories: {source}"))]
    Insert { source: diesel::result::Error },
    #[snafu(display("could not update category metadata version: {source}"))]
    Version { source: BumpVersionError },
}
impl From<diesel::result::Error> for SeedCategoriesError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Insert { source }
    }
}
impl Store {
    /// Inserts only absent builtin keys, retaining all existing metadata and archived rows.
    pub(crate) fn seed_categories(&mut self) -> Result<(), SeedCategoriesError> {
        self.write(|tx| {
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
                            CategoryTendency::Distraction => enums::CategoryTendency::Distraction,
                        },
                        sort_order: i64::try_from(order).expect("five builtin categories fit i64"),
                        archived: 0,
                    })
                    .on_conflict(categories::builtin_key)
                    .do_nothing()
                    .execute(tx.connection)?;
            }
            if inserted > 0 {
                tx.effects
                    .bump_metadata(tx.connection)
                    .map_err(|source| SeedCategoriesError::Version { source })?;
            }
            Ok(())
        })
    }
}
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum ExecuteCategoryCommandError {
    #[snafu(display("category edit rejected: {source}"))]
    Validation { source: EditCategoryError },
    #[snafu(display("could not access categories"))]
    Database { source: diesel::result::Error },
    #[snafu(display("could not version category metadata"))]
    Version { source: BumpVersionError },
    #[snafu(display("persisted category metadata or hierarchy is invalid"))]
    InvalidStoredCategory,
}
impl From<diesel::result::Error> for ExecuteCategoryCommandError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}
impl From<EditCategoryError> for ExecuteCategoryCommandError {
    fn from(source: EditCategoryError) -> Self {
        Self::Validation { source }
    }
}

/// One database snapshot of the hierarchy and its metadata input.
#[derive(Debug)]
pub(crate) struct CategoryListing {
    pub entities: Vec<CategoryEntity>,
    pub metadata_version: i64,
}

impl Store {
    pub(crate) fn category_listing(
        &mut self,
        archived: IncludeArchivedCategories,
    ) -> Result<CategoryListing, ExecuteCategoryCommandError> {
        self.writer.transaction(|connection| {
            let entities = CategoryTree::load(connection)?.list(archived)?;
            let metadata_version = super::settings::Settings::new(connection)
                .metadata_version()
                .map_err(|source| ExecuteCategoryCommandError::Version {
                    source: BumpVersionError::Setting { source },
                })?;
            Ok(CategoryListing {
                entities,
                metadata_version,
            })
        })
    }

    /// Returns hierarchy preorder with siblings ordered by sort_order, then UUID.
    /// Filtering archives preserves active descendants of archived parents.
    pub(crate) fn list_categories(
        &mut self,
        archived: IncludeArchivedCategories,
    ) -> Result<Vec<CategoryEntity>, ExecuteCategoryCommandError> {
        self.writer
            .transaction(|connection| CategoryTree::load(connection)?.list(archived))
    }

    pub(crate) fn create_category(
        &mut self,
        command: CreateCategory,
    ) -> Result<CategoryEntity, ExecuteCategoryCommandError> {
        self.write(|tx| {
            let tree = CategoryTree::load(tx.connection)?;
            // UUID retries return before validating replacement values or parent references.
            if let Some(row) = tree.find(&command.uuid) {
                return tree.entity(row);
            }
            if BuiltinCategory::ALL
                .into_iter()
                .any(|builtin| builtin.uuid() == command.uuid)
            {
                return Err(EditCategoryError::BuiltinIdentityViolation.into());
            }
            command.values.validate_for(None)?;
            let parent_id = tree.parent_id(command.values.parent_uuid.as_ref())?;
            let row = CategoriesRow {
                id: None,
                uuid: command.uuid.0,
                parent_id,
                builtin_key: None,
                name: command.values.name.map(String::from),
                color: command.values.color.into(),
                tendency: command.values.tendency.into(),
                sort_order: tree.append_order(parent_id)?,
                archived: 0,
            };
            let row = diesel::insert_into(categories::table)
                .values(&row)
                .returning(CategoriesRow::as_returning())
                .get_result(tx.connection)?;
            let entity = tree.entity(&row)?;
            tx.bump_category_metadata()?;
            Ok(entity)
        })
    }

    /// Moving a category appends it after all children of the new parent, including archives.
    /// Archived parents are permitted; identity and archive state remain unchanged.
    pub(crate) fn update_category(
        &mut self,
        command: UpdateCategory,
    ) -> Result<CategoryEntity, ExecuteCategoryCommandError> {
        self.write(|tx| {
            let tree = CategoryTree::load(tx.connection)?;
            let before = tree.row(&command.uuid)?;
            let original = tree.entity(before)?;
            command.values.validate_for(original.builtin_key)?;
            let parent_id = tree.parent_id(command.values.parent_uuid.as_ref())?;
            tree.validate_parent(original.id.0, parent_id)?;
            let name = command.values.name.as_ref().map(|name| name.as_str());
            if original.values == command.values && before.name.as_deref() == name {
                return Ok(original);
            }
            let sort_order = if before.parent_id == parent_id {
                before.sort_order
            } else {
                tree.append_order(parent_id)?
            };
            let after = CategoriesRow {
                parent_id,
                name: command.values.name.map(String::from),
                color: command.values.color.into(),
                tendency: command.values.tendency.into(),
                sort_order,
                ..before.clone()
            };
            diesel::update(categories::table.filter(categories::uuid.eq(&after.uuid)))
                .set(CategoryChanges::from(&after))
                .execute(tx.connection)?;
            let entity = tree.entity(&after)?;
            tx.bump_category_metadata()?;
            Ok(entity)
        })
    }

    pub(crate) fn reorder_categories(
        &mut self,
        command: ReorderCategories,
    ) -> Result<(), ExecuteCategoryCommandError> {
        self.write(|tx| {
            let tree = CategoryTree::load(tx.connection)?;
            let parent_id = tree.parent_id(command.parent_uuid.as_ref())?;
            let siblings: Vec<_> = tree
                .rows
                .iter()
                .filter(|row| row.parent_id == parent_id)
                .collect();
            let expected: BTreeSet<_> = siblings
                .iter()
                .map(|row| CategoryUuid(row.uuid.clone()))
                .collect();
            let requested: BTreeSet<_> = command.uuids.iter().cloned().collect();
            if requested.len() != command.uuids.len() || requested != expected {
                return Err(EditCategoryError::ReorderSetMismatch.into());
            }
            // Repeating the current order is a no-op even when its numeric positions have gaps.
            if siblings
                .iter()
                .map(|row| &row.uuid)
                .eq(command.uuids.iter().map(|uuid| &uuid.0))
            {
                return Ok(());
            }
            let mut order = 0_i64;
            for uuid in &command.uuids {
                diesel::update(categories::table.filter(categories::uuid.eq(&uuid.0)))
                    .set(categories::sort_order.eq(order))
                    .execute(tx.connection)?;
                order = order
                    .checked_add(1)
                    .ok_or(EditCategoryError::OrderOverflow)?;
            }
            tx.bump_category_metadata()
        })
    }

    pub(crate) fn archive_category(
        &mut self,
        command: ArchiveCategory,
    ) -> Result<CategoryEntity, ExecuteCategoryCommandError> {
        self.write(|tx| {
            let tree = CategoryTree::load(tx.connection)?;
            let row = tree.row(&command.uuid)?;
            let mut entity = tree.entity(row)?;
            if entity.archive_state != command.state {
                let archived = match command.state {
                    CategoryArchiveState::Active => 0,
                    CategoryArchiveState::Archived => 1,
                };
                diesel::update(categories::table.filter(categories::uuid.eq(&row.uuid)))
                    .set(categories::archived.eq(archived))
                    .execute(tx.connection)?;
                tx.bump_category_metadata()?;
                entity.archive_state = command.state;
            }
            Ok(entity)
        })
    }
}
impl WriteTx<'_> {
    fn bump_category_metadata(&mut self) -> Result<(), ExecuteCategoryCommandError> {
        self.effects
            .bump_metadata(self.connection)
            .map_err(|source| ExecuteCategoryCommandError::Version { source })
    }
}

/// Nullable final values must write SQL NULL when clearing a parent or builtin rename.
#[derive(diesel::AsChangeset)]
#[diesel(table_name = categories, treat_none_as_null = true)]
struct CategoryChanges<'row> {
    parent_id: Option<i64>,
    name: Option<&'row str>,
    color: &'row str,
    tendency: enums::CategoryTendency,
    sort_order: i64,
}
impl<'row> From<&'row CategoriesRow> for CategoryChanges<'row> {
    fn from(row: &'row CategoriesRow) -> Self {
        Self {
            parent_id: row.parent_id,
            name: row.name.as_deref(),
            color: &row.color,
            tendency: row.tendency,
            sort_order: row.sort_order,
        }
    }
}

struct CategoryTree {
    rows: Vec<CategoriesRow>,
}
impl CategoryTree {
    fn load(connection: &mut SqliteConnection) -> Result<Self, ExecuteCategoryCommandError> {
        let rows = categories::table
            .order(categories::sort_order)
            .then_order_by(categories::uuid)
            .select(CategoriesRow::as_select())
            .load(connection)?;
        Ok(Self { rows })
    }
    fn find(&self, uuid: &CategoryUuid) -> Option<&CategoriesRow> {
        self.rows.iter().find(|row| row.uuid == uuid.0)
    }
    fn row(&self, uuid: &CategoryUuid) -> Result<&CategoriesRow, EditCategoryError> {
        self.find(uuid).ok_or(EditCategoryError::NotFound)
    }
    fn parent_id(
        &self,
        uuid: Option<&CategoryUuid>,
    ) -> Result<Option<i64>, ExecuteCategoryCommandError> {
        uuid.map(|uuid| {
            self.find(uuid)
                .ok_or_else(|| ExecuteCategoryCommandError::from(EditCategoryError::InvalidParent))?
                .id
                .ok_or(ExecuteCategoryCommandError::InvalidStoredCategory)
        })
        .transpose()
    }
    fn append_order(&self, parent_id: Option<i64>) -> Result<i64, EditCategoryError> {
        match self
            .rows
            .iter()
            .filter(|row| row.parent_id == parent_id)
            .map(|row| row.sort_order)
            .max()
        {
            Some(order) => order.checked_add(1).ok_or(EditCategoryError::OrderOverflow),
            None => Ok(0),
        }
    }
    fn validate_parent(
        &self,
        id: i64,
        mut parent: Option<i64>,
    ) -> Result<(), ExecuteCategoryCommandError> {
        let mut visited = BTreeSet::new();
        while let Some(parent_id) = parent {
            if parent_id == id {
                return Err(EditCategoryError::Cycle.into());
            }
            if !visited.insert(parent_id) {
                return Err(ExecuteCategoryCommandError::InvalidStoredCategory);
            }
            parent = self
                .rows
                .iter()
                .find(|row| row.id == Some(parent_id))
                .ok_or(ExecuteCategoryCommandError::InvalidStoredCategory)?
                .parent_id;
        }
        Ok(())
    }
    fn entity(&self, row: &CategoriesRow) -> Result<CategoryEntity, ExecuteCategoryCommandError> {
        let invalid = || ExecuteCategoryCommandError::InvalidStoredCategory;
        let builtin_key = row
            .builtin_key
            .as_deref()
            .map(|key| BuiltinCategory::from_key(key).ok_or_else(invalid))
            .transpose()?;
        let parent_uuid = row
            .parent_id
            .map(|parent| {
                self.rows
                    .iter()
                    .find(|candidate| candidate.id == Some(parent))
                    .map(|parent| CategoryUuid(parent.uuid.clone()))
                    .ok_or_else(invalid)
            })
            .transpose()?;
        let values = CategoryValues {
            parent_uuid,
            name: row
                .name
                .as_deref()
                .map(CategoryName::from_stored)
                .transpose()
                .map_err(|_| invalid())?,
            color: row.color.parse().map_err(|_| invalid())?,
            tendency: match row.tendency {
                enums::CategoryTendency::Focus => CategoryTendency::Focus,
                enums::CategoryTendency::Neutral => CategoryTendency::Neutral,
                enums::CategoryTendency::Distraction => CategoryTendency::Distraction,
            },
        };
        if values.name.is_none() && builtin_key.is_none() {
            return Err(invalid());
        }
        Ok(CategoryEntity {
            id: CategoryId(row.id.ok_or_else(invalid)?),
            uuid: CategoryUuid(row.uuid.clone()),
            builtin_key,
            values,
            sort_order: row.sort_order,
            archive_state: match row.archived {
                0 => CategoryArchiveState::Active,
                1 => CategoryArchiveState::Archived,
                _ => return Err(invalid()),
            },
        })
    }
    fn list(
        &self,
        archived: IncludeArchivedCategories,
    ) -> Result<Vec<CategoryEntity>, ExecuteCategoryCommandError> {
        let mut children: BTreeMap<Option<i64>, Vec<&CategoriesRow>> = BTreeMap::new();
        for row in &self.rows {
            children.entry(row.parent_id).or_default().push(row);
        }
        let mut pending = Vec::new();
        if let Some(roots) = children.get(&None) {
            pending.extend(roots.iter().rev().copied());
        }
        let mut visited = 0;
        let mut entities = Vec::new();
        while let Some(row) = pending.pop() {
            let entity = self.entity(row)?;
            if let Some(descendants) = children.get(&Some(entity.id.0)) {
                pending.extend(descendants.iter().rev().copied());
            }
            visited += 1;
            if archived == IncludeArchivedCategories::Include
                || entity.archive_state == CategoryArchiveState::Active
            {
                entities.push(entity);
            }
        }
        if visited != self.rows.len() {
            return Err(ExecuteCategoryCommandError::InvalidStoredCategory);
        }
        Ok(entities)
    }
}
impl From<CategoryTendency> for enums::CategoryTendency {
    fn from(value: CategoryTendency) -> Self {
        match value {
            CategoryTendency::Focus => Self::Focus,
            CategoryTendency::Neutral => Self::Neutral,
            CategoryTendency::Distraction => Self::Distraction,
        }
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
        let color = self
            .color
            .parse()
            .map_err(|_| ReadQueryError::InvalidCategories)?;
        if self.name.is_none() && builtin_key.is_none() {
            return Err(ReadQueryError::InvalidCategories);
        }
        Ok(Category {
            id: CategoryId(self.id.ok_or(ReadQueryError::InvalidCategories)?),
            uuid: CategoryUuid(self.uuid),
            builtin_key,
            name: self.name,
            color,
            tendency: match self.tendency {
                enums::CategoryTendency::Focus => CategoryTendency::Focus,
                enums::CategoryTendency::Neutral => CategoryTendency::Neutral,
                enums::CategoryTendency::Distraction => CategoryTendency::Distraction,
            },
        })
    }
}

#[cfg(test)]
mod tests;
