use super::*;
use crate::{
    query::{QueryRequest, QueryResponse},
    store::{annotations::ExecuteAnnotationWritesError, query::test_support::QueryFixture},
};
use quartz_domain::capture::UtcInstant;
use quartz_protocol::{
    ANNOTATION_MAX_LABEL_SCALARS, ANNOTATION_MAX_NOTE_SCALARS, Annotation, AnnotationCategory,
    AnnotationId, AnnotationKind, AnnotationValues, AnnotationsCreateRequest,
    AnnotationsDeleteRequest, AnnotationsUpdateRequest, BuiltinCategory, CreateAnnotation,
    MAX_FRAME_LEN, QueryAnnotationsRequest, UpdateAnnotation,
};

struct EditingFixture {
    query: QueryFixture,
}
impl EditingFixture {
    fn new() -> Self {
        Self {
            query: QueryFixture::new(0),
        }
    }
    fn entity(&self, id: u32, kind: AnnotationKind, range: QueryRange) -> CreateAnnotation {
        CreateAnnotation {
            uuid: AnnotationId(
                format!("abcdef00-0000-4000-8000-{id:012x}")
                    .parse()
                    .expect("UUID"),
            ),
            kind,
            values: AnnotationValues {
                start_utc_ms: self.query.start + range.start,
                end_utc_ms: self.query.start + range.end,
                category: None,
                label_preset: None,
                label: None,
                note: None,
            },
        }
    }
    fn write(&mut self, entities: Vec<CreateAnnotation>, now: i64) -> Vec<Annotation> {
        self.query
            .store
            .create_annotations(
                AnnotationsCreateRequest { entities },
                UtcInstant::try_from(now).expect("UTC"),
            )
            .expect("create annotations")
            .entities
    }
    fn request(&self, range: QueryRange) -> QueryAnnotationsRequest {
        QueryAnnotationsRequest {
            start_utc_ms: self.query.start + range.start,
            end_utc_ms: self.query.start + range.end,
        }
    }
    fn read(&mut self, request: QueryAnnotationsRequest) -> QueryAnnotationsResponse {
        let QueryResponse::Annotations(response) = self
            .query
            .run(QueryRequest::Annotations(request))
            .expect("read annotations")
        else {
            panic!("annotation response")
        };
        assert!(
            serde_json::to_vec(&response)
                .expect("encoded response")
                .len()
                <= MAX_FRAME_LEN
        );
        response
    }
    fn assert_final_values(&self, actual: &SavedAnnotation, expected: &Annotation) {
        assert_eq!(actual.uuid, expected.uuid);
        assert_eq!(actual.kind, expected.kind);
        assert_eq!(actual.start_utc_ms, expected.values.start_utc_ms);
        assert_eq!(actual.end_utc_ms, expected.values.end_utc_ms);
        assert_eq!(actual.category, expected.values.category);
        assert_eq!(actual.label_preset, expected.values.label_preset);
        assert_eq!(actual.label, expected.values.label);
        assert_eq!(actual.note, expected.values.note);
        assert_eq!(actual.created_utc_ms, expected.created_utc_ms);
        assert_eq!(actual.updated_utc_ms, expected.updated_utc_ms);
    }
}

#[test]
fn overlapping_notes_dismissals_and_archived_categories_keep_exact_editing_values() {
    let mut fixture = EditingFixture::new();
    let mut note = fixture.entity(
        3,
        AnnotationKind::Note,
        QueryRange {
            start: -100,
            end: 200,
        },
    );
    note.values.label = Some(String::new());
    note.values.note = Some("  备注🦀\n保持原值  ".into());
    note.values.category = Some(AnnotationCategory::Builtin {
        key: BuiltinCategory::Learning,
    });
    let mut same_range = note.clone();
    same_range.uuid = fixture
        .entity(2, AnnotationKind::Note, QueryRange { start: 0, end: 1 })
        .uuid;
    same_range.values.label = None;
    same_range.values.note = Some("第二条".into());
    let mut earlier_end = same_range.clone();
    earlier_end.uuid = fixture
        .entity(4, AnnotationKind::Note, QueryRange { start: 0, end: 1 })
        .uuid;
    earlier_end.values.end_utc_ms -= 1;
    let dismissal = fixture.entity(
        5,
        AnnotationKind::DismissedAway,
        QueryRange { start: 0, end: 100 },
    );
    let mut outside_left = same_range.clone();
    outside_left.uuid = fixture
        .entity(6, AnnotationKind::Note, QueryRange { start: 0, end: 1 })
        .uuid;
    outside_left.values.end_utc_ms = fixture.query.start;
    let mut outside_right = same_range.clone();
    outside_right.uuid = fixture
        .entity(7, AnnotationKind::Note, QueryRange { start: 0, end: 1 })
        .uuid;
    outside_right.values.start_utc_ms = fixture.query.start + 100;
    let written = fixture.write(
        vec![
            note,
            same_range,
            earlier_end,
            dismissal,
            outside_left,
            outside_right,
        ],
        10,
    );
    let Some(AnnotationCategory::Uuid { uuid }) = &written[0].values.category else {
        panic!("normalized category UUID")
    };
    fixture
        .query
        .store
        .archive_category(quartz_domain::categories::ArchiveCategory {
            uuid: quartz_domain::categories::CategoryUuid(uuid.to_string()),
            state: quartz_domain::categories::CategoryArchiveState::Archived,
        })
        .expect("archive referenced category");
    let response = fixture.read(fixture.request(QueryRange { start: 0, end: 100 }));
    assert_eq!(
        response.entities.len(),
        4,
        "no foreground activity is needed to read saved annotations"
    );
    let mut expected_order = [2, 1, 0, 3].into_iter();
    for actual in &response.entities {
        let index = expected_order.next().expect("expected ordered entity");
        fixture.assert_final_values(actual, &written[index]);
    }
    assert!(!response.truncated);
}

