use super::*;
use crate::{
    query::{QueryRequest, QueryResponse, RunQueryError},
    store::query::test_support::QueryFixture,
};
use quartz_domain::capture::UtcInstant;
use quartz_protocol::{
    Annotation, AnnotationCategory, AnnotationId, AnnotationKind, AnnotationValues,
    AnnotationsCreateRequest, AnnotationsDeleteRequest, AnnotationsUpdateRequest, BuiltinCategory,
    CreateAnnotation, QUERY_PAGE_BYTES, QueryAnnotationsRequest, UpdateAnnotation,
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
            cursor: None,
        }
    }
    fn read(&mut self, request: QueryAnnotationsRequest) -> QueryAnnotationsResponse {
        let QueryResponse::Annotations(page) = self
            .query
            .run(QueryRequest::Annotations(request))
            .expect("read annotations")
        else {
            panic!("annotation response")
        };
        assert!(serde_json::to_vec(&page).expect("encoded page").len() < QUERY_PAGE_BYTES);
        page
    }
    fn assert_final_values(&self, actual: &SavedAnnotation, expected: &Annotation) {
        assert_eq!(actual.uuid, expected.uuid);
        assert_eq!(actual.kind, expected.kind);
        assert_eq!(actual.start_utc_ms, expected.values.start_utc_ms);
        assert_eq!(actual.end_utc_ms, expected.values.end_utc_ms);
        assert_eq!(actual.category, expected.values.category);
        assert_eq!(actual.label_preset, expected.values.label_preset);
        assert_eq!(
            actual.label,
            expected.values.label.clone().map(AnnotationText::Complete)
        );
        assert_eq!(
            actual.note,
            expected.values.note.clone().map(AnnotationText::Complete)
        );
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
    let page = fixture.read(fixture.request(QueryRange { start: 0, end: 100 }));
    assert_eq!(
        page.entities.len(),
        4,
        "no foreground activity is needed to read saved annotations"
    );
    let mut expected_order = [2, 1, 0, 3].into_iter();
    for actual in &page.entities {
        let index = expected_order.next().expect("expected ordered entity");
        fixture.assert_final_values(actual, &written[index]);
    }
    assert!(page.next_cursor.is_none());
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
    let page = fixture.read(fixture.request(QueryRange { start: 20, end: 80 }));
    assert_eq!(page.entities.len(), 3);
    let left = &page.entities[0];
    let right = &page.entities[2];
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
    assert_eq!(right.note, Some(AnnotationText::Complete(String::new())));
    fixture.assert_final_values(&page.entities[1], &replacement);
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

#[derive(Default)]
struct ReassembledText {
    text: String,
    completed: Option<usize>,
}
impl ReassembledText {
    fn append(&mut self, field: Option<AnnotationText>, original: &str) {
        let Some(AnnotationText::Chunk(chunk)) = field else {
            panic!("chunked text")
        };
        assert_eq!(chunk.offset_utf8, self.text.len());
        assert!(original.is_char_boundary(chunk.offset_utf8));
        assert!(original.is_char_boundary(chunk.offset_utf8 + chunk.text.len()));
        if self.completed.is_some() {
            assert!(chunk.text.is_empty());
        }
        self.text.push_str(&chunk.text);
        if chunk.complete {
            self.completed = Some(self.text.len());
        }
    }
}

#[test]
fn oversized_multibyte_label_and_note_reassemble_across_bounded_pages() {
    let mut fixture = EditingFixture::new();
    // Mix escaped control bytes with two-, three- and four-byte UTF-8 characters.
    let label = "é界🦀\n\"\\".repeat(QUERY_PAGE_BYTES / 5);
    let note = "🦀界é\u{0001}".repeat(QUERY_PAGE_BYTES / 3);
    let mut entity = fixture.entity(
        1,
        AnnotationKind::Note,
        QueryRange {
            start: -100,
            end: 200,
        },
    );
    entity.values.label = Some(label.clone());
    entity.values.note = Some(note.clone());
    let written = fixture.write(vec![entity], 10).remove(0);
    let mut request = fixture.request(QueryRange { start: 0, end: 100 });
    let mut label_result = ReassembledText::default();
    let mut note_result = ReassembledText::default();
    let mut pages = 0;
    loop {
        let page = fixture.read(request.clone());
        pages += 1;
        assert!(!page.entities.is_empty());
        for part in page.entities {
            assert_eq!(part.uuid, written.uuid);
            assert_eq!(part.kind, written.kind);
            assert_eq!(part.category, written.values.category);
            assert_eq!(part.start_utc_ms, written.values.start_utc_ms);
            assert_eq!(part.end_utc_ms, written.values.end_utc_ms);
            assert_eq!(part.created_utc_ms, written.created_utc_ms);
            assert_eq!(part.updated_utc_ms, written.updated_utc_ms);
            label_result.append(part.label, &label);
            note_result.append(part.note, &note);
        }
        let Some(cursor) = page.next_cursor else {
            break;
        };
        assert_eq!(
            cursor.decode().expect("issued cursor").query(),
            &CursorQuery::from(&request)
        );
        request.cursor = Some(cursor);
    }
    assert!(pages > 1);
    assert_eq!(label_result.text, label);
    assert_eq!(note_result.text, note);
    assert_eq!(label_result.completed, Some(label.len()));
    assert_eq!(note_result.completed, Some(note.len()));
}

#[test]
fn ordinary_paging_preserves_order_and_rejects_changed_parameters_or_positions() {
    const ENTITY_COUNT: u32 = 40;
    let mut fixture = EditingFixture::new();
    let entities = (0..ENTITY_COUNT)
        .rev()
        .map(|id| {
            let mut entity =
                fixture.entity(id, AnnotationKind::Note, QueryRange { start: 0, end: 100 });
            entity.values.note = Some("文".repeat(QUERY_PAGE_BYTES / 30));
            entity
        })
        .collect();
    fixture.write(entities, 10);
    let mut request = fixture.request(QueryRange { start: 0, end: 100 });
    let first = fixture.read(request.clone());
    let cursor = first.next_cursor.clone().expect("normal continuation");
    let mut changed = request.clone();
    changed.end_utc_ms += 1;
    changed.cursor = Some(cursor.clone());
    assert!(matches!(
        fixture.query.run(QueryRequest::Annotations(changed)),
        Err(RunQueryError::InvalidCursor)
    ));
    let mut invalid_position = request.clone();
    invalid_position.cursor = Some(QueryCursor::new(
        first.versions.clone(),
        CursorQuery::from(&request),
        usize::MAX,
    ));
    assert!(matches!(
        fixture
            .query
            .run(QueryRequest::Annotations(invalid_position)),
        Err(RunQueryError::InvalidCursor)
    ));
    let mut all = first.entities;
    request.cursor = Some(cursor);
    loop {
        let page = fixture.read(request.clone());
        assert_eq!(page.versions, first.versions);
        all.extend(page.entities);
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    assert_eq!(all.len(), usize::try_from(ENTITY_COUNT).expect("count"));
    let expected_ids = (0..ENTITY_COUNT)
        .map(|id| {
            fixture
                .entity(id, AnnotationKind::Note, QueryRange { start: 0, end: 100 })
                .uuid
        })
        .collect::<Vec<_>>();
    assert_eq!(
        all.iter().map(|row| row.uuid.clone()).collect::<Vec<_>>(),
        expected_ids
    );
    for row in &all {
        assert!(matches!(&row.note, Some(AnnotationText::Complete(_))));
    }
}

#[test]
fn long_notes_preserve_null_empty_and_short_labels_on_every_page() {
    let mut fixture = EditingFixture::new();
    let note = "\u{0001}".repeat(QUERY_PAGE_BYTES);
    let mut entities = Vec::new();
    for label in [None, Some(String::new()), Some("短标签🦀".into())] {
        let id = u32::try_from(entities.len()).expect("fixture entity count");
        let mut entity =
            fixture.entity(id, AnnotationKind::Note, QueryRange { start: 0, end: 100 });
        entity.values.label = label;
        entity.values.note = Some(note.clone());
        entities.push(entity);
    }
    let written = fixture.write(entities, 10);
    let mut results = written
        .iter()
        .map(|_| ReassembledText::default())
        .collect::<Vec<_>>();
    let mut request = fixture.request(QueryRange { start: 0, end: 100 });
    loop {
        let page = fixture.read(request.clone());
        for row in page.entities {
            let index = written
                .iter()
                .position(|entity| entity.uuid == row.uuid)
                .expect("stored UUID");
            assert_eq!(
                row.label,
                written[index]
                    .values
                    .label
                    .clone()
                    .map(AnnotationText::Complete)
            );
            results[index].append(row.note, &note);
        }
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    for result in results {
        assert_eq!(result.text, note);
        assert_eq!(result.completed, Some(note.len()));
    }
}

#[test]
fn snapshot_retains_old_values_and_writes_invalidate_annotation_cursors() {
    let mut fixture = EditingFixture::new();
    let mut entity = fixture.entity(
        1,
        AnnotationKind::Note,
        QueryRange {
            start: -100,
            end: 200,
        },
    );
    entity.values.note = Some("界".repeat(QUERY_PAGE_BYTES));
    let stored = fixture.write(vec![entity], 10).remove(0);
    let mut request = fixture.request(QueryRange { start: 0, end: 100 });
    let first = fixture.read(request.clone());
    request.cursor = first.next_cursor.clone();
    assert!(request.cursor.is_some());
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
        if let Some(page) = job.advance().expect("snapshot turn") {
            break page;
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
    assert!(matches!(
        fixture
            .query
            .run(QueryRequest::Annotations(request.clone())),
        Err(RunQueryError::StaleCursor)
    ));
    request.cursor = None;
    let updated = fixture.read(request.clone());
    assert_ne!(updated.versions, first.versions);
    assert_eq!(
        updated.entities[0].note,
        Some(AnnotationText::Complete("已更新".into()))
    );
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
                cursor: None,
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
                    cursor: None
                })
                .entities
                .is_empty()
        );
    }
}

