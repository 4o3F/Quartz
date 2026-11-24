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
            cursor.decode().expect("issued cursor").versions().clone(),
            cursor.decode().expect("issued cursor").query().clone(),
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
                        label_preset: None,
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
        cursor.decode().expect("issued cursor").versions().clone(),
        CursorQuery::Day {
            local_date: request.local_date,
        },
        cursor.decode().expect("issued cursor").position(),
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
        reference_range: None,
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
    fixture.install_document_rules(
        r"format='quartz-rules/1'
[set]
key='user.document-tests'
kind='extraction'
name='Shared detail identity'
[[rule]]
id='document'
label='Shared identity'
app=[{field='app.exe_name',op='regex',value='.*'}]
pattern='\A(?P<document>detail)-title-[0-9]+\z'
"
        .into(),
    );
    let ranking = fixture.document_ranking();
    assert_eq!(ranking.documents.len(), 1);
    let document_request = ranking.detail_request(0);
    let document_first = entity_page(&mut fixture, document_request.clone());
    assert!(document_first.next_cursor.is_some());
    let detail = fixture.document_detail(document_request.clone());
    assert_eq!(detail.rules.len(), 24);
    assert_eq!(detail.extraction.len(), 1);
    assert_eq!(
        detail
            .daily
            .expect("document days")
            .last()
            .expect("selected day")
            .active_ms,
        24_000
    );
    let mut changed_range = document_request.clone();
    changed_range
        .reference_range
        .as_mut()
        .expect("range")
        .start_utc_ms -= 1;
    changed_range.cursor = document_first.next_cursor.clone();
    assert_eq!(
        fixture
            .run(QueryRequest::EntityDetail(changed_range))
            .expect_err("changed reference range")
            .kind(),
        ErrorKind::BadRequest
    );
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
    let mut document_continuation = document_request;
    document_continuation.cursor = document_first.next_cursor;
    assert_eq!(
        fixture
            .run(QueryRequest::EntityDetail(document_continuation))
            .expect_err("stale document continuation")
            .kind(),
        ErrorKind::StaleDocumentRef
    );

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

#[derive(Debug)]
struct DocumentRanking {
    applications: Vec<quartz_protocol::ApplicationRank>,
    documents: Vec<quartz_protocol::DocumentRank>,
    reference_range: quartz_protocol::ReferenceRange,
    versions: quartz_protocol::QueryVersions,
}
impl QueryFixture {
    fn document_ranking(&mut self) -> DocumentRanking {
        let QueryRequest::Day(mut request) = self.request_day() else {
            panic!("day")
        };
        let mut applications = Vec::new();
        let mut documents = Vec::new();
        let mut reference_range = None;
        let mut versions = None;
        loop {
            let QueryResponse::Day(page) = self
                .run(QueryRequest::Day(request.clone()))
                .expect("ranking page")
            else {
                panic!("day")
            };
            assert!(serde_json::to_vec(&page).expect("page JSON").len() <= QUERY_PAGE_BYTES);
            assert_eq!(page.reference_range.is_some(), request.cursor.is_none());
            assert!(page.applications.is_empty() || page.documents.is_empty());
            if request.cursor.is_none() {
                reference_range = page.reference_range;
                versions = Some(page.versions.clone());
            }
            assert_eq!(Some(&page.versions), versions.as_ref());
            applications.extend(page.applications);
            documents.extend(page.documents);
            let Some(cursor) = page.next_cursor else {
                break;
            };
            request.cursor = Some(cursor);
        }
        DocumentRanking {
            applications,
            documents,
            reference_range: reference_range.expect("first page range"),
            versions: versions.expect("versions"),
        }
    }
    fn document_detail(
        &mut self,
        mut request: quartz_protocol::QueryEntityDetailRequest,
    ) -> quartz_protocol::QueryEntityDetailResponse {
        let mut result = entity_page(self, request.clone());
        let mut next = result.next_cursor.clone();
        while let Some(cursor) = next {
            request.cursor = Some(cursor);
            let page = entity_page(self, request.clone());
            assert!(serde_json::to_vec(&page).expect("detail JSON").len() <= QUERY_PAGE_BYTES);
            assert!(page.daily.is_none() && page.titles.is_none());
            assert_eq!(page.versions, result.versions);
            assert_ne!(page.next_cursor, request.cursor);
            result.rules.extend(page.rules);
            result.extraction.extend(page.extraction);
            next = page.next_cursor;
        }
        result.next_cursor = None;
        result
    }
    fn replace_document_title(&mut self, fact: i64, title: &str) {
        diesel::insert_into(titles::table)
            .values(titles::text.eq(title))
            .on_conflict_do_nothing()
            .execute(&mut self.store.writer)
            .expect("title");
        let title_id = titles::table
            .filter(titles::text.eq(title))
            .select(titles::id.assume_not_null())
            .first::<i64>(&mut self.store.writer)
            .expect("title id");
        diesel::update(intervals::table.filter(intervals::id.eq(Some(fact))))
            .set(intervals::title_id.eq(Some(title_id)))
            .execute(&mut self.store.writer)
            .expect("title reference");
    }
    fn install_document_rules(&mut self, text: String) {
        let now = quartz_domain::capture::UtcInstant::try_from(0).expect("UTC");
        let mut shell =
            crate::rules::registry::RegistryShell::open(&mut self.store, now).expect("registry");
        shell
            .run(
                &mut self.store,
                now,
                quartz_domain::rules::registry::RegistryCommand::Apply {
                    key: "user.document-tests".parse().expect("key"),
                    file: "document-tests.toml".into(),
                    text,
                    base_version: 0,
                },
            )
            .expect("document rules");
    }
    fn use_document_app(&mut self) {
        diesel::update(intervals::table)
            .set(intervals::app_id.eq(Some(1_i64)))
            .execute(&mut self.store.writer)
            .expect("one app");
        diesel::update(applications::table.filter(applications::id.eq(Some(1_i64))))
            .set(applications::exe_name.eq("code.exe"))
            .execute(&mut self.store.writer)
            .expect("editor");
    }
}
impl DocumentRanking {
    fn detail_request(&self, index: usize) -> quartz_protocol::QueryEntityDetailRequest {
        quartz_protocol::QueryEntityDetailRequest {
            entity: quartz_protocol::DetailEntity::Document {
                document_ref: self.documents[index].document_ref.clone(),
            },
            local_date: FIXTURE_DATE.parse().expect("date"),
            reference_range: Some(self.reference_range),
            cursor: None,
        }
    }
}

#[test]
fn document_ranking_agrees_with_raw_timeline_and_daily_details_and_keeps_identity_components() {
    let mut fixture = QueryFixture::new(9);
    fixture.use_document_app();
    fixture.install_document_rules(
        r"format='quartz-rules/1'
[set]
key='user.document-tests'
kind='extraction'
name='Document identities'
[[rule]]
id='parts'
label='Document parts'
app=[{field='app.exe_name',op='regex',value='.*'}]
pattern='\A(?P<document>[^|]+)(?:\|(?P<project>[^|]*))?(?:\|(?P<profile>[^|]*))?\z'
"
        .into(),
    );
    let titles = [
        "main.rs",
        "main.rs|",
        "main.rs||",
        "main.rs||Work",
        "main.rs|Quartz",
        "main.rs|Other",
        "Main.rs",
        "café.rs",
        "cafe\u{301}.rs",
    ];
    for (index, title) in titles.iter().enumerate() {
        fixture.replace_document_title(i64::try_from(index).expect("index") + 1, title);
    }
    fixture.mask_idle(250, 500);
    // Same visible text in another application remains a separate identity.
    diesel::update(intervals::table.filter(intervals::id.eq(Some(7_i64))))
        .set(intervals::app_id.eq(Some(2_i64)))
        .execute(&mut fixture.store.writer)
        .expect("second app");
    let ranking = fixture.document_ranking();
    assert_eq!(
        ranking.documents.len(),
        8,
        "canonical-equivalent Unicode titles share an identity"
    );
    assert!(
        ranking
            .documents
            .iter()
            .map(|row| row.active_ms)
            .sum::<i64>()
            <= ranking
                .applications
                .iter()
                .map(|row| row.active_ms)
                .sum::<i64>()
    );
    let request = fixture.request_timeline();
    let timeline = collect_timeline(&mut fixture, request);
    for (index, row) in ranking.documents.iter().enumerate() {
        let duration: i64 = timeline
            .iter()
            .filter(|segment| {
                matches!(
                    segment.activity,
                    Activity::Active | Activity::ActiveOverride { .. }
                ) && segment.app.as_ref().is_some_and(|app| app.id == row.app)
                    && segment.extraction.as_ref().is_some_and(|fields| {
                        fields
                            .document
                            .as_deref()
                            .map(quartz_domain::text::NfcText::new)
                            .is_some_and(|name| name.as_str() == row.document)
                            && fields.project == row.project
                            && fields.profile == row.profile
                    })
            })
            .map(|segment| segment.duration_ms)
            .sum();
        assert_eq!(duration, row.active_ms, "timeline identity {index}");
        let detail = fixture.document_detail(ranking.detail_request(index));
        assert_eq!(
            detail
                .daily
                .expect("daily")
                .last()
                .expect("selected day")
                .active_ms,
            row.active_ms
        );
        assert!(!detail.extraction.is_empty());
    }
    let same_name: Vec<_> = ranking
        .documents
        .iter()
        .filter(|row| row.document == "main.rs" && row.active_ms == 1000)
        .collect();
    assert_eq!(
        same_name
            .iter()
            .map(|row| row.project.as_deref())
            .collect::<Vec<_>>(),
        vec![Some(""), Some(""), Some(""), Some("Other"), Some("Quartz")]
    );
    assert_eq!(
        same_name[..3]
            .iter()
            .map(|row| row.profile.as_deref())
            .collect::<Vec<_>>(),
        vec![None, Some(""), Some("Work")]
    );
    let date: quartz_protocol::LocalDate = FIXTURE_DATE.parse().expect("date");
    let first = date
        .checked_sub(
            jiff::Span::new()
                .days(i64::try_from(quartz_protocol::ENTITY_DETAIL_DAYS - 1).expect("days")),
        )
        .expect("first day");
    let start = crate::query::calendar::DayCalendar::new(
        first,
        crate::query::calendar::DayStartOffset::MIDNIGHT,
    )
    .expect("first calendar")
    .range
    .start;
    assert_eq!(ranking.reference_range.start_utc_ms, start);
    assert_eq!(
        ranking.reference_range.end_utc_ms,
        fixture.start + UTC_DAY_MS
    );
    assert!(!ranking.versions.0.is_empty());
}

#[test]
fn document_ranking_null_precedes_empty_and_text_keeps_scalar_order() {
    let mut fixture = QueryFixture::new(7);
    fixture.use_document_app();
    fixture.install_document_rules(
        r"format='quartz-rules/1'
[set]
key='user.document-tests'
kind='extraction'
name='Ordering'
[[rule]]
id='parts'
label='Document parts'
app=[{field='app.exe_name',op='regex',value='.*'}]
pattern='\A(?P<document>[^|]+)(?:\|(?P<project>[^|]*))?(?:\|(?P<profile>.*))?\z'
"
        .into(),
    );
    for (index, title) in [
        "main.rs|",
        "main.rs",
        "main.rs||",
        "main.rs|é",
        "main.rs|Z",
        "main.rs|a",
        "main.rs||Work",
    ]
    .iter()
    .enumerate()
    {
        fixture.replace_document_title(i64::try_from(index).expect("index") + 1, title);
    }
    let ranking = fixture.document_ranking();
    assert_eq!(
        ranking
            .documents
            .iter()
            .map(|row| row.project.as_deref())
            .collect::<Vec<_>>(),
        vec![
            None,
            Some(""),
            Some(""),
            Some(""),
            Some("Z"),
            Some("a"),
            Some("é")
        ]
    );
    assert_eq!(
        ranking.documents[1..4]
            .iter()
            .map(|row| row.profile.as_deref())
            .collect::<Vec<_>>(),
        vec![None, Some(""), Some("Work")]
    );
}

#[test]
fn document_reference_rejects_tampering_missing_or_short_ranges_and_changed_inputs() {
    let mut fixture = QueryFixture::new(1);
    let ranking = fixture.document_ranking();
    let request = ranking.detail_request(0);
    for reference_range in [
        None,
        Some(quartz_protocol::ReferenceRange {
            start_utc_ms: fixture.start,
            end_utc_ms: ranking.reference_range.end_utc_ms,
        }),
        Some(quartz_protocol::ReferenceRange {
            start_utc_ms: ranking.reference_range.start_utc_ms - 1,
            ..ranking.reference_range
        }),
    ] {
        let mut invalid = request.clone();
        invalid.reference_range = reference_range;
        assert_eq!(
            fixture
                .run(QueryRequest::EntityDetail(invalid))
                .expect_err("invalid reference range")
                .kind(),
            ErrorKind::BadRequest
        );
    }
    let mut tampered = request.clone();
    let quartz_protocol::DetailEntity::Document { document_ref } = &mut tampered.entity else {
        panic!("document")
    };
    *document_ref = quartz_protocol::DocumentRef::from_encoded(
        document_ref
            .as_str()
            .replace("private title fixture", "changed title"),
    );
    assert_eq!(
        fixture
            .run(QueryRequest::EntityDetail(tampered))
            .expect_err("checksum")
            .kind(),
        ErrorKind::BadRequest
    );
    fixture.bump_fact_version();
    assert_eq!(
        fixture
            .run(QueryRequest::EntityDetail(request))
            .expect_err("stale reference")
            .kind(),
        ErrorKind::StaleDocumentRef
    );
    let refreshed = fixture.document_ranking();
    let request = refreshed.detail_request(0);
    fixture.bump_metadata();
    assert_eq!(
        fixture
            .run(QueryRequest::EntityDetail(request))
            .expect_err("stale metadata")
            .kind(),
        ErrorKind::StaleDocumentRef
    );
}

