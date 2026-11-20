use super::*;
use crate::query::{QueryJob, QueryRequest, QueryResponse, RunQueryError};
use quartz_protocol::{Activity, ErrorKind, QUERY_PAGE_BYTES, QueryCursor};
use test_support::QueryFixture;

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
    for segment in pipeline.into_segments() {
        projection.push(&segment, request.detail, request.ms_per_pixel.0.get());
    }
    fixture
        .store
        .return_query_reader(snapshot.finish().expect("finish snapshot"));
    projection.into_segments()
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
                revision: 2,
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
            RegistryCommand::InstallBuiltin { set, revision: 2 },
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
        matches!(&old.segments[0].category_source, CategorySource::Rule { rule } if rule.as_str() == "builtin.classification/vscode@1")
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
        matches!(&new.segments[0].category_source, CategorySource::Rule { rule } if rule.as_str() == "builtin.classification/vscode@2")
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
