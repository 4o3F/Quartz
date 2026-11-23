use super::*;
use crate::query::{QueryJob, QueryRequest, QueryResponse, RunQueryError};
use crate::store::{rows::TimedPoliciesRow, schema::timed_policies};
use quartz_protocol::{Activity, ErrorKind, QUERY_PAGE_BYTES, QueryCursor};
use test_support::{FIXTURE_DATE, QueryFixture};

fn collect_timeline(
    fixture: &mut QueryFixture,
    request: QueryRequest,
) -> Vec<quartz_protocol::TimelineSegment> {
    let QueryRequest::Timeline(mut request) = request else {
        panic!("timeline request")
    };
    let mut all = Vec::new();
    loop {
        let response = fixture
            .run(QueryRequest::Timeline(request.clone()))
            .expect("timeline page");
        assert!(serde_json::to_vec(&response).expect("encoded page").len() <= QUERY_PAGE_BYTES);
        let QueryResponse::Timeline(page) = response else {
            panic!("timeline response")
        };
        assert_eq!(page.reference_range.is_some(), request.cursor.is_none());
        all.extend(page.segments);
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    all
}
fn unpaged_timeline(
    fixture: &mut QueryFixture,
    request: &QueryRequest,
) -> Vec<quartz_protocol::TimelineSegment> {
    let QueryRequest::Timeline(request) = request else {
        panic!("timeline request")
    };
    let range = QueryRange {
        start: request.start_utc_ms,
        end: request.end_utc_ms,
    };
    let mut snapshot = fixture
        .store
        .take_query_reader()
        .expect("reader")
        .snapshot();
    snapshot.begin().expect("begin unpaged snapshot");
    let mut pipeline = crate::query::pipeline::Pipeline::new(
        range,
        crate::query::classification::Classifier::new(fixture.rules(), Vec::new()),
    );
    while let Some(rows) = snapshot.read_batch(range).expect("typed batch") {
        for row in rows {
            pipeline.ingest(row).expect("valid facts");
        }
    }
    pipeline.finish_input();
    while !pipeline.advance() {}
    let mut projection = crate::query::pipeline::TimelineProjection::default();
    let derived = pipeline.into_segments();
    for segment in &derived.segments {
        projection.push(
            segment,
            &derived.facts,
            request.detail,
            request.ms_per_pixel.0.get(),
        );
    }
    fixture
        .store
        .return_query_reader(snapshot.finish().expect("finish snapshot"));
    let QueryResponse::Timeline(page) = fixture
        .run(QueryRequest::Timeline(request.clone()))
        .expect("reference versions")
    else {
        panic!("timeline");
    };
    let mut segments = projection.into_segments();
    let mut references = crate::query::documents::PageDocumentReferences::new(
        quartz_protocol::ReferenceRange {
            start_utc_ms: range.start,
            end_utc_ms: range.end,
        },
        &page.versions,
    );
    for segment in &mut segments {
        references.attach(segment).expect("attach reference");
    }
    segments
}
#[test]
fn known_day_applies_idle_and_clips_open_rows_at_their_confirmation() {
    let mut fixture = QueryFixture::new(4);
    fixture.mask_idle(500, 1500);
    fixture.open_last_fact(3500);
    let request = fixture.request_day();
    let QueryResponse::Day(day) = fixture.run(request).expect("day") else {
        panic!("day response")
    };
    let metrics = day.metrics.expect("first page metrics");
    assert_eq!(metrics.active_ms, 2500);
    assert_eq!(metrics.unclassified_ms, 2500);
    assert_eq!(metrics.focus_ratio, 0.0);
    assert_eq!(metrics.distract_ratio, 0.0);
    assert_eq!(metrics.longest_focus_ms, 0);
    assert_eq!(metrics.switches_per_hour, 2880.0);
    assert_eq!(day.applications.len(), 4);
    assert_eq!(
        day.applications
            .iter()
            .map(|rank| rank.active_ms)
            .sum::<i64>(),
        2500
    );
    assert_eq!(
        day.hours
            .expect("hours")
            .iter()
            .flat_map(|hour| &hour.categories)
            .map(|bucket| bucket.duration_ms)
            .sum::<i64>(),
        2500
    );
    let request = fixture.request_timeline();
    let segments = collect_timeline(&mut fixture, request);
    assert_eq!(
        segments
            .iter()
            .filter(|segment| segment.activity == Activity::Active)
            .map(|segment| segment.duration_ms)
            .sum::<i64>(),
        2500
    );
}
#[test]
fn pages_reassemble_every_timeline_segment_and_every_ranking_entry() {
    let mut fixture = QueryFixture::new(2000);
    let request = fixture.request_timeline();
    let expected = unpaged_timeline(&mut fixture, &request);
    let segments = collect_timeline(&mut fixture, request);
    assert_eq!(segments, expected);
    assert_eq!(segments.len(), 2001);
    let QueryRequest::Day(mut request) = fixture.request_day() else {
        panic!("day request")
    };
    let mut ranking = Vec::new();
    let mut pages = 0;
    loop {
        let response = fixture
            .run(QueryRequest::Day(request.clone()))
            .expect("ranking page");
        assert!(serde_json::to_vec(&response).expect("page bytes").len() <= QUERY_PAGE_BYTES);
        let QueryResponse::Day(page) = response else {
            panic!("day response")
        };
        assert_eq!(page.metrics.is_some(), pages == 0);
        assert_eq!(page.hours.is_some(), pages == 0);
        ranking.extend(page.applications);
        pages += 1;
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    assert!(pages > 1);
    assert_eq!(ranking.len(), 2000);
    assert!(
        ranking
            .windows(2)
            .all(|pair| pair[0].app.id < pair[1].app.id)
    );
}
#[test]
fn old_cursor_is_rejected_after_fact_metadata_or_rule_changes() {
    let mut fixture = QueryFixture::new(1000);
    let QueryRequest::Timeline(request) = fixture.request_timeline() else {
        panic!("timeline request")
    };
    for change in 0..3 {
        let QueryResponse::Timeline(first) = fixture
            .run(QueryRequest::Timeline(request.clone()))
            .expect("first page")
        else {
            panic!("timeline")
        };
        let old = QueryRequest::Timeline(quartz_protocol::QueryTimelineRequest {
            cursor: Some(first.next_cursor.expect("continuation")),
            ..request.clone()
        });
        if change == 0 {
            fixture.bump_fact_version();
        } else if change == 1 {
            fixture.bump_metadata();
        }
        if change < 2 {
            assert!(matches!(fixture.run(old), Err(RunQueryError::StaleCursor)));
        } else {
            let mut reader = fixture.store.take_query_reader();
            assert!(matches!(
                QueryJob::start(
                    &mut reader,
                    old,
                    quartz_domain::rules::registry::RuleRegistry::from_state(Default::default())
                        .expect("changed rules")
                        .snapshots()
                        .load()
                ),
                Err(RunQueryError::StaleCursor)
            ));
            fixture
                .store
                .return_query_reader(reader.expect("reader returned on stale cursor"));
        }
    }
}
#[test]
fn query_holds_one_snapshot_while_writer_commits_and_next_query_sees_change() {
    let mut fixture = QueryFixture::new(1000);
    let request = fixture.request_day();
    let mut job = fixture.start(request.clone());
    assert!(job.advance().expect("first bounded read").is_none());
    fixture.append_fact();
    let response = loop {
        if let Some(response) = job.advance().expect("snapshot turn") {
            break response;
        }
    };
    fixture
        .store
        .return_query_reader(job.finish().expect("close snapshot"));
    let QueryResponse::Day(old) = response else {
        panic!("day")
    };
    assert_eq!(old.applications.len(), 1000);
    let QueryResponse::Day(new) = fixture.run(request).expect("new snapshot") else {
        panic!("day")
    };
    assert_eq!(new.applications.len(), 1001);
    assert_ne!(old.versions, new.versions);
}
#[test]
fn shared_application_metadata_stays_in_one_snapshot_and_refreshes_on_the_next_query() {
    let count = i64::try_from(READ_BATCH_ROWS + 1).expect("fixture count");
    let mut fixture = QueryFixture::new(count);
    diesel::update(intervals::table)
        .set(intervals::app_id.eq(1_i64))
        .execute(&mut fixture.store.writer)
        .expect("one application across fact batches");
    let request = fixture.request_day();
    let mut job = fixture.start(request.clone());
    assert!(job.advance().expect("read clocks").is_none());
    assert!(job.advance().expect("read first fact batch").is_none());
    diesel::update(applications::table.filter(applications::id.eq(1_i64)))
        .set(applications::display_name.eq("Renamed application"))
        .execute(&mut fixture.store.writer)
        .expect("rename application");
    let mut effects = crate::store::versions::TxEffects::default();
    effects
        .bump_metadata(&mut fixture.store.writer)
        .expect("metadata version");
    fixture.store.pending_effects.merge(effects);
    let old = loop {
        if let Some(response) = job.advance().expect("remaining snapshot batches") {
            break response;
        }
    };
    fixture
        .store
        .return_query_reader(job.finish().expect("finish old snapshot"));
    let QueryResponse::Day(old) = old else {
        panic!("day response")
    };
    assert_eq!(old.applications.len(), 1);
    assert_eq!(old.applications[0].app.display_name, "Application 1");
    let QueryResponse::Day(new) = fixture.run(request).expect("next query") else {
        panic!("day response")
    };
    assert_eq!(new.applications[0].app.display_name, "Renamed application");
    assert_ne!(old.versions, new.versions);
}

#[test]
fn raw_projection_enforces_capture_privacy_even_if_title_is_present_in_storage() {
    let mut fixture = QueryFixture::new(2);
    for level in [
        enums::CaptureLevel::TitleHidden,
        enums::CaptureLevel::Excluded,
    ] {
        fixture.redact_titles(level);
        let request = fixture.request_timeline();
        let rows = collect_timeline(&mut fixture, request);
        assert!(rows.iter().all(|row| row.title.is_none()));
        if level == enums::CaptureLevel::Excluded {
            assert!(rows.iter().all(|row| row.app.is_none()));
        }
    }
}

#[test]
fn typed_cursor_rejects_parameter_mismatch_and_out_of_range_positions() {
    let mut fixture = QueryFixture::new(1000);
    let QueryRequest::Timeline(request) = fixture.request_timeline() else {
        panic!("timeline request")
    };
    let QueryResponse::Timeline(page) = fixture
        .run(QueryRequest::Timeline(request.clone()))
        .expect("first page")
    else {
        panic!("timeline response")
    };
    let cursor = page.next_cursor.expect("continuation");
    let mut mismatched = request.clone();
    mismatched.detail = quartz_protocol::TimelineDetail::Category;
    mismatched.cursor = Some(cursor.clone());
    assert_eq!(
        fixture
            .run(QueryRequest::Timeline(mismatched))
            .expect_err("changed parameters")
            .kind(),
        ErrorKind::BadRequest
    );
    let count = unpaged_timeline(&mut fixture, &QueryRequest::Timeline(request.clone())).len();
    for position in [count, usize::MAX] {
        let mut invalid = request.clone();
        invalid.cursor = Some(QueryCursor::new(
            cursor.versions().clone(),
            cursor.query().clone(),
            position,
        ));
        assert_eq!(
            fixture
                .run(QueryRequest::Timeline(invalid))
                .expect_err("position outside list")
                .kind(),
            ErrorKind::BadRequest
        );
    }
    let mut valid = request;
    valid.cursor = Some(
        serde_json::from_value(serde_json::to_value(cursor).expect("cursor object"))
            .expect("opaque cursor round trip"),
    );
    assert!(fixture.run(QueryRequest::Timeline(valid)).is_ok());
}
#[test]
fn first_page_accepts_an_empty_list_but_a_cursor_cannot_point_into_it() {
    let mut fixture = QueryFixture::new(0);
    let QueryRequest::Day(mut request) = fixture.request_day() else {
        panic!("day request")
    };
    let QueryResponse::Day(page) = fixture
        .run(QueryRequest::Day(request.clone()))
        .expect("empty first page")
    else {
        panic!("day response")
    };
    assert!(page.applications.is_empty());
    request.cursor = Some(QueryCursor::new(
        page.versions,
        quartz_protocol::CursorQuery::from(&request),
        0,
    ));
    assert_eq!(
        fixture
            .run(QueryRequest::Day(request))
            .expect_err("empty list cursor")
            .kind(),
        ErrorKind::BadRequest
    );
}
#[test]
fn batched_reads_keep_row_id_ties_at_the_same_timestamp() {
    let mut fixture = QueryFixture::new(600);
    fixture.collapse_foreground_times();
    let request = fixture.request_day();
    let QueryResponse::Day(day) = fixture.run(request).expect("same timestamp batches") else {
        panic!("day response")
    };
    assert_eq!(day.applications.len(), 1);
    assert_eq!(
        day.applications[0].app.id,
        quartz_protocol::ApplicationId(600)
    );
    assert_eq!(day.metrics.expect("metrics").active_ms, 1000);
}

#[test]
fn builtin_classification_upgrade_preserves_user_sets_invalidates_cursor_and_retains_inflight_snapshot()
 {
    use crate::rules::registry::RegistryShell;
    use quartz_domain::{
        capture::UtcInstant,
        rules::{self, ParseContext, RuleOrigin, RuleSet, registry::RegistryCommand},
    };
    use quartz_protocol::{BuiltinCategory, CategoryRef, CategorySource};
    let mut fixture = QueryFixture::new(1000);
    diesel::update(applications::table.filter(applications::id.eq(1_i64)))
        .set(applications::exe_name.eq("code.exe"))
        .execute(&mut fixture.store.writer)
        .expect("matching app");
    let now = UtcInstant::try_from(0).expect("UTC");
    let mut shell = RegistryShell::open(&mut fixture.store, now).expect("shell");
    shell.run(&mut fixture.store, now, RegistryCommand::Apply {
        key: "user.unmatched".parse().expect("user key"), file: "user.toml".into(), text: "format='quartz-rules/1'\n[set]\nkey='user.unmatched'\nkind='classification'\nname='User rules'\nparent='builtin.classification'\n[[rule]]\nid='other'\nlabel='Other'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='other.exe'}]".into(), base_version: 0,
    }).expect("user overlay");
    let request = fixture.request_timeline();
    let QueryResponse::Timeline(first) = fixture.run(request.clone()).expect("first page") else {
        panic!("timeline");
    };
    assert!(
        matches!(&first.segments[0].category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Development))
    );
    let QueryRequest::Timeline(mut continuation) = request.clone() else {
        panic!("timeline request");
    };
    continuation.cursor = Some(first.next_cursor.expect("continuation"));
    let mut inflight = fixture.start(request);
    let next_revision = rules::BUILTIN_CLASSIFICATION_REVISION
        .checked_add(1)
        .expect("next revision");
    let changed =
        rules::BUILTIN_CLASSIFICATION.replace("builtin:development", "builtin:communication");
    let set = RuleSet::parse(
        "builtin.toml",
        &changed,
        ParseContext {
            origin: RuleOrigin::Builtin,
            references: &shell.core().state().references,
        },
    )
    .expect("upgraded builtin");
    shell
        .run(
            &mut fixture.store,
            now,
            RegistryCommand::InstallBuiltin {
                set: set.clone(),
                revision: next_revision,
            },
        )
        .expect("upgrade");
    assert_eq!(
        shell.core().state().sets[&"user.unmatched".parse().expect("key")]
            .versions
            .len(),
        1
    );
    let hash = shell.snapshots().load().hashes().classification.clone();
    shell
        .run(
            &mut fixture.store,
            now,
            RegistryCommand::InstallBuiltin {
                set,
                revision: next_revision,
            },
        )
        .expect("idempotent upgrade");
    assert_eq!(&hash, &shell.snapshots().load().hashes().classification);
    assert_eq!(
        shell.core().state().sets[&"builtin.classification".parse().expect("key")]
            .versions
            .len(),
        2
    );
    let old = loop {
        if let Some(response) = inflight.advance().expect("inflight query") {
            break response;
        }
    };
    let QueryResponse::Timeline(old) = old else {
        panic!("timeline");
    };
    assert!(
        matches!(&old.segments[0].category_source, CategorySource::Rule { rule } if rule.as_str() == "builtin.classification/vscode@2")
    );
    fixture
        .store
        .return_query_reader(inflight.finish().expect("finish old snapshot"));
    assert!(matches!(
        fixture.run(QueryRequest::Timeline(continuation)),
        Err(RunQueryError::StaleCursor)
    ));
    let QueryResponse::Timeline(new) = fixture.run(fixture.request_timeline()).expect("new query")
    else {
        panic!("timeline");
    };
    assert!(
        matches!(&new.segments[0].category, CategoryRef::Classified { category } if category.builtin_key == Some(BuiltinCategory::Communication))
    );
    assert!(
        matches!(&new.segments[0].category_source, CategorySource::Rule { rule } if rule.as_str() == format!("builtin.classification/vscode@{next_revision}"))
    );
}
#[test]
fn missing_or_archived_category_returns_unclassified_without_trying_a_later_rule() {
    use crate::{rules::registry::RegistryShell, store::schema::categories};
    use quartz_domain::{capture::UtcInstant, rules::registry::RegistryCommand};
    let mut fixture = QueryFixture::new(2);
    diesel::update(applications::table)
        .set(applications::exe_name.eq("code.exe"))
        .execute(&mut fixture.store.writer)
        .expect("matching apps");
    let now = UtcInstant::try_from(0).expect("UTC");
    let mut shell = RegistryShell::open(&mut fixture.store, now).expect("rules");
    shell.run(&mut fixture.store, now, RegistryCommand::Apply { key: "user.first".parse().expect("key"), file: "first.toml".into(), text: "format='quartz-rules/1'\n[set]\nkey='user.first'\nkind='classification'\nname='First'\nparent='builtin.classification'\n[[rule]]\nid='first'\nlabel='First'\ncategory='builtin:learning'\npriority=60\nwhen=[{field='app.exe_name',op='equals',value='code.exe'}]".into(), base_version: 0 }).expect("higher priority rule");
    for retirement in [CategoryRetirement::Archive, CategoryRetirement::Delete] {
        match retirement {
            CategoryRetirement::Archive => {
                diesel::update(categories::table.filter(categories::builtin_key.eq("learning")))
                    .set(categories::archived.eq(1))
                    .execute(&mut fixture.store.writer)
                    .expect("archive category");
            }
            CategoryRetirement::Delete => {
                diesel::delete(categories::table.filter(categories::builtin_key.eq("learning")))
                    .execute(&mut fixture.store.writer)
                    .expect("delete category");
            }
        }
        // Retain the installed rule snapshot after deletion; startup would recreate the missing builtin.
        let mut reader = fixture.store.take_query_reader();
        let job = QueryJob::start(
            &mut reader,
            fixture.request_timeline(),
            shell.snapshots().load(),
        )
        .expect("query");
        let completed = job.run_measured().expect("run");
        let QueryResponse::Timeline(response) = completed.response else {
            panic!("timeline");
        };
        assert_eq!(
            response.segments[0].category,
            quartz_protocol::CategoryRef::Unclassified
        );
        assert_eq!(
            response.segments[0].category_source,
            quartz_protocol::CategorySource::Unclassified
        );
        fixture.store.return_query_reader(completed.reader);
    }
}
enum CategoryRetirement {
    Archive,
    Delete,
}

#[test]
fn user_category_uuid_and_metadata_are_returned_by_the_query() {
    use crate::{
        rules::registry::RegistryShell,
        store::{enums::CategoryTendency, rows::CategoriesRow, schema::categories},
    };
    use quartz_domain::{capture::UtcInstant, rules::registry::RegistryCommand};
    const USER_UUID: &str = "c2c2c2c2-1234-4000-8000-000000000001";
    let mut fixture = QueryFixture::new(1);
    diesel::insert_into(categories::table)
        .values(CategoriesRow {
            id: None,
            uuid: USER_UUID.into(),
            parent_id: None,
            builtin_key: None,
            name: Some("Research".into()),
            color: "cat.2".into(),
            tendency: CategoryTendency::Focus,
            sort_order: 10,
            archived: 0,
        })
        .execute(&mut fixture.store.writer)
        .expect("user category");
    let now = UtcInstant::try_from(0).expect("UTC");
    let mut shell = RegistryShell::open(&mut fixture.store, now).expect("rules");
    shell.run(&mut fixture.store, now, RegistryCommand::Apply { key: "user.research".parse().expect("key"), file: "research.toml".into(), text: format!("format='quartz-rules/1'\n[set]\nkey='user.research'\nkind='classification'\nname='Research'\n[[rule]]\nid='research'\nlabel='Research app'\ncategory='{USER_UUID}'\nwhen=[{{field='app.exe_name',op='equals',value='app-1.exe'}}]"), base_version: 0 }).expect("UUID rule reference");
    let QueryResponse::Timeline(response) =
        fixture.run(fixture.request_timeline()).expect("timeline")
    else {
        panic!("timeline");
    };
    let quartz_protocol::CategoryRef::Classified { category } = &response.segments[0].category
    else {
        panic!("user category");
    };
    assert_eq!(category.uuid.0, USER_UUID);
    assert_eq!(category.builtin_key, None);
    assert_eq!(category.name.as_deref(), Some("Research"));
    assert_eq!(category.color, "cat.2");
    assert_eq!(category.tendency, quartz_protocol::CategoryTendency::Focus);
}

#[test]
fn store_read_projects_private_fields_before_returning_foreground_facts() {
    let mut fixture = QueryFixture::new(2);
    for id in 1..=fixture.count {
        diesel::insert_into(crate::store::schema::window_instances::table)
            .values(crate::store::rows::WindowInstancesRow {
                id: Some(id),
                run_id: 1,
                app_id: id,
                pid: id,
                process_created_utc_ms: None,
                hwnd: id,
                generation: 1,
                retired_utc_ms: None,
                window_class: Some("StoredClass".into()),
                first_seen_utc_ms: fixture.start,
                last_seen_utc_ms: fixture.start,
            })
            .execute(&mut fixture.store.writer)
            .expect("stored window instance");
        diesel::update(intervals::table.filter(intervals::id.eq(Some(id))))
            .set(intervals::window_instance_id.eq(Some(id)))
            .execute(&mut fixture.store.writer)
            .expect("link stored window instance");
    }
    for level in [
        enums::CaptureLevel::Full,
        enums::CaptureLevel::TitleHidden,
        enums::CaptureLevel::Excluded,
    ] {
        fixture.redact_titles(level);
        let range = QueryRange {
            start: fixture.start,
            end: fixture.start + 2000,
        };
        let mut snapshot = fixture
            .store
            .take_query_reader()
            .expect("reader")
            .snapshot();
        snapshot.begin().expect("begin snapshot");
        let mut foreground_count = 0;
        while let Some(rows) = snapshot.read_batch(range).expect("read facts") {
            for row in rows {
                if let InputRow::Foreground(row) = row {
                    foreground_count += 1;
                    assert_eq!(
                        row.visible_app().is_some(),
                        level != enums::CaptureLevel::Excluded
                    );
                    assert_eq!(
                        row.visible_window_instance(),
                        (level != enums::CaptureLevel::Excluded)
                            .then_some(quartz_domain::facts::WindowInstanceId(row.span.id.0))
                    );
                    assert_eq!(
                        row.visible_window_class(),
                        (level != enums::CaptureLevel::Excluded).then_some("StoredClass")
                    );
                    assert_eq!(
                        row.visible_title().is_some(),
                        level == enums::CaptureLevel::Full
                    );
                    assert_eq!(
                        row.visible_title_id().is_some(),
                        level == enums::CaptureLevel::Full
                    );
                }
            }
        }
        assert_eq!(foreground_count, 2);
        fixture
            .store
            .return_query_reader(snapshot.finish().expect("finish snapshot"));
    }
}

#[test]
fn annotation_commit_invalidates_an_existing_timeline_cursor() {
    let mut fixture = QueryFixture::new(1000);
    let QueryRequest::Timeline(mut request) = fixture.request_timeline() else {
        panic!("timeline request")
    };
    let QueryResponse::Timeline(first) = fixture
        .run(QueryRequest::Timeline(request.clone()))
        .expect("first page")
    else {
        panic!("timeline response")
    };
    request.cursor = Some(first.next_cursor.expect("continuation"));
    fixture
        .store
        .create_annotations(
            quartz_protocol::AnnotationsCreateRequest {
                entities: vec![quartz_protocol::CreateAnnotation {
                    uuid: quartz_protocol::AnnotationId(
                        "00000000-0000-4000-8000-000000000001"
                            .parse()
                            .expect("UUID"),
                    ),
                    kind: quartz_protocol::AnnotationKind::Note,
                    values: quartz_protocol::AnnotationValues {
                        start_utc_ms: fixture.start,
                        end_utc_ms: fixture.start + 1000,
                        category: None,
                        label: Some("note".into()),
                        note: None,
                    },
                }],
            },
            quartz_domain::capture::UtcInstant::try_from(0).expect("clock"),
        )
        .expect("annotation commit");
    assert!(matches!(
        fixture.run(QueryRequest::Timeline(request)),
        Err(RunQueryError::StaleCursor)
    ));
}

#[test]
fn annotations_drive_day_metrics_hour_stacks_and_timeline_from_one_snapshot() {
    use quartz_protocol::{AnnotationKind, BuiltinCategory, CategoryRef, CategorySource};
    let mut fixture = QueryFixture::new(4);
    fixture.mask_idle(1000, 3000);
    fixture.annotate(
        1,
        AnnotationKind::CategoryOverride,
        0,
        1000,
        Some(BuiltinCategory::Development),
    );
    fixture.annotate(
        2,
        AnnotationKind::ActiveOverride,
        1000,
        2000,
        Some(BuiltinCategory::Learning),
    );
    fixture.annotate(
        3,
        AnnotationKind::OfflineActivity,
        1500,
        5000,
        Some(BuiltinCategory::Entertainment),
    );
    let request = fixture.request_day();
    let QueryResponse::Day(day) = fixture.run(request).expect("annotated day") else {
        panic!("day")
    };
    let metrics = day.metrics.expect("metrics");
    assert_eq!(metrics.active_ms, 5000);
    assert_eq!(metrics.focus_ratio, 0.4);
    assert_eq!(metrics.distract_ratio, 0.4);
    assert_eq!(metrics.unclassified_ms, 1000);
    assert_eq!(metrics.longest_focus_ms, 2000);
    assert_eq!(metrics.switches_per_hour, 4.0 * 3_600_000.0 / 5000.0);
    assert_eq!(
        day.applications
            .iter()
            .map(|app| app.active_ms)
            .sum::<i64>(),
        3000
    );
    assert_eq!(
        day.hours
            .expect("hours")
            .iter()
            .flat_map(|hour| &hour.categories)
            .map(|category| category.duration_ms)
            .sum::<i64>(),
        5000
    );
    let request = fixture.request_timeline();
    let timeline = collect_timeline(&mut fixture, request);
    assert_eq!(timeline.len(), 6);
    assert!(matches!(
        &timeline[0].category_source,
        CategorySource::Annotation { .. }
    ));
    assert!(matches!(
        &timeline[1].activity,
        Activity::ActiveOverride { .. }
    ));
    assert!(matches!(
        &timeline[2].activity,
        Activity::ActiveOverride { .. }
    ));
    assert!(matches!(
        &timeline[3].activity,
        Activity::OfflineActivity { .. }
    ));
    assert_eq!(timeline[4].activity, Activity::Active);
    assert!(matches!(&timeline[4].category, CategoryRef::Unclassified));
    assert!(matches!(
        &timeline[5].activity,
        Activity::OfflineActivity { .. }
    ));
    assert!(timeline[5].app.is_none());
}

#[test]
fn annotation_write_invalidates_old_timeline_and_pending_away_cursors() {
    use quartz_protocol::{AnnotationKind, CursorQuery, QueryPendingAwayRequest};
    let mut fixture = QueryFixture::new(1000);
    fixture.mask_idle(0, 2_400_000);
    let QueryRequest::Timeline(request) = fixture.request_timeline() else {
        panic!("timeline")
    };
    let QueryResponse::Timeline(first) = fixture
        .run(QueryRequest::Timeline(request.clone()))
        .expect("first page")
    else {
        panic!("timeline")
    };
    let away = QueryPendingAwayRequest {
        local_date: FIXTURE_DATE.parse().expect("date"),
        cursor: None,
    };
    let QueryResponse::PendingAway(pending) = fixture
        .run(QueryRequest::PendingAway(away.clone()))
        .expect("pending")
    else {
        panic!("pending")
    };
    assert_eq!(pending.ranges.len(), 1);
    let pending_cursor = QueryCursor::new(pending.versions, CursorQuery::from(&away), 0);
    fixture.annotate(1, AnnotationKind::DismissedAway, 0, 1000, None);
    assert!(matches!(
        fixture.run(QueryRequest::Timeline(
            quartz_protocol::QueryTimelineRequest {
                cursor: Some(first.next_cursor.expect("continuation")),
                ..request
            }
        )),
        Err(RunQueryError::StaleCursor)
    ));
    assert!(matches!(
        fixture.run(QueryRequest::PendingAway(QueryPendingAwayRequest {
            cursor: Some(pending_cursor),
            ..away
        })),
        Err(RunQueryError::StaleCursor)
    ));
}

#[test]
fn pending_away_subtracts_partial_coverage_and_includes_fifteen_minute_idle() {
    use quartz_protocol::{AnnotationKind, QueryPendingAwayRequest};
    const MINUTE_MS: i64 = 60_000;
    let mut fixture = QueryFixture::new(1);
    fixture.mask_idle(0, 20 * MINUTE_MS);
    fixture.mask_idle(30 * MINUTE_MS, 50 * MINUTE_MS);
    fixture.mask_idle(60 * MINUTE_MS, 100 * MINUTE_MS);
    fixture.mask_locked(110 * MINUTE_MS, 140 * MINUTE_MS);
    fixture.mask_idle(150 * MINUTE_MS, 165 * MINUTE_MS);
    fixture.mask_idle(170 * MINUTE_MS, 200 * MINUTE_MS);
    fixture.mask_idle(210 * MINUTE_MS, 240 * MINUTE_MS);
    fixture.annotate(
        1,
        AnnotationKind::DismissedAway,
        35 * MINUTE_MS,
        45 * MINUTE_MS,
        None,
    );
    fixture.annotate(
        2,
        AnnotationKind::OfflineActivity,
        60 * MINUTE_MS,
        70 * MINUTE_MS,
        None,
    );
    fixture.annotate(
        3,
        AnnotationKind::CategoryOverride,
        0,
        20 * MINUTE_MS,
        Some(quartz_protocol::BuiltinCategory::Learning),
    );
    fixture.annotate(
        4,
        AnnotationKind::Note,
        170 * MINUTE_MS,
        200 * MINUTE_MS,
        None,
    );
    fixture.annotate(
        5,
        AnnotationKind::ActiveOverride,
        210 * MINUTE_MS,
        240 * MINUTE_MS,
        None,
    );
    let request = QueryPendingAwayRequest {
        local_date: FIXTURE_DATE.parse().expect("date"),
        cursor: None,
    };
    let QueryResponse::PendingAway(away) = fixture
        .run(QueryRequest::PendingAway(request.clone()))
        .expect("pending away")
    else {
        panic!("pending")
    };
    assert_eq!(
        away.ranges,
        vec![
            quartz_protocol::PendingAway {
                start_utc_ms: fixture.start,
                end_utc_ms: fixture.start + 20 * MINUTE_MS,
                duration_ms: 20 * MINUTE_MS
            },
            quartz_protocol::PendingAway {
                start_utc_ms: fixture.start + 70 * MINUTE_MS,
                end_utc_ms: fixture.start + 100 * MINUTE_MS,
                duration_ms: 30 * MINUTE_MS
            },
            quartz_protocol::PendingAway {
                start_utc_ms: fixture.start + 150 * MINUTE_MS,
                end_utc_ms: fixture.start + 165 * MINUTE_MS,
                duration_ms: 15 * MINUTE_MS
            },
        ]
    );
    let continuation = QueryCursor::new(
        away.versions,
        quartz_protocol::CursorQuery::from(&request),
        1,
    );
    let QueryResponse::PendingAway(next) = fixture
        .run(QueryRequest::PendingAway(QueryPendingAwayRequest {
            cursor: Some(continuation),
            ..request
        }))
        .expect("pending continuation")
    else {
        panic!("pending")
    };
    assert_eq!(next.ranges, away.ranges[1..]);
}

#[test]
fn annotation_scan_pages_by_start_and_id_and_reads_full_intersections() {
    use quartz_protocol::AnnotationKind;
    let mut fixture = QueryFixture::new(1);
    for id in 1..=u32::try_from(READ_BATCH_ROWS + 1).expect("batch count") {
        fixture.annotate(id, AnnotationKind::Note, -1000, 2000, None);
    }
    fixture.annotate(1000, AnnotationKind::Note, -2000, 0, None);
    fixture.annotate(1001, AnnotationKind::Note, 1000, 2000, None);
    let mut snapshot = fixture
        .store
        .take_query_reader()
        .expect("reader")
        .snapshot();
    snapshot.begin().expect("snapshot");
    let mut count = 0;
    while let Some(rows) = snapshot
        .read_batch(QueryRange {
            start: fixture.start,
            end: fixture.start + 1000,
        })
        .expect("batch")
    {
        assert!(rows.len() <= READ_BATCH_ROWS);
        count += rows
            .iter()
            .filter(|row| matches!(row, InputRow::Annotation(_)))
            .count();
    }
    assert_eq!(count, READ_BATCH_ROWS + 1);
    fixture
        .store
        .return_query_reader(snapshot.finish().expect("finish snapshot"));
}

#[test]
fn annotations_written_after_query_start_do_not_enter_the_pinned_snapshot() {
    let mut fixture = QueryFixture::new(1);
    let request = fixture.request_day();
    let mut job = fixture.start(request.clone());
    fixture.annotate(
        1,
        quartz_protocol::AnnotationKind::OfflineActivity,
        1000,
        2000,
        Some(quartz_protocol::BuiltinCategory::Learning),
    );
    let response = loop {
        if let Some(response) = job.advance().expect("snapshot turn") {
            break response;
        }
    };
    fixture
        .store
        .return_query_reader(job.finish().expect("finish snapshot"));
    let QueryResponse::Day(before) = response else {
        panic!("day")
    };
    let QueryResponse::Day(after) = fixture.run(request).expect("updated day") else {
        panic!("day")
    };
    assert_eq!(before.metrics.expect("old metrics").active_ms, 1000);
    assert_eq!(after.metrics.expect("new metrics").active_ms, 2000);
    assert_ne!(before.versions, after.versions);
}

#[test]
fn facts_pages_reassemble_dump_range_and_reject_stale_and_mismatched_cursors() {
    use quartz_protocol::{CursorQuery, QueryFactsRequest};
    let mut fixture = QueryFixture::new(2000);
    fixture.collapse_foreground_times();
    fixture.open_last_fact(5000);
    fixture.mask_idle(0, 1000);
    let mut reader = ReadOnlyStore::open_facts(
        &crate::data_directory::DataDirectory::new(fixture.directory.path()).database(),
    )
    .expect("offline reader");
    let calendar = crate::query::calendar::DayCalendar::new(
        FIXTURE_DATE.parse().expect("date"),
        crate::query::calendar::DayStartOffset::MIDNIGHT,
    )
    .expect("calendar");
    let expected: Vec<_> = reader
        .dump_range(calendar.range.start, calendar.range.end)
        .expect("dump range")
        .iter()
        .map(|fact| fact.record().expect("wire fact"))
        .collect();
    let mut request = QueryFactsRequest {
        local_date: FIXTURE_DATE.parse().expect("date"),
        cursor: None,
    };
    let mut all = Vec::new();
    let mut first_cursor = None;
    let mut pages = 0;
    loop {
        let response = fixture
            .run(QueryRequest::Facts(request.clone()))
            .expect("facts page");
        assert!(serde_json::to_vec(&response).expect("page JSON").len() <= QUERY_PAGE_BYTES);
        let QueryResponse::Facts(page) = response else {
            panic!("facts response")
        };
        pages += 1;
        all.extend(page.facts);
        let Some(cursor) = page.next_cursor else {
            break;
        };
        first_cursor.get_or_insert_with(|| cursor.clone());
        request.cursor = Some(cursor);
    }
    assert!(pages > 1, "fixture must exceed a page");
    assert_eq!(all, expected);
    let cursor = first_cursor.expect("continuation");
    request.cursor = Some(QueryCursor::new(
        cursor.versions().clone(),
        CursorQuery::Day {
            local_date: request.local_date,
        },
        cursor.position(),
    ));
    assert!(matches!(
        fixture.run(QueryRequest::Facts(request.clone())),
        Err(RunQueryError::InvalidCursor)
    ));
    request.cursor = Some(cursor);
    fixture.append_fact();
    assert!(matches!(
        fixture.run(QueryRequest::Facts(request)),
        Err(RunQueryError::StaleCursor)
    ));
}

#[test]
fn facts_cursor_tracks_full_run_endpoints_and_timed_policies() {
    use quartz_protocol::QueryFactsRequest;
    let mut fixture = QueryFixture::new(600);
    let mut request = QueryFactsRequest {
        local_date: FIXTURE_DATE.parse().expect("date"),
        cursor: None,
    };
    let QueryResponse::Facts(page) = fixture
        .run(QueryRequest::Facts(request.clone()))
        .expect("facts")
    else {
        panic!("facts response")
    };
    request.cursor = page.next_cursor;
    assert!(request.cursor.is_some(), "fixture exceeds one page");
    diesel::update(capture_runs::table)
        .set(capture_runs::ended_utc_ms.eq(fixture.start + UTC_DAY_MS + 1000))
        .execute(&mut fixture.store.writer)
        .expect("change full run endpoint");
    assert!(matches!(
        fixture.run(QueryRequest::Facts(request.clone())),
        Err(RunQueryError::StaleCursor)
    ));
    request.cursor = None;
    let QueryResponse::Facts(page) = fixture
        .run(QueryRequest::Facts(request.clone()))
        .expect("fresh facts")
    else {
        panic!("facts response")
    };
    request.cursor = page.next_cursor;
    diesel::insert_into(timed_policies::table)
        .values(TimedPoliciesRow {
            kind: enums::TimedPolicyKind::KeepTiming,
            privacy_mode: None,
            application_id: None,
            started_utc_ms: fixture.start,
            until_utc_ms: fixture.start + 1000,
        })
        .execute(&mut fixture.store.writer)
        .expect("timed policy write");
    assert!(matches!(
        fixture.run(QueryRequest::Facts(request)),
        Err(RunQueryError::StaleCursor)
    ));
}

fn segment_request(fixture: &QueryFixture) -> quartz_protocol::QuerySegmentDetailRequest {
    quartz_protocol::QuerySegmentDetailRequest {
        start_utc_ms: fixture.start,
        end_utc_ms: fixture.start + fixture.count * 1000,
        app: None,
        window_instance: None,
        cursor: None,
    }
}
fn segment_page(
    fixture: &mut QueryFixture,
    request: quartz_protocol::QuerySegmentDetailRequest,
) -> quartz_protocol::QuerySegmentDetailResponse {
    let QueryResponse::SegmentDetail(page) = fixture
        .run(QueryRequest::SegmentDetail(request))
        .expect("segment detail")
    else {
        panic!("segment detail response")
    };
    page
}
fn entity_request(id: i64) -> quartz_protocol::QueryEntityDetailRequest {
    quartz_protocol::QueryEntityDetailRequest {
        entity: quartz_protocol::DetailEntity::App {
            id: quartz_protocol::ApplicationId(id),
        },
        local_date: FIXTURE_DATE.parse().expect("date"),
        cursor: None,
    }
}
fn entity_page(
    fixture: &mut QueryFixture,
    request: quartz_protocol::QueryEntityDetailRequest,
) -> quartz_protocol::QueryEntityDetailResponse {
    let QueryResponse::EntityDetail(page) = fixture
        .run(QueryRequest::EntityDetail(request))
        .expect("entity detail")
    else {
        panic!("entity detail response")
    };
    page
}
#[test]
fn segment_detail_unmerges_clips_filters_and_preserves_capture_privacy() {
    let mut fixture = QueryFixture::new(3);
    diesel::update(intervals::table)
        .set(intervals::app_id.eq(Some(1_i64)))
        .execute(&mut fixture.store.writer)
        .expect("one app");
    for id in 1..=2_i64 {
        diesel::insert_into(crate::store::schema::window_instances::table)
            .values(crate::store::rows::WindowInstancesRow {
                id: Some(id),
                run_id: 1,
                app_id: 1,
                pid: 1,
                process_created_utc_ms: None,
                hwnd: id,
                generation: 1,
                retired_utc_ms: None,
                window_class: None,
                first_seen_utc_ms: fixture.start,
                last_seen_utc_ms: fixture.start,
            })
            .execute(&mut fixture.store.writer)
            .expect("window instance");
    }
    diesel::update(intervals::table)
        .set(intervals::window_instance_id.eq(Some(1_i64)))
        .execute(&mut fixture.store.writer)
        .expect("window lane");
    let QueryRequest::Timeline(mut timeline) = fixture.request_timeline() else {
        panic!("timeline")
    };
    timeline.end_utc_ms = fixture.start + 3000;
    timeline.ms_per_pixel =
        quartz_protocol::MillisecondsPerPixel(std::num::NonZeroU64::new(4000).expect("pixel"));
    let QueryResponse::Timeline(merged) = fixture
        .run(QueryRequest::Timeline(timeline))
        .expect("merged block")
    else {
        panic!("timeline")
    };
    assert_eq!(merged.segments.len(), 1);
    let mut request = segment_request(&fixture);
    request.start_utc_ms += 250;
    request.end_utc_ms -= 250;
    let page = segment_page(&mut fixture, request.clone());
    assert_eq!(page.intervals.len(), 3);
    assert_eq!(page.intervals[0].start_utc_ms, request.start_utc_ms);
    assert_eq!(page.intervals[2].end_utc_ms, request.end_utc_ms);
    assert_eq!(
        page.intervals
            .iter()
            .map(|interval| interval.duration_ms)
            .sum::<i64>(),
        2500
    );
    diesel::update(intervals::table.filter(intervals::id.eq(Some(3_i64))))
        .set(intervals::window_instance_id.eq(Some(2_i64)))
        .execute(&mut fixture.store.writer)
        .expect("second lane");
    request.app = Some(quartz_protocol::ApplicationId(1));
    request.window_instance = Some(quartz_protocol::WindowInstanceRef::from_stored_id(
        quartz_protocol::facts::WindowInstanceId(1),
    ));
    assert_eq!(
        segment_page(&mut fixture, request.clone()).intervals.len(),
        2
    );
    request.app = Some(quartz_protocol::ApplicationId(2));
    assert!(
        segment_page(&mut fixture, request.clone())
            .intervals
            .is_empty()
    );
    request.app = None;
    request.window_instance = None;
    fixture.redact_titles(enums::CaptureLevel::TitleHidden);
    assert!(
        segment_page(&mut fixture, request.clone())
            .intervals
            .iter()
            .all(|interval| interval.title.is_none())
    );
    fixture.redact_titles(enums::CaptureLevel::Excluded);
    let page = segment_page(&mut fixture, request);
    assert!(
        page.intervals
            .iter()
            .all(|interval| interval.activity == Activity::Excluded
                && interval.app.is_none()
                && interval.window_instance.is_none()
                && interval.title.is_none())
    );
}
#[test]
fn segment_detail_pages_bound_ranges_and_reject_changed_versions_and_parameters() {
    let mut fixture = QueryFixture::new(1500);
    let mut request = segment_request(&fixture);
    let mut count = 0;
    loop {
        let page = segment_page(&mut fixture, request.clone());
        assert!(serde_json::to_vec(&page).expect("page encoding").len() <= QUERY_PAGE_BYTES);
        assert_eq!(page.reference_range.is_some(), request.cursor.is_none());
        assert!(
            page.intervals
                .iter()
                .all(|interval| interval.document_ref.is_some())
        );
        count += page.intervals.len();
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    assert_eq!(count, 1500);
    request.cursor = None;
    let page = segment_page(&mut fixture, request.clone());
    request.cursor = page.next_cursor;
    assert!(request.cursor.is_some());
    let mut mismatched = request.clone();
    mismatched.app = Some(quartz_protocol::ApplicationId(1));
    assert_eq!(
        fixture
            .run(QueryRequest::SegmentDetail(mismatched))
            .expect_err("changed lane")
            .kind(),
        ErrorKind::BadRequest
    );
    fixture.bump_fact_version();
    assert_eq!(
        fixture
            .run(QueryRequest::SegmentDetail(request.clone()))
            .expect_err("stale facts")
            .kind(),
        ErrorKind::StaleCursor
    );
    request.cursor = None;
    request.end_utc_ms = request.start_utc_ms + quartz_protocol::SEGMENT_DETAIL_MAX_SPAN_MS + 1;
    assert_eq!(
        fixture
            .run(QueryRequest::SegmentDetail(request.clone()))
            .expect_err("span limit")
            .kind(),
        ErrorKind::BadRequest
    );
    request.end_utc_ms = request.start_utc_ms;
    assert_eq!(
        fixture
            .run(QueryRequest::SegmentDetail(request))
            .expect_err("empty range")
            .kind(),
        ErrorKind::BadRequest
    );
}
#[test]
fn entity_detail_aggregates_app_activity_rules_titles_and_unknown_apps() {
    let mut fixture = QueryFixture::new(3);
    diesel::update(applications::table.filter(applications::id.eq(Some(1_i64))))
        .set(applications::exe_name.eq("code.exe"))
        .execute(&mut fixture.store.writer)
        .expect("classified app");
    fixture.mask_idle(200, 600);
    let request = entity_request(1);
    let page = entity_page(&mut fixture, request.clone());
    let daily = page.daily.as_ref().expect("first-page daily totals");
    let titles = page.titles.as_ref().expect("first-page top titles");
    assert_eq!(daily.len(), quartz_protocol::ENTITY_DETAIL_DAYS);
    assert_eq!(daily.last().expect("last day").active_ms, 600);
    assert_eq!(
        daily.last().expect("last day").local_date,
        request.local_date
    );
    assert!(daily[..6].iter().all(|day| day.active_ms == 0));
    assert_eq!(titles[0].duration_ms, 600);
    assert_eq!(page.rules.len(), 1);
    assert_eq!(
        page.rules[0].rule.as_str(),
        "builtin.classification/vscode@2"
    );
    assert!(page.rules[0].label.is_some() || page.rules[0].label_key.is_some());
    let unknown = entity_page(&mut fixture, entity_request(i64::MAX));
    let daily = unknown.daily.expect("first-page daily totals");
    assert_eq!(daily.len(), quartz_protocol::ENTITY_DETAIL_DAYS);
    assert!(daily.iter().all(|day| day.active_ms == 0));
    assert!(
        unknown.titles.expect("first-page empty titles").is_empty() && unknown.rules.is_empty()
    );
    for level in [
        enums::CaptureLevel::TitleHidden,
        enums::CaptureLevel::Excluded,
    ] {
        fixture.redact_titles(level);
        let page = entity_page(&mut fixture, request.clone());
        assert!(page.titles.expect("first-page empty titles").is_empty());
        if level == enums::CaptureLevel::Excluded {
            assert!(page.rules.is_empty());
            assert!(
                page.daily
                    .expect("first-page daily totals")
                    .iter()
                    .all(|day| day.active_ms == 0)
            );
        }
    }
}

#[test]
fn entity_detail_pages_rule_metadata_and_invalidates_versions() {
    use quartz_domain::{capture::UtcInstant, rules::registry::RegistryCommand};
    let mut fixture = QueryFixture::new(24);
    diesel::update(intervals::table)
        .set(intervals::app_id.eq(Some(1_i64)))
        .execute(&mut fixture.store.writer)
        .expect("one app");
    let mut text = String::from(
        "format='quartz-rules/1'\n[set]\nkey='user.details'\nkind='classification'\nname='Details'\n",
    );
    let label = "x".repeat(12_000);
    for id in 1..=24_i64 {
        let title = format!("detail-title-{id}");
        diesel::insert_into(titles::table)
            .values(crate::store::rows::TitlesRow {
                id: Some(id + 1),
                text: title.clone(),
            })
            .execute(&mut fixture.store.writer)
            .expect("title");
        diesel::update(intervals::table.filter(intervals::id.eq(Some(id))))
            .set(intervals::title_id.eq(Some(id + 1)))
            .execute(&mut fixture.store.writer)
            .expect("title reference");
        text.push_str(&format!("[[rule]]\nid='r-{id}'\nlabel='{label}'\ncategory='builtin:learning'\nwhen=[{{field='title.raw',op='equals',value='{title}'}}]\n"));
    }
    let now = UtcInstant::try_from(0).expect("UTC");
    let mut shell =
        crate::rules::registry::RegistryShell::open(&mut fixture.store, now).expect("rules");
    let key = "user.details".parse().expect("set key");
    shell
        .run(
            &mut fixture.store,
            now,
            RegistryCommand::Apply {
                key,
                file: "details.toml".into(),
                text,
                base_version: 0,
            },
        )
        .expect("classification rules");
    let mut request = entity_request(1);
    let first = entity_page(&mut fixture, request.clone());
    assert_eq!(
        first.daily.as_ref().expect("first-page daily totals").len(),
        quartz_protocol::ENTITY_DETAIL_DAYS
    );
    assert_eq!(
        first.titles.as_ref().expect("first-page top titles").len(),
        quartz_protocol::ENTITY_DETAIL_TOP_TITLES
    );
    let encoded = serde_json::to_value(&first).expect("encoded first page");
    assert!(
        encoded
            .get("daily")
            .is_some_and(serde_json::Value::is_array)
    );
    assert!(
        encoded
            .get("titles")
            .is_some_and(serde_json::Value::is_array)
    );
    let cursor = first.next_cursor.clone().expect("rules exceed one page");
    let mut rules = first.rules.clone();
    request.cursor = Some(cursor);
    loop {
        let page = entity_page(&mut fixture, request.clone());
        assert!(serde_json::to_vec(&page).expect("encoded response").len() <= QUERY_PAGE_BYTES);
        assert!(page.daily.is_none());
        assert!(page.titles.is_none());
        let encoded = serde_json::to_value(&page).expect("encoded continuation page");
        assert!(encoded.get("daily").is_none());
        assert!(encoded.get("titles").is_none());
        rules.extend(page.rules);
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    assert_eq!(rules.len(), 24);
    assert!(rules.windows(2).all(|pair| pair[0].rule < pair[1].rule));
    request.cursor = first.next_cursor;
    fixture.bump_metadata();
    assert_eq!(
        fixture
            .run(QueryRequest::EntityDetail(request.clone()))
            .expect_err("metadata changed")
            .kind(),
        ErrorKind::StaleCursor
    );
    request.cursor = None;
    let page = entity_page(&mut fixture, request.clone());
    request.cursor = page.next_cursor;
    let previous_day = (fixture.start - 3 * UTC_DAY_MS).div_euclid(UTC_DAY_MS);
    diesel::insert_into(crate::store::schema::data_day_versions::table)
        .values((
            crate::store::schema::data_day_versions::source.eq(enums::DataSource::Intervals),
            crate::store::schema::data_day_versions::utc_date.eq(previous_day),
            crate::store::schema::data_day_versions::version.eq(1_i64),
        ))
        .execute(&mut fixture.store.writer)
        .expect("previous logical day's input changed");
    assert_eq!(
        fixture
            .run(QueryRequest::EntityDetail(request.clone()))
            .expect_err("earlier UTC day changed")
            .kind(),
        ErrorKind::StaleCursor
    );
    request.cursor = None;
    let page = entity_page(&mut fixture, request.clone());
    request.cursor = page.next_cursor;
    shell.run(&mut fixture.store, now, RegistryCommand::Apply {
        key: "user.other".parse().expect("set key"), file: "other.toml".into(),
        text: "format='quartz-rules/1'\n[set]\nkey='user.other'\nkind='classification'\nname='Other'\n[[rule]]\nid='other'\nlabel='Other'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='unmatched.exe'}]".into(), base_version: 0,
    }).expect("new rule hash");
    assert_eq!(
        fixture
            .run(QueryRequest::EntityDetail(request))
            .expect_err("rules changed")
            .kind(),
        ErrorKind::StaleCursor
    );
}

#[test]
fn stored_window_class_classifies_closed_and_open_facts_with_hidden_titles() {
    let mut fixture = QueryFixture::new(2);
    diesel::update(intervals::table)
        .set(intervals::app_id.eq(Some(1_i64)))
        .execute(&mut fixture.store.writer)
        .expect("same application");
    for id in 1..=2_i64 {
        diesel::insert_into(crate::store::schema::window_instances::table)
            .values(crate::store::rows::WindowInstancesRow {
                id: Some(id),
                run_id: 1,
                app_id: 1,
                pid: 1,
                process_created_utc_ms: None,
                hwnd: id,
                generation: 1,
                retired_utc_ms: None,
                window_class: Some(if id == 1 { "Editor" } else { "Viewer" }.into()),
                first_seen_utc_ms: fixture.start,
                last_seen_utc_ms: fixture.start,
            })
            .execute(&mut fixture.store.writer)
            .expect("window class");
        diesel::update(intervals::table.filter(intervals::id.eq(Some(id))))
            .set(intervals::window_instance_id.eq(Some(id)))
            .execute(&mut fixture.store.writer)
            .expect("window identity");
    }
    diesel::update(intervals::table.filter(intervals::id.eq(Some(2_i64))))
        .set((
            intervals::end_utc_ms.eq(None::<i64>),
            intervals::end_mono_ms.eq(None::<i64>),
            intervals::confirmed_utc_ms.eq(Some(fixture.start + 2000)),
            intervals::confirmed_mono_ms.eq(Some(2000_i64)),
        ))
        .execute(&mut fixture.store.writer)
        .expect("open foreground");
    let now = quartz_domain::capture::UtcInstant::try_from(0).expect("UTC");
    let mut shell =
        crate::rules::registry::RegistryShell::open(&mut fixture.store, now).expect("rules");
    shell.run(&mut fixture.store, now, quartz_domain::rules::registry::RegistryCommand::Apply {
        key: "user.windows".parse().expect("key"), file: "windows.toml".into(), base_version: 0,
        text: "format='quartz-rules/1'\n[set]\nkey='user.windows'\nkind='classification'\nname='Window class'\n[[rule]]\nid='editor'\nlabel='Editor windows'\ncategory='builtin:learning'\nwhen=[{field='window.class',op='equals',value='Editor'}]".into(),
    }).expect("class rule");
    for level in [enums::CaptureLevel::Full, enums::CaptureLevel::TitleHidden] {
        fixture.redact_titles(level);
        let request = fixture.request_timeline();
        let segments = collect_timeline(&mut fixture, request);
        let foreground: Vec<_> = segments
            .iter()
            .filter(|segment| segment.app.is_some())
            .collect();
        assert_eq!(foreground.len(), 2);
        assert!(matches!(
            foreground[0].category,
            quartz_protocol::CategoryRef::Classified { .. }
        ));
        assert_eq!(
            foreground[1].category,
            quartz_protocol::CategoryRef::Unclassified
        );
        assert_eq!(
            foreground
                .iter()
                .map(|segment| segment.duration_ms)
                .sum::<i64>(),
            2000
        );
    }
}