#[test]
fn document_ranking_pages_are_complete_and_previous_day_changes_stale_the_cursor() {
    let mut fixture = QueryFixture::new(900);
    let QueryRequest::Day(mut request) = fixture.request_day() else {
        panic!("day")
    };
    let mut documents = Vec::new();
    let mut document_pages = 0;
    let mut continuation = None;
    loop {
        let QueryResponse::Day(page) = fixture
            .run(QueryRequest::Day(request.clone()))
            .expect("day")
        else {
            panic!("day")
        };
        assert!(serde_json::to_vec(&page).expect("JSON").len() <= QUERY_PAGE_BYTES);
        if !page.documents.is_empty() {
            document_pages += 1;
            if continuation.is_none() {
                continuation = page.next_cursor.clone();
            }
        }
        documents.extend(page.documents);
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    assert!(document_pages > 1);
    assert_eq!(documents.len(), 900);
    assert!(documents.windows(2).all(|pair| pair[0].app < pair[1].app));
    request.cursor = continuation;
    assert!(request.cursor.is_some());
    diesel::insert_into(crate::store::schema::data_day_versions::table)
        .values(crate::store::rows::DataDayVersionsRow {
            source: enums::DataSource::Intervals,
            utc_date: (fixture.start - 3 * UTC_DAY_MS).div_euclid(UTC_DAY_MS),
            version: 1,
        })
        .execute(&mut fixture.store.writer)
        .expect("previous day version");
    assert_eq!(
        fixture
            .run(QueryRequest::Day(request))
            .expect_err("stale seven-day scope")
            .kind(),
        ErrorKind::StaleCursor
    );
}

#[test]
fn title_hidden_and_excluded_facts_never_produce_document_rankings() {
    let mut fixture = QueryFixture::new(3);
    let ranking = fixture.document_ranking();
    let request = ranking.detail_request(0);
    for level in [
        enums::CaptureLevel::TitleHidden,
        enums::CaptureLevel::Excluded,
    ] {
        fixture.redact_titles(level);
        fixture.bump_fact_version();
        let ranking = fixture.document_ranking();
        assert!(ranking.documents.is_empty());
        assert_eq!(
            ranking.applications.is_empty(),
            level == enums::CaptureLevel::Excluded
        );
        let QueryRequest::Timeline(timeline) = fixture.request_timeline() else {
            panic!("timeline")
        };
        let page = collect_timeline(&mut fixture, QueryRequest::Timeline(timeline));
        assert!(page.iter().all(|segment| segment.document_ref.is_none()));
        assert_eq!(
            fixture
                .run(QueryRequest::EntityDetail(request.clone()))
                .expect_err("old visible reference")
                .kind(),
            ErrorKind::StaleDocumentRef
        );
    }
}

#[test]
fn document_detail_collects_clean_dirty_and_fallback_titles_and_final_rules() {
    let mut fixture = QueryFixture::new(3);
    fixture.use_document_app();
    for (index, title) in [
        "main.rs - Visual Studio Code",
        "● main.rs - Visual Studio Code",
        "main.rs - Application 1",
    ]
    .iter()
    .enumerate()
    {
        fixture.replace_document_title(i64::try_from(index).expect("index") + 1, title);
    }
    fixture.mask_idle(1000, 2000);
    fixture.annotate(
        1,
        quartz_protocol::AnnotationKind::ActiveOverride,
        1000,
        2000,
        None,
    );
    let ranking = fixture.document_ranking();
    assert_eq!(ranking.documents.len(), 1);
    assert_eq!(ranking.documents[0].active_ms, 3000);
    let detail = fixture.document_detail(ranking.detail_request(0));
    let titles = detail.titles.expect("titles");
    assert_eq!(titles.len(), 3);
    assert_eq!(
        titles.iter().map(|title| title.duration_ms).sum::<i64>(),
        3000
    );
    assert_eq!(
        detail.extraction,
        vec![
            quartz_protocol::EntityExtractionProvenance {
                extractor_rule: None,
                status: quartz_protocol::ExtractionStatus::Fallback
            },
            quartz_protocol::EntityExtractionProvenance {
                extractor_rule: Some("builtin.extraction/vscode-document@1".into()),
                status: quartz_protocol::ExtractionStatus::Extracted
            }
        ]
    );
    assert_eq!(detail.rules.len(), 1);
    assert_eq!(
        detail.rules[0].rule.as_str(),
        "builtin.classification/vscode@2"
    );
    fixture.annotate(
        2,
        quartz_protocol::AnnotationKind::CategoryOverride,
        0,
        3000,
        Some(quartz_protocol::BuiltinCategory::Learning),
    );
    let ranking = fixture.document_ranking();
    assert!(
        fixture
            .document_detail(ranking.detail_request(0))
            .rules
            .is_empty(),
        "overridden classification rules supplied no final classification"
    );
}

#[test]
fn document_entity_each_day_matches_that_days_ranking() {
    let mut fixture = QueryFixture::new(2);
    fixture.use_document_app();
    const DAYS_BEFORE: i64 = 3;
    let origin = fixture.start - DAYS_BEFORE * UTC_DAY_MS;
    diesel::update(capture_runs::table)
        .set(capture_runs::started_utc_ms.eq(origin))
        .execute(&mut fixture.store.writer)
        .expect("run origin");
    let mut clock = clock_segments::table
        .select(crate::store::rows::ClockSegmentsRow::as_select())
        .first::<crate::store::rows::ClockSegmentsRow>(&mut fixture.store.writer)
        .expect("clock");
    clock.start_utc_ms = origin;
    clock.end_mono_ms = Some((DAYS_BEFORE + 1) * UTC_DAY_MS);
    diesel::update(clock_segments::table)
        .set(&clock)
        .execute(&mut fixture.store.writer)
        .expect("clock origin");
    for id in 1..=2_i64 {
        let mut row = intervals::table
            .filter(intervals::id.eq(Some(id)))
            .select(crate::store::rows::IntervalsRow::as_select())
            .first::<crate::store::rows::IntervalsRow>(&mut fixture.store.writer)
            .expect("fact");
        if id == 1 {
            row.start_mono_ms = DAYS_BEFORE * UTC_DAY_MS;
            row.end_mono_ms = Some(row.start_mono_ms + 1000);
        } else {
            row.start_utc_ms = origin;
            row.end_utc_ms = Some(origin + 2000);
            row.start_mono_ms = 0;
            row.end_mono_ms = Some(2000);
        }
        diesel::update(intervals::table.filter(intervals::id.eq(Some(id))))
            .set(&row)
            .execute(&mut fixture.store.writer)
            .expect("fact boundaries");
    }
    let ranking = fixture.document_ranking();
    let detail = fixture.document_detail(ranking.detail_request(0));
    let daily = detail.daily.expect("seven days");
    assert_eq!(daily.iter().map(|day| day.active_ms).sum::<i64>(), 3000);
    for day in daily {
        let mut request = quartz_protocol::QueryDayRequest {
            local_date: day.local_date,
            cursor: None,
        };
        let mut active_ms = 0;
        loop {
            let QueryResponse::Day(page) = fixture
                .run(QueryRequest::Day(request.clone()))
                .expect("day")
            else {
                panic!("day")
            };
            active_ms += page
                .documents
                .iter()
                .map(|document| document.active_ms)
                .sum::<i64>();
            let Some(cursor) = page.next_cursor else {
                break;
            };
            request.cursor = Some(cursor);
        }
        assert_eq!(day.active_ms, active_ms, "date {}", day.local_date);
    }
}

#[test]
fn document_details_validate_versions_in_the_entire_original_timeline_range() {
    let mut fixture = QueryFixture::new(1);
    let QueryRequest::Timeline(mut timeline) = fixture.request_timeline() else {
        panic!("timeline")
    };
    timeline.start_utc_ms = fixture.start - 7 * UTC_DAY_MS;
    timeline.end_utc_ms = fixture.start + UTC_DAY_MS;
    let QueryResponse::Timeline(page) = fixture
        .run(QueryRequest::Timeline(timeline))
        .expect("eight-day timeline")
    else {
        panic!("timeline")
    };
    let document_ref = page
        .segments
        .iter()
        .find_map(|segment| segment.document_ref.clone())
        .expect("visible document");
    let request = quartz_protocol::QueryEntityDetailRequest {
        entity: quartz_protocol::DetailEntity::Document { document_ref },
        local_date: FIXTURE_DATE.parse().expect("date"),
        reference_range: page.reference_range,
        cursor: None,
    };
    let detail = fixture.document_detail(request.clone());
    assert_eq!(
        detail
            .daily
            .expect("daily")
            .last()
            .expect("selected day")
            .active_ms,
        1000
    );
    diesel::insert_into(crate::store::schema::data_day_versions::table)
        .values(crate::store::rows::DataDayVersionsRow {
            source: enums::DataSource::Intervals,
            utc_date: (fixture.start - 7 * UTC_DAY_MS).div_euclid(UTC_DAY_MS),
            version: 1,
        })
        .execute(&mut fixture.store.writer)
        .expect("version outside detail within reference");
    assert_eq!(
        fixture
            .run(QueryRequest::EntityDetail(request))
            .expect_err("original reference inputs changed")
            .kind(),
        ErrorKind::StaleDocumentRef
    );
}

#[test]
fn empty_unknown_and_workspace_only_extractions_are_excluded_from_rankings() {
    let mut fixture = QueryFixture::new(4);
    fixture.use_document_app();
    fixture.replace_document_title(1, "");
    fixture.replace_document_title(2, "Quartz (Workspace) - Visual Studio Code");
    diesel::update(intervals::table.filter(intervals::id.eq(Some(3_i64))))
        .set(intervals::title_id.eq(None::<i64>))
        .execute(&mut fixture.store.writer)
        .expect("unavailable title");
    let ranking = fixture.document_ranking();
    assert_eq!(ranking.applications[0].active_ms, 4000);
    assert_eq!(ranking.documents.len(), 1);
    assert_eq!(ranking.documents[0].active_ms, 1000);
}

impl QueryFixture {
    fn unclassified_request(
        &self,
        group_by: quartz_protocol::UnclassifiedGroupBy,
    ) -> quartz_protocol::QueryUnclassifiedRequest {
        let start_date: quartz_protocol::LocalDate = FIXTURE_DATE.parse().expect("date");
        quartz_protocol::QueryUnclassifiedRequest {
            start_date,
            end_date: start_date.tomorrow().expect("next date"),
            group_by,
            search: None,
            cursor: None,
        }
    }
    fn unclassified(
        &mut self,
        request: quartz_protocol::QueryUnclassifiedRequest,
    ) -> quartz_protocol::QueryUnclassifiedResponse {
        let response = self
            .run(QueryRequest::Unclassified(request))
            .expect("unclassified query");
        assert!(serde_json::to_vec(&response).expect("wire").len() <= QUERY_PAGE_BYTES);
        let QueryResponse::Unclassified(page) = response else {
            panic!("unclassified response")
        };
        page
    }
    fn unclassified_metric(&mut self) -> i64 {
        let request = self.request_day();
        let QueryResponse::Day(day) = self.run(request).expect("day") else {
            panic!("day response")
        };
        day.metrics.expect("first page metrics").unclassified_ms
    }
}
#[test]
fn unclassified_groups_equal_day_metric_with_apps_and_omit_entityless_active_time() {
    use quartz_protocol::{AnnotationKind, BuiltinCategory, UnclassifiedGroupBy};
    let mut fixture = QueryFixture::new(4);
    fixture.mask_idle(500, 750);
    fixture.annotate(
        1,
        AnnotationKind::CategoryOverride,
        1000,
        2000,
        Some(BuiltinCategory::Learning),
    );
    fixture.annotate(2, AnnotationKind::ActiveOverride, 500, 750, None);
    fixture.annotate(3, AnnotationKind::Note, 0, 500, None);
    let request = fixture.unclassified_request(UnclassifiedGroupBy::App);
    let page = fixture.unclassified(request);
    assert_eq!(
        page.items.len(),
        3,
        "the classified app is removed; note and unclassified active override remain"
    );
    let grouped_ms: i64 = page.items.iter().map(|item| item.active_ms).sum();
    assert_eq!(grouped_ms, 3000);
    assert_eq!(
        grouped_ms,
        fixture.unclassified_metric(),
        "without search, every unclassified active slice with a visible app contributes exactly once"
    );
    fixture.mask_idle(3000, 4000);
    fixture.annotate(4, AnnotationKind::OfflineActivity, 3000, 4000, None);
    let request = fixture.unclassified_request(UnclassifiedGroupBy::App);
    let page = fixture.unclassified(request);
    let grouped_ms: i64 = page.items.iter().map(|item| item.active_ms).sum();
    assert_eq!(grouped_ms, 2000);
    assert_eq!(
        fixture.unclassified_metric() - grouped_ms,
        1000,
        "entityless offline time is the exact omitted duration"
    );
    let request = fixture.unclassified_request(UnclassifiedGroupBy::Document);
    let documents = fixture.unclassified(request);
    assert_eq!(
        documents
            .items
            .iter()
            .map(|item| item.active_ms)
            .sum::<i64>(),
        grouped_ms,
        "these captured facts all have document identities"
    );
    fixture.replace_document_title(1, "");
    let request = fixture.unclassified_request(UnclassifiedGroupBy::Document);
    let documents = fixture.unclassified(request);
    let document_ms: i64 = documents.items.iter().map(|item| item.active_ms).sum();
    assert_eq!(document_ms, 1000);
    assert!(document_ms <= grouped_ms && grouped_ms <= fixture.unclassified_metric());
}
#[test]
fn unclassified_privacy_hides_stored_titles_and_excluded_apps_from_search_and_groups() {
    use quartz_protocol::{UnclassifiedGroupBy, WorkEntity};
    let mut fixture = QueryFixture::new(3);
    diesel::update(intervals::table.filter(intervals::id.eq(Some(1_i64))))
        .set(intervals::capture_level.eq(enums::CaptureLevel::Excluded))
        .execute(&mut fixture.store.writer)
        .expect("excluded");
    diesel::update(intervals::table.filter(intervals::id.eq(Some(2_i64))))
        .set(intervals::capture_level.eq(enums::CaptureLevel::TitleHidden))
        .execute(&mut fixture.store.writer)
        .expect("hidden title");
    fixture.replace_document_title(3, "");
    let request = fixture.unclassified_request(UnclassifiedGroupBy::App);
    let page = fixture.unclassified(request);
    assert_eq!(page.items.len(), 2);
    assert!(matches!(
        page.items[0].entity,
        WorkEntity::App {
            id: quartz_protocol::ApplicationId(2),
            ..
        }
    ));
    let wire = serde_json::to_string(&page).expect("wire");
    assert!(!wire.contains("private title fixture"));
    assert!(!wire.contains("Application 1"));
    for grouping in [UnclassifiedGroupBy::App, UnclassifiedGroupBy::Document] {
        let mut request = fixture.unclassified_request(grouping);
        request.search = Some("private title fixture".into());
        assert!(
            fixture.unclassified(request).items.is_empty(),
            "hidden fields never match"
        );
    }
    let request = fixture.unclassified_request(UnclassifiedGroupBy::Document);
    assert!(
        fixture.unclassified(request).items.is_empty(),
        "TitleHidden and empty titles have no document group"
    );
}
#[test]
fn unclassified_paging_is_complete_filtered_before_paging_and_rejects_stale_or_changed_requests() {
    use quartz_protocol::{UnclassifiedGroupBy, WorkEntity};
    let mut fixture = QueryFixture::new(1000);
    for grouping in [UnclassifiedGroupBy::App, UnclassifiedGroupBy::Document] {
        let mut request = fixture.unclassified_request(grouping);
        let first = fixture.unclassified(request.clone());
        let reference_range = first.reference_range.expect("first page reference range");
        assert_eq!(reference_range.start_utc_ms, fixture.start);
        let versions = first.versions.clone();
        let cursor = first.next_cursor.clone().expect("multiple pages");
        let mut items = first.items;
        request.cursor = Some(cursor.clone());
        loop {
            let page = fixture.unclassified(request.clone());
            assert!(page.reference_range.is_none());
            assert_eq!(page.versions, versions);
            items.extend(page.items);
            let Some(next) = page.next_cursor else { break };
            request.cursor = Some(next);
        }
        assert_eq!(items.len(), 1000);
        for (index, item) in items.iter().enumerate() {
            let app = match &item.entity {
                WorkEntity::App { id, .. } => *id,
                WorkEntity::Document {
                    app, document_ref, ..
                } => {
                    let raw_request =
                        QueryRequest::SegmentDetail(quartz_protocol::QuerySegmentDetailRequest {
                            start_utc_ms: reference_range.start_utc_ms,
                            end_utc_ms: reference_range.end_utc_ms,
                            app: Some(*app),
                            window_instance: None,
                            cursor: None,
                        });
                    if index == 0 || index == items.len() - 1 {
                        let QueryResponse::SegmentDetail(raw) =
                            fixture.run(raw_request).expect("same range raw projection")
                        else {
                            panic!("raw response")
                        };
                        assert_eq!(raw.versions, versions);
                        assert!(
                            raw.intervals
                                .iter()
                                .any(|segment| segment.document_ref.as_ref() == Some(document_ref)),
                            "document references agree across queries over the same range"
                        );
                    }
                    *app
                }
            };
            assert_eq!(app.0, i64::try_from(index).expect("index") + 1);
        }
        request.cursor = Some(cursor.clone());
        request.search = Some("Application 1".into());
        assert_eq!(
            fixture
                .run(QueryRequest::Unclassified(request.clone()))
                .expect_err("changed search")
                .kind(),
            ErrorKind::BadRequest
        );
        request.cursor = None;
        let page = fixture.unclassified(request.clone());
        assert!(
            page.next_cursor.is_none(),
            "filter applied before the page budget"
        );
        assert_eq!(page.items.len(), 112);
        request.search = None;
        request.cursor = Some(QueryCursor::new(
            versions.clone(),
            quartz_protocol::CursorQuery::from(&request),
            usize::MAX,
        ));
        assert_eq!(
            fixture
                .run(QueryRequest::Unclassified(request.clone()))
                .expect_err("invalid position")
                .kind(),
            ErrorKind::BadRequest
        );
        request.cursor = Some(cursor);
        fixture.bump_fact_version();
        assert_eq!(
            fixture
                .run(QueryRequest::Unclassified(request))
                .expect_err("changed inputs")
                .kind(),
            ErrorKind::StaleCursor
        );
    }
}
#[test]
fn unclassified_folds_multiple_days_and_binds_the_entire_request_range() {
    use quartz_protocol::UnclassifiedGroupBy;
    let mut fixture = QueryFixture::new(2);
    let tomorrow = fixture.start + UTC_DAY_MS;
    diesel::update(capture_runs::table)
        .set(capture_runs::ended_utc_ms.eq(Some(tomorrow + UTC_DAY_MS)))
        .execute(&mut fixture.store.writer)
        .expect("run end");
    diesel::update(clock_segments::table)
        .set(clock_segments::end_utc_ms.eq(Some(tomorrow + UTC_DAY_MS)))
        .execute(&mut fixture.store.writer)
        .expect("clock end");
    diesel::update(clock_segments::table)
        .set(clock_segments::end_mono_ms.eq(Some(2 * UTC_DAY_MS)))
        .execute(&mut fixture.store.writer)
        .expect("clock duration");
    let mut row = intervals::table
        .filter(intervals::id.eq(Some(2_i64)))
        .select(crate::store::rows::IntervalsRow::as_select())
        .first::<crate::store::rows::IntervalsRow>(&mut fixture.store.writer)
        .expect("second fact");
    row.start_utc_ms = tomorrow;
    row.end_utc_ms = Some(tomorrow + 1000);
    row.start_mono_ms = UTC_DAY_MS;
    row.end_mono_ms = Some(UTC_DAY_MS + 1000);
    row.app_id = Some(1);
    diesel::update(intervals::table.filter(intervals::id.eq(Some(2_i64))))
        .set(&row)
        .execute(&mut fixture.store.writer)
        .expect("next-day fact");
    let mut request = fixture.unclassified_request(UnclassifiedGroupBy::Document);
    request.end_date = request.end_date.tomorrow().expect("two days");
    let page = fixture.unclassified(request.clone());
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].active_ms, 2000);
    assert_eq!(page.items[0].sample_ranges.len(), 2);
    assert_eq!(page.items[0].sample_ranges[1].start_utc_ms, tomorrow);
    assert_eq!(
        page.reference_range.expect("range").end_utc_ms,
        tomorrow + UTC_DAY_MS
    );
    let mut sum = 0;
    for date in [
        request.start_date,
        request.start_date.tomorrow().expect("tomorrow"),
    ] {
        let QueryResponse::Day(day) = fixture
            .run(QueryRequest::Day(quartz_protocol::QueryDayRequest {
                local_date: date,
                cursor: None,
            }))
            .expect("daily baseline")
        else {
            panic!("day")
        };
        sum += day.metrics.expect("first metrics").unclassified_ms;
    }
    assert_eq!(page.items[0].active_ms, sum);
    let mut cursor_request = request.clone();
    cursor_request.cursor = Some(QueryCursor::new(
        page.versions,
        quartz_protocol::CursorQuery::from(&request),
        0,
    ));
    let next_day = tomorrow.div_euclid(UTC_DAY_MS);
    diesel::insert_into(crate::store::schema::data_day_versions::table)
        .values(crate::store::rows::DataDayVersionsRow {
            source: enums::DataSource::Intervals,
            utc_date: next_day,
            version: 1,
        })
        .execute(&mut fixture.store.writer)
        .expect("later input version");
    assert_eq!(
        fixture
            .run(QueryRequest::Unclassified(cursor_request))
            .expect_err("later day changed")
            .kind(),
        ErrorKind::StaleCursor
    );
}
#[test]
fn unclassified_invalid_dates_and_search_return_bad_request_and_restore_the_reader() {
    use quartz_protocol::UnclassifiedGroupBy;
    let mut fixture = QueryFixture::new(1);
    let mut request = fixture.unclassified_request(UnclassifiedGroupBy::App);
    request.end_date = request.start_date;
    assert_eq!(
        fixture
            .run(QueryRequest::Unclassified(request.clone()))
            .expect_err("empty range")
            .kind(),
        ErrorKind::BadRequest
    );
    request.end_date = "2026-01-04".parse().expect("date");
    assert_eq!(
        fixture
            .run(QueryRequest::Unclassified(request.clone()))
            .expect_err("too many logical days")
            .kind(),
        ErrorKind::BadRequest
    );
    request.end_date = request.start_date.tomorrow().expect("date");
    request.search = Some("界".repeat(quartz_protocol::UNCLASSIFIED_MAX_SEARCH_SCALARS + 1));
    assert_eq!(
        fixture
            .run(QueryRequest::Unclassified(request))
            .expect_err("search bound")
            .kind(),
        ErrorKind::BadRequest
    );
    let request = fixture.unclassified_request(UnclassifiedGroupBy::App);
    assert_eq!(fixture.unclassified(request).items.len(), 1);
}