#[test]
fn fragment_resume_skips_without_materializing_earlier_text() {
    let entity = SavedAnnotation {
        uuid: AnnotationId(
            "abcdef00-0000-4000-8000-000000000001"
                .parse()
                .expect("UUID"),
        ),
        kind: AnnotationKind::Note,
        start_utc_ms: 0,
        end_utc_ms: 1,
        category: None,
        label_preset: None,
        label: Some(AnnotationText::Complete("short label".into())),
        note: Some(AnnotationText::Complete("🦀\n".repeat(40_000))),
        created_utc_ms: 0,
        updated_utc_ms: 0,
    };
    let bytes = serde_json::to_vec(&entity).expect("wire entity").len();
    assert!(bytes > QUERY_PAGE_BYTES && bytes < quartz_protocol::MAX_FRAME_LEN);
    let budget = PageBudget::new(&()).expect("budget");
    let mut whole = EntityChunks::new(entity.clone(), &budget).expect("fragment");
    let mut expected = Vec::new();
    while let Some(fragment) = whole.next() {
        expected.push(fragment);
    }
    assert!(expected.len() > 3);
    let mut resumed = EntityChunks::new(entity.clone(), &budget).expect("fragment");
    MATERIALIZED_FRAGMENTS.set(0);
    resumed.seek(3).expect("resume fourth fragment");
    assert_eq!(MATERIALIZED_FRAGMENTS.get(), 0);
    assert_eq!(resumed.next(), Some(expected[3].clone()));
    assert_eq!(MATERIALIZED_FRAGMENTS.get(), 1);
    let mut invalid = EntityChunks::new(entity, &budget).expect("fragment");
    assert!(matches!(
        invalid.seek(usize::MAX),
        Err(RunQueryError::InvalidCursor)
    ));
}

