use super::*;
use crate::store::{
    enums::{AnnotationKind, ApplicationKind, GapReason, IntegerCode, TitleStatus},
    rows::{ApplicationsRow, CaptureRunsRow, ClockSegmentsRow, DataDayVersionsRow, TitlesRow},
    schema::{
        applications, browser_streams, capture_runs, clock_segments, data_day_versions, domains,
        titles,
    },
};
use diesel::connection::SimpleConnection;

fn open_test_store() -> (tempfile::TempDir, DataDirectory, Store) {
    let temp = tempfile::tempdir().expect("isolated store directory");
    let directory = DataDirectory::new(temp.path());
    let store = Store::open(&directory).expect("open store");
    (temp, directory, store)
}

fn seed_parents(store: &mut Store) -> (i64, i64, i64) {
    let run_id = diesel::insert_into(capture_runs::table)
        .values(CaptureRunsRow {
            id: None,
            device_id: "device-1".into(),
            win_session_id: 1,
            app_version: "0.1.0".into(),
            started_utc_ms: 0,
            confirmed_utc_ms: Some(10 * UTC_DAY_MS),
            ended_utc_ms: None,
            end_reason: None,
        })
        .returning(capture_runs::id.assume_not_null())
        .get_result::<i64>(&mut store.writer)
        .expect("insert run");
    let clock_id = diesel::insert_into(clock_segments::table)
        .values(ClockSegmentsRow {
            id: None,
            run_id,
            start_utc_ms: 0,
            start_mono_ms: 0,
            confirmed_utc_ms: Some(10 * UTC_DAY_MS),
            confirmed_mono_ms: Some(10 * UTC_DAY_MS),
            end_utc_ms: None,
            end_mono_ms: None,
            end_reason: None,
        })
        .returning(clock_segments::id.assume_not_null())
        .get_result::<i64>(&mut store.writer)
        .expect("insert clock segment");
    let app_id = diesel::insert_into(applications::table)
        .values(ApplicationsRow {
            id: None,
            identity_key: "exe:editor.exe".into(),
            kind: ApplicationKind::Exe,
            exe_path: Some("C:\\editor.exe".into()),
            aumid: None,
            exe_name: Some("editor.exe".into()),
            display_name: "Editor".into(),
            first_seen_utc_ms: 0,
        })
        .returning(applications::id.assume_not_null())
        .get_result::<i64>(&mut store.writer)
        .expect("insert app");
    (run_id, clock_id, app_id)
}

fn insert_title(store: &mut Store, text: &str) -> i64 {
    diesel::insert_into(titles::table)
        .values(TitlesRow {
            id: None,
            text: text.into(),
        })
        .returning(titles::id.assume_not_null())
        .get_result::<i64>(&mut store.writer)
        .expect("insert title")
}

fn insert_domain(store: &mut Store, host: &str) -> i64 {
    diesel::insert_into(domains::table)
        .values(domains::host.eq(host))
        .returning(domains::id.assume_not_null())
        .get_result::<i64>(&mut store.writer)
        .expect("insert domain")
}

fn insert_stream(store: &mut Store) -> i64 {
    diesel::insert_into(browser_streams::table)
        .values((
            browser_streams::browser.eq("chrome"),
            browser_streams::profile_instance_id.eq("profile-1"),
            browser_streams::extension_id.eq("ext-1"),
            browser_streams::first_seen_utc_ms.eq(0),
            browser_streams::last_seen_utc_ms.eq(10 * UTC_DAY_MS),
        ))
        .returning(browser_streams::id.assume_not_null())
        .get_result::<i64>(&mut store.writer)
        .expect("insert browser stream")
}