#[test]
fn unclassified_document_ties_use_canonical_identity_and_distinguish_null_and_empty() {
    use quartz_protocol::{UnclassifiedGroupBy, WorkEntity};
    let mut fixture = QueryFixture::new(5);
    diesel::update(intervals::table)
        .set(intervals::app_id.eq(Some(1_i64)))
        .execute(&mut fixture.store.writer)
        .expect("same app");
    fixture.install_document_rules("format='quartz-rules/1'\n[set]\nkey='user.document-tests'\nkind='extraction'\nname='Parts'\n[[rule]]\nid='parts'\nlabel='Parts'\napp=[{field='app.exe_name',op='regex',value='.*'}]\npattern='\\A(?P<document>[^|]+)(?:\\|(?P<project>[^|]*))?(?:\\|(?P<profile>[^|]*))?\\z'".into());
    for (index, title) in [
        "main.rs||",
        "main.rs|",
        "main.rs",
        "cafe\u{301}.rs",
        "café.rs",
    ]
    .iter()
    .enumerate()
    {
        fixture.replace_document_title(i64::try_from(index).expect("index") + 1, title);
    }
    let request = fixture.unclassified_request(UnclassifiedGroupBy::Document);
    let page = fixture.unclassified(request);
    assert_eq!(page.items.len(), 4);
    assert_eq!(page.items[0].active_ms, 2000);
    let WorkEntity::Document { document, .. } = &page.items[0].entity else {
        panic!("document")
    };
    assert_eq!(document, "café.rs");
    for (index, item) in page.items.iter().enumerate().skip(1) {
        let WorkEntity::Document {
            document,
            project,
            profile,
            ..
        } = &item.entity
        else {
            panic!("document")
        };
        assert_eq!(document, "main.rs");
        assert_eq!(project.as_deref(), if index == 1 { None } else { Some("") });
        assert_eq!(profile.as_deref(), if index == 3 { Some("") } else { None });
    }
}
#[test]
fn unclassified_uses_final_rule_classification_and_classified_active_overrides() {
    use quartz_protocol::{AnnotationKind, BuiltinCategory, UnclassifiedGroupBy};
    let mut fixture = QueryFixture::new(3);
    diesel::update(applications::table.filter(applications::id.eq(Some(1_i64))))
        .set(applications::exe_name.eq("code.exe"))
        .execute(&mut fixture.store.writer)
        .expect("classified editor");
    fixture.mask_idle(1000, 2000);
    fixture.annotate(
        1,
        AnnotationKind::ActiveOverride,
        1000,
        2000,
        Some(BuiltinCategory::Learning),
    );
    let request = fixture.unclassified_request(UnclassifiedGroupBy::App);
    let page = fixture.unclassified(request);
    assert_eq!(
        page.items.len(),
        1,
        "rule and manual classification both remove active work"
    );
    assert_eq!(page.items[0].active_ms, 1000);
    assert_eq!(fixture.unclassified_metric(), 1000);
}
#[test]
fn unclassified_returns_effective_day_offset_and_rejects_cursors_after_offset_changes() {
    use quartz_protocol::UnclassifiedGroupBy;
    let mut fixture = QueryFixture::new(1);
    let request = fixture.unclassified_request(UnclassifiedGroupBy::App);
    let before = fixture.unclassified(request.clone());
    let mut continuation = request.clone();
    continuation.cursor = Some(QueryCursor::new(
        before.versions,
        quartz_protocol::CursorQuery::from(&request),
        0,
    ));
    fixture
        .store
        .test_day_offset_write(240)
        .expect("day offset");
    assert_eq!(
        fixture
            .run(QueryRequest::Unclassified(continuation))
            .expect_err("changed calendar")
            .kind(),
        ErrorKind::StaleCursor
    );
    let after = fixture.unclassified(request);
    assert_eq!(after.day_start_offset_min, 240);
    assert!(!after.time_zone.is_empty());
    let expected = crate::query::calendar::DayCalendar::new(
        FIXTURE_DATE.parse().expect("date"),
        crate::query::calendar::DayStartOffset::try_from(240).expect("offset"),
    )
    .expect("offset calendar");
    let range = after.reference_range.expect("first range");
    assert_eq!(range.start_utc_ms, expected.range.start);
    assert_eq!(range.end_utc_ms, expected.range.end);
    assert!(
        after.items.is_empty(),
        "midnight fixture falls before the shifted day start"
    );
}

