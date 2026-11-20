use super::*;
use crate::query::{QueryJob, QueryRequest, QueryResponse};
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
    let mut pipeline = crate::query::pipeline::Pipeline::new(range);
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
                QueryJob::start(&mut reader, old, "changed-classification-hash"),
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