fn insert_closed_interval(
    store: &mut Store,
    run_id: i64,
    clock_id: i64,
    app_id: i64,
    title_id: Option<i64>,
    span: std::ops::Range<i64>,
    capture_level: CaptureLevel,
) -> i64 {
    let std::ops::Range { start, end } = span;
    diesel::insert_into(intervals::table)
        .values(IntervalsRow {
            id: None,
            run_id,
            sequence: start,
            clock_segment_id: clock_id,
            kind: IntervalKind::Window,
            no_foreground_reason: None,
            app_id: Some(app_id),
            window_instance_id: None,
            title_id,
            title_status: if title_id.is_some() {
                TitleStatus::Ok
            } else {
                TitleStatus::Redacted
            },
            capture_level,
            privacy_rule_id: None,
            privacy_label_key: None,
            privacy_label: None,
            session_key: 1,
            start_utc_ms: start,
            start_mono_ms: start,
            end_utc_ms: Some(end),
            end_mono_ms: Some(end),
            confirmed_utc_ms: None,
            confirmed_mono_ms: None,
            duration_ms: Some(end - start),
            end_reason: Some(IntervalEndReason::Switch),
        })
        .returning(intervals::id.assume_not_null())
        .get_result::<i64>(&mut store.writer)
        .expect("insert interval")
}

#[test]
fn data_info_lists_and_classifies_backups_newest_first_and_ignores_other_files() {
    let (_temp, directory, mut store) = open_test_store();
    // Write a transaction so the WAL file has data too.
    store
        .test_metadata_write(crate::store::versions::TestCommit::Commit)
        .expect("write WAL");

    let backups = directory.backups();
    fs::create_dir_all(&backups).expect("backups dir");
    // Remove initial migration backup to control fixture timestamps exactly.
    for entry in fs::read_dir(&backups).expect("read backups") {
        fs::remove_file(entry.expect("entry").path()).expect("remove initial backup");
    }
    fs::write(backups.join("pre-migrate-0-1700000000000.db"), b"old-mig").expect("old mig");
    fs::write(backups.join("manual-20261127T085340Z.db"), b"manual-new").expect("manual new");
    fs::write(
        backups.join("pre-migrate-20261116000000-1750000000000.db"),
        b"mid-mig",
    )
    .expect("mid mig");
    fs::write(backups.join("manual-20250101T000000Z.db"), b"manual-old").expect("manual old");
    fs::write(backups.join("notes.txt"), b"ignore").expect("unrelated file");
    fs::write(backups.join("manual-bad.db"), b"ignore-bad").expect("bad manual");
    fs::create_dir(backups.join("manual-20261128T000000Z.db")).expect("ignore directory");

    let info = store.data_info().expect("data info");
    assert_eq!(
        info.database_path,
        directory.database().to_str().expect("db")
    );
    assert_eq!(info.logs_path, directory.logs().to_str().expect("logs"));
    assert_eq!(info.backups_path, backups.to_str().expect("backups"));
    let expected_main = fs::metadata(directory.database()).expect("main").len();
    let expected_wal = fs::metadata(directory.database_wal()).map_or(0, |meta| meta.len());
    assert_eq!(info.database_bytes, expected_main + expected_wal);

    let names: Vec<_> = info
        .backups
        .iter()
        .map(|entry| entry.file_name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "manual-20261127T085340Z.db",
            "pre-migrate-20261116000000-1750000000000.db",
            "manual-20250101T000000Z.db",
            "pre-migrate-0-1700000000000.db",
        ]
    );
    assert_eq!(info.backups[0].kind, BackupKind::Manual);
    assert_eq!(info.backups[0].bytes, 10);
    assert_eq!(info.backups[1].kind, BackupKind::Migration);
    assert_eq!(info.backups[1].created_utc_ms, 1_750_000_000_000);
    assert_eq!(info.backups[2].kind, BackupKind::Manual);
    assert_eq!(info.backups[3].kind, BackupKind::Migration);
}

#[test]
fn manual_backup_creates_valid_file_and_refuses_to_overwrite() {
    let (_temp, directory, mut store) = open_test_store();
    let now_ts: jiff::Timestamp = "2026-11-27T08:53:40.500Z".parse().expect("timestamp");
    let expected_ms = "2026-11-27T08:53:40Z"
        .parse::<jiff::Timestamp>()
        .expect("truncated timestamp")
        .as_millisecond();
    let entry = store
        .create_manual_backup(UtcInstant::new(now_ts))
        .expect("create manual backup");
    assert_eq!(entry.file_name, "manual-20261127T085340Z.db");
    assert_eq!(entry.kind, BackupKind::Manual);
    assert_eq!(entry.created_utc_ms, expected_ms);
    assert!(entry.bytes > 0);
    let path = directory.backups().join(&entry.file_name);
    assert!(path.is_file());

    // A second backup in the same UTC second must fail instead of overwriting.
    let same_second: jiff::Timestamp = "2026-11-27T08:53:40.999Z".parse().expect("timestamp");
    let err = store
        .create_manual_backup(UtcInstant::new(same_second))
        .expect_err("refuse overwrite");
    assert!(matches!(err, CreateManualBackupError::AlreadyExists { .. }));

    // The created backup appears in data_info with identical metadata.
    let info = store.data_info().expect("info");
    assert!(info.backups.contains(&entry));
}