#[test]
fn split_remainders_keep_endpoints_content_and_the_right_hand_creation_time() {
    let mut fixture = EditingFixture::new();
    let mut original = fixture.entity(
        1,
        AnnotationKind::ActiveOverride,
        QueryRange { start: 0, end: 100 },
    );
    original.values.label = Some("原始标签".into());
    original.values.note = Some(String::new());
    let original = fixture.write(vec![original], 10).remove(0);
    let replacement = fixture.entity(
        2,
        AnnotationKind::ActiveOverride,
        QueryRange { start: 40, end: 60 },
    );
    let replacement = fixture.write(vec![replacement], 20).remove(0);
    let response = fixture.read(fixture.request(QueryRange { start: 20, end: 80 }));
    assert_eq!(response.entities.len(), 3);
    let left = &response.entities[0];
    let right = &response.entities[2];
    assert_eq!(left.uuid, original.uuid);
    assert_eq!(left.start_utc_ms, fixture.query.start);
    assert_eq!(left.end_utc_ms, fixture.query.start + 40);
    assert_eq!(left.created_utc_ms, 10);
    assert_eq!(left.updated_utc_ms, 20);
    assert_ne!(right.uuid, original.uuid);
    assert_ne!(right.uuid, replacement.uuid);
    assert_eq!(right.start_utc_ms, fixture.query.start + 60);
    assert_eq!(right.end_utc_ms, fixture.query.start + 100);
    assert_eq!(right.created_utc_ms, 20);
    assert_eq!(right.updated_utc_ms, 20);
    assert_eq!(right.label, left.label);
    assert_eq!(right.note.as_deref(), Some(""));
    fixture.assert_final_values(&response.entities[1], &replacement);
    let updated = fixture
        .query
        .store
        .update_annotations(
            AnnotationsUpdateRequest {
                entities: vec![UpdateAnnotation {
                    uuid: left.uuid.clone(),
                    kind: None,
                    values: AnnotationValues {
                        start_utc_ms: left.start_utc_ms,
                        end_utc_ms: left.end_utc_ms,
                        category: None,
                        label_preset: None,
                        label: None,
                        note: None,
                    },
                }],
            },
            UtcInstant::try_from(30).expect("UTC"),
        )
        .expect("clear final values")
        .entities
        .remove(0);
    let reloaded = fixture.read(fixture.request(QueryRange { start: 20, end: 80 }));
    fixture.assert_final_values(&reloaded.entities[0], &updated);
    assert_eq!(reloaded.entities[0].created_utc_ms, 10);
    assert_eq!(reloaded.entities[0].updated_utc_ms, 30);
}

#[test]
fn snapshot_retains_old_values_and_writes_change_versions() {
    let mut fixture = EditingFixture::new();
    let mut entity = fixture.entity(
        1,
        AnnotationKind::Note,
        QueryRange {
            start: -100,
            end: 200,
        },
    );
    entity.values.note = Some("原始备注".into());
    let stored = fixture.write(vec![entity], 10).remove(0);
    let request = fixture.request(QueryRange { start: 0, end: 100 });
    let first = fixture.read(request.clone());
    let mut job = fixture
        .query
        .start(QueryRequest::Annotations(request.clone()));
    fixture
        .query
        .store
        .update_annotations(
            AnnotationsUpdateRequest {
                entities: vec![UpdateAnnotation {
                    uuid: stored.uuid.clone(),
                    kind: None,
                    values: AnnotationValues {
                        note: Some("已更新".into()),
                        ..stored.values.clone()
                    },
                }],
            },
            UtcInstant::try_from(20).expect("UTC"),
        )
        .expect("update text only");
    let old = loop {
        if let Some(response) = job.advance().expect("snapshot turn") {
            break response;
        }
    };
    fixture
        .query
        .store
        .return_query_reader(job.finish().expect("release snapshot"));
    let QueryResponse::Annotations(old) = old else {
        panic!("annotation response")
    };
    assert_eq!(old.versions, first.versions);
    assert!(old.entities.iter().all(|row| row.updated_utc_ms == 10));
    let updated = fixture.read(request.clone());
    assert_ne!(updated.versions, first.versions);
    assert_eq!(updated.entities[0].note.as_deref(), Some("已更新"));
    fixture
        .query
        .store
        .delete_annotations(AnnotationsDeleteRequest {
            uuids: vec![stored.uuid],
        })
        .expect("delete");
    let deleted = fixture.read(request);
    assert!(deleted.entities.is_empty());
    assert_ne!(deleted.versions, updated.versions);
}