#[test]
fn annotation_page_reserves_maximal_resume_for_entities_and_fragments() {
    let versions = QueryVersions("v".repeat(64));
    let parameters = CursorQuery::Annotations {
        start_utc_ms: i64::MIN,
        end_utc_ms: i64::MAX,
    };
    let resume = quartz_protocol::AnnotationResume {
        start_utc_ms: i64::MIN,
        end_utc_ms: i64::MIN,
        uuid: AnnotationId(
            "ffffffff-ffff-ffff-ffff-ffffffffffff"
                .parse()
                .expect("maximum-width UUID"),
        ),
        chunk: usize::MAX,
    };
    let mut page = AnnotationPage::new(
        versions.clone(),
        parameters.clone(),
        PagePosition::First,
        None,
    )
    .expect("page");
    let expected = PageBudget::new(&page.response)
        .expect("response budget")
        .with_cursor(&QueryCursor::new_annotation(
            versions,
            parameters,
            resume.clone(),
        ))
        .expect("maximal cursor");
    let available = page.budget.remaining_bytes();
    let mut entity = SavedAnnotation {
        uuid: resume.uuid.clone(),
        kind: AnnotationKind::Note,
        start_utc_ms: i64::MIN,
        end_utc_ms: i64::MAX,
        category: None,
        label_preset: Some(quartz_protocol::AnnotationLabelPreset::Rest),
        label: None,
        note: Some(AnnotationText::Complete(String::new())),
        created_utc_ms: i64::MIN,
        updated_utc_ms: i64::MIN,
    };
    let entity_bytes = serde_json::to_vec(&entity).expect("empty note").len() + 1;
    entity.note = Some(AnnotationText::Complete(
        "x".repeat(available - entity_bytes),
    ));
    assert!(page.budget.admit(&entity).expect("full-page entity"));
    assert_eq!(page.budget.remaining_bytes(), 0);
    page.response.entities.push(entity.clone());
    let response = page.finish(Some(resume));
    assert!(
        serde_json::to_vec(&response)
            .expect("full page with maximal cursor")
            .len()
            <= QUERY_PAGE_BYTES
    );
    assert_eq!(
        available,
        expected.remaining_bytes(),
        "reserve the emitted cursor shape"
    );
    let empty = QueryAnnotationsResponse {
        entities: Vec::new(),
        versions: response.versions,
        next_cursor: None,
    };
    entity.note = None;
    let actual = AnnotationPage::budget(&empty, &page.parameters).expect("fragment budget");
    assert_eq!(
        actual
            .annotation_chunk_bytes(&entity)
            .expect("fragment capacity"),
        expected
            .annotation_chunk_bytes(&entity)
            .expect("expected fragment capacity")
    );
}