#[test]
fn compact_vacuums_and_truncates_wal_returning_sizes() {
    let (_temp, directory, mut store) = open_test_store();
    // Create and then drop large titles so VACUUM reclaims pages and WAL truncates.
    for index in 0..200 {
        let _ = insert_title(&mut store, &format!("padding-{index}-{}", "x".repeat(512)));
    }
    ConnectionSql::new(&mut store.writer)
        .delete_unreferenced_titles()
        .expect("delete unreferenced padding");

    let response = store.compact().expect("compact database");
    assert!(response.before_bytes > 0);
    assert!(response.after_bytes > 0);
    assert!(
        response.after_bytes < response.before_bytes,
        "expected compaction to shrink DB+WAL ({} -> {})",
        response.before_bytes,
        response.after_bytes
    );
    let wal_len = fs::metadata(directory.database_wal()).map_or(0, |meta| meta.len());
    assert_eq!(wal_len, 0, "WAL should be truncated after compact");
}

#[test]
fn delete_range_removes_clips_splits_cleans_dictionaries_and_bumps_affected_versions_only() {
    let (_temp, _directory, mut store) = open_test_store();
    let (run_id, clock_id, app_id) = seed_parents(&mut store);
    let stream_id = insert_stream(&mut store);

    let day1 = UTC_DAY_MS;
    let day2 = 2 * UTC_DAY_MS;
    let day3 = 3 * UTC_DAY_MS;
    let delete_range = QueryRange {
        start: day2,
        end: day3,
    };

    let title_inside = insert_title(&mut store, "only-inside");
    let title_shared = insert_title(&mut store, "kept-outside");
    let title_web_only = insert_title(&mut store, "web-inside-only");
    let domain_inside = insert_domain(&mut store, "deleted.example");
    let domain_kept = insert_domain(&mut store, "kept.example");

    // Intervals:
    // 1. Outside before [day1 + 100, day1 + 500) -> untouched
    let outside_before = insert_closed_interval(
        &mut store,
        run_id,
        clock_id,
        app_id,
        Some(title_shared),
        day1 + 100..day1 + 500,
        CaptureLevel::Full,
    );
    // 2. Straddling left [day2 - 1000, day2 + 1000) -> clipped to [day2 - 1000, day2), removes 1000ms
    let straddle_left = insert_closed_interval(
        &mut store,
        run_id,
        clock_id,
        app_id,
        Some(title_shared),
        day2 - 1_000..day2 + 1_000,
        CaptureLevel::Full,
    );
    // 3. Fully inside [day2 + 2000, day2 + 7000) -> deleted, removes 5000ms
    let fully_inside = insert_closed_interval(
        &mut store,
        run_id,
        clock_id,
        app_id,
        Some(title_inside),
        day2 + 2_000..day2 + 7_000,
        CaptureLevel::Full,
    );
    // 4. Fully inside Excluded [day2 + 7000, day2 + 9000) -> deleted, adds 0 to deleted_ms
    let _excluded_inside = insert_closed_interval(
        &mut store,
        run_id,
        clock_id,
        app_id,
        None,
        day2 + 7_000..day2 + 9_000,
        CaptureLevel::Excluded,
    );
    // 5. Enclosing [day2 - 500, day3 + 500) -> split into [day2 - 500, day2) and [day3, day3 + 500), removes UTC_DAY_MS
    let enclosing = insert_closed_interval(
        &mut store,
        run_id,
        clock_id,
        app_id,
        Some(title_shared),
        day2 - 500..day3 + 500,
        CaptureLevel::Full,
    );
    // 6. Straddling right [day3 - 300, day3 + 700) -> clipped to [day3, day3 + 700), removes 300ms
    let straddle_right = insert_closed_interval(
        &mut store,
        run_id,
        clock_id,
        app_id,
        Some(title_shared),
        day3 - 300..day3 + 700,
        CaptureLevel::Full,
    );
    // 7. Outside after [day3 + 1000, day3 + 2000) -> untouched
    let outside_after = insert_closed_interval(
        &mut store,
        run_id,
        clock_id,
        app_id,
        Some(title_shared),
        day3 + 1_000..day3 + 2_000,
        CaptureLevel::Full,
    );

    // Web intervals: inside (linked to fully_inside) + enclosing (linked to enclosing)
    diesel::insert_into(web_intervals::table)
        .values(WebIntervalsRow {
            id: None,
            stream_id,
            interval_id: fully_inside,
            start_utc_ms: day2 + 2_500,
            end_utc_ms: Some(day2 + 6_500),
            confirmed_utc_ms: None,
            end_reason: Some(1),
            domain_id: Some(domain_inside),
            tab_title_id: Some(title_web_only),
            tab_title_status: TitleStatus::Ok,
            tab_id: Some(1),
            browser_window_id: Some(1),
            navigation_id: Some(1),
            audible: 0,
            link_status: 1,
            web_level: 1,
            privacy_rule_id: None,
            privacy_label_key: None,
            privacy_label: None,
        })
        .execute(&mut store.writer)
        .expect("insert inside web");
    diesel::insert_into(web_intervals::table)
        .values(WebIntervalsRow {
            id: None,
            stream_id,
            interval_id: enclosing,
            start_utc_ms: day2 - 400,
            end_utc_ms: Some(day3 + 400),
            confirmed_utc_ms: None,
            end_reason: Some(1),
            domain_id: Some(domain_kept),
            tab_title_id: Some(title_shared),
            tab_title_status: TitleStatus::Ok,
            tab_id: Some(2),
            browser_window_id: Some(1),
            navigation_id: Some(2),
            audible: 0,
            link_status: 1,
            web_level: 1,
            privacy_rule_id: None,
            privacy_label_key: None,
            privacy_label: None,
        })
        .execute(&mut store.writer)
        .expect("insert enclosing web");

    // Availability: enclosing closed row + open Idle row straddling right boundary.
    diesel::insert_into(availability_intervals::table)
        .values(AvailabilityIntervalsRow {
            id: None,
            run_id,
            clock_segment_id: clock_id,
            reason: AvailabilityReason::Locked,
            start_utc_ms: day2 - 200,
            start_mono_ms: day2 - 200,
            end_utc_ms: Some(day3 + 200),
            end_mono_ms: Some(day3 + 200),
            confirmed_utc_ms: None,
            confirmed_mono_ms: None,
            end_reason: Some(AvailabilityEndReason::Resumed),
            last_input_utc_ms: None,
            detected_utc_ms: None,
            threshold_ms: None,
            exception: None,
            policy_version: 1,
        })
        .execute(&mut store.writer)
        .expect("insert enclosing availability");
    let open_idle_id = diesel::insert_into(availability_intervals::table)
        .values(AvailabilityIntervalsRow {
            id: None,
            run_id,
            clock_segment_id: clock_id,
            reason: AvailabilityReason::Idle,
            start_utc_ms: day3 - 60_000,
            start_mono_ms: day3 - 60_000,
            end_utc_ms: None,
            end_mono_ms: None,
            confirmed_utc_ms: Some(day3 + 120_000),
            confirmed_mono_ms: Some(day3 + 120_000),
            end_reason: None,
            last_input_utc_ms: Some(day3 - 360_000),
            detected_utc_ms: Some(day3 - 10_000),
            threshold_ms: Some(300_000),
            exception: None,
            policy_version: 1,
        })
        .returning(availability_intervals::id.assume_not_null())
        .get_result::<i64>(&mut store.writer)
        .expect("insert open idle");

    // Capture gap: enclosing [day2 - 100, day3 + 100) -> split
    diesel::insert_into(capture_gaps::table)
        .values(CaptureGapsRow {
            id: None,
            run_id,
            clock_segment_id: clock_id,
            start_utc_ms: day2 - 100,
            end_utc_ms: day3 + 100,
            start_mono_ms: day2 - 100,
            end_mono_ms: day3 + 100,
            reason: GapReason::DatabaseError,
        })
        .execute(&mut store.writer)
        .expect("insert enclosing gap");

    // Annotations: inside Note + enclosing Note
    diesel::insert_into(annotations::table)
        .values(AnnotationsRow {
            id: None,
            uuid: "00000000-0000-4000-8000-000000000010".into(),
            kind: AnnotationKind::Note,
            start_utc_ms: day2 + 100,
            end_utc_ms: day2 + 900,
            category_id: None,
            label: Some("inside".into()),
            note: None,
            created_utc_ms: 1,
            updated_utc_ms: 1,
            label_preset: None,
        })
        .execute(&mut store.writer)
        .expect("insert inside annotation");
    diesel::insert_into(annotations::table)
        .values(AnnotationsRow {
            id: None,
            uuid: "00000000-0000-4000-8000-000000000020".into(),
            kind: AnnotationKind::Note,
            start_utc_ms: day2 - 300,
            end_utc_ms: day3 + 300,
            category_id: None,
            label: Some("enclosing".into()),
            note: Some("keep both sides".into()),
            created_utc_ms: 1,
            updated_utc_ms: 1,
            label_preset: None,
        })
        .execute(&mut store.writer)
        .expect("insert enclosing annotation");

    // Add a row far away on day 5 to verify unaffected dates do NOT get bumped.
    let _far_away = insert_closed_interval(
        &mut store,
        run_id,
        clock_id,
        app_id,
        Some(title_shared),
        5 * UTC_DAY_MS + 100..5 * UTC_DAY_MS + 200,
        CaptureLevel::Full,
    );

    let now = UtcInstant::try_from(6 * UTC_DAY_MS).expect("now");
    let summary = store.delete_range(delete_range, now).expect("delete range");

    assert_eq!(summary.deleted_ms, 1_000 + 5_000 + UTC_DAY_MS + 300);

    // Check intervals: outside_before, straddle_left, enclosing-left, enclosing-right, straddle_right, outside_after, far_away = 7 rows.
    let remaining_intervals = intervals::table
        .order((intervals::start_utc_ms, intervals::end_utc_ms))
        .select(IntervalsRow::as_select())
        .load::<IntervalsRow>(&mut store.writer)
        .expect("load intervals");
    let spans: Vec<_> = remaining_intervals
        .iter()
        .map(|row| (row.start_utc_ms, row.end_utc_ms, row.duration_ms))
        .collect();
    assert_eq!(
        spans,
        vec![
            (day1 + 100, Some(day1 + 500), Some(400)),
            (day2 - 1_000, Some(day2), Some(1_000)),
            (day2 - 500, Some(day2), Some(500)),
            (day3, Some(day3 + 500), Some(500)),
            (day3, Some(day3 + 700), Some(700)),
            (day3 + 1_000, Some(day3 + 2_000), Some(1_000)),
            (5 * UTC_DAY_MS + 100, Some(5 * UTC_DAY_MS + 200), Some(100)),
        ]
    );
    assert_eq!(remaining_intervals[0].id, Some(outside_before));
    assert_eq!(remaining_intervals[1].id, Some(straddle_left));
    assert_eq!(remaining_intervals[2].id, Some(enclosing));
    let enclosing_right_id = remaining_intervals[3].id.expect("right split id");
    assert_ne!(enclosing_right_id, enclosing);
    assert_eq!(remaining_intervals[4].id, Some(straddle_right));
    assert_eq!(remaining_intervals[5].id, Some(outside_after));

    // Check web_intervals: inside deleted; enclosing split and right piece repointed to enclosing_right_id.
    let remaining_web = web_intervals::table
        .order(web_intervals::start_utc_ms)
        .select(WebIntervalsRow::as_select())
        .load::<WebIntervalsRow>(&mut store.writer)
        .expect("load web");
    assert_eq!(remaining_web.len(), 2);
    assert_eq!(remaining_web[0].start_utc_ms, day2 - 400);
    assert_eq!(remaining_web[0].end_utc_ms, Some(day2));
    assert_eq!(remaining_web[0].interval_id, enclosing);
    assert_eq!(remaining_web[1].start_utc_ms, day3);
    assert_eq!(remaining_web[1].end_utc_ms, Some(day3 + 400));
    assert_eq!(remaining_web[1].interval_id, enclosing_right_id);

    // Check availability: Locked split into 2 closed rows; open Idle kept same id and clamped invariants.
    let remaining_avail = availability_intervals::table
        .order((
            availability_intervals::reason,
            availability_intervals::start_utc_ms,
        ))
        .select(AvailabilityIntervalsRow::as_select())
        .load::<AvailabilityIntervalsRow>(&mut store.writer)
        .expect("load availability");
    assert_eq!(remaining_avail.len(), 3);
    let open_idle = &remaining_avail[0];
    assert_eq!(open_idle.id, Some(open_idle_id));
    assert_eq!(open_idle.start_utc_ms, day3);
    assert_eq!(open_idle.end_utc_ms, None);
    assert_eq!(open_idle.confirmed_utc_ms, Some(day3 + 120_000));
    assert!(open_idle.detected_utc_ms.expect("detected") >= open_idle.start_utc_ms);
    assert!(open_idle.last_input_utc_ms.expect("last_input") <= open_idle.start_utc_ms);
    assert_eq!(remaining_avail[1].start_utc_ms, day2 - 200);
    assert_eq!(remaining_avail[1].end_utc_ms, Some(day2));
    assert_eq!(remaining_avail[2].start_utc_ms, day3);
    assert_eq!(remaining_avail[2].end_utc_ms, Some(day3 + 200));

    // Check capture_gaps: enclosing split into 2 rows.
    let remaining_gaps = capture_gaps::table
        .order(capture_gaps::start_utc_ms)
        .select(CaptureGapsRow::as_select())
        .load::<CaptureGapsRow>(&mut store.writer)
        .expect("load gaps");
    assert_eq!(remaining_gaps.len(), 2);
    assert_eq!(
        (remaining_gaps[0].start_utc_ms, remaining_gaps[0].end_utc_ms),
        (day2 - 100, day2)
    );
    assert_eq!(
        (remaining_gaps[1].start_utc_ms, remaining_gaps[1].end_utc_ms),
        (day3, day3 + 100)
    );

    // Check annotations: inside deleted; enclosing split into 2 rows with distinct UUIDs.
    let remaining_ann = annotations::table
        .order(annotations::start_utc_ms)
        .select(AnnotationsRow::as_select())
        .load::<AnnotationsRow>(&mut store.writer)
        .expect("load annotations");
    assert_eq!(remaining_ann.len(), 2);
    assert_eq!(
        remaining_ann[0].uuid,
        "00000000-0000-4000-8000-000000000020"
    );
    assert_eq!(
        (remaining_ann[0].start_utc_ms, remaining_ann[0].end_utc_ms),
        (day2 - 300, day2)
    );
    assert_eq!(remaining_ann[0].updated_utc_ms, now.get().as_millisecond());
    assert_ne!(remaining_ann[1].uuid, remaining_ann[0].uuid);
    assert_eq!(
        (remaining_ann[1].start_utc_ms, remaining_ann[1].end_utc_ms),
        (day3, day3 + 300)
    );
    assert_eq!(remaining_ann[1].created_utc_ms, now.get().as_millisecond());

    // Check dictionary cleanup: unreferenced titles & domains removed, referenced kept.
    let title_texts: Vec<String> = titles::table
        .select(titles::text)
        .order(titles::text)
        .load(&mut store.writer)
        .expect("titles");
    assert_eq!(title_texts, vec!["kept-outside"]);
    let domain_hosts: Vec<String> = domains::table
        .select(domains::host)
        .order(domains::host)
        .load(&mut store.writer)
        .expect("domains");
    assert_eq!(domain_hosts, vec!["kept.example"]);

    // Check data_day_versions: days 1, 2, 3 bumped for all 5 touched sources; day 5 NOT bumped!
    let versions: Vec<(i32, i64, i64)> = data_day_versions::table
        .select(DataDayVersionsRow::as_select())
        .load::<DataDayVersionsRow>(&mut store.writer)
        .expect("versions")
        .into_iter()
        .map(|row| (row.source.code(), row.utc_date, row.version))
        .collect();
    for source in [
        DataSource::Intervals,
        DataSource::Availability,
        DataSource::Web,
        DataSource::Annotations,
        DataSource::Gaps,
    ] {
        for utc_day in [1, 2, 3] {
            assert!(
                versions.contains(&(source.code(), utc_day, 1)),
                "expected version bump for {source:?} day {utc_day}"
            );
        }
        assert!(
            !versions
                .iter()
                .any(|&(s, d, _)| s == source.code() && d == 5),
            "day 5 must not be bumped for {source:?}"
        );
    }

    // Verify full SQLite integrity + foreign keys after all mutations.
    assert!(
        ConnectionSql::new(&mut store.writer)
            .is_healthy()
            .expect("integrity check")
    );
    // Verify no deletion_barriers rows were written.
    let barriers: i64 = diesel::sql_query("SELECT count(*) AS value FROM deletion_barriers")
        .get_result::<BarrierCount>(&mut store.writer)
        .expect("barriers count")
        .value;
    assert_eq!(barriers, 0);
}

