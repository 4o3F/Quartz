//! Category wire conversion and database-actor execution.

use super::dispatch::WriteResponse;
use crate::{
    query::paging::{PageBudget, PagePosition},
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
                if let Some(entity) = store
                    .list_categories(IncludeArchivedCategories::Include)
                    .map_err(Self::store_error)?
                    .into_iter()
                    .find(|entity| entity.uuid == uuid)
                {
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
                .map_err(|_| ProtocolError::new(ErrorKind::Internal))?,
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
                .map_err(|_| ProtocolError::new(ErrorKind::Internal))?,
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
                domain::EditCategoryError::NotFound
                | domain::EditCategoryError::BlankName
                | domain::EditCategoryError::NameTooLong
                | domain::EditCategoryError::MissingName
                | domain::EditCategoryError::InvalidParent
                | domain::EditCategoryError::Cycle
                | domain::EditCategoryError::BuiltinIdentityViolation
                | domain::EditCategoryError::ReorderSetMismatch
                | domain::EditCategoryError::OrderOverflow => ErrorKind::BadRequest,
            },
            ExecuteCategoryCommandError::Database { .. }
            | ExecuteCategoryCommandError::Version { .. }
            | ExecuteCategoryCommandError::InvalidStoredCategory => ErrorKind::Internal,
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
        let query = wire::CursorQuery::Categories {
            include_archived: request.include_archived,
        };
        let position = PagePosition::from_cursor(request.cursor.as_ref(), &versions, &query)
            .map_err(|error| ProtocolError::new(error.kind()))?;
        let mut response = wire::CategoriesListResponse {
            entities: Vec::new(),
            versions,
            next_cursor: None,
        };
        let budget = PageBudget::new(&response)
            .and_then(|budget| {
                budget.with_cursor(&wire::QueryCursor::new(
                    response.versions.clone(),
                    query.clone(),
                    usize::MAX,
                ))
            })
            .map_err(|error| ProtocolError::new(error.kind()))?;
        let entities = listing
            .entities
            .into_iter()
            .map(Self::entity)
            .collect::<Result<Vec<_>, _>>()?;
        let page = budget
            .page(entities, position)
            .map_err(|error| ProtocolError::new(error.kind()))?;
        response.entities = page.items;
        response.next_cursor = page
            .next
            .map(|next| wire::QueryCursor::new(response.versions.clone(), query, next));
        Ok(response)
    }
}
