use super::*;
use crate::store::{ReadOnlyStore, Store};
use crate::{
    data_directory::DataDirectory,
    store::capture::{CaptureKeys, RunMetadata, WindowsSessionId},
};
use quartz_domain::capture::{
    self as capture, AvailabilityEndReason, AvailabilityReason, CaptureWrite, ClockEndReason,
    OpenAvailability, RowId, RunEndReason, RunId, SegmentId, StopReason,
};

#[test]
fn decoding_rejects_missing_ids_and_ambiguous_endpoints() {
    let kind = FactKind::Foreground;
    assert!(matches!(
        Fact::new(kind, None, 0, Some(1), None),
        Err(ReadFactsError::MissingId {
            kind: FactKind::Foreground
        })
    ));
    for (end, confirmed) in [(None, None), (Some(1), Some(1))] {
        assert!(matches!(
            Fact::new(kind, Some(1), 0, end, confirmed),
            Err(ReadFactsError::InvalidEnd { id: 1, .. })
        ));
    }
    let closed = Fact::new(kind, Some(1), 0, Some(2), None).expect("closed endpoint");
    let open = Fact::new(kind, Some(1), 0, None, Some(2)).expect("open confirmation");
    assert_eq!(closed.end, FactEnd::Closed(2));
    assert_eq!(open.end, FactEnd::Open { confirmed: 2 });
    let json = serde_json::to_value(open).expect("serialize typed endpoint");
    assert!(json["end_utc_ms"].is_null());
    assert_eq!(json["confirmed_utc_ms"], 2);
    assert_eq!(json["kind"], "foreground");
    assert_eq!(
        serde_json::to_value(enums::AvailabilityReason::Locked).expect("strum label"),
        "locked"
    );
}

#[test]
fn consistency_unions_stops_and_rejects_a_coverage_hole() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    for (name, covered_ms, expected) in [("complete", 60_000, Ok(())), ("missing", 30_000, Err(()))]
    {
        let directory = DataDirectory::new(temporary.path().join(name));
        std::fs::create_dir(directory.as_path()).expect("data directory");
        let mut store = Store::open(&directory).expect("database");
        let at = |millis| capture::Timestamp {
            utc: capture::UtcInstant::try_from(millis).expect("UTC time"),
            mono: capture::MonoInstant::try_from(millis)
                .expect("fixture uses nonnegative monotonic readings"),
        };
        let run = RunId::new(1);
        let segment = SegmentId::from_persisted(1);
        let mut writes = vec![
            CaptureWrite::OpenRun { run, at: at(0) },
            CaptureWrite::OpenClockSegment { segment, at: at(0) },
        ];
        for (logical, reason, start, end) in [
            (
                1,
                AvailabilityReason::Stopped(StopReason::Locked),
                0,
                covered_ms,
            ),
            (
                2,
                AvailabilityReason::Stopped(StopReason::Disconnected),
                10_000,
                20_000,
            ),
            (3, AvailabilityReason::Idle, 0, 60_000),
        ] {
            let id = RowId::from_persisted(logical);
            writes.push(CaptureWrite::OpenAvailability(OpenAvailability {
                id,
                segment,
                reason,
                start: at(start),
                idle: None,
            }));
            writes.push(CaptureWrite::CloseAvailability {
                id,
                at: at(end),
                reason: AvailabilityEndReason::Resumed,
            });
        }
        writes.extend([
            CaptureWrite::CloseClockSegment {
                segment,
                at: at(60_000),
                reason: ClockEndReason::RunEnd,
            },
            CaptureWrite::CloseRun {
                run,
                at: at(60_000),
                reason: RunEndReason::Shutdown,
            },
        ]);
        let metadata = RunMetadata {
            device_id: "test".into(),
            win_session_id: WindowsSessionId::new(1),
            app_version: "test".into(),
        };
        store
            .capture_transaction(&mut CaptureKeys::default(), &metadata, |capture| {
                capture.execute_writes(run, &writes)
            })
            .expect("capture writes");
        let result = ReadOnlyStore::open(&directory.database())
            .expect("read-only database")
            .check_consistency(0, 60_000);
        assert_eq!(
            result.map_err(|_| ()),
            expected,
            "{name}: stop masks must be unioned and idle must remain independent"
        );
    }
}