#[test]
fn annotation_range_requires_valid_endpoints_and_at_most_366_utc_days() {
    use quartz_domain::capture::limits::UTC_DAY_MS;
    let mut fixture = EditingFixture::new();
    let max = 366 * UTC_DAY_MS;
    for range in [
        QueryRange { start: 0, end: 0 },
        QueryRange { start: 1, end: 0 },
        QueryRange {
            start: 0,
            end: max + 1,
        },
        QueryRange {
            start: i64::MIN,
            end: i64::MAX,
        },
        QueryRange {
            start: i64::MAX - 1,
            end: i64::MAX,
        },
    ] {
        let result = fixture
            .query
            .run(QueryRequest::Annotations(QueryAnnotationsRequest {
                start_utc_ms: range.start,
                end_utc_ms: range.end,
            }));
        assert_eq!(
            result.expect_err("invalid range").kind(),
            quartz_protocol::ErrorKind::InvalidRange
        );
    }
    for range in [
        QueryRange { start: -1, end: 0 },
        QueryRange { start: 0, end: 1 },
        QueryRange { start: 0, end: max },
    ] {
        assert!(
            fixture
                .read(QueryAnnotationsRequest {
                    start_utc_ms: range.start,
                    end_utc_ms: range.end,
                })
                .entities
                .is_empty()
        );
    }
}

#[test]
fn label_preset_is_returned_as_metadata_with_full_note_text() {
    let mut fixture = EditingFixture::new();
    let mut rest = fixture.entity(1, AnnotationKind::Note, QueryRange { start: 0, end: 100 });
    rest.values.label_preset = Some(quartz_protocol::AnnotationLabelPreset::Rest);
    rest.values.note = Some("🦀".repeat(ANNOTATION_MAX_NOTE_SCALARS));
    let created = fixture.write(vec![rest], 10);
    let response = fixture.read(fixture.request(QueryRange { start: 0, end: 100 }));
    assert_eq!(response.entities.len(), 1);
    assert!(!response.truncated);
    fixture.assert_final_values(&response.entities[0], &created[0]);
    let wire = serde_json::to_value(&response.entities[0]).expect("annotation wire");
    assert_eq!(wire["label_preset"], "rest");
}

#[test]
fn write_limits_reject_oversized_text_and_read_truncates_at_frame_limit() {
    let mut fixture = EditingFixture::new();
    let mut oversized_label =
        fixture.entity(1, AnnotationKind::Note, QueryRange { start: 0, end: 100 });
    oversized_label.values.label = Some("a".repeat(ANNOTATION_MAX_LABEL_SCALARS + 1));
    assert!(matches!(
        fixture.query.store.create_annotations(
            AnnotationsCreateRequest {
                entities: vec![oversized_label],
            },
            UtcInstant::try_from(10).expect("UTC"),
        ),
        Err(ExecuteAnnotationWritesError::InvalidFields)
    ));
    let mut oversized_note =
        fixture.entity(2, AnnotationKind::Note, QueryRange { start: 0, end: 100 });
    oversized_note.values.note = Some("🦀".repeat(ANNOTATION_MAX_NOTE_SCALARS + 1));
    assert!(matches!(
        fixture.query.store.create_annotations(
            AnnotationsCreateRequest {
                entities: vec![oversized_note],
            },
            UtcInstant::try_from(10).expect("UTC"),
        ),
        Err(ExecuteAnnotationWritesError::InvalidFields)
    ));
    // Fill more than 1 MiB across many valid annotations to trigger frame-limit truncation.
    const HEAVY_NOTES: u32 = 80;
    let note = "🦀".repeat(ANNOTATION_MAX_NOTE_SCALARS);
    let entities = (10..10 + HEAVY_NOTES)
        .map(|id| {
            let mut entity =
                fixture.entity(id, AnnotationKind::Note, QueryRange { start: 0, end: 100 });
            entity.values.note = Some(note.clone());
            entity
        })
        .collect();
    fixture.write(entities, 10);
    let response = fixture.read(fixture.request(QueryRange { start: 0, end: 100 }));
    assert!(response.truncated);
    assert!(!response.entities.is_empty());
    assert!(response.entities.len() < usize::try_from(HEAVY_NOTES).expect("count"));
    assert!(
        serde_json::to_vec(&response).expect("truncated JSON").len() <= QUERY_MAX_RESPONSE_BYTES
    );
}