impl QueryFixture {
    fn range_request(
        &self,
        bucket: quartz_protocol::RangeBucket,
    ) -> quartz_protocol::QueryRangeRequest {
        let start_date: quartz_protocol::LocalDate = FIXTURE_DATE.parse().expect("date");
        quartz_protocol::QueryRangeRequest {
            start_date,
            end_date: start_date.tomorrow().expect("end"),
            bucket,
            dimensions: vec![
                quartz_protocol::RangeDimension::Category,
                quartz_protocol::RangeDimension::App,
                quartz_protocol::RangeDimension::Document,
            ],
            cursor: None,
        }
    }
    fn range_page(
        &mut self,
        request: quartz_protocol::QueryRangeRequest,
    ) -> quartz_protocol::QueryRangeResponse {
        let response = self.run(QueryRequest::Range(request)).expect("range query");
        assert!(serde_json::to_vec(&response).expect("wire").len() <= QUERY_PAGE_BYTES);
        let QueryResponse::Range(page) = response else {
            panic!("range response")
        };
        page
    }
    fn run_in_zone(
        &mut self,
        request: QueryRequest,
        zone: jiff::tz::TimeZone,
    ) -> Result<QueryResponse, RunQueryError> {
        let mut reader = self.store.take_query_reader();
        match QueryJob::start_in_zone(&mut reader, request, self.rules(), zone) {
            Ok(job) => {
                let completed = job.run_measured()?;
                self.store.return_query_reader(completed.reader);
                Ok(completed.response)
            }
            Err(error) => {
                self.store
                    .return_query_reader(reader.expect("restored reader"));
                Err(error)
            }
        }
    }
    fn set_first_weekday(&mut self, first: quartz_protocol::FirstDayOfWeekSetting) {
        crate::store::settings::Settings::new(&mut self.store.writer)
            .update_ui(quartz_protocol::SettingsSetRequest {
                first_day_of_week: Some(first),
                ..Default::default()
            })
            .expect("weekday setting");
    }
}
#[test]
fn range_pages_totals_once_preserve_order_and_reject_changed_or_stale_cursors() {
    use quartz_protocol::{RangeBucket, RangeDimension, RangeEntity, WorkEntity};
    let mut fixture = QueryFixture::new(900);
    fixture.set_first_weekday(quartz_protocol::FirstDayOfWeekSetting::Sunday);
    let mut request = fixture.range_request(RangeBucket::Week);
    request.dimensions = vec![
        RangeDimension::Document,
        RangeDimension::Category,
        RangeDimension::App,
    ];
    let original = request.clone();
    let mut groups = Vec::new();
    let mut totals = Vec::new();
    let first = fixture.range_page(request.clone());
    let continuation = first.next_cursor.clone().expect("multiple pages");
    let versions = first.versions.clone();
    let mut pages = 0;
    loop {
        let page = fixture.range_page(request.clone());
        assert_eq!(page.reference_range.is_some(), request.cursor.is_none());
        for bucket in page.buckets {
            totals.extend(bucket.active_ms);
            groups.extend(bucket.groups);
        }
        pages += 1;
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    assert!(pages > 1);
    assert_eq!(totals, vec![900_000]);
    assert_eq!(groups.len(), 1801);
    assert!(
        groups[..900]
            .iter()
            .all(|group| group.dimension == RangeDimension::Document)
    );
    assert_eq!(groups[900].dimension, RangeDimension::Category);
    assert!(
        groups[901..]
            .iter()
            .all(|group| group.dimension == RangeDimension::App)
    );
    let ids: Vec<_> = groups[..900]
        .iter()
        .map(|group| match &group.entity {
            RangeEntity::Work(WorkEntity::Document { app, .. }) => app.0,
            _ => panic!("document"),
        })
        .collect();
    assert_eq!(ids, (1..=900).collect::<Vec<_>>());
    let mut changed = original.clone();
    changed.cursor = Some(continuation.clone());
    changed.dimensions.reverse();
    assert_eq!(
        fixture
            .run(QueryRequest::Range(changed))
            .expect_err("changed dimensions")
            .kind(),
        ErrorKind::BadRequest
    );
    let mut changed = original.clone();
    changed.cursor = Some(QueryCursor::new(
        versions.clone(),
        quartz_protocol::CursorQuery::from(&changed),
        usize::MAX,
    ));
    assert_eq!(
        fixture
            .run(QueryRequest::Range(changed))
            .expect_err("invalid position")
            .kind(),
        ErrorKind::BadRequest
    );
    fixture.set_first_weekday(quartz_protocol::FirstDayOfWeekSetting::Saturday);
    let mut changed = original.clone();
    changed.cursor = Some(continuation.clone());
    assert_eq!(
        fixture
            .run(QueryRequest::Range(changed))
            .expect_err("changed week boundary")
            .kind(),
        ErrorKind::StaleCursor
    );
    fixture.set_first_weekday(quartz_protocol::FirstDayOfWeekSetting::Sunday);
    fixture.bump_fact_version();
    let mut changed = original;
    changed.cursor = Some(continuation);
    assert_eq!(
        fixture
            .run(QueryRequest::Range(changed))
            .expect_err("changed inputs")
            .kind(),
        ErrorKind::StaleCursor
    );
}
#[test]
fn range_rejects_empty_duplicate_dimensions_and_invalid_dates_as_bad_request() {
    use quartz_protocol::{RangeBucket, RangeDimension};
    let mut fixture = QueryFixture::new(1);
    let original = fixture.range_request(RangeBucket::Day);
    let mut invalid = Vec::new();
    for dimensions in [vec![], vec![RangeDimension::App, RangeDimension::App]] {
        let mut request = original.clone();
        request.dimensions = dimensions;
        invalid.push(request);
    }
    for end in ["2025-01-02", "2025-01-01", "2026-01-04"] {
        let mut request = original.clone();
        request.end_date = end.parse().expect("end");
        invalid.push(request);
    }
    for request in invalid {
        assert_eq!(
            fixture
                .run(QueryRequest::Range(request))
                .expect_err("invalid range")
                .kind(),
            ErrorKind::BadRequest
        );
    }
    assert_eq!(fixture.range_page(original).buckets.len(), 1);
}
#[test]
fn range_privacy_and_offline_activity_match_day_metrics_and_rankings() {
    use quartz_protocol::{AnnotationKind, RangeBucket, RangeDimension, RangeEntity, WorkEntity};
    for level in [
        enums::CaptureLevel::Full,
        enums::CaptureLevel::TitleHidden,
        enums::CaptureLevel::Excluded,
    ] {
        let mut fixture = QueryFixture::new(4);
        fixture.mask_idle(500, 750);
        fixture.annotate(
            1,
            AnnotationKind::OfflineActivity,
            500,
            750,
            Some(quartz_protocol::BuiltinCategory::Learning),
        );
        fixture.annotate(
            2,
            AnnotationKind::CategoryOverride,
            1000,
            2000,
            Some(quartz_protocol::BuiltinCategory::Development),
        );
        fixture.redact_titles(level);
        let request = fixture.range_request(RangeBucket::Day);
        let page = fixture.range_page(request);
        let bucket = &page.buckets[0];
        let day_request = fixture.request_day();
        let QueryResponse::Day(day) = fixture.run(day_request).expect("day") else {
            panic!("day")
        };
        assert_eq!(
            bucket.active_ms,
            Some(day.metrics.expect("metrics").active_ms)
        );
        let app_groups: Vec<_> = bucket
            .groups
            .iter()
            .filter(|group| group.dimension == RangeDimension::App)
            .collect();
        assert_eq!(app_groups.len(), day.applications.len());
        for (group, app) in app_groups.iter().zip(day.applications) {
            assert_eq!(group.active_ms, app.active_ms);
            assert!(
                matches!(&group.entity, RangeEntity::Work(WorkEntity::App {id, ..}) if *id == app.app.id)
            );
        }
        let documents: Vec<_> = bucket
            .groups
            .iter()
            .filter(|group| group.dimension == RangeDimension::Document)
            .collect();
        if level != enums::CaptureLevel::Full {
            assert!(documents.is_empty());
        }
        let category_ms: i64 = bucket
            .groups
            .iter()
            .filter(|group| group.dimension == RangeDimension::Category)
            .map(|group| group.active_ms)
            .sum();
        assert_eq!(Some(category_ms), bucket.active_ms);
        let wire = serde_json::to_string(&page).expect("wire");
        if level != enums::CaptureLevel::Full {
            assert!(!wire.contains("private title fixture"));
        }
        if level == enums::CaptureLevel::Excluded {
            assert!(!wire.contains("Application"));
        }
    }
}

fn range_entity_key(entity: &quartz_protocol::RangeEntity) -> String {
    let mut value = serde_json::to_value(entity).expect("entity");
    value
        .as_object_mut()
        .expect("object")
        .remove("document_ref");
    serde_json::to_string(&value).expect("identity")
}
#[test]
fn range_every_dimension_equals_sum_of_days_across_clock_changes_and_day_offsets() {
    use quartz_protocol::{
        CategoryEntity, RangeBucket, RangeDimension, RangeEntity, RangeGroup, WorkEntity,
    };
    let mut fixture = QueryFixture::new(8);
    fixture.store.test_day_offset_write(240).expect("offset");
    fixture.set_first_weekday(quartz_protocol::FirstDayOfWeekSetting::Sunday);
    let end = fixture.start + 9 * UTC_DAY_MS;
    diesel::update(capture_runs::table)
        .set(capture_runs::ended_utc_ms.eq(Some(end)))
        .execute(&mut fixture.store.writer)
        .expect("run");
    diesel::update(clock_segments::table)
        .set((
            clock_segments::end_utc_ms.eq(Some(end)),
            clock_segments::end_mono_ms.eq(Some(9 * UTC_DAY_MS)),
        ))
        .execute(&mut fixture.store.writer)
        .expect("clock");
    for id in 1..=8_i64 {
        let start = (id - 1) * UTC_DAY_MS + 5 * 3_600_000;
        diesel::update(intervals::table.filter(intervals::id.eq(Some(id))))
            .set((
                intervals::start_utc_ms.eq(fixture.start + start),
                intervals::end_utc_ms.eq(Some(fixture.start + start + 1000)),
                intervals::start_mono_ms.eq(start),
                intervals::end_mono_ms.eq(Some(start + 1000)),
            ))
            .execute(&mut fixture.store.writer)
            .expect("spread facts");
    }
    // A second clock segment revisits the same UTC interval with a different monotonic slope.
    let mut clock = clock_segments::table
        .select(crate::store::rows::ClockSegmentsRow::as_select())
        .first::<crate::store::rows::ClockSegmentsRow>(&mut fixture.store.writer)
        .expect("clock");
    clock.id = Some(2);
    clock.start_utc_ms = fixture.start + 5 * 3_600_000;
    clock.start_mono_ms = 10 * UTC_DAY_MS;
    clock.end_utc_ms = Some(clock.start_utc_ms + 1000);
    clock.end_mono_ms = Some(clock.start_mono_ms + 2000);
    diesel::insert_into(clock_segments::table)
        .values(&clock)
        .execute(&mut fixture.store.writer)
        .expect("repeated UTC clock");
    diesel::update(intervals::table.filter(intervals::id.eq(Some(8_i64))))
        .set((
            intervals::clock_segment_id.eq(2_i64),
            intervals::start_utc_ms.eq(clock.start_utc_ms),
            intervals::end_utc_ms.eq(clock.end_utc_ms),
            intervals::start_mono_ms.eq(clock.start_mono_ms),
            intervals::end_mono_ms.eq(clock.end_mono_ms),
            intervals::duration_ms.eq(Some(2000_i64)),
        ))
        .execute(&mut fixture.store.writer)
        .expect("repeated UTC fact");
    fixture.mask_idle(5 * 3_600_000 + 100, 5 * 3_600_000 + 300);
    fixture.annotate(
        1,
        quartz_protocol::AnnotationKind::OfflineActivity,
        5 * 3_600_000 + 100,
        5 * 3_600_000 + 300,
        Some(quartz_protocol::BuiltinCategory::Learning),
    );
    let mut request = fixture.range_request(RangeBucket::Day);
    request.end_date = request
        .start_date
        .checked_add(jiff::Span::new().days(9))
        .expect("end");
    let day_range = fixture.range_page(request.clone());
    assert_eq!(day_range.buckets.len(), 9);
    let mut expected_total = 0;
    let mut expected_groups = std::collections::BTreeMap::<String, i64>::new();
    for bucket in day_range.buckets {
        let mut day_request = quartz_protocol::QueryDayRequest {
            local_date: bucket.start_date,
            cursor: None,
        };
        let mut daily_groups = std::collections::BTreeMap::<String, i64>::new();
        loop {
            let QueryResponse::Day(day) = fixture
                .run(QueryRequest::Day(day_request.clone()))
                .expect("daily baseline")
            else {
                panic!("day")
            };
            if let Some(metrics) = day.metrics {
                assert_eq!(bucket.active_ms, Some(metrics.active_ms));
                expected_total += metrics.active_ms;
            }
            for hour in day.hours.into_iter().flatten() {
                for category in hour
                    .categories
                    .into_iter()
                    .filter(|category| category.duration_ms > 0)
                {
                    let key = range_entity_key(&RangeEntity::Category(CategoryEntity::Category {
                        category: category.category,
                    }));
                    *daily_groups.entry(key).or_default() += category.duration_ms;
                }
            }
            for app in day.applications {
                let key = range_entity_key(&RangeEntity::Work(WorkEntity::App {
                    id: app.app.id,
                    exe_name: app.app.exe_name,
                    display_name: app.app.display_name,
                }));
                *daily_groups.entry(key).or_default() += app.active_ms;
            }
            for document in day.documents {
                let key = range_entity_key(&RangeEntity::Work(WorkEntity::Document {
                    document_ref: document.document_ref,
                    app: document.app,
                    document: document.document,
                    project: document.project,
                    profile: document.profile,
                }));
                *daily_groups.entry(key).or_default() += document.active_ms;
            }
            let Some(cursor) = day.next_cursor else { break };
            day_request.cursor = Some(cursor);
        }
        let actual: std::collections::BTreeMap<_, _> = bucket
            .groups
            .iter()
            .map(|group| (range_entity_key(&group.entity), group.active_ms))
            .collect();
        assert_eq!(actual, daily_groups, "day {}", bucket.start_date);
        for group in bucket.groups {
            *expected_groups
                .entry(range_entity_key(&group.entity))
                .or_default() += group.active_ms;
        }
    }
    request.bucket = RangeBucket::Week;
    let page = fixture.range_page(request.clone());
    assert_eq!(
        page.buckets.first().expect("first").start_date,
        request.start_date
    );
    assert_eq!(
        page.buckets.last().expect("last").end_date,
        request.end_date
    );
    assert!(
        page.buckets
            .iter()
            .skip(1)
            .all(|bucket| bucket.start_date.weekday() == jiff::civil::Weekday::Sunday)
    );
    assert_eq!(
        page.buckets
            .iter()
            .filter_map(|bucket| bucket.active_ms)
            .sum::<i64>(),
        expected_total
    );
    let mut actual = std::collections::BTreeMap::<String, i64>::new();
    for RangeGroup {
        entity, active_ms, ..
    } in page.buckets.into_iter().flat_map(|bucket| bucket.groups)
    {
        *actual.entry(range_entity_key(&entity)).or_default() += active_ms;
    }
    assert_eq!(actual, expected_groups);
    // Requesting one dimension preserves its result and the same denominator.
    request.bucket = RangeBucket::Day;
    for dimension in [
        RangeDimension::Category,
        RangeDimension::App,
        RangeDimension::Document,
    ] {
        request.dimensions = vec![dimension];
        let page = fixture.range_page(request.clone());
        assert_eq!(
            page.buckets
                .iter()
                .filter_map(|bucket| bucket.active_ms)
                .sum::<i64>(),
            expected_total
        );
        assert!(
            page.buckets
                .iter()
                .flat_map(|bucket| &bucket.groups)
                .all(|group| group.dimension == dimension)
        );
    }
}
#[test]
fn range_and_unclassified_document_refs_round_trip_across_a_366_day_dst_range() {
    use quartz_protocol::{
        DetailEntity, QueryEntityDetailRequest, RangeBucket, RangeDimension, RangeEntity,
        WorkEntity,
    };
    let mut fixture = QueryFixture::new(1);
    fixture.store.test_day_offset_write(240).expect("offset");
    fixture.set_first_weekday(quartz_protocol::FirstDayOfWeekSetting::Sunday);
    let zone = jiff::tz::TimeZone::get("America/Los_Angeles").expect("zone");
    let mut request = fixture.range_request(RangeBucket::Week);
    request.start_date = "2024-11-01".parse().expect("start");
    request.end_date = "2025-11-02".parse().expect("end");
    request.dimensions = vec![RangeDimension::Document];
    let unclassified = quartz_protocol::QueryUnclassifiedRequest {
        start_date: request.start_date,
        end_date: request.end_date,
        group_by: quartz_protocol::UnclassifiedGroupBy::Document,
        search: None,
        cursor: None,
    };
    for query in [
        QueryRequest::Range(request),
        QueryRequest::Unclassified(unclassified),
    ] {
        let response = fixture
            .run_in_zone(query, zone.clone())
            .expect("366 logical days");
        struct IssuedDocument {
            reference_range: quartz_protocol::ReferenceRange,
            document_ref: quartz_protocol::DocumentRef,
        }
        let issued = match response {
            QueryResponse::Range(page) => {
                let entity = page
                    .buckets
                    .into_iter()
                    .flat_map(|bucket| bucket.groups)
                    .find_map(|group| match group.entity {
                        RangeEntity::Work(WorkEntity::Document { document_ref, .. }) => {
                            Some(document_ref)
                        }
                        _ => None,
                    })
                    .expect("document");
                IssuedDocument {
                    reference_range: page.reference_range.expect("range"),
                    document_ref: entity,
                }
            }
            QueryResponse::Unclassified(page) => {
                let WorkEntity::Document { document_ref, .. } =
                    page.items.into_iter().next().expect("item").entity
                else {
                    panic!("document")
                };
                IssuedDocument {
                    reference_range: page.reference_range.expect("range"),
                    document_ref,
                }
            }
            _ => panic!("group query"),
        };
        let reference_range = issued.reference_range;
        assert_eq!(
            reference_range.end_utc_ms - reference_range.start_utc_ms,
            366 * UTC_DAY_MS + 3_600_000
        );
        let detail = QueryEntityDetailRequest {
            entity: DetailEntity::Document {
                document_ref: issued.document_ref,
            },
            reference_range: Some(reference_range),
            local_date: "2025-01-07".parse().expect("detail date"),
            cursor: None,
        };
        let QueryResponse::EntityDetail(page) = fixture
            .run_in_zone(QueryRequest::EntityDetail(detail.clone()), zone.clone())
            .expect("document round trip")
        else {
            panic!("detail")
        };
        assert_eq!(
            page.daily
                .expect("daily")
                .iter()
                .map(|day| day.active_ms)
                .sum::<i64>(),
            1000
        );
        let mut invalid = detail;
        invalid.reference_range.as_mut().expect("range").end_utc_ms =
            reference_range.start_utc_ms + 367 * UTC_DAY_MS + 1;
        assert_eq!(
            fixture
                .run_in_zone(QueryRequest::EntityDetail(invalid), zone.clone())
                .expect_err("reference bound")
                .kind(),
            ErrorKind::BadRequest
        );
    }
}

#[test]
fn range_dst_day_buckets_equal_day_queries_with_a_four_hour_start_offset() {
    use crate::query::calendar::{DayCalendar, DayStartOffset};
    use quartz_protocol::{QueryDayRequest, RangeBucket};
    struct Case {
        start: &'static str,
        end: &'static str,
        hours: i64,
    }
    let zone = jiff::tz::TimeZone::get("America/Los_Angeles").expect("zone");
    for case in [
        Case {
            start: "2026-03-07",
            end: "2026-03-09",
            hours: 47,
        },
        Case {
            start: "2026-10-31",
            end: "2026-11-02",
            hours: 49,
        },
    ] {
        let mut fixture = QueryFixture::new(1);
        fixture.store.test_day_offset_write(240).expect("offset");
        let offset = DayStartOffset::try_from(240).expect("offset");
        let start_date = case.start.parse().expect("date");
        let end_date: quartz_protocol::LocalDate = case.end.parse().expect("date");
        let start = DayCalendar::in_zone(start_date, offset, zone.clone())
            .expect("start")
            .range
            .start;
        let end = DayCalendar::in_zone(
            end_date.yesterday().expect("last date"),
            offset,
            zone.clone(),
        )
        .expect("end")
        .range
        .end;
        let duration = end - start;
        diesel::update(capture_runs::table)
            .set((
                capture_runs::started_utc_ms.eq(start),
                capture_runs::ended_utc_ms.eq(Some(end)),
            ))
            .execute(&mut fixture.store.writer)
            .expect("run");
        diesel::update(clock_segments::table)
            .set((
                clock_segments::start_utc_ms.eq(start),
                clock_segments::end_utc_ms.eq(Some(end)),
                clock_segments::end_mono_ms.eq(Some(duration)),
            ))
            .execute(&mut fixture.store.writer)
            .expect("clock");
        // Stored closed facts are at most one UTC day; split the synthetic activity accordingly.
        const FACT_CHUNK_MS: i64 = UTC_DAY_MS / 2;
        let mut fact = intervals::table
            .select(crate::store::rows::IntervalsRow::as_select())
            .first::<crate::store::rows::IntervalsRow>(&mut fixture.store.writer)
            .expect("fact template");
        diesel::delete(intervals::table)
            .execute(&mut fixture.store.writer)
            .expect("replace fact");
        let mut at = start;
        let mut sequence = 0;
        while at < end {
            let next = (at + FACT_CHUNK_MS).min(end);
            sequence += 1;
            fact.id = None;
            fact.sequence = sequence;
            fact.start_utc_ms = at;
            fact.end_utc_ms = Some(next);
            fact.start_mono_ms = at - start;
            fact.end_mono_ms = Some(next - start);
            fact.duration_ms = Some(next - at);
            diesel::insert_into(intervals::table)
                .values(&fact)
                .execute(&mut fixture.store.writer)
                .expect("bounded fact");
            at = next;
        }
        let mut request = fixture.range_request(RangeBucket::Day);
        request.start_date = start_date;
        request.end_date = end_date;
        let QueryResponse::Range(page) = fixture
            .run_in_zone(QueryRequest::Range(request), zone.clone())
            .expect("range")
        else {
            panic!("range")
        };
        assert_eq!(page.time_zone, "America/Los_Angeles");
        assert_eq!(page.day_start_offset_min, 240);
        let mut total = 0;
        for bucket in page.buckets {
            let QueryResponse::Day(day) = fixture
                .run_in_zone(
                    QueryRequest::Day(QueryDayRequest {
                        local_date: bucket.start_date,
                        cursor: None,
                    }),
                    zone.clone(),
                )
                .expect("day")
            else {
                panic!("day")
            };
            let active = day.metrics.expect("metrics").active_ms;
            assert_eq!(bucket.active_ms, Some(active));
            assert_eq!(bucket.groups.len(), 3);
            assert!(bucket.groups.iter().all(|group| group.active_ms == active));
            total += active;
        }
        assert_eq!(total, case.hours * 3_600_000);
    }
}

#[test]
fn range_paging_spans_clipped_week_buckets_and_releases_finished_groups() {
    use quartz_protocol::RangeBucket;
    let mut fixture = QueryFixture::new(900);
    fixture.set_first_weekday(quartz_protocol::FirstDayOfWeekSetting::Sunday);
    let mut request = fixture.range_request(RangeBucket::Week);
    request.end_date = "2025-01-11".parse().expect("clipped end");
    let mut totals = Vec::new();
    let mut groups = 0;
    let mut first_bucket_pages = 0;
    loop {
        let page = fixture.range_page(request.clone());
        for bucket in page.buckets {
            if bucket.start_date == request.start_date {
                first_bucket_pages += 1;
            }
            if let Some(active_ms) = bucket.active_ms {
                totals.push(active_ms);
            }
            groups += bucket.groups.len();
        }
        let Some(cursor) = page.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    assert!(first_bucket_pages > 1);
    assert_eq!(totals, [900_000, 0]);
    assert_eq!(groups, 1801);
}

#[test]
fn resident_reader_reuses_fixed_statements_for_varying_lookup_widths_and_queries() {
    use diesel::{Connection, connection::InstrumentationEvent};
    use std::sync::{Arc, Mutex};
    const QUERY_ROUNDS: i64 = 64;
    const DATE_VARIANTS: i64 = 9;
    const FIXTURE_INTERVAL_MS: i64 = 1000;
    let count = i64::try_from(READ_BATCH_ROWS).expect("fixture count");
    let mut fixture = QueryFixture::new(count);
    fixture.set_first_weekday(quartz_protocol::FirstDayOfWeekSetting::Sunday);
    let rules = fixture.rules();
    let rows = intervals::table
        .order(intervals::id)
        .select(crate::store::rows::IntervalsRow::as_select())
        .load::<crate::store::rows::IntervalsRow>(&mut fixture.store.writer)
        .expect("fixture rows");
    let rows: Vec<_> = rows
        .into_iter()
        .map(|fact| ForegroundDetails {
            fact,
            title: None,
            window_class: None,
        })
        .collect();
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = events.clone();
    let mut reader = fixture.store.take_query_reader().expect("resident reader");
    reader
        .connection
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::CacheQuery { sql, .. } = event {
                observed.lock().expect("cached SQL").push(sql.to_owned());
            }
        });
    let mut warmed = None;
    // Every width and partial final chunk uses the same cached application lookup.
    for width in 1..=READ_BATCH_ROWS {
        let mut snapshot = reader.snapshot();
        snapshot.begin().expect("snapshot");
        snapshot.read_applications(&rows[..width]).expect("lookup");
        assert_eq!(snapshot.applications.len(), width, "lookup width {width}");
        reader = snapshot.finish().expect("same reader");
        let count = events.lock().expect("cached SQL").len();
        assert_eq!(count, *warmed.get_or_insert(count));
    }
    {
        let cached = events.lock().expect("cached SQL");
        let lookups: Vec<_> = cached
            .iter()
            .filter(|sql| sql.contains("FROM `applications`"))
            .collect();
        assert_eq!(lookups.len(), 1);
        assert_eq!(lookups[0].matches('?').count(), APPLICATION_LOOKUP_ARITY);
    }
    let run = |reader: ReadOnlyStore, request: QueryRequest| {
        let mut slot = Some(reader);
        QueryJob::start(&mut slot, request, rules.clone())
            .expect("query on resident reader")
            .run_measured()
            .expect("query result")
            .reader
    };
    let date: quartz_protocol::LocalDate = FIXTURE_DATE.parse().expect("date");
    let requests = |round: i64| {
        vec![
            QueryRequest::Day(quartz_protocol::QueryDayRequest {
                local_date: date
                    .checked_add(jiff::Span::new().days(round % DATE_VARIANTS))
                    .expect("date variant"),
                cursor: None,
            }),
            QueryRequest::Timeline(quartz_protocol::QueryTimelineRequest {
                start_utc_ms: fixture.start + round * FIXTURE_INTERVAL_MS,
                end_utc_ms: fixture.start + (count - round) * FIXTURE_INTERVAL_MS,
                detail: quartz_protocol::TimelineDetail::App,
                ms_per_pixel: quartz_protocol::MillisecondsPerPixel(std::num::NonZeroU64::MIN),
                cursor: None,
            }),
            QueryRequest::Range(quartz_protocol::QueryRangeRequest {
                start_date: date,
                end_date: date
                    .checked_add(jiff::Span::new().days(round % DATE_VARIANTS + 1))
                    .expect("range variant"),
                bucket: quartz_protocol::RangeBucket::Week,
                dimensions: vec![
                    quartz_protocol::RangeDimension::Category,
                    quartz_protocol::RangeDimension::App,
                    quartz_protocol::RangeDimension::Document,
                ],
                cursor: None,
            }),
        ]
    };
    for request in requests(0) {
        reader = run(reader, request);
    }
    let prepared = events.lock().expect("cached SQL").len();
    for round in 0..QUERY_ROUNDS {
        for request in requests(round) {
            reader = run(reader, request);
        }
        assert_eq!(
            events.lock().expect("cached SQL").len(),
            prepared,
            "round {round} prepares no per-call SQL"
        );
    }
}

