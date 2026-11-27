//! Category wire conversion and database-actor execution.

use super::{diagnostics::WriteFailure, dispatch::WriteResponse};
use crate::{
    query::{QUERY_MAX_RESPONSE_BYTES, fit_single_list},
    store::{Store, categories::ExecuteCategoryCommandError},
};
use quartz_domain::categories::{self as domain, CategoryArchiveState, IncludeArchivedCategories};
use quartz_protocol::{self as wire, ErrorKind, ProtocolError};

/// Category transaction or snapshot selected by the IPC operation.
#[derive(Debug)]
pub(crate) enum CategoryRequest {
    List(wire::CategoriesListRequest),
    Create(wire::CategoriesCreateRequest),
    Update(wire::CategoriesUpdateRequest),
    Reorder(wire::CategoriesReorderRequest),
    Archive(wire::CategoriesArchiveRequest),
}
impl CategoryRequest {
    pub(crate) fn execute(self, store: &mut Store) -> Result<WriteResponse, ProtocolError> {
        let entity = match self {
            Self::List(request) => {
                return Self::list(store, request).map(WriteResponse::Categories);
            }
            Self::Create(request) => {
                let uuid = domain::CategoryUuid(request.entity.uuid.to_string());
                // Preserve UUID retry semantics before validating replacement values.
                if let Some(entity) = store.category(&uuid).map_err(Self::store_error)? {
                    return Self::response(entity);
                }
                let values = Self::values(request.entity.into())?;
                store.create_category(domain::CreateCategory { uuid, values })
            }
            Self::Update(request) => {
                let uuid = domain::CategoryUuid(request.entity.uuid.to_string());
                let values = Self::values(request.entity)?;
                store.update_category(domain::UpdateCategory { uuid, values })
            }
            Self::Reorder(request) => {
                store
                    .reorder_categories(domain::ReorderCategories {
                        parent_uuid: request
                            .parent_uuid
                            .map(|uuid| domain::CategoryUuid(uuid.to_string())),
                        uuids: request
                            .uuids
                            .into_iter()
                            .map(|uuid| domain::CategoryUuid(uuid.to_string()))
                            .collect(),
                    })
                    .map_err(Self::store_error)?;
                return Ok(WriteResponse::Accepted(wire::AcceptedBody));
            }
            Self::Archive(request) => store.archive_category(domain::ArchiveCategory {
                uuid: domain::CategoryUuid(request.uuid.to_string()),
                state: if request.archived {
                    CategoryArchiveState::Archived
                } else {
                    CategoryArchiveState::Active
                },
            }),
        }
        .map_err(Self::store_error)?;
        Self::response(entity)
    }
    fn values(entity: wire::CategoryEdit) -> Result<domain::CategoryValues, ProtocolError> {
        Ok(domain::CategoryValues {
            parent_uuid: entity
                .parent_uuid
                .map(|uuid| domain::CategoryUuid(uuid.to_string())),
            name: entity
                .name
                .map(|name| name.parse())
                .transpose()
                .map_err(|source| {
                    Self::store_error(ExecuteCategoryCommandError::Validation { source })
                })?,
            color: entity
                .color
                .parse()
                .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?,
            tendency: match entity.tendency {
                wire::CategoryTendency::Focus => domain::CategoryTendency::Focus,
                wire::CategoryTendency::Neutral => domain::CategoryTendency::Neutral,
                wire::CategoryTendency::Distraction => domain::CategoryTendency::Distraction,
            },
        })
    }
    fn entity(entity: domain::CategoryEntity) -> Result<wire::EditableCategory, ProtocolError> {
        Ok(wire::EditableCategory {
            uuid: entity
                .uuid
                .0
                .parse()
                .map_err(|_| WriteFailure::Identity.into_protocol_error())?,
            builtin_key: entity.builtin_key.map(|builtin| match builtin {
                domain::BuiltinCategory::Development => wire::BuiltinCategory::Development,
                domain::BuiltinCategory::Learning => wire::BuiltinCategory::Learning,
                domain::BuiltinCategory::Communication => wire::BuiltinCategory::Communication,
                domain::BuiltinCategory::Entertainment => wire::BuiltinCategory::Entertainment,
                domain::BuiltinCategory::System => wire::BuiltinCategory::System,
            }),
            parent_uuid: entity
                .values
                .parent_uuid
                .map(|uuid| uuid.0.parse())
                .transpose()
                .map_err(|_| WriteFailure::Identity.into_protocol_error())?,
            name: entity.values.name.map(String::from),
            color: entity.values.color.to_string(),
            tendency: match entity.values.tendency {
                domain::CategoryTendency::Focus => wire::CategoryTendency::Focus,
                domain::CategoryTendency::Neutral => wire::CategoryTendency::Neutral,
                domain::CategoryTendency::Distraction => wire::CategoryTendency::Distraction,
            },
            sort_order: entity.sort_order,
            archived: entity.archive_state == CategoryArchiveState::Archived,
        })
    }
    fn response(entity: domain::CategoryEntity) -> Result<WriteResponse, ProtocolError> {
        Self::entity(entity)
            .map(|entity| WriteResponse::Category(wire::CategoryResponse { entity }))
    }
    fn store_error(error: ExecuteCategoryCommandError) -> ProtocolError {
        let kind = match error {
            ExecuteCategoryCommandError::Validation { source } => match source {
                domain::EditCategoryError::NotFound => ErrorKind::CategoryNotFound,
                domain::EditCategoryError::BuiltinIdentityViolation => ErrorKind::Forbidden,
                domain::EditCategoryError::OrderOverflow => ErrorKind::VersionExhausted,
                domain::EditCategoryError::BlankName
                | domain::EditCategoryError::NameTooLong
                | domain::EditCategoryError::ReservedName
                | domain::EditCategoryError::MissingName
                | domain::EditCategoryError::InvalidParent
                | domain::EditCategoryError::Cycle
                | domain::EditCategoryError::ReorderSetMismatch => ErrorKind::BadRequest,
            },
            ExecuteCategoryCommandError::Database { source } => {
                return WriteFailure::database(&source).into_protocol_error();
            }
            ExecuteCategoryCommandError::Version { source } => {
                return WriteFailure::version(&source).into_protocol_error();
            }
            ExecuteCategoryCommandError::InvalidStoredCategory => {
                return WriteFailure::StoredCategory.into_protocol_error();
            }
        };
        ProtocolError::new(kind)
    }
    fn list(
        store: &mut Store,
        request: wire::CategoriesListRequest,
    ) -> Result<wire::CategoriesListResponse, ProtocolError> {
        let selection = if request.include_archived {
            IncludeArchivedCategories::Include
        } else {
            IncludeArchivedCategories::Exclude
        };
        let listing = store
            .category_listing(selection)
            .map_err(Self::store_error)?;
        let versions = wire::QueryVersions(format!("categories.v1:{}", listing.metadata_version));
        let truncated = listing.entities.len() > wire::CATEGORIES_MAX_ENTITIES;
        let entities = listing
            .entities
            .into_iter()
            .take(wire::CATEGORIES_MAX_ENTITIES)
            .map(Self::entity)
            .collect::<Result<Vec<_>, _>>()?;
        let response = wire::CategoriesListResponse {
            entities,
            versions,
            truncated,
        };
        fit_single_list(
            response,
            QUERY_MAX_RESPONSE_BYTES,
            |r| &mut r.entities,
            |r| r.truncated = true,
        )
        .map_err(|_| ProtocolError::new(ErrorKind::Internal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_directory::DataDirectory;

    #[test]
    fn create_and_rename_reject_reserved_names_as_bad_request() {
        let directory = tempfile::tempdir().expect("isolated database");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("builtins");
        for name in [
            "builtin:development",
            "builtin:future",
            "71f5a800-423a-4000-8000-000000000001",
        ] {
            let create = CategoryRequest::Create(wire::CategoriesCreateRequest {
                entity: wire::NewCategory {
                    uuid: "00000000-0000-4000-8000-000000000001"
                        .parse()
                        .expect("UUID"),
                    parent_uuid: None,
                    name: Some(name.into()),
                    color: "neutral".into(),
                    tendency: wire::CategoryTendency::Neutral,
                },
            });
            let rename = CategoryRequest::Update(wire::CategoriesUpdateRequest {
                entity: wire::CategoryEdit {
                    uuid: domain::BuiltinCategory::Development
                        .uuid()
                        .0
                        .parse()
                        .expect("UUID"),
                    parent_uuid: None,
                    name: Some(name.into()),
                    color: "neutral".into(),
                    tendency: wire::CategoryTendency::Neutral,
                },
            });
            for command in [create, rename] {
                let before = store
                    .category_listing(IncludeArchivedCategories::Include)
                    .expect("before");
                let error = command.execute(&mut store).expect_err("reserved name");
                assert_eq!(error.kind, ErrorKind::BadRequest);
                let after = store
                    .category_listing(IncludeArchivedCategories::Include)
                    .expect("after");
                assert_eq!(after.entities, before.entities);
                assert_eq!(after.metadata_version, before.metadata_version);
            }
        }
    }

    #[test]
    fn legacy_reserved_names_remain_readable_and_can_be_renamed() {
        use diesel::{Connection, RunQueryDsl};
        let directory = tempfile::tempdir().expect("isolated database");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("builtins");
        let uuid = domain::BuiltinCategory::Learning.uuid();
        let mut connection = diesel::SqliteConnection::establish(
            directory.path().join("quartz.db").to_str().expect("path"),
        )
        .expect("connection");
        diesel::sql_query("UPDATE categories SET name='builtin:development' WHERE uuid=?")
            .bind::<diesel::sql_types::Text, _>(&uuid.0)
            .execute(&mut connection)
            .expect("legacy name");
        let listed = store
            .list_categories(IncludeArchivedCategories::Include)
            .expect("legacy names can be read");
        assert_eq!(
            listed
                .iter()
                .find(|entity| entity.uuid == uuid)
                .expect("learning")
                .values
                .name
                .as_ref()
                .expect("legacy name")
                .as_str(),
            "builtin:development"
        );
        CategoryRequest::Update(wire::CategoriesUpdateRequest {
            entity: wire::CategoryEdit {
                uuid: uuid.0.parse().expect("UUID"),
                parent_uuid: None,
                name: Some("Renamed".into()),
                color: "neutral".into(),
                tendency: wire::CategoryTendency::Neutral,
            },
        })
        .execute(&mut store)
        .expect("rename legacy name");
        let listed = store
            .list_categories(IncludeArchivedCategories::Include)
            .expect("renamed categories");
        assert_eq!(
            listed
                .iter()
                .find(|entity| entity.uuid == uuid)
                .expect("learning")
                .values
                .name
                .as_ref()
                .expect("name")
                .as_str(),
            "Renamed"
        );
    }

    #[test]
    fn store_error_maps_not_found_forbidden_and_order_overflow_specifically() {
        let map = |source: domain::EditCategoryError| {
            CategoryRequest::store_error(ExecuteCategoryCommandError::Validation { source }).kind
        };
        assert_eq!(
            map(domain::EditCategoryError::NotFound),
            ErrorKind::CategoryNotFound
        );
        assert_eq!(
            map(domain::EditCategoryError::BuiltinIdentityViolation),
            ErrorKind::Forbidden
        );
        assert_eq!(
            map(domain::EditCategoryError::OrderOverflow),
            ErrorKind::VersionExhausted
        );
        assert_eq!(
            map(domain::EditCategoryError::BlankName),
            ErrorKind::BadRequest
        );
        assert_eq!(
            map(domain::EditCategoryError::ReorderSetMismatch),
            ErrorKind::BadRequest
        );
    }
}
