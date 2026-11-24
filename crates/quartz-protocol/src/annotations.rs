//! Annotation writes and saved editing values with paged UTF-8 text.

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

/// Built-in annotation label localized by the displaying client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationLabelPreset {
    Rest,
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
    pub label_preset: Option<AnnotationLabelPreset>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}
/// One entity in an atomic create request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AnnotationRequestWire<AnnotationKind>")]
pub struct CreateAnnotation {
    pub uuid: AnnotationId,
    pub kind: AnnotationKind,
    #[serde(flatten)]
    pub values: AnnotationValues,
}
/// One entity in an atomic final-value update request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AnnotationRequestWire<Option<AnnotationKind>>")]
pub struct UpdateAnnotation {
    pub uuid: AnnotationId,
    /// Optional assertion of the stored kind; changing it is rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AnnotationKind>,
    #[serde(flatten)]
    pub values: AnnotationValues,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnnotationRequestWire<Kind> {
    uuid: AnnotationId,
    kind: Kind,
    start_utc_ms: i64,
    end_utc_ms: i64,
    category: Option<RequestAnnotationCategory>,
    label_preset: Option<AnnotationLabelPreset>,
    label: Option<String>,
    note: Option<String>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RequestAnnotationCategory {
    Uuid { uuid: Uuid },
    Builtin { key: BuiltinCategory },
}
impl<Kind> AnnotationRequestWire<Kind> {
    fn take_values(&mut self) -> AnnotationValues {
        AnnotationValues {
            start_utc_ms: self.start_utc_ms,
            end_utc_ms: self.end_utc_ms,
            category: self.category.take().map(|category| match category {
                RequestAnnotationCategory::Uuid { uuid } => AnnotationCategory::Uuid { uuid },
                RequestAnnotationCategory::Builtin { key } => AnnotationCategory::Builtin { key },
            }),
            label_preset: self.label_preset.take(),
            label: self.label.take(),
            note: self.note.take(),
        }
    }
}
impl From<AnnotationRequestWire<AnnotationKind>> for CreateAnnotation {
    fn from(mut wire: AnnotationRequestWire<AnnotationKind>) -> Self {
        let values = wire.take_values();
        Self {
            uuid: wire.uuid,
            kind: wire.kind,
            values,
        }
    }
}
impl From<AnnotationRequestWire<Option<AnnotationKind>>> for UpdateAnnotation {
    fn from(mut wire: AnnotationRequestWire<Option<AnnotationKind>>) -> Self {
        let values = wire.take_values();
        Self {
            uuid: wire.uuid,
            kind: wire.kind,
            values,
        }
    }
}
/// Creates entities in one transaction, returning existing entities for UUID retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationsCreateRequest {
    pub entities: Vec<CreateAnnotation>,
}
/// Sets all submitted entities to their final values in one transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationsUpdateRequest {
    pub entities: Vec<UpdateAnnotation>,
}
/// Deletes these identities in one transaction; missing identities succeed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// Saved entities intersecting a positive UTC range of at most 366 days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryAnnotationsRequest {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<crate::QueryCursor>,
}
impl From<&QueryAnnotationsRequest> for crate::CursorQuery {
    fn from(request: &QueryAnnotationsRequest) -> Self {
        Self::Annotations {
            start_utc_ms: request.start_utc_ms,
            end_utc_ms: request.end_utc_ms,
        }
    }
}

/// A complete text value or one fragment positioned by its UTF-8 byte offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnnotationText {
    Complete(String),
    Chunk(AnnotationTextChunk),
}

/// Text fragment; `complete` marks the end of this field's stored value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationTextChunk {
    pub text: String,
    pub offset_utf8: usize,
    pub complete: bool,
}

/// Stored endpoints and final editing values, repeated unchanged around text fragments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAnnotation {
    pub uuid: AnnotationId,
    pub kind: AnnotationKind,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub category: Option<AnnotationCategory>,
    #[serde(default)]
    pub label_preset: Option<AnnotationLabelPreset>,
    pub label: Option<AnnotationText>,
    pub note: Option<AnnotationText>,
    pub created_utc_ms: i64,
    pub updated_utc_ms: i64,
}