#[derive(diesel::QueryableByName)]
struct ClockQueryPlan {
    #[diesel(sql_type = diesel::sql_types::Text)]
    detail: String,
}

struct ClockHistoryBenchmark {
    fixture: test_support::QueryFixture,
}
impl ClockHistoryBenchmark {
    const HISTORY_ROWS: i64 = 100_000;
    const HOUR_MS: i64 = 3_600_000;
    // The predicates, ordering and page bound mirror ScanCursor::load_clock.
    const SCAN: &str = "SELECT * FROM clock_segments WHERE start_utc_ms < ? AND (end_utc_ms > ? OR confirmed_utc_ms > ?) AND (start_utc_ms > ? OR (start_utc_ms = ? AND id > ?)) ORDER BY start_utc_ms, id LIMIT ?";
    fn new() -> Self {
        Self {
            fixture: test_support::QueryFixture::new(0),
        }
    }
    fn index(&mut self) {
        use diesel::connection::SimpleConnection;
        self.fixture
            .store
            .writer
            .batch_execute("CREATE INDEX measure_clock_start ON clock_segments(start_utc_ms, id);")
            .expect("temporary database index");
    }
    fn insert_history(&mut self) -> std::time::Duration {
        let started = std::time::Instant::now();
        diesel::sql_query("WITH RECURSIVE history(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM history WHERE n < ?) INSERT INTO clock_segments(run_id, start_utc_ms, start_mono_ms, end_utc_ms, end_mono_ms, end_reason) SELECT 1, ? - n * ?, 0, ? - (n - 1) * ?, ?, ? FROM history")
            .bind::<diesel::sql_types::BigInt, _>(Self::HISTORY_ROWS)
            .bind::<diesel::sql_types::BigInt, _>(self.fixture.start)
            .bind::<diesel::sql_types::BigInt, _>(Self::HOUR_MS)
            .bind::<diesel::sql_types::BigInt, _>(self.fixture.start)
            .bind::<diesel::sql_types::BigInt, _>(Self::HOUR_MS)
            .bind::<diesel::sql_types::BigInt, _>(Self::HOUR_MS)
            .bind::<diesel::sql_types::Integer, _>(crate::store::enums::ClockEndReason::RunEnd.code())
            .execute(&mut self.fixture.store.writer).expect("synthetic closed clocks");
        started.elapsed()
    }
    fn plan(&mut self) -> Vec<String> {
        let start = self.fixture.start - UTC_DAY_MS;
        diesel::sql_query(format!("EXPLAIN QUERY PLAN {}", Self::SCAN))
            .bind::<diesel::sql_types::BigInt, _>(self.fixture.start)
            .bind::<diesel::sql_types::BigInt, _>(start)
            .bind::<diesel::sql_types::BigInt, _>(start)
            .bind::<diesel::sql_types::BigInt, _>(i64::MIN)
            .bind::<diesel::sql_types::BigInt, _>(i64::MIN)
            .bind::<diesel::sql_types::BigInt, _>(i64::MIN)
            .bind::<diesel::sql_types::BigInt, _>(
                i64::try_from(READ_BATCH_ROWS).expect("batch limit"),
            )
            .load::<ClockQueryPlan>(&mut self.fixture.store.writer)
            .expect("clock query plan")
            .into_iter()
            .map(|row| row.detail)
            .collect()
    }
    fn scan_days(&mut self, days: i64) -> std::time::Duration {
        let started = std::time::Instant::now();
        for day in 1..=days {
            let range = QueryRange {
                start: self.fixture.start - day * UTC_DAY_MS,
                end: self.fixture.start - (day - 1) * UTC_DAY_MS,
            };
            let mut cursor = ScanCursor::initial();
            let rows = cursor
                .load_clock(
                    &mut self.fixture.store.writer,
                    range,
                    StartCursor::initial(ClockSegmentId(i64::MIN)),
                )
                .expect("clock overlap scan");
            assert_eq!(
                rows.len(),
                usize::try_from(UTC_DAY_MS / Self::HOUR_MS).expect("hours")
            );
        }
        started.elapsed()
    }
    fn median_scan(&mut self, days: i64) -> std::time::Duration {
        const SAMPLES: usize = 9;
        let mut samples: Vec<_> = (0..SAMPLES).map(|_| self.scan_days(days)).collect();
        samples.sort();
        samples[SAMPLES / 2]
    }
}

