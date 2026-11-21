//! Annotation write bodies and durable entity identities.

use crate::{BuiltinCategory, Uuid};
use serde::{Deserialize, Serialize};

/// Client-supplied durable identity of an annotation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnnotationId(pub Uuid);

/// Stored annotation behavior; notes retain overlapping ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    CategoryOverride,
    OfflineActivity,
    ActiveOverride,
    DismissedAway,
    Note,
}
impl AnnotationKind {
    /// Whether a write removes overlapping parts of older annotations of this kind.
    pub const fn clips(self) -> bool {
        !matches!(self, Self::Note)
    }
}

/// Category lookup accepted by annotation writes; responses use the durable UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnnotationCategory {
    Uuid { uuid: Uuid },
    Builtin { key: BuiltinCategory },
}

/// Final range and optional content; absent optional fields clear previous values on update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationValues {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    #[serde(default)]
    pub category: Option<AnnotationCategory>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}
/// One entity in an atomic create request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAnnotation {
    pub uuid: AnnotationId,
    pub kind: AnnotationKind,
    #[serde(flatten)]
    pub values: AnnotationValues,
}
/// One entity in an atomic final-value update request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAnnotation {
    pub uuid: AnnotationId,
    /// Optional assertion of the stored kind; changing it is rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AnnotationKind>,
    #[serde(flatten)]
    pub values: AnnotationValues,
}
/// Creates entities in one transaction, returning existing entities for UUID retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationsCreateRequest {
    pub entities: Vec<CreateAnnotation>,
}
/// Sets all submitted entities to their final values in one transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationsUpdateRequest {
    pub entities: Vec<UpdateAnnotation>,
}
/// Deletes these identities in one transaction; missing identities succeed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationsDeleteRequest {
    pub uuids: Vec<AnnotationId>,
}
/// Stored entity returned after a successful write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    pub uuid: AnnotationId,
    pub kind: AnnotationKind,
    #[serde(flatten)]
    pub values: AnnotationValues,
    pub created_utc_ms: i64,
    pub updated_utc_ms: i64,
}
/// Entities in request order, excluding remainders created by clipping older rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationsResponse {
    pub entities: Vec<Annotation>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_write_bodies_round_trip_and_canonicalize_uuid_identity() {
        let wire = serde_json::json!({"entities":[{"uuid":"ABCDEF00-0000-4000-8000-000000000001","kind":"offline_activity","start_utc_ms":-1000,"end_utc_ms":1000,"category":{"kind":"builtin","key":"communication"},"label":"会议","note":null}]});
        let request: AnnotationsCreateRequest = serde_json::from_value(wire).expect("create body");
        assert_eq!(
            request.entities[0].uuid.0.to_string(),
            "abcdef00-0000-4000-8000-000000000001"
        );
        assert_eq!(
            request,
            serde_json::from_value(serde_json::to_value(&request).expect("encode"))
                .expect("round trip")
        );
        let update: AnnotationsUpdateRequest = serde_json::from_value(serde_json::json!({"entities":[{"uuid":"abcdef00-0000-4000-8000-000000000001","start_utc_ms":0,"end_utc_ms":1}]})).expect("update without kind");
        assert_eq!(update.entities[0].kind, None);
        assert_eq!(update.entities[0].values.category, None);
        assert_eq!(update.entities[0].values.label, None);
        assert_eq!(update.entities[0].values.note, None);
        let delete: AnnotationsDeleteRequest = serde_json::from_value(
            serde_json::json!({"uuids":["abcdef00-0000-4000-8000-000000000001"]}),
        )
        .expect("delete body");
        assert_eq!(delete.uuids[0], request.entities[0].uuid);
        for invalid in [
            serde_json::json!({"uuids":["invalid"]}),
            serde_json::json!({"uuids":[null]}),
        ] {
            assert!(serde_json::from_value::<AnnotationsDeleteRequest>(invalid).is_err());
        }
    }
}