#[test]
fn rejects_extreme_and_reversed_fact_durations() {
    for start in [i64::MIN, 2] {
        assert!(
            Fact::new(
                FactKind::CaptureGap,
                Some(1),
                start,
                Some(if start == 2 { 1 } else { i64::MAX }),
                None
            )
            .is_err()
        );
    }
    let mut fact = Fact::new(FactKind::CaptureGap, Some(1), 0, Some(1), None).expect("valid UTC");
    fact.start_mono_ms = Some(i64::MIN);
    fact.end_mono_ms = Some(i64::MAX);
    assert!(fact.monotonic_duration().is_err());
    fact.start_mono_ms = Some(2);
    fact.end_mono_ms = Some(1);
    assert!(fact.monotonic_duration().is_err());
    let gap = crate::store::rows::CaptureGapsRow {
        id: Some(1),
        run_id: 1,
        clock_segment_id: 1,
        start_utc_ms: 0,
        end_utc_ms: 1,
        start_mono_ms: i64::MIN,
        end_mono_ms: i64::MAX,
        reason: enums::GapReason::DatabaseError,
    };
    assert!(gap.into_fact().is_err());
}

#[test]
fn classification_remaps_executable_paths_consistently() {
    use diesel::{QueryDsl, RunQueryDsl, SelectableHelper};
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut store = Store::open(&DataDirectory::new(temporary.path())).expect("store");
    for name in ["editor", "terminal", "player", "browser"] {
        let path = format!(r"c:\simulation\{name}.exe");
        diesel::insert_into(applications::table)
            .values(crate::store::rows::ApplicationsRow {
                id: None,
                identity_key: format!("exe:{path}"),
                kind: enums::ApplicationKind::Exe,
                exe_path: Some(path),
                aumid: None,
                exe_name: Some(format!("{name}.exe")),
                display_name: name.into(),
                first_seen_utc_ms: 0,
            })
            .execute(&mut store.writer)
            .expect("application");
    }
    store.use_classification_workload().expect("remap");
    let rows = applications::table
        .select(crate::store::rows::ApplicationsRow::as_select())
        .load::<crate::store::rows::ApplicationsRow>(&mut store.writer)
        .expect("applications");
    assert_eq!(rows.len(), 4);
    for row in rows {
        let path = row.exe_path.expect("executable path");
        assert_eq!(row.identity_key, format!("exe:{path}"));
        assert!(path.ends_with(&row.exe_name.expect("executable name")));
    }
}

#[test]
fn persisted_extreme_endpoints_return_typed_errors() {
    use diesel::{ExpressionMethods, RunQueryDsl};
    let directory = tempfile::tempdir().expect("isolated facts database");
    let data = DataDirectory::new(directory.path());
    let mut store = Store::open(&data).expect("store");
    diesel::insert_into(capture_runs::table)
        .values(crate::store::rows::CaptureRunsRow {
            id: Some(1),
            device_id: "test".into(),
            win_session_id: 1,
            app_version: "test".into(),
            started_utc_ms: i64::MIN,
            confirmed_utc_ms: None,
            ended_utc_ms: Some(i64::MAX),
            end_reason: Some(enums::RunEndReason::Shutdown),
        })
        .execute(&mut store.writer)
        .expect("schema-compatible extreme row");
    let mut reader = ReadOnlyStore::open_facts(&data.database()).expect("reader");
    assert!(matches!(
        reader.dump_range(0, 1),
        Err(ReadFactsError::InvalidDuration {
            kind: FactKind::Run,
            id: 1
        })
    ));
    diesel::update(capture_runs::table)
        .set((
            capture_runs::started_utc_ms.eq(0),
            capture_runs::ended_utc_ms.eq(1),
        ))
        .execute(&mut store.writer)
        .expect("valid run");
    diesel::insert_into(clock_segments::table)
        .values(crate::store::rows::ClockSegmentsRow {
            id: Some(1),
            run_id: 1,
            start_utc_ms: 0,
            start_mono_ms: i64::MIN,
            confirmed_utc_ms: None,
            confirmed_mono_ms: None,
            end_utc_ms: Some(1),
            end_mono_ms: Some(i64::MAX),
            end_reason: Some(enums::ClockEndReason::RunEnd),
        })
        .execute(&mut store.writer)
        .expect("schema-compatible extreme clock");
    assert!(matches!(
        reader.dump_range(0, 1),
        Err(ReadFactsError::InvalidDuration {
            kind: FactKind::ClockSegment,
            id: 1
        })
    ));
    diesel::update(clock_segments::table)
        .set((
            clock_segments::start_mono_ms.eq(0),
            clock_segments::end_mono_ms.eq(1),
        ))
        .execute(&mut store.writer)
        .expect("valid clock");
    diesel::insert_into(capture_gaps::table)
        .values(crate::store::rows::CaptureGapsRow {
            id: Some(1),
            run_id: 1,
            clock_segment_id: 1,
            start_utc_ms: 0,
            end_utc_ms: 1,
            start_mono_ms: i64::MIN,
            end_mono_ms: i64::MAX,
            reason: enums::GapReason::DatabaseError,
        })
        .execute(&mut store.writer)
        .expect("schema-compatible extreme gap");
    assert!(matches!(
        reader.dump_range(0, 1),
        Err(ReadFactsError::InvalidDuration {
            kind: FactKind::CaptureGap,
            id: 1
        })
    ));
}