#[test]
#[ignore = "synthetic history measurement; creates indexes only in disposable test databases"]
fn clock_overlap_history_plan_and_index_benchmark() {
    let mut baseline = ClockHistoryBenchmark::new();
    let mut indexed = ClockHistoryBenchmark::new();
    indexed.index();
    let baseline_write = baseline.insert_history();
    let indexed_write = indexed.insert_history();
    let baseline_plan = baseline.plan();
    let indexed_plan = indexed.plan();
    assert!(
        baseline_plan
            .iter()
            .any(|line| line.contains("TEMP B-TREE")),
        "{baseline_plan:?}"
    );
    assert!(
        indexed_plan
            .iter()
            .any(|line| line.contains("measure_clock_start")),
        "{indexed_plan:?}"
    );
    println!(
        "clock history rows={}, write baseline={baseline_write:?} indexed={indexed_write:?}, baseline plan={baseline_plan:?}, indexed plan={indexed_plan:?}",
        ClockHistoryBenchmark::HISTORY_ROWS
    );
    for days in [1, 7, 366] {
        println!(
            "clock scans days={days}: baseline={:?}, indexed={:?}",
            baseline.median_scan(days),
            indexed.median_scan(days)
        );
    }
}

#[test]
fn owning_clock_lookups_cache_one_statement_for_all_batch_widths() {
    use diesel::{Connection, connection::InstrumentationEvent};
    use std::sync::{Arc, Mutex};
    let mut fixture = QueryFixture::new(0);
    let clock = clock_segments::table
        .select(ClockSegmentsRow::as_select())
        .first::<ClockSegmentsRow>(&mut fixture.store.writer)
        .expect("fixture clock");
    for id in 2..=i64::try_from(READ_BATCH_ROWS).expect("batch") {
        let mut clock = clock.clone();
        clock.id = Some(id);
        diesel::insert_into(clock_segments::table)
            .values(clock)
            .execute(&mut fixture.store.writer)
            .expect("clock");
    }
    let rows: Vec<_> = (1..=READ_BATCH_ROWS)
        .map(|id| InputRow::Availability {
            span: FactSpan {
                id: FactId(i64::try_from(id).expect("id")),
                segment: ClockSegmentId(i64::try_from(id).expect("clock id")),
                start_utc_ms: fixture.start,
                end_utc_ms: fixture.start + 1,
                start_mono_ms: 0,
                end_mono_ms: 1,
            },
            state: Availability::Stopped(StopReason::Suspended),
        })
        .collect();
    let cached = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = cached.clone();
    let mut reader = fixture.store.take_query_reader().expect("resident reader");
    reader
        .connection
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::CacheQuery { sql, .. } = event
                && sql.contains("FROM `clock_segments`")
            {
                observed.lock().expect("cached SQL").push(sql.to_owned());
            }
        });
    for width in 1..=READ_BATCH_ROWS {
        let mut snapshot = reader.snapshot();
        snapshot.begin().expect("snapshot");
        let loaded = snapshot
            .load_owning_clocks(rows[..width].to_vec())
            .expect("owning clocks");
        assert_eq!(
            loaded
                .iter()
                .filter(|row| matches!(row, InputRow::Clock(_)))
                .count(),
            width,
            "width {width}"
        );
        assert_eq!(loaded.len(), width * 2);
        let repeated = snapshot
            .load_owning_clocks(rows[..width].to_vec())
            .expect("already loaded clocks");
        assert_eq!(repeated.len(), width);
        reader = snapshot.finish().expect("resident reader");
        let statements = cached.lock().expect("cached SQL");
        assert_eq!(statements.len(), 1, "one reusable lookup for width {width}");
        assert_eq!(
            statements.first().expect("lookup").matches('?').count(),
            APPLICATION_LOOKUP_ARITY
        );
    }
}

fn reject_snapshot_rollback(reader: &mut ReadOnlyStore) {
    use libsqlite3_sys as sqlite;
    unsafe extern "C" fn authorize(
        _context: *mut std::ffi::c_void,
        action: std::ffi::c_int,
        argument: *const std::ffi::c_char,
        _second: *const std::ffi::c_char,
        _database: *const std::ffi::c_char,
        _trigger: *const std::ffi::c_char,
    ) -> std::ffi::c_int {
        if action == sqlite::SQLITE_TRANSACTION && !argument.is_null() {
            // SAFETY: SQLite supplies a valid C string for the duration of this callback.
            let operation = unsafe { std::ffi::CStr::from_ptr(argument) };
            if operation.to_bytes() == b"ROLLBACK" {
                return sqlite::SQLITE_DENY;
            }
        }
        sqlite::SQLITE_OK
    }
    // SAFETY: The callback has no borrowed state and changes no Diesel transaction or statement.
    let status = unsafe {
        reader.connection.with_raw_connection(|connection| {
            sqlite::sqlite3_set_authorizer(connection, Some(authorize), std::ptr::null_mut())
        })
    };
    assert_eq!(status, sqlite::SQLITE_OK, "install rollback failure");
}

#[test]
fn preparation_rollback_failure_returns_error_to_owner_and_recovers_reader() {
    use crate::query::{dispatch::QueryCommand, runner::QueryRunner};
    let mut fixture = QueryFixture::new(1);
    let rules = fixture.rules();
    reject_snapshot_rollback(fixture.store.reader.as_mut().expect("resident reader"));
    let (reply, mut received) = tokio::sync::oneshot::channel();
    let mut runner = QueryRunner::default();
    let result = runner.start(
        &mut fixture.store,
        QueryCommand::Query {
            request: QueryRequest::Annotations(quartz_protocol::QueryAnnotationsRequest {
                start_utc_ms: 1,
                end_utc_ms: 0,
                cursor: None,
            }),
            reply,
        },
        rules,
    );
    assert!(
        matches!(result, Err(RunQueryError::Cleanup { .. })),
        "cleanup error reaches database owner"
    );
    assert_eq!(
        received
            .try_recv()
            .expect("query reply")
            .expect_err("cleanup failure")
            .kind,
        ErrorKind::Internal
    );
    assert!(
        fixture.store.reader.is_some(),
        "failed preparation must restore the resident reader"
    );
    fixture
        .run(fixture.request_day())
        .expect("subsequent query succeeds");
}

#[test]
fn running_query_rollback_failure_recovers_reader() {
    use crate::query::{dispatch::QueryCommand, runner::QueryRunner};
    let mut fixture = QueryFixture::new(1);
    let rules = fixture.rules();
    let request = fixture.request_day();
    reject_snapshot_rollback(fixture.store.reader.as_mut().expect("resident reader"));
    let (reply, received) = tokio::sync::oneshot::channel();
    let mut runner = QueryRunner::default();
    let result = runner.start(
        &mut fixture.store,
        QueryCommand::Query { request, reply },
        rules,
    );
    result.expect("start query");
    drop(received);
    assert!(
        runner.step(&mut fixture.store).is_err(),
        "cleanup error reaches database owner"
    );
    assert!(
        fixture.store.reader.is_some(),
        "failed cleanup must restore the resident reader"
    );
    fixture
        .run(fixture.request_day())
        .expect("subsequent query succeeds");
}

#[test]
fn rollback_and_reopen_failure_reports_recover_reader_without_retrying() {
    use crate::query::{dispatch::QueryCommand, runner::QueryRunner};
    use crate::store::{OpenStoreError, query_error::ReadQueryError};
    let mut fixture = QueryFixture::new(1);
    let rules = fixture.rules();
    let request = fixture.request_day();
    reject_snapshot_rollback(fixture.store.reader.as_mut().expect("resident reader"));
    let original_path = fixture.store.path.clone();
    fixture.store.path = fixture.directory.path().join("missing-reader.db");
    let (reply, mut received) = tokio::sync::oneshot::channel();
    let mut runner = QueryRunner::default();
    runner
        .start(
            &mut fixture.store,
            QueryCommand::Query { request, reply },
            rules.clone(),
        )
        .expect("start query before rollback");
    let error = loop {
        if let Err(error) = runner.step(&mut fixture.store) {
            break error;
        }
        assert!(runner.is_running(), "query must fail during cleanup");
    };
    let RunQueryError::RecoverReader { source, cleanup } = error else {
        panic!("rollback and reopen failures must both reach the owner");
    };
    assert!(matches!(
        source,
        ReadQueryError::Reopen {
            source: OpenStoreError::Connect { .. }
        }
    ));
    assert!(matches!(
        *cleanup,
        RunQueryError::Read {
            source: ReadQueryError::Read { .. }
        }
    ));
    assert_eq!(
        received
            .try_recv()
            .expect("first reply")
            .expect_err("cleanup failure")
            .kind,
        ErrorKind::Internal
    );
    assert!(fixture.store.reader.is_none());
    assert!(!runner.is_running());
    fixture.store.path = original_path;
    let request = fixture.request_day();
    let (reply, mut received) = tokio::sync::oneshot::channel();
    assert!(
        matches!(
            runner.start(
                &mut fixture.store,
                QueryCommand::Query { request, reply },
                rules
            ),
            Err(RunQueryError::ReaderUnavailable)
        ),
        "the next request must report the absent reader without retrying"
    );
    assert_eq!(
        received
            .try_recv()
            .expect("second reply")
            .expect_err("unavailable reader")
            .kind,
        ErrorKind::Internal
    );
    assert!(
        fixture.store.reader.is_none(),
        "restoring the path must not trigger an implicit reopen loop"
    );
    assert!(!runner.is_running());
    runner
        .step(&mut fixture.store)
        .expect("no work queued for retry");
}

#[test]
fn snapshot_finish_does_not_retry_or_log_failed_rollback() {
    let mut fixture = QueryFixture::new(1);
    let mut reader = fixture.store.take_query_reader().expect("resident reader");
    reject_snapshot_rollback(&mut reader);
    let mut snapshot = reader.snapshot();
    snapshot.begin().expect("read transaction");
    let log = tempfile::NamedTempFile::new().expect("isolated snapshot log");
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_writer(std::sync::Mutex::new(log.reopen().expect("log writer")))
        .finish();
    let result = tracing::subscriber::with_default(subscriber, || snapshot.finish());
    assert!(
        matches!(result, Err(ReadQueryError::Read { .. })),
        "finish must report rollback failure"
    );
    assert_eq!(
        log.as_file().metadata().expect("log size").len(),
        0,
        "Drop must not retry cleanup and log a duplicate failure"
    );
}

#[test]
fn raw_foreground_uses_one_joined_scan_per_partition() {
    use diesel::{Connection, connection::InstrumentationEvent};
    use std::sync::{Arc, Mutex};
    let fixture = QueryFixture::new(3);
    let mut reader = ReadOnlyStore::open_facts(
        &crate::data_directory::DataDirectory::new(fixture.directory.path()).database(),
    )
    .expect("reader");
    let statements = Arc::new(Mutex::new(Vec::new()));
    let observed = statements.clone();
    reader
        .connection
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event {
                observed.lock().expect("statements").push(query.to_string());
            }
        });
    let facts = reader
        .dump_range(fixture.start, fixture.start + 10_000)
        .expect("raw facts");
    assert_eq!(
        facts
            .iter()
            .filter(|fact| fact.kind == crate::store::facts::FactKind::Foreground)
            .count(),
        3
    );
    let statements = statements.lock().expect("statements");
    let foreground: Vec<_> = statements
        .iter()
        .filter(|sql| sql.starts_with("SELECT") && sql.contains("`intervals`"))
        .collect();
    assert_eq!(foreground.len(), 2, "closed and open scans: {foreground:?}");
    assert!(
        foreground
            .iter()
            .all(|sql| sql.contains("`titles`") && sql.contains("`applications`"))
    );
}