#[test]
fn label_preset_is_returned_as_metadata_with_and_without_text_chunks() {
    let mut fixture = EditingFixture::new();
    let mut rest = fixture.entity(1, AnnotationKind::Note, QueryRange { start: 0, end: 100 });
    rest.values.label_preset = Some(quartz_protocol::AnnotationLabelPreset::Rest);
    let created = fixture.write(vec![rest.clone()], 10);
    let request = fixture.request(QueryRange { start: 0, end: 100 });
    let page = fixture.read(request.clone());
    assert_eq!(page.entities.len(), 1);
    fixture.assert_final_values(&page.entities[0], &created[0]);
    rest.uuid = fixture
        .entity(2, AnnotationKind::Note, QueryRange { start: 0, end: 100 })
        .uuid;
    rest.values.note = Some("🦀".repeat(QUERY_PAGE_BYTES));
    fixture.write(vec![rest], 20);
    let mut request = request;
    let mut fragments = 0;
    loop {
        let page = fixture.read(request.clone());
        for entity in page.entities {
            assert_eq!(
                entity.label_preset,
                Some(quartz_protocol::AnnotationLabelPreset::Rest)
            );
            let wire = serde_json::to_value(&entity).expect("annotation wire");
            assert_eq!(wire["label_preset"], "rest");
            if matches!(entity.note, Some(AnnotationText::Chunk(_))) {
                fragments += 1;
            }
        }
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    assert!(fragments > 1, "long note spans several pages");
}
