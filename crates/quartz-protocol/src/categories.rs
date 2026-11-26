//! Category editing bodies and metadata-versioned list responses.

use crate::{BuiltinCategory, CategoryTendency, QueryVersions, Uuid};
use serde::{Deserialize, Serialize};

/// Maximum categories returned in a single `categories.list` response.
pub const CATEGORIES_MAX_ENTITIES: usize = 500;

/// Category selection for hierarchy listing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoriesListRequest {
    #[serde(default)]
    pub include_archived: bool,
}

/// Complete editable values, with a client-supplied durable identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryEdit {
    pub uuid: Uuid,
    pub parent_uuid: Option<Uuid>,
    pub name: Option<String>,
    pub color: String,
    pub tendency: CategoryTendency,
}
/// New category values; the tendency defaults to neutral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewCategory {
    pub uuid: Uuid,
    pub parent_uuid: Option<Uuid>,
    pub name: Option<String>,
    pub color: String,
    #[serde(default = "NewCategory::neutral")]
    pub tendency: CategoryTendency,
}
impl NewCategory {
    fn neutral() -> CategoryTendency {
        CategoryTendency::Neutral
    }
}
impl From<NewCategory> for CategoryEdit {
    fn from(entity: NewCategory) -> Self {
        Self {
            uuid: entity.uuid,
            parent_uuid: entity.parent_uuid,
            name: entity.name,
            color: entity.color,
            tendency: entity.tendency,
        }
    }
}
/// Creates a category or returns the original entity on a UUID retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoriesCreateRequest {
    pub entity: NewCategory,
}
/// Replaces the complete editable values of an existing category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoriesUpdateRequest {
    pub entity: CategoryEdit,
}
/// Sets the complete sibling order, including archived siblings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoriesReorderRequest {
    pub parent_uuid: Option<Uuid>,
    pub uuids: Vec<Uuid>,
}
/// Sets one category's final archive state without changing descendants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoriesArchiveRequest {
    pub uuid: Uuid,
    pub archived: bool,
}
/// Stored category metadata for editing; database row identifiers stay private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditableCategory {
    pub uuid: Uuid,
    pub builtin_key: Option<BuiltinCategory>,
    pub parent_uuid: Option<Uuid>,
    pub name: Option<String>,
    pub color: String,
    pub tendency: CategoryTendency,
    pub sort_order: i64,
    pub archived: bool,
}
/// Stored entity returned after create, update, or archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryResponse {
    pub entity: EditableCategory,
}
/// Hierarchy preorder listing bound to the current metadata version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoriesListResponse {
    pub entities: Vec<EditableCategory>,
    pub versions: QueryVersions,
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AcceptedBody, CoreRequest, Op};

    fn snapshot<T: serde::de::DeserializeOwned + Serialize>(value: &serde_json::Value) {
        let decoded: T = serde_json::from_value(value.clone()).expect("typed category wire value");
        assert_eq!(
            serde_json::to_value(decoded).expect("category JSON"),
            *value
        );
    }

    #[test]
    fn category_wire_snapshots_cover_every_request_and_response() {
        let wire: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/categories.json"))
                .expect("wire snapshots");
        snapshot::<CategoriesCreateRequest>(&wire["create"]);
        snapshot::<CategoriesUpdateRequest>(&wire["update"]);
        snapshot::<CategoriesReorderRequest>(&wire["reorder"]);
        snapshot::<CategoriesArchiveRequest>(&wire["archive"]);
        snapshot::<CategoriesListRequest>(&wire["list"]);
        snapshot::<CategoryResponse>(&wire["entity_response"]);
        snapshot::<CategoriesListResponse>(&wire["list_response"]);
        snapshot::<AcceptedBody>(&wire["accepted"]);
        assert_eq!(CategoriesListRequest::OP, Op::CategoriesList);
        assert_eq!(CategoriesCreateRequest::OP, Op::CategoriesCreate);
        assert_eq!(CategoriesUpdateRequest::OP, Op::CategoriesUpdate);
        assert_eq!(CategoriesReorderRequest::OP, Op::CategoriesReorder);
        assert_eq!(CategoriesArchiveRequest::OP, Op::CategoriesArchive);
        assert_eq!(
            CategoriesCreateRequest::response(wire["entity_response"].clone())
                .expect("typed response")
                .entity
                .name
                .as_deref(),
            Some("研究")
        );
    }
}