#[test]
fn range_stops_reading_after_its_first_full_bucket() {
    use diesel::{Connection, connection::InstrumentationEvent};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let mut fixture = QueryFixture::new(3000);
    let statements = Arc::new(AtomicUsize::new(0));
    let observed = statements.clone();
    let mut reader = fixture.store.take_query_reader().expect("reader");
    reader
        .connection
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event
                && query.to_string().contains("FROM `clock_segments`")
            {
                observed.fetch_add(1, Ordering::Relaxed);
            }
        });
    fixture.store.return_query_reader(reader);
    let short = fixture.range_request(quartz_protocol::RangeBucket::Day);
    let first = fixture.range_page(short.clone());
    assert!(first.next_cursor.is_some(), "fixture fills a page");
    let first_scans = statements.swap(0, Ordering::Relaxed);
    let mut long = short;
    long.end_date = long
        .end_date
        .checked_add(jiff::Span::new().days(20))
        .expect("later end");
    let page = fixture.range_page(long);
    assert!(page.next_cursor.is_some());
    assert_eq!(
        statements.load(Ordering::Relaxed),
        first_scans,
        "later dates must not be scanned after continuation is established"
    );
}

#[test]
fn facts_convert_a_bounded_batch_per_turn() {
    let mut fixture = QueryFixture::new(1000);
    let mut job = fixture.start(QueryRequest::Facts(quartz_protocol::QueryFactsRequest {
        local_date: FIXTURE_DATE.parse().expect("date"),
        cursor: None,
    }));
    let mut conversions = 0;
    loop {
        crate::store::facts::work::RECORDS.set(0);
        let response = job.advance().expect("advance");
        let count = crate::store::facts::work::RECORDS.get();
        assert!(
            count <= crate::query::FINALIZE_BATCH_RECORDS,
            "one turn converted {count} records"
        );
        conversions += count;
        if response.is_some() {
            break;
        }
    }
    assert!(conversions >= 1000);
    fixture
        .store
        .return_query_reader(job.finish().expect("finish"));
}

#[test]
fn annotation_pages_join_categories_and_resume_without_prefix_reads() {
    use diesel::{Connection, connection::InstrumentationEvent};
    use std::sync::{Arc, Mutex};
    let mut fixture = QueryFixture::new(0);
    let entities = (0..30)
        .map(|id| quartz_protocol::CreateAnnotation {
            uuid: quartz_protocol::AnnotationId(
                format!("abcdef00-0000-4000-8000-{id:012x}")
                    .parse()
                    .expect("UUID"),
            ),
            kind: quartz_protocol::AnnotationKind::Note,
            values: quartz_protocol::AnnotationValues {
                start_utc_ms: fixture.start,
                end_utc_ms: fixture.start + 100,
                category: Some(quartz_protocol::AnnotationCategory::Builtin {
                    key: quartz_protocol::BuiltinCategory::Learning,
                }),
                label_preset: None,
                label: None,
                note: Some("x".repeat(30_000)),
            },
        })
        .collect();
    fixture
        .store
        .create_annotations(
            quartz_protocol::AnnotationsCreateRequest { entities },
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("notes");
    let statements = Arc::new(Mutex::new(Vec::new()));
    let observed = statements.clone();
    let mut reader = fixture.store.take_query_reader().expect("reader");
    reader
        .connection
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event {
                observed.lock().expect("SQL").push(query.to_string());
            }
        });
    fixture.store.return_query_reader(reader);
    let mut request = quartz_protocol::QueryAnnotationsRequest {
        start_utc_ms: fixture.start,
        end_utc_ms: fixture.start + 100,
        cursor: None,
    };
    let QueryResponse::Annotations(first) = fixture
        .run(QueryRequest::Annotations(request.clone()))
        .expect("first")
    else {
        panic!("annotations")
    };
    request.cursor = first.next_cursor;
    assert!(request.cursor.is_some());
    let first_statements = std::mem::take(&mut *statements.lock().expect("SQL"));
    let first_reads = first_statements
        .iter()
        .filter(|sql| sql.contains("FROM `annotations`"))
        .count();
    let QueryResponse::Annotations(second) = fixture
        .run(QueryRequest::Annotations(request))
        .expect("second")
    else {
        panic!("annotations")
    };
    let second_statements = statements.lock().expect("SQL");
    let second_reads = second_statements
        .iter()
        .filter(|sql| sql.contains("FROM `annotations`"))
        .count();
    assert!(
        second_reads <= first_reads,
        "first {first_reads}, second {second_reads}: previously returned rows must not be read again"
    );
    assert!(
        first_statements
            .iter()
            .chain(second_statements.iter())
            .all(|sql| !sql.contains("FROM `categories`")),
        "category identity comes from the annotation join"
    );
    assert!(!second.entities.is_empty());
    assert!(
        second
            .entities
            .iter()
            .all(|entity| entity.category.is_some())
    );
}

#[test]
fn write_exhaustion_returns_documented_code_and_rolls_back() {
    use crate::persistence::dispatch::WriteRequest;
    use crate::store::schema::{data_day_versions, settings};
    use quartz_protocol::{
        AnnotationId, AnnotationKind, AnnotationValues, AnnotationsCreateRequest, CreateAnnotation,
    };
    let mut fixture = QueryFixture::new(0);
    let mut registry = crate::rules::registry::RegistryShell::open(
        &mut fixture.store,
        quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
    )
    .expect("registry");
    let uuid = AnnotationId(
        "abcdef00-0000-4000-8000-000000000001"
            .parse()
            .expect("UUID"),
    );
    let request = AnnotationsCreateRequest {
        entities: vec![CreateAnnotation {
            uuid,
            kind: AnnotationKind::Note,
            values: AnnotationValues {
                start_utc_ms: fixture.start,
                end_utc_ms: fixture.start + 1,
                category: None,
                label_preset: None,
                label: None,
                note: Some("private note".into()),
            },
        }],
    };
    diesel::insert_into(data_day_versions::table)
        .values(crate::store::rows::DataDayVersionsRow {
            source: enums::DataSource::Annotations,
            utc_date: fixture.start.div_euclid(UTC_DAY_MS),
            version: i64::MAX,
        })
        .execute(&mut fixture.store.writer)
        .expect("exhaust day");
    let error = WriteRequest::decode(
        quartz_protocol::Op::AnnotationsCreate,
        serde_json::to_value(request).expect("body"),
    )
    .expect("decode")
    .execute(
        &mut fixture.store,
        quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        None,
        &mut registry,
    )
    .expect_err("exhaustion");
    assert_eq!(error.kind.code(), "version_exhausted");
    assert_eq!(
        annotations::table
            .count()
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("rows"),
        0
    );
    diesel::update(settings::table.filter(settings::key.eq("metadata_version")))
        .set(settings::value.eq(i64::MAX.to_string()))
        .execute(&mut fixture.store.writer)
        .expect("exhaust metadata");
    let request = quartz_protocol::CategoriesCreateRequest {
        entity: quartz_protocol::NewCategory {
            uuid: "abcdef00-0000-4000-8000-000000000002"
                .parse()
                .expect("UUID"),
            parent_uuid: None,
            name: Some("Private category".into()),
            color: "neutral".into(),
            tendency: quartz_protocol::CategoryTendency::Neutral,
        },
    };
    let error = WriteRequest::decode(
        quartz_protocol::Op::CategoriesCreate,
        serde_json::to_value(request).expect("body"),
    )
    .expect("decode")
    .execute(
        &mut fixture.store,
        quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        None,
        &mut registry,
    )
    .expect_err("exhaustion");
    assert_eq!(error.kind.code(), "version_exhausted");
    assert_eq!(
        crate::store::schema::categories::table
            .count()
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("categories"),
        5
    );
}

#[test]
fn write_failures_log_typed_causes_without_user_content() {
    use crate::persistence::dispatch::WriteRequest;
    use diesel::connection::SimpleConnection;
    use std::io::{Read, Seek};
    let mut fixture = QueryFixture::new(0);
    let mut registry = crate::rules::registry::RegistryShell::open(
        &mut fixture.store,
        quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
    )
    .expect("registry");
    fixture.store.writer.batch_execute("CREATE TRIGGER reject_note BEFORE INSERT ON annotations BEGIN SELECT RAISE(ABORT, 'private user content'); END; CREATE TRIGGER reject_category BEFORE INSERT ON categories BEGIN SELECT RAISE(ABORT, 'private user content'); END;").expect("inject failures");
    let mut log = tempfile::tempfile().expect("log");
    let subscriber = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(log.try_clone().expect("writer")))
            .finish(),
    );
    let bodies = [
        (
            quartz_protocol::Op::AnnotationsCreate,
            serde_json::json!({"entities":[{"uuid":"abcdef00-0000-4000-8000-000000000001","kind":"note","start_utc_ms":fixture.start,"end_utc_ms":fixture.start+1,"note":"private user content"}]}),
        ),
        (
            quartz_protocol::Op::CategoriesCreate,
            serde_json::json!({"entity":{"uuid":"abcdef00-0000-4000-8000-000000000002","name":"private user content","color":"neutral","tendency":"neutral"}}),
        ),
    ];
    for (op, body) in bodies {
        let error = tracing::dispatcher::with_default(&subscriber, || {
            WriteRequest::decode(op, body).expect("decode").execute(
                &mut fixture.store,
                quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
                None,
                &mut registry,
            )
        })
        .expect_err("injected failure");
        assert_eq!(error.kind, ErrorKind::Internal);
    }
    log.rewind().expect("rewind");
    let mut text = String::new();
    log.read_to_string(&mut text).expect("log text");
    assert_eq!(
        text.lines().count(),
        2,
        "each failed write logs one safe diagnostic: {text}"
    );
    assert!(text.contains("Database"), "typed database cause: {text}");
    assert!(!text.contains("private user content"));
}

#[test]
fn annotation_write_response_joins_category_identities_once() {
    use diesel::{Connection, connection::InstrumentationEvent};
    use std::sync::{Arc, Mutex};
    let mut fixture = QueryFixture::new(0);
    let queries = Arc::new(Mutex::new(Vec::new()));
    let observed = queries.clone();
    fixture
        .store
        .writer
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event {
                observed.lock().expect("queries").push(query.to_string());
            }
        });
    let entities = (0..2)
        .map(|id| quartz_protocol::CreateAnnotation {
            uuid: quartz_protocol::AnnotationId(
                format!("abcdef00-0000-4000-8000-{id:012x}")
                    .parse()
                    .expect("UUID"),
            ),
            kind: quartz_protocol::AnnotationKind::Note,
            values: quartz_protocol::AnnotationValues {
                start_utc_ms: fixture.start,
                end_utc_ms: fixture.start + 1,
                category: Some(quartz_protocol::AnnotationCategory::Builtin {
                    key: quartz_protocol::BuiltinCategory::Learning,
                }),
                label_preset: None,
                label: None,
                note: Some("note".into()),
            },
        })
        .collect();
    let response = fixture
        .store
        .create_annotations(
            quartz_protocol::AnnotationsCreateRequest { entities },
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("annotations");
    assert_eq!(response.entities.len(), 2);
    assert!(response.entities.iter().all(|entity| matches!(
        entity.values.category,
        Some(quartz_protocol::AnnotationCategory::Uuid { .. })
    )));
    let queries = queries.lock().expect("queries");
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.contains("FROM `categories`"))
            .count(),
        2,
        "only category validation looks up categories separately"
    );
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.starts_with("SELECT") && sql.contains("LEFT OUTER JOIN `categories`"))
            .count(),
        1,
        "response identities share one joined statement"
    );
}

#[test]
fn broken_annotation_storage_invariants_return_internal_and_log_safe_causes() {
    use crate::persistence::dispatch::WriteRequest;
    use diesel::connection::SimpleConnection;
    use std::io::{Read, Seek};
    enum Fault {
        DanglingCategory,
        MissingWrittenRow,
    }
    for fault in [Fault::DanglingCategory, Fault::MissingWrittenRow] {
        let mut fixture = QueryFixture::new(0);
        let mut registry = crate::rules::registry::RegistryShell::open(
            &mut fixture.store,
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("registry");
        let request = quartz_protocol::AnnotationsCreateRequest {
            entities: vec![quartz_protocol::CreateAnnotation {
                uuid: quartz_protocol::AnnotationId(
                    "abcdef00-0000-4000-8000-000000000001"
                        .parse()
                        .expect("UUID"),
                ),
                kind: quartz_protocol::AnnotationKind::Note,
                values: quartz_protocol::AnnotationValues {
                    start_utc_ms: fixture.start,
                    end_utc_ms: fixture.start + 1,
                    category: None,
                    label_preset: None,
                    label: None,
                    note: Some("private invariant fixture".into()),
                },
            }],
        };
        let now = quartz_domain::capture::UtcInstant::try_from(0).expect("UTC");
        let expected_rows = match fault {
            Fault::DanglingCategory => {
                fixture
                    .store
                    .create_annotations(request.clone(), now)
                    .expect("existing annotation");
                fixture.store.writer.batch_execute("PRAGMA foreign_keys=OFF; UPDATE annotations SET category_id=9223372036854775807; PRAGMA foreign_keys=ON;").expect("dangling category fixture");
                1
            }
            Fault::MissingWrittenRow => {
                fixture.store.writer.batch_execute("CREATE TRIGGER remove_written_annotation AFTER INSERT ON annotations BEGIN DELETE FROM annotations WHERE id=NEW.id; END;").expect("remove inserted row");
                0
            }
        };
        fixture.store.take_committed_versions();
        let mut log = tempfile::tempfile().expect("isolated log");
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(log.try_clone().expect("writer")))
            .finish();
        let error = tracing::subscriber::with_default(subscriber, || {
            WriteRequest::decode(
                quartz_protocol::Op::AnnotationsCreate,
                serde_json::to_value(request).expect("request"),
            )
            .expect("decode")
            .execute(&mut fixture.store, now, None, &mut registry)
        })
        .expect_err("stored invariant");
        assert_eq!(error.kind, ErrorKind::Internal);
        assert_eq!(
            annotations::table
                .count()
                .get_result::<i64>(&mut fixture.store.writer)
                .expect("rows after rollback"),
            expected_rows
        );
        let changes = fixture.store.take_committed_versions();
        assert!(changes.facts.is_empty() && changes.metadata_version.is_none());
        log.rewind().expect("rewind");
        let mut text = String::new();
        log.read_to_string(&mut text).expect("diagnostic");
        assert_eq!(text.lines().count(), 1);
        assert!(
            text.contains("StoredAnnotation"),
            "typed storage invariant diagnostic: {text}"
        );
        assert!(!text.contains("private invariant fixture"));
    }
}

