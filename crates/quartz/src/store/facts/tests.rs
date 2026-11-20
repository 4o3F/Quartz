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