#[derive(diesel::QueryableByName)]
struct BarrierCount {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    value: i64,
}

#[test]
fn delete_range_clips_open_foreground_interval_preserving_id_and_utc_midnight_split() {
    let (_temp, _directory, mut store) = open_test_store();
    let (run_id, clock_id, app_id) = seed_parents(&mut store);

    let day1 = UTC_DAY_MS;
    let day3 = 3 * UTC_DAY_MS;
    let day4 = 4 * UTC_DAY_MS;

    // Open interval starting on day 1, confirmed on day 4 (encloses [day3, day4) and spans day 1..3 on the left).
    let open_id = diesel::insert_into(intervals::table)
        .values(IntervalsRow {
            id: None,
            run_id,
            sequence: 1,
            clock_segment_id: clock_id,
            kind: IntervalKind::Window,
            no_foreground_reason: None,
            app_id: Some(app_id),
            window_instance_id: None,
            title_id: None,
            title_status: TitleStatus::Empty,
            capture_level: CaptureLevel::Full,
            privacy_rule_id: None,
            privacy_label_key: None,
            privacy_label: None,
            session_key: 42,
            start_utc_ms: day1 + 1_000,
            start_mono_ms: day1 + 1_000,
            end_utc_ms: None,
            end_mono_ms: None,
            confirmed_utc_ms: Some(day4 + 5_000),
            confirmed_mono_ms: Some(day4 + 5_000),
            duration_ms: None,
            end_reason: None,
        })
        .returning(intervals::id.assume_not_null())
        .get_result::<i64>(&mut store.writer)
        .expect("insert open interval");

    let summary = store
        .delete_range(
            QueryRange {
                start: day3,
                end: day4,
            },
            UtcInstant::try_from(day4 + 6_000).expect("now"),
        )
        .expect("delete range");
    assert_eq!(summary.deleted_ms, UTC_DAY_MS);

    let rows = intervals::table
        .order(intervals::start_utc_ms)
        .select(IntervalsRow::as_select())
        .load::<IntervalsRow>(&mut store.writer)
        .expect("load intervals");
    // Left remainder [day1 + 1000, day3) split at UTC midnight day2 into 2 closed rows + 1 open row at [day4, confirmed day4 + 5000).
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].start_utc_ms, day1 + 1_000);
    assert_eq!(rows[0].end_utc_ms, Some(2 * UTC_DAY_MS));
    assert_eq!(rows[0].end_reason, Some(IntervalEndReason::MidnightSplit));
    assert_eq!(rows[1].start_utc_ms, 2 * UTC_DAY_MS);
    assert_eq!(rows[1].end_utc_ms, Some(day3));
    assert_eq!(rows[1].end_reason, Some(IntervalEndReason::PolicyChanged));
    assert_eq!(rows[2].id, Some(open_id));
    assert_eq!(rows[2].start_utc_ms, day4);
    assert_eq!(rows[2].end_utc_ms, None);
    assert_eq!(rows[2].confirmed_utc_ms, Some(day4 + 5_000));
    // Ensure closed-row foreign keys and integrity hold.
    store
        .writer
        .batch_execute("PRAGMA foreign_key_check")
        .expect("fk check");
}