/// Saved annotations ordered by start UTC, end UTC, then UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryAnnotationsResponse {
    pub entities: Vec<SavedAnnotation>,
    pub versions: crate::QueryVersions,
    pub next_cursor: Option<crate::QueryCursor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_preset_round_trips_without_localized_text() {
        let wire = serde_json::json!({"uuid":"abcdef00-0000-4000-8000-000000000001","kind":"note","start_utc_ms":0,"end_utc_ms":1,"label_preset":"rest"});
        let entity: CreateAnnotation = serde_json::from_value(wire).expect("preset request");
        let encoded = serde_json::to_value(&entity).expect("encode preset");
        assert_eq!(encoded["label_preset"], "rest");
        assert_eq!(encoded["label"], serde_json::Value::Null);
    }

    #[test]
    fn label_preset_requests_reject_unknown_fields_and_codes() {
        let wire = serde_json::json!({"uuid":"abcdef00-0000-4000-8000-000000000001","kind":"note","start_utc_ms":0,"end_utc_ms":1,"label_preset":"rest"});
        for field in ["label_preset", "unexpected"] {
            let mut invalid = wire.clone();
            invalid[field] = serde_json::json!("unknown");
            assert!(serde_json::from_value::<CreateAnnotation>(invalid.clone()).is_err());
            assert!(serde_json::from_value::<UpdateAnnotation>(invalid).is_err());
        }
    }

    #[test]
    fn label_preset_defaults_to_null_and_round_trips_in_write_responses() {
        let wire = serde_json::json!({"uuid":"abcdef00-0000-4000-8000-000000000001","kind":"note","start_utc_ms":0,"end_utc_ms":1,"label_preset":"rest"});
        let create: CreateAnnotation = serde_json::from_value(wire.clone()).expect("create");
        let update: UpdateAnnotation = serde_json::from_value(wire.clone()).expect("update");
        assert_eq!(
            create.values.label_preset,
            Some(AnnotationLabelPreset::Rest)
        );
        assert_eq!(update.values, create.values);
        let response = AnnotationsResponse {
            entities: vec![Annotation {
                uuid: create.uuid,
                kind: create.kind,
                values: create.values,
                created_utc_ms: 10,
                updated_utc_ms: 20,
            }],
        };
        let encoded = serde_json::to_value(&response).expect("encode response");
        assert_eq!(encoded["entities"][0]["label_preset"], "rest");
        assert_eq!(
            serde_json::from_value::<AnnotationsResponse>(encoded).expect("response round trip"),
            response
        );
        for value in [None, Some(serde_json::Value::Null)] {
            let mut body = wire.clone();
            body.as_object_mut().expect("entity").remove("label_preset");
            if let Some(value) = value {
                body["label_preset"] = value;
            }
            let create: CreateAnnotation =
                serde_json::from_value(body.clone()).expect("default create");
            let update: UpdateAnnotation = serde_json::from_value(body).expect("default update");
            assert_eq!(create.values.label_preset, None);
            assert_eq!(update.values.label_preset, None);
            assert_eq!(
                serde_json::to_value(create).expect("encode null")["label_preset"],
                serde_json::Value::Null
            );
        }
        let mut unknown = serde_json::json!({"entities":[],"future":true});
        assert!(serde_json::from_value::<AnnotationsCreateRequest>(unknown.clone()).is_err());
        assert!(serde_json::from_value::<AnnotationsUpdateRequest>(unknown.clone()).is_err());
        unknown.as_object_mut().expect("body").remove("entities");
        unknown["uuids"] = serde_json::json!([]);
        assert!(serde_json::from_value::<AnnotationsDeleteRequest>(unknown).is_err());
    }

    #[test]
    fn annotation_responses_default_omitted_label_presets_to_null() {
        let wire: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/annotations-normal.json"))
                .expect("normal page");
        let mut entity = wire["entities"][0].clone();
        entity
            .as_object_mut()
            .expect("annotation object")
            .remove("label_preset");
        let saved: SavedAnnotation =
            serde_json::from_value(entity.clone()).expect("saved annotation without preset");
        assert_eq!(saved.label_preset, None);
        let response: AnnotationsResponse =
            serde_json::from_value(serde_json::json!({"entities": [entity]}))
                .expect("write response without preset");
        assert_eq!(response.entities[0].values.label_preset, None);
        assert_eq!(
            serde_json::to_value(saved).expect("encode saved annotation")["label_preset"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn saved_annotation_wire_snapshots_and_continuations_round_trip() {
        use crate::{CoreRequest, CursorQuery, Op};
        let request_wire: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/annotations-request.json"))
                .expect("request snapshot");
        let request: QueryAnnotationsRequest =
            serde_json::from_value(request_wire.clone()).expect("request");
        assert_eq!(request.body().expect("request body"), request_wire);
        assert_eq!(QueryAnnotationsRequest::OP, Op::QueryAnnotations);
        assert_eq!(
            serde_json::to_value(crate::Capability::QueryAnnotationsV1).expect("capability"),
            "query.annotations.v1"
        );
        for fixture in [
            include_str!("../tests/fixtures/annotations-normal.json"),
            include_str!("../tests/fixtures/annotations-chunked.json"),
            include_str!("../tests/fixtures/annotations-continuation.json"),
        ] {
            let wire: serde_json::Value = serde_json::from_str(fixture).expect("snapshot");
            let response = QueryAnnotationsRequest::response(wire.clone()).expect("typed page");
            assert_eq!(serde_json::to_value(&response).expect("encode page"), wire);
            if let Some(cursor) = &response.next_cursor {
                assert_eq!(
                    cursor.decode().expect("issued cursor").query(),
                    &CursorQuery::from(&request)
                );
            }
        }
    }

    #[test]
    fn saved_annotation_schemas_ignore_additive_fields_and_reject_malformed_values() {
        for invalid in [
            serde_json::json!({"start_utc_ms":0,"end_utc_ms":1,"unexpected":true}),
            serde_json::json!({"start_utc_ms":0}),
            serde_json::json!({"start_utc_ms":0.5,"end_utc_ms":1}),
        ] {
            assert!(serde_json::from_value::<QueryAnnotationsRequest>(invalid).is_err());
        }
        for invalid in [
            serde_json::json!({"text":"x","offset_utf8":-1,"complete":true}),
            serde_json::json!({"text":"x","offset_utf8":0.5,"complete":true}),
            serde_json::json!({"text":"x","offset_utf8":0}),
            serde_json::json!({"text":"x","offset_utf8":0,"complete":"true"}),
            serde_json::json!(17),
        ] {
            assert!(serde_json::from_value::<AnnotationText>(invalid).is_err());
        }
        let normal: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/annotations-normal.json"))
                .expect("normal page");
        let mut unknown_page = normal.clone();
        unknown_page["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<QueryAnnotationsResponse>(unknown_page).is_ok());
        let mut unknown_entity = normal.clone();
        unknown_entity["entities"][0]["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<QueryAnnotationsResponse>(unknown_entity).is_ok());
        let mut unknown_category = normal.clone();
        unknown_category["entities"][0]["category"]["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<QueryAnnotationsResponse>(unknown_category).is_ok());
        for invalid in ["unknown", "NOTE"] {
            let mut page = normal.clone();
            page["entities"][0]["kind"] = serde_json::json!(invalid);
            assert!(serde_json::from_value::<QueryAnnotationsResponse>(page).is_err());
        }
    }

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
        assert_eq!(update.entities[0].values.label_preset, None);
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
    #[test]
    fn annotation_category_rejects_unknown_request_fields_but_accepts_response_fields() {
        let category = serde_json::json!({"kind":"builtin","key":"communication","future":true});
        assert!(serde_json::from_value::<AnnotationCategory>(category.clone()).is_ok());
        let entity = serde_json::json!({"uuid":"abcdef00-0000-4000-8000-000000000001","kind":"note","start_utc_ms":0,"end_utc_ms":1,"category":category});
        assert!(serde_json::from_value::<CreateAnnotation>(entity.clone()).is_err());
        assert!(serde_json::from_value::<UpdateAnnotation>(entity).is_err());
    }
}