#[test]
fn range_pages_match_full_scans_for_day_and_week_buckets() {
    use quartz_protocol::{FirstDayOfWeekSetting, RangeBucket};
    const ROWS_PER_DATE: i64 = 900;
    const POPULATED_DATES: i64 = 3;
    const WEEK_MS: i64 = 7 * UTC_DAY_MS;
    let mut fixture = QueryFixture::new(ROWS_PER_DATE * POPULATED_DATES);
    fixture.set_first_weekday(FirstDayOfWeekSetting::Monday);
    let mut request = fixture.range_request(RangeBucket::Day);
    request.end_date = request
        .start_date
        .checked_add(jiff::Span::new().days(21))
        .expect("end");
    let end = crate::query::calendar::DayCalendar::new(
        request.end_date,
        crate::query::calendar::DayStartOffset::MIDNIGHT,
    )
    .expect("end calendar")
    .range
    .start;
    diesel::sql_query("UPDATE intervals SET start_utc_ms=?1+((id-1)/?2)*?3+((id-1)%?2)*duration_ms, end_utc_ms=?1+((id-1)/?2)*?3+((id-1)%?2+1)*duration_ms, start_mono_ms=((id-1)/?2)*?3+((id-1)%?2)*duration_ms, end_mono_ms=((id-1)/?2)*?3+((id-1)%?2+1)*duration_ms")
        .bind::<diesel::sql_types::BigInt, _>(fixture.start)
        .bind::<diesel::sql_types::BigInt, _>(ROWS_PER_DATE)
        .bind::<diesel::sql_types::BigInt, _>(WEEK_MS)
        .execute(&mut fixture.store.writer).expect("spread facts across weeks");
    diesel::update(clock_segments::table)
        .set((
            clock_segments::end_utc_ms.eq(end),
            clock_segments::end_mono_ms.eq(end - fixture.start),
        ))
        .execute(&mut fixture.store.writer)
        .expect("clock coverage");
    diesel::update(capture_runs::table)
        .set(capture_runs::ended_utc_ms.eq(end))
        .execute(&mut fixture.store.writer)
        .expect("run coverage");
    for bucket in [RangeBucket::Day, RangeBucket::Week] {
        request.bucket = bucket;
        request.cursor = None;
        let mut pages = 0;
        let mut total_active = 0;
        loop {
            let optimized = fixture.range_page(request.clone());
            let mut reference = fixture.start(QueryRequest::Range(request.clone()));
            reference.scan_entire_range();
            let reference_page = loop {
                if let Some(response) = reference.advance().expect("full scan") {
                    let QueryResponse::Range(page) = response else {
                        panic!("range response")
                    };
                    break page;
                }
            };
            fixture
                .store
                .return_query_reader(reference.finish().expect("finish reference"));
            assert_eq!(
                optimized.buckets, reference_page.buckets,
                "bucket {bucket:?}, page {pages}"
            );
            assert_eq!(
                optimized.next_cursor, reference_page.next_cursor,
                "cursor {bucket:?}, page {pages}"
            );
            assert_eq!(
                optimized.versions, reference_page.versions,
                "versions {bucket:?}, page {pages}"
            );
            assert_eq!(optimized, reference_page);
            total_active += optimized
                .buckets
                .iter()
                .filter_map(|bucket| bucket.active_ms)
                .sum::<i64>();
            pages += 1;
            let Some(cursor) = optimized.next_cursor else {
                break;
            };
            request.cursor = Some(cursor);
        }
        assert!(
            pages > POPULATED_DATES,
            "multiple pages cross populated bucket boundaries"
        );
        assert_eq!(
            total_active,
            ROWS_PER_DATE * POPULATED_DATES * test_support::FACT_MS
        );
    }
}

#[test]
fn forged_annotation_resume_positions_are_invalid_and_old_versions_are_stale() {
    let mut fixture = QueryFixture::new(0);
    let entity = quartz_protocol::CreateAnnotation {
        uuid: quartz_protocol::AnnotationId(
            "abcdef00-0000-4000-8000-000000000001"
                .parse()
                .expect("UUID"),
        ),
        kind: quartz_protocol::AnnotationKind::Note,
        values: quartz_protocol::AnnotationValues {
            start_utc_ms: fixture.start,
            end_utc_ms: fixture.start + 1,
            category: None,
            label_preset: None,
            label: None,
            note: Some("note".into()),
        },
    };
    fixture
        .store
        .create_annotations(
            quartz_protocol::AnnotationsCreateRequest {
                entities: vec![entity.clone()],
            },
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("note");
    let mut request = quartz_protocol::QueryAnnotationsRequest {
        start_utc_ms: fixture.start,
        end_utc_ms: fixture.start + 100,
        cursor: None,
    };
    let QueryResponse::Annotations(first) = fixture
        .run(QueryRequest::Annotations(request.clone()))
        .expect("first")
    else {
        panic!("annotations")
    };
    let query = quartz_protocol::CursorQuery::Annotations {
        start_utc_ms: request.start_utc_ms,
        end_utc_ms: request.end_utc_ms,
    };
    let resume = quartz_protocol::AnnotationResume {
        start_utc_ms: entity.values.start_utc_ms,
        end_utc_ms: entity.values.end_utc_ms,
        uuid: entity.uuid,
        chunk: 0,
    };
    let valid = QueryCursor::new_annotation(first.versions, query.clone(), resume.clone());
    request.cursor = Some(valid.clone());
    let QueryResponse::Annotations(repeated) = fixture
        .run(QueryRequest::Annotations(request.clone()))
        .expect("valid row position")
    else {
        panic!("annotations")
    };
    assert_eq!(repeated.entities, first.entities);
    enum Forgery {
        MissingUuid,
        WrongStart,
        WrongEnd,
        ChunkMismatch,
        OutOfRangeChunk,
    }
    for forgery in [
        Forgery::MissingUuid,
        Forgery::WrongStart,
        Forgery::WrongEnd,
        Forgery::ChunkMismatch,
        Forgery::OutOfRangeChunk,
    ] {
        let mut wire = serde_json::to_value(&valid).expect("cursor");
        match forgery {
            Forgery::MissingUuid => {
                wire["annotation_resume"]["uuid"] =
                    serde_json::json!("abcdef00-0000-4000-8000-000000000002")
            }
            Forgery::WrongStart => {
                wire["annotation_resume"]["start_utc_ms"] =
                    serde_json::json!(resume.start_utc_ms + 1)
            }
            Forgery::WrongEnd => {
                wire["annotation_resume"]["end_utc_ms"] = serde_json::json!(resume.end_utc_ms + 1)
            }
            Forgery::ChunkMismatch => wire["position"] = serde_json::json!(1),
            Forgery::OutOfRangeChunk => {
                wire["position"] = serde_json::json!(usize::MAX);
                wire["annotation_resume"]["chunk"] = serde_json::json!(usize::MAX);
            }
        }
        request.cursor = Some(serde_json::from_value(wire).expect("opaque forged cursor"));
        let error = fixture
            .run(QueryRequest::Annotations(request.clone()))
            .expect_err("invalid row or chunk");
        assert!(matches!(error, RunQueryError::InvalidCursor));
        assert_eq!(error.kind(), ErrorKind::BadRequest);
    }
    request.cursor = Some(QueryCursor::new(
        quartz_protocol::QueryVersions("older semantics".into()),
        query,
        1,
    ));
    assert!(matches!(
        fixture.run(QueryRequest::Annotations(request)),
        Err(RunQueryError::StaleCursor)
    ));
}

#[test]
fn settings_metadata_exhaustion_returns_version_exhausted_and_rolls_back() {
    use crate::persistence::dispatch::WriteRequest;
    use crate::store::schema::settings;
    let mut fixture = QueryFixture::new(0);
    let mut registry = crate::rules::registry::RegistryShell::open(
        &mut fixture.store,
        quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
    )
    .expect("registry");
    let original = fixture.store.ui_settings().expect("original settings");
    diesel::update(settings::table.filter(settings::key.eq("metadata_version")))
        .set(settings::value.eq(i64::MAX.to_string()))
        .execute(&mut fixture.store.writer)
        .expect("exhaust metadata");
    fixture.store.take_committed_versions();
    let system = quartz_protocol::EffectiveUiSettings {
        resident: Default::default(),
        prewarm: Default::default(),
        ui_language: quartz_protocol::UiLanguage::English,
        locale: None,
    };
    let error = WriteRequest::SettingsSet(quartz_protocol::SettingsSetRequest {
        language: Some(quartz_protocol::UiLanguageSetting::En),
        ..Default::default()
    })
    .execute(
        &mut fixture.store,
        quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        Some(system),
        &mut registry,
    )
    .expect_err("exhausted settings write");
    assert_eq!(error.kind, ErrorKind::VersionExhausted);
    assert_eq!(
        fixture.store.ui_settings().expect("unchanged settings"),
        original
    );
    assert_eq!(
        settings::table
            .filter(settings::key.eq("metadata_version"))
            .select(settings::value)
            .first::<String>(&mut fixture.store.writer)
            .expect("metadata"),
        i64::MAX.to_string()
    );
    let effects = fixture.store.take_committed_versions();
    assert!(effects.facts.is_empty() && effects.metadata_version.is_none());
}

#[test]
fn committed_rule_mutations_stale_issued_rule_and_day_page_cursors() {
    use crate::rules::{
        read::{ReadInputs, ReadRequest, ReadResponse},
        registry::RegistryShell,
        write::RuleMutation,
    };
    use quartz_protocol::{
        RuleKind, RuleSetsEditRequest, RuleSetsGetRequest, RuleSetsTreeRequest, RuleView,
    };
    const FACT_COUNT: i64 = 2000;
    const KEY_PADDING: usize = 80 * 1024;
    const RULES_PER_SET: usize = 24;
    const VALUE_BYTES: usize = 3000;
    let mut fixture = QueryFixture::new(FACT_COUNT);
    let now = quartz_domain::capture::UtcInstant::try_from(0).expect("UTC");
    let mut registry = RegistryShell::open(&mut fixture.store, now).expect("rules");
    let root = "user.cursor-root";
    RuleMutation::Edit(RuleSetsEditRequest {
        key: root.into(),
        base_version: 0,
        text: format!(
            "format='quartz-rules/1'\n[set]\nkey='{root}'\nkind='classification'\nname='Root'\n"
        ),
        preview_receipt: None,
    })
    .execute(&mut registry, &mut fixture.store, now)
    .expect("root");
    let entries:String=(0..RULES_PER_SET).map(|index|format!("[[rule]]\nid='rule-{index}'\nlabel='Rule'\ncategory='builtin:learning'\nwhen=[{{field='app.exe_name',op='equals',value='{}'}}]\n","x".repeat(VALUE_BYTES))).collect();
    for suffix in ["a", "b", "c"] {
        let key = format!("user.cursor-{suffix}-{}", "x".repeat(KEY_PADDING));
        let text = format!(
            "format='quartz-rules/1'\n[set]\nkey='{key}'\nkind='classification'\nname='Child'\nparent='{root}'\n{entries}"
        );
        assert!(text.len() <= quartz_protocol::MAX_RULE_TEXT_BYTES);
        RuleMutation::Edit(RuleSetsEditRequest {
            key,
            base_version: 0,
            text,
            preview_receipt: None,
        })
        .execute(&mut registry, &mut fixture.store, now)
        .expect("bounded child");
    }
    let old = registry.snapshots().load();
    let metadata_version = fixture.store.rule_read_metadata().expect("metadata");
    let old_inputs = || ReadInputs {
        snapshot: old.clone(),
        metadata_version,
    };
    let mut get = RuleSetsGetRequest {
        key: root.into(),
        version: None,
        view: RuleView::Effective,
        cursor: None,
    };
    let ReadResponse::Get(first_get) = ReadRequest::Get(get.clone())
        .execute(old_inputs())
        .expect("get page")
    else {
        panic!("get response")
    };
    get.cursor = Some(first_get.next_cursor.expect("issued get continuation"));
    let mut tree = RuleSetsTreeRequest {
        kind: RuleKind::Classification,
        cursor: None,
    };
    let ReadResponse::Tree(first_tree) = ReadRequest::Tree(tree.clone())
        .execute(old_inputs())
        .expect("tree page")
    else {
        panic!("tree response")
    };
    tree.cursor = Some(first_tree.next_cursor.expect("issued tree continuation"));
    let QueryRequest::Day(mut day) = fixture.request_day() else {
        panic!("day request")
    };
    let QueryResponse::Day(first_day) = fixture
        .run(QueryRequest::Day(day.clone()))
        .expect("day page")
    else {
        panic!("day response")
    };
    day.cursor = Some(first_day.next_cursor.expect("issued day continuation"));
    RuleMutation::Edit(RuleSetsEditRequest {key:"user.after-cursor".into(),base_version:0,text:"format='quartz-rules/1'\n[set]\nkey='user.after-cursor'\nkind='classification'\nname='Changed'\n".into(),preview_receipt:None}).execute(&mut registry,&mut fixture.store,now).expect("committed mutation");
    for request in [
        ReadRequest::Get(get.clone()),
        ReadRequest::Tree(tree.clone()),
    ] {
        let error = request
            .execute(ReadInputs {
                snapshot: registry.snapshots().load(),
                metadata_version,
            })
            .expect_err("cursor changed after commit");
        assert_eq!(error.kind, ErrorKind::StaleCursor);
    }
    assert!(matches!(
        fixture.run(QueryRequest::Day(day)),
        Err(RunQueryError::StaleCursor)
    ));
    let ReadResponse::Get(continued) = ReadRequest::Get(get)
        .execute(old_inputs())
        .expect("retained snapshot continuation")
    else {
        panic!("get response")
    };
    assert_eq!(continued.versions, first_get.versions);
    let ReadResponse::Tree(continued) = ReadRequest::Tree(tree)
        .execute(old_inputs())
        .expect("retained tree continuation")
    else {
        panic!("tree response")
    };
    assert_eq!(continued.versions, first_tree.versions);
}
