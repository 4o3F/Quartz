use super::*;
use crate::store::identifiers::{ColumnName, TableName};
use crate::store::{
    enums,
    rows::{
        AvailabilityIntervalsRow, CaptureGapsRow, CaptureRunsRow, ClockSegmentsRow, IntervalsRow,
    },
    schema::{
        applications, availability_intervals, capture_gaps, capture_runs, clock_segments,
        data_day_versions, intervals, titles, window_instances,
    },
};
use crate::{
    capture::{CheckpointCollection, Persistence, PersistenceState},
    data_directory::DataDirectory,
    store::Store,
};
use diesel::prelude::*;
use domain::*;
use quartz_domain::capture as domain;
use quartz_domain::rules::registry::{RegistryState, RuleRegistry};
use std::{collections::BTreeSet, sync::Arc};

#[test]
fn integer_decode_errors_keep_typed_width_schema_context_and_display() {
    let context = RowContext::new(Some(7));
    let unsigned = context
        .unsigned(-1, intervals::sequence)
        .expect_err("negative unsigned ID");
    assert!(matches!(unsigned, DecodeCaptureRowError::Integer {
        target: errors::IntegerTarget::U64, field, value: -1, ..
    } if field.table == TableName::of(intervals::table)
        && field.column == ColumnName::of(intervals::sequence) && field.row_id == Some(7)));
    assert_eq!(
        unsigned.to_string(),
        "invalid integer -1 at intervals.sequence (row 7); expected u64"
    );
    let invalid_version = i64::from(u32::MAX) + 1;
    let version = context
        .policy_version(invalid_version, availability_intervals::policy_version)
        .expect_err("policy version exceeds u32");
    assert!(matches!(version, DecodeCaptureRowError::Integer {
        target: errors::IntegerTarget::U32, field, value, ..
    } if value == invalid_version
        && field.table == TableName::of(availability_intervals::table)
        && field.column == ColumnName::of(availability_intervals::policy_version)));
    assert_eq!(
        version.to_string(),
        format!(
            "invalid integer {invalid_version} at availability_intervals.policy_version (row 7); expected u32"
        )
    );
}

fn at(ms: u64) -> Timestamp {
    Timestamp {
        utc: UtcInstant::try_from(i64::try_from(ms).expect("test milliseconds")).expect("UTC"),
        mono: MonoInstant::from_millis(ms),
    }
}
fn metadata() -> RunMetadata {
    RunMetadata {
        device_id: "test-device".into(),
        win_session_id: WindowsSessionId::new(7),
        app_version: "test".into(),
    }
}
struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    shell: Persistence,
    core: CaptureCore,
    rules: Arc<quartz_domain::rules::registry::RuleSnapshot>,
}
impl Fixture {
    fn new(start: Timestamp) -> Self {
        let directory = tempfile::tempdir().expect("temp database");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("open database");
        let opened = Persistence::open(&mut store, metadata()).expect("initial recovery");
        assert_eq!(opened.startup.next_run, RunId::new(1));
        let mut shell = opened.persistence;
        let StartedCapture { core, step } = CaptureCore::start(RunId::new(1), start).expect("core");
        let result = shell.accept(step.into_parts(RunId::new(1)), start);
        assert!(result.feedback.is_empty());
        let initial = shell.flush(&mut store, start, CheckpointCollection::Skip);
        assert!(initial.failure().is_none());
        let rules = RuleRegistry::from_state(RegistryState::default())
            .expect("rules")
            .snapshots()
            .load();
        let mut fixture = Self {
            _directory: directory,
            store,
            shell,
            core,
            rules,
        };
        for feedback in initial.feedback {
            let _ = fixture.step(start, feedback);
        }
        fixture
    }
    fn step(&mut self, now: Timestamp, event: CaptureEvent) -> CaptureParts {
        self.core
            .step(CaptureInput {
                at: now,
                rules: self.rules.clone(),
                event,
            })
            .expect("scripted event")
            .into_parts(RunId::new(1))
    }
    fn persist(&mut self, now: Timestamp, event: CaptureEvent) {
        let parts = self.step(now, event);
        let accepted = self.shell.accept(parts, now);
        assert!(accepted.feedback.is_empty());
        let result = self
            .shell
            .flush(&mut self.store, now, CheckpointCollection::Skip);
        assert!(result.failure().is_none(), "{:?}", result.failure());
        for feedback in result.feedback {
            let _ = self.step(now, feedback);
        }
    }
    fn ready(&mut self, now: Timestamp) {
        self.persist(
            now,
            CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
        );
        self.persist(now, foreground(1, "first"));
    }
    fn intervals(&mut self) -> Vec<IntervalsRow> {
        intervals::table
            .order(intervals::id)
            .select(IntervalsRow::as_select())
            .load(&mut self.store._writer)
            .expect("interval rows")
    }
}
fn foreground(handle: u64, title: &str) -> CaptureEvent {
    CaptureEvent::Foreground {
        generation: ForegroundGeneration::new(handle),
        source: ObservationSource::Review,
        foreground: Foreground::Window {
            facts: Box::new(WindowFacts {
                handle: WindowHandle::new(handle),
                process: ProcessId::new(7),
                process_created_utc: Some(at(0).utc),
                class: "TestClass".into(),
                app: AppIdentity::Exe("c:\\app.exe".into()),
                exe_name: Some("app.exe".into()),
                display_name: "App".into(),
                title: Title::readable(title),
            }),
            privacy: PrivacyContext::default(),
        },
    }
}

#[test]
fn real_core_script_round_trips_and_conserves_persisted_duration() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    fixture.persist(at(100), foreground(2, "second"));
    fixture.persist(at(200), CaptureEvent::Destroy(WindowId::from_persisted(1)));
    fixture.persist(
        at(300),
        CaptureEvent::SetTimedState(TimedState {
            policy: TimedPolicy::Pause,
            started: at(300).utc,
            until: at(10_000).utc,
        }),
    );
    fixture.persist(
        at(400),
        CaptureEvent::CancelTimedPolicy(TimedPolicyKind::Pause),
    );
    fixture.persist(
        at(500),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    fixture.persist(
        at(600),
        CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
    );
    fixture.persist(at(600), foreground(2, "second"));
    fixture.persist(at(700), CaptureEvent::ClockChanged(ClockChange::TimeChange));
    fixture.persist(at(700), foreground(2, "third"));
    fixture.persist(at(800), CaptureEvent::EventOverload);
    fixture.persist(at(900), foreground(2, "fourth"));
    fixture.persist(at(1000), CaptureEvent::Shutdown);
    let rows = fixture.intervals();
    assert!(rows.len() >= 4);
    assert!(
        rows.iter()
            .all(|row| row.confirmed_utc_ms.is_none() && row.confirmed_mono_ms.is_none())
    );
    for row in &rows {
        assert_eq!(
            row.duration_ms,
            Some(row.end_mono_ms.expect("closed") - row.start_mono_ms)
        );
    }
    assert_eq!(
        applications::table
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("apps"),
        1
    );
    assert_eq!(
        window_instances::table
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("windows"),
        2
    );
    assert!(
        window_instances::table
            .filter(window_instances::retired_utc_ms.is_not_null())
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("retired")
            >= 1
    );
    let mut persisted = vec![CaptureWrite::OpenRun {
        run: RunId::new(1),
        at: at(0),
    }];
    let segments = clock_segments::table
        .order(clock_segments::id)
        .select(ClockSegmentsRow::as_select())
        .load::<ClockSegmentsRow>(&mut fixture.store._writer)
        .expect("segments");
    for segment in segments {
        let key =
            SegmentId::from_persisted(u64::try_from(segment.id.expect("id")).expect("positive id"));
        persisted.push(CaptureWrite::OpenClockSegment {
            segment: key,
            at: Capture::timestamp(segment.start_utc_ms, segment.start_mono_ms).expect("time"),
        });
        for row in rows
            .iter()
            .filter(|row| Some(row.clock_segment_id) == segment.id)
        {
            let id =
                RowId::from_persisted(u64::try_from(row.id.expect("id")).expect("positive id"));
            persisted.push(CaptureWrite::OpenInterval(OpenInterval {
                id,
                segment: key,
                session: SessionKey::from_persisted(1),
                start: Capture::timestamp(row.start_utc_ms, row.start_mono_ms).expect("time"),
                fact: IntervalFact::NoForeground(NoForegroundReason::Empty),
            }));
            persisted.push(CaptureWrite::CloseInterval {
                id,
                at: Capture::timestamp(
                    row.end_utc_ms.expect("closed"),
                    row.end_mono_ms.expect("closed"),
                )
                .expect("time"),
                reason: IntervalEndReason::Shutdown,
            });
        }
        let availability = availability_intervals::table
            .filter(availability_intervals::clock_segment_id.eq(segment.id.expect("id")))
            .select(AvailabilityIntervalsRow::as_select())
            .load::<AvailabilityIntervalsRow>(&mut fixture.store._writer)
            .expect("availability");
        for row in availability {
            let id = RowId::from_persisted(
                u64::try_from(row.id.expect("id")).expect("positive id") + 10_000,
            );
            persisted.push(CaptureWrite::OpenAvailability(OpenAvailability {
                id,
                segment: key,
                reason: AvailabilityReason::Stopped(StopReason::Locked),
                start: Capture::timestamp(row.start_utc_ms, row.start_mono_ms).expect("time"),
                idle: None,
            }));
            persisted.push(CaptureWrite::CloseAvailability {
                id,
                at: Capture::timestamp(
                    row.end_utc_ms.expect("closed"),
                    row.end_mono_ms.expect("closed"),
                )
                .expect("time"),
                reason: AvailabilityEndReason::Shutdown,
            });
        }
        let gaps = capture_gaps::table
            .filter(capture_gaps::clock_segment_id.eq(segment.id.expect("id")))
            .select(CaptureGapsRow::as_select())
            .load::<CaptureGapsRow>(&mut fixture.store._writer)
            .expect("gaps");
        for row in gaps {
            persisted.push(CaptureWrite::RecordGap(CaptureGap {
                segment: key,
                range: TimeRange {
                    start: Capture::timestamp(row.start_utc_ms, row.start_mono_ms).expect("time"),
                    end: Capture::timestamp(row.end_utc_ms, row.end_mono_ms).expect("time"),
                },
                reason: GapReason::EventOverload,
            }));
        }
        persisted.push(CaptureWrite::CloseClockSegment {
            segment: key,
            at: Capture::timestamp(
                segment.end_utc_ms.expect("closed"),
                segment.end_mono_ms.expect("closed"),
            )
            .expect("time"),
            reason: ClockEndReason::RunEnd,
        });
    }
    persisted.push(CaptureWrite::CloseRun {
        run: RunId::new(1),
        at: at(1000),
        reason: RunEndReason::Shutdown,
    });
    domain::test_support::assert_conservation(&persisted);
}

#[test]
fn failed_switch_rolls_back_close_open_dictionaries_and_key_mapping() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let mut parts = fixture.step(at(100), foreground(2, "new title"));
    parts
        .batch
        .as_mut()
        .expect("switch batch")
        .writes
        .push(CaptureWrite::OpenRun {
            run: RunId::new(1),
            at: at(100),
        });
    let _ = fixture.shell.accept(parts, at(100));
    let failed = fixture
        .shell
        .flush(&mut fixture.store, at(100), CheckpointCollection::Skip);
    assert!(matches!(
        failed.failure(),
        Some(ExecuteCaptureBatchError::Database { .. })
    ));
    let rows = fixture.intervals();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].end_utc_ms.is_none());
    assert_eq!(
        titles::table
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("title count"),
        1
    );
    assert!(matches!(failed.state(), PersistenceState::Failed { commands } if commands > 0));
}

#[test]
fn recovery_closes_each_row_at_its_confirmation_and_splits_midnight() {
    let day = u64::try_from(limits::UTC_DAY.as_millis()).expect("day");
    let mut fixture = Fixture::new(at(day - 1000));
    fixture.ready(at(day - 1000));
    fixture.persist(
        at(day - 500),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    fixture.persist(at(day + 1000), CaptureEvent::Wake);
    // Simulate durable independent confirmations from a checkpoint before process death.
    diesel::update(availability_intervals::table)
        .set((
            availability_intervals::confirmed_utc_ms.eq(i64::try_from(day + 1000).expect("time")),
            availability_intervals::confirmed_mono_ms.eq(i64::try_from(day + 1000).expect("time")),
        ))
        .execute(&mut fixture.store._writer)
        .expect("confirm availability");
    diesel::update(clock_segments::table)
        .set((
            clock_segments::confirmed_utc_ms.eq(i64::try_from(day + 1000).expect("time")),
            clock_segments::confirmed_mono_ms.eq(i64::try_from(day + 1000).expect("time")),
        ))
        .execute(&mut fixture.store._writer)
        .expect("confirm segment");
    diesel::update(capture_runs::table)
        .set(capture_runs::confirmed_utc_ms.eq(i64::try_from(day + 1000).expect("time")))
        .execute(&mut fixture.store._writer)
        .expect("confirm run");
    let opened = Persistence::open(&mut fixture.store, metadata()).expect("recover");
    assert_eq!(opened.startup.next_run, RunId::new(2));
    let masks = availability_intervals::table
        .order(availability_intervals::id)
        .select(AvailabilityIntervalsRow::as_select())
        .load::<AvailabilityIntervalsRow>(&mut fixture.store._writer)
        .expect("masks");
    assert_eq!(masks.len(), 2);
    assert_eq!(
        masks[0].end_reason,
        Some(enums::AvailabilityEndReason::MidnightSplit)
    );
    assert_eq!(
        masks[1].end_reason,
        Some(enums::AvailabilityEndReason::CrashRecovered)
    );
    assert!(
        masks
            .iter()
            .all(|row| row.confirmed_utc_ms.is_none() && row.confirmed_mono_ms.is_none())
    );
    let run = capture_runs::table
        .select(CaptureRunsRow::as_select())
        .first::<CaptureRunsRow>(&mut fixture.store._writer)
        .expect("run");
    assert_eq!(run.end_reason, Some(enums::RunEndReason::CrashRecovered));
    assert!(run.confirmed_utc_ms.is_none());
    let versions = data_day_versions::table
        .filter(data_day_versions::source.eq(enums::DataSource::Availability))
        .select(data_day_versions::utc_date)
        .load::<i64>(&mut fixture.store._writer)
        .expect("versions");
    assert_eq!(versions, vec![0, 1]);
}

#[test]
fn checkpoint_is_atomic_and_advances_every_open_row() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let mut keys = CaptureKeys::default();
    let metadata = metadata();
    let recovered = fixture
        .store
        .capture(&mut keys, &metadata)
        .load_recoverable()
        .expect("load")
        .remove(0);
    let availability_id = recovered.next_row;
    fixture
        .store
        .capture_transaction(&mut keys, &metadata, |capture| {
            capture.execute_writes(
                RunId::new(1),
                &[CaptureWrite::OpenAvailability(OpenAvailability {
                    id: availability_id,
                    segment: recovered.segments[0].segment,
                    reason: AvailabilityReason::Idle,
                    start: at(10),
                    idle: Some(IdleRecord {
                        last_input: at(0),
                        start: at(10),
                        detected: at(10),
                        policy: IdlePolicy {
                            threshold: std::time::Duration::from_millis(10),
                            version: 1,
                        },
                    }),
                })],
            )
        })
        .expect("open overlapping idle mask");
    let point = Checkpoint {
        run: recovered.run,
        segment: recovered.segments[0].segment,
        interval: Some(recovered.intervals[0].row.id),
        availability: vec![availability_id],
        at: at(100),
        commit_by_mono: at(1000).mono,
        web_extension: WebRowExtension::PendingM5,
    };
    let failure = fixture
        .store
        .capture_transaction(&mut keys, &metadata, |capture| {
            capture.execute_writes(
                RunId::new(1),
                &[
                    CaptureWrite::Checkpoint(point.clone()),
                    CaptureWrite::OpenRun {
                        run: RunId::new(1),
                        at: at(100),
                    },
                ],
            )
        });
    assert!(matches!(
        failure,
        Err(ExecuteCaptureBatchError::Database { .. })
    ));
    assert_eq!(fixture.intervals()[0].confirmed_mono_ms, Some(0));
    assert_eq!(
        availability_intervals::table
            .select(availability_intervals::confirmed_mono_ms)
            .first::<Option<i64>>(&mut fixture.store._writer)
            .expect("idle confirmation"),
        Some(10)
    );
    fixture
        .store
        .capture_transaction(&mut keys, &metadata, |capture| {
            capture.execute_writes(RunId::new(1), &[CaptureWrite::Checkpoint(point)])
        })
        .expect("checkpoint");
    assert_eq!(fixture.intervals()[0].confirmed_mono_ms, Some(100));
    assert_eq!(
        availability_intervals::table
            .select(availability_intervals::confirmed_mono_ms)
            .first::<Option<i64>>(&mut fixture.store._writer)
            .expect("idle confirmation"),
        Some(100)
    );
    assert_eq!(
        capture_runs::table
            .select(capture_runs::confirmed_utc_ms)
            .first::<Option<i64>>(&mut fixture.store._writer)
            .expect("run confirmation"),
        Some(100)
    );
    assert_eq!(
        clock_segments::table
            .select(clock_segments::confirmed_mono_ms)
            .first::<Option<i64>>(&mut fixture.store._writer)
            .expect("segment confirmation"),
        Some(100)
    );
}

#[test]
fn deadlines_round_trip_and_invalid_time_rolls_back() {
    let mut fixture = Fixture::new(at(0));
    fixture.persist(
        at(1),
        CaptureEvent::RegisterDeadline {
            owner: SchedulerKind::Midnight,
            deadline: Deadline::Utc(at(5000).utc),
        },
    );
    let mut keys = CaptureKeys::default();
    let metadata = metadata();
    let startup = fixture
        .store
        .capture(&mut keys, &metadata)
        .load_startup()
        .expect("startup");
    assert_eq!(startup.deadlines.len(), 1);
    assert_eq!(startup.deadlines[0].owner, SchedulerKind::Midnight);
    assert_eq!(startup.deadlines[0].at, at(5000).utc);
    assert!(startup.last_segment.is_some());
    let invalid = Timestamp {
        utc: UtcInstant::new(jiff::Timestamp::from_nanosecond(1).expect("timestamp")),
        mono: at(0).mono,
    };
    let error = fixture
        .store
        .capture_transaction(&mut keys, &metadata, |capture| {
            capture.execute_writes(
                RunId::new(2),
                &[CaptureWrite::OpenRun {
                    run: RunId::new(2),
                    at: invalid,
                }],
            )
        });
    assert!(matches!(error, Err(ExecuteCaptureBatchError::Time { .. })));
}

#[test]
fn database_failure_retains_batches_then_writes_loss_before_new_facts() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let parts = fixture.step(at(100), foreground(2, "queued private title"));
    let _ = fixture.shell.accept(parts, at(100));
    crate::store::raw::ConnectionSql::new(&mut fixture.store._writer)
        .configure_reader()
        .expect("read only");
    let failed = fixture
        .shell
        .flush(&mut fixture.store, at(100), CheckpointCollection::Skip);
    assert!(matches!(
        failed.failure(),
        Some(ExecuteCaptureBatchError::Database { .. })
    ));
    assert!(matches!(failed.state(), PersistenceState::Failed { commands } if commands > 0));
    assert_eq!(fixture.intervals().len(), 1);
    let overdue = at(100)
        .checked_advance(limits::BACKLOG_RETENTION)
        .expect("retention");
    let expired = fixture
        .shell
        .flush(&mut fixture.store, overdue, CheckpointCollection::Skip);
    assert!(
        expired
            .feedback
            .iter()
            .any(|event| matches!(event, CaptureEvent::BatchDiscarded(_)))
    );
    assert!(expired.feedback.iter().any(|event| matches!(
        event,
        CaptureEvent::PersistenceGap {
            reason: GapReason::DatabaseError,
            ..
        }
    )));
    for feedback in expired.feedback {
        let parts = fixture.step(overdue, feedback);
        let _ = fixture.shell.accept(parts, overdue);
    }
    let parts = fixture.step(overdue, foreground(2, "recovered title"));
    let _ = fixture.shell.accept(parts, overdue);
    crate::store::raw::ConnectionSql::new(&mut fixture.store._writer)
        .resume_writes()
        .expect("writable");
    let recovered = fixture
        .shell
        .flush(&mut fixture.store, overdue, CheckpointCollection::Skip);
    assert!(recovered.failure().is_none(), "{:?}", recovered.failure());
    assert!(
        matches!(recovered.progress, crate::capture::FlushProgress::Committed { commands } if commands > 0)
    );
    assert!(matches!(
        recovered.state(),
        PersistenceState::Buffered { .. }
    ));
    assert_eq!(
        capture_gaps::table
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("repair precedes fresh facts"),
        1
    );
    let repaired = fixture.intervals();
    assert_eq!(
        repaired.len(),
        1,
        "fresh facts wait for the loss transaction"
    );
    assert_eq!(repaired[0].end_mono_ms, Some(0));
    assert_eq!(
        titles::table
            .filter(titles::text.eq("recovered title"))
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("recovery title is still pending"),
        0
    );
    let facts = fixture
        .shell
        .flush(&mut fixture.store, overdue, CheckpointCollection::Skip);
    assert!(facts.failure().is_none(), "{:?}", facts.failure());
    assert!(
        matches!(facts.progress, crate::capture::FlushProgress::Committed { commands } if commands > 0)
    );
    assert_eq!(facts.state(), PersistenceState::Ready);
    let gaps = capture_gaps::table
        .order(capture_gaps::id)
        .select(CaptureGapsRow::as_select())
        .load::<CaptureGapsRow>(&mut fixture.store._writer)
        .expect("loss rows");
    assert_eq!(gaps.len(), 2);
    assert_eq!(gaps[0].reason, enums::GapReason::DatabaseError);
    assert_eq!(gaps[0].start_mono_ms, 0);
    assert_eq!(gaps[0].end_mono_ms, 100);
    assert_eq!(gaps[1].reason, enums::GapReason::DatabaseError);
    assert_eq!(gaps[1].start_mono_ms, gaps[0].end_mono_ms);
    assert_eq!(
        gaps[1].end_mono_ms,
        i64::try_from(overdue.mono).expect("time")
    );
    assert_eq!(
        fixture
            .intervals()
            .last()
            .expect("fresh fact")
            .start_mono_ms,
        gaps[1].end_mono_ms
    );
    assert_eq!(
        fixture.intervals()[0].end_reason,
        Some(enums::IntervalEndReason::CaptureGap)
    );
    assert_eq!(
        titles::table
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("dictionary"),
        2
    );
    assert_eq!(
        titles::table
            .filter(titles::text.eq("queued private title"))
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("lost title absent"),
        0
    );
    assert!(
        fixture
            .intervals()
            .last()
            .expect("fresh interval")
            .end_utc_ms
            .is_none()
    );
}

#[test]
fn availability_bulk_split_preserves_idle_evidence_and_exception() {
    let mut fixture = Fixture::new(at(0));
    let mut keys = CaptureKeys::default();
    let metadata = metadata();
    let recovered = fixture
        .store
        .capture(&mut keys, &metadata)
        .load_recoverable()
        .expect("load")
        .remove(0);
    let segment = recovered.segments[0].segment;
    let key = RowId::from_persisted(1);
    let idle = IdleRecord {
        last_input: at(0),
        start: at(100),
        detected: at(200),
        policy: IdlePolicy {
            threshold: std::time::Duration::from_millis(100),
            version: 9,
        },
    };
    let day = u64::try_from(limits::UTC_DAY.as_millis()).expect("day");
    fixture
        .store
        .capture_transaction(&mut keys, &metadata, |capture| {
            capture.execute_writes(
                RunId::new(1),
                &[
                    CaptureWrite::OpenAvailability(OpenAvailability {
                        id: key,
                        segment,
                        reason: AvailabilityReason::Idle,
                        start: at(100),
                        idle: Some(idle),
                    }),
                    CaptureWrite::CloseAvailability {
                        id: key,
                        at: at(day * 100 + 100),
                        reason: AvailabilityEndReason::Exception(IdleException::KeepTiming),
                    },
                ],
            )
        })
        .expect("split long idle mask");
    let rows = availability_intervals::table
        .order(availability_intervals::start_utc_ms)
        .select(AvailabilityIntervalsRow::as_select())
        .load::<AvailabilityIntervalsRow>(&mut fixture.store._writer)
        .expect("pieces");
    assert_eq!(rows.len(), 101);
    assert!(rows.iter().all(|row| row.last_input_utc_ms == Some(0)
        && row.detected_utc_ms == Some(200)
        && row.threshold_ms == Some(100)
        && row.policy_version == 9));
    assert!(rows[..100].iter().all(|row| row.exception.is_none()
        && row.end_reason == Some(enums::AvailabilityEndReason::MidnightSplit)));
    assert_eq!(rows[100].exception, Some(enums::IdleException::KeepAlive));
    assert_eq!(
        rows[100].end_reason,
        Some(enums::AvailabilityEndReason::Resumed)
    );
}

#[test]
fn queue_capacity_discards_whole_batch_and_persists_identity_free_gap() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let mut parts = fixture.step(at(100), foreground(2, "discarded title"));
    let batch = parts.batch.as_mut().expect("batch");
    batch.writes = vec![
        CaptureWrite::ClearDeadline {
            owner: SchedulerKind::Midnight
        };
        limits::WRITE_QUEUE_CAPACITY + 1
    ];
    let outcome = fixture.shell.accept(parts, at(100));
    assert!(
        outcome
            .feedback
            .iter()
            .any(|event| matches!(event, CaptureEvent::BatchDiscarded(_)))
    );
    assert!(outcome.feedback.iter().any(|event| matches!(
        event,
        CaptureEvent::PersistenceGap {
            reason: GapReason::WriteQueueOverload,
            ..
        }
    )));
    let result = fixture
        .shell
        .flush(&mut fixture.store, at(100), CheckpointCollection::Skip);
    assert!(result.failure().is_none(), "{:?}", result.failure());
    let gaps = capture_gaps::table
        .select(CaptureGapsRow::as_select())
        .load::<CaptureGapsRow>(&mut fixture.store._writer)
        .expect("gaps");
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].reason, enums::GapReason::WriteQueueOverload);
    assert_eq!(gaps[0].start_mono_ms, 0);
    assert_eq!(gaps[0].end_mono_ms, 100);
    assert_eq!(fixture.intervals()[0].end_mono_ms, Some(0));
    assert_eq!(
        data_day_versions::table
            .filter(data_day_versions::source.eq(enums::DataSource::Gaps))
            .select(data_day_versions::version)
            .first::<i64>(&mut fixture.store._writer)
            .expect("gap version"),
        1
    );
    let schema = include_str!("../../../migrations/20261116000000_schema_v1/up.sql");
    let columns = schema
        .split("CREATE TABLE capture_gaps (")
        .nth(1)
        .expect("gap schema")
        .split(");")
        .next()
        .expect("column list");
    for forbidden in ["app_id", "title_id", "domain_id", "window_instance_id"] {
        assert!(!columns.contains(forbidden));
    }
}

#[test]
fn flush_waits_one_second_and_keeps_command_groups_bounded() {
    let mut fixture = Fixture::new(at(0));
    let parts = fixture.step(
        at(1),
        CaptureEvent::RegisterDeadline {
            owner: SchedulerKind::Midnight,
            deadline: Deadline::Utc(at(5000).utc),
        },
    );
    let _ = fixture.shell.accept(parts, at(1));
    assert!(
        fixture
            .shell
            .flush_due(&mut fixture.store, at(1000), CheckpointCollection::Skip)
            .feedback
            .is_empty()
    );
    assert!(
        fixture
            .shell
            .flush_due(&mut fixture.store, at(1001), CheckpointCollection::Skip)
            .feedback
            .iter()
            .any(|event| matches!(event, CaptureEvent::Committed(_)))
    );
    for _ in 0..70 {
        let parts = fixture.step(
            at(1002),
            CaptureEvent::RegisterDeadline {
                owner: SchedulerKind::Midnight,
                deadline: Deadline::Utc(at(5000).utc),
            },
        );
        let _ = fixture.shell.accept(parts, at(1002));
    }
    let result = fixture
        .shell
        .flush_due(&mut fixture.store, at(1002), CheckpointCollection::Skip);
    assert!(result.failure().is_none());
    assert_eq!(
        result
            .feedback
            .iter()
            .filter(|event| matches!(event, CaptureEvent::Committed(_)))
            .count(),
        64
    );
    assert!(matches!(
        result.state(),
        PersistenceState::Buffered { commands: 6 }
    ));
}

#[test]
fn crash_recovery_preserves_open_foreground_and_idle_confirmations() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    fixture.persist(
        at(200_000),
        CaptureEvent::Idle(IdleSample {
            tick: std::time::Duration::from_millis(200_000),
            last_input: LastInput::Tick(0),
            policy: IdlePolicy {
                threshold: limits::IDLE_THRESHOLD,
                version: 7,
            },
            exceptions: Vec::new(),
        }),
    );
    let opened =
        Persistence::open(&mut fixture.store, metadata()).expect("recover foreground and idle");
    let mut restarted = opened.persistence;
    let startup = opened.startup;
    assert_eq!(startup.next_run, RunId::new(2));
    let rows = fixture.intervals();
    assert_eq!(
        rows[0].end_reason,
        Some(enums::IntervalEndReason::CrashRecovered)
    );
    assert_eq!(rows[0].end_utc_ms, Some(200_000));
    assert_eq!(rows[0].duration_ms, Some(200_000));
    assert!(rows[0].confirmed_utc_ms.is_none() && rows[0].confirmed_mono_ms.is_none());
    let idle = availability_intervals::table
        .select(AvailabilityIntervalsRow::as_select())
        .first::<AvailabilityIntervalsRow>(&mut fixture.store._writer)
        .expect("idle row");
    assert_eq!(
        idle.end_reason,
        Some(enums::AvailabilityEndReason::CrashRecovered)
    );
    assert_eq!(idle.end_mono_ms, Some(200_000));
    assert_eq!(idle.last_input_utc_ms, Some(0));
    assert_eq!(idle.detected_utc_ms, Some(200_000));
    assert_eq!(idle.policy_version, 7);
    assert!(idle.confirmed_utc_ms.is_none() && idle.confirmed_mono_ms.is_none());
    let segment = clock_segments::table
        .select(ClockSegmentsRow::as_select())
        .first::<ClockSegmentsRow>(&mut fixture.store._writer)
        .expect("closed segment");
    assert_eq!(
        segment.end_reason,
        Some(enums::ClockEndReason::CrashRecovered)
    );
    assert!(segment.confirmed_utc_ms.is_none() && segment.confirmed_mono_ms.is_none());
    let StartedCapture { mut core, step } =
        CaptureCore::start(startup.next_run, at(210_000)).expect("new run");
    let _ = restarted.accept(step.into_parts(startup.next_run), at(210_000));
    assert!(
        restarted
            .flush(&mut fixture.store, at(210_000), CheckpointCollection::Skip)
            .failure()
            .is_none()
    );
    for event in [
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
        foreground(1, "new run title"),
    ] {
        let parts = core
            .step(CaptureInput {
                at: at(210_000),
                rules: fixture.rules.clone(),
                event,
            })
            .expect("new run event")
            .into_parts(startup.next_run);
        let _ = restarted.accept(parts, at(210_000));
        assert!(
            restarted
                .flush(&mut fixture.store, at(210_000), CheckpointCollection::Skip)
                .failure()
                .is_none()
        );
    }
    let rows = fixture.intervals();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].sequence, rows[1].sequence);
    assert_ne!(rows[0].clock_segment_id, rows[1].clock_segment_id);
    assert_ne!(rows[0].window_instance_id, rows[1].window_instance_id);
}

#[test]
fn checkpoint_commit_deadline_preempts_the_batch_wait() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let parts = fixture.step(at(59_000), CaptureEvent::Wake);
    assert!(parts.batch.as_ref().expect("checkpoint batch").writes.iter().any(|write| matches!(write, CaptureWrite::Checkpoint(point) if point.commit_by_mono == at(60_000).mono)));
    let _ = fixture.shell.accept(parts, at(59_900));
    assert!(
        fixture
            .shell
            .flush_due(&mut fixture.store, at(59_999), CheckpointCollection::Skip)
            .feedback
            .is_empty()
    );
    let result =
        fixture
            .shell
            .flush_due(&mut fixture.store, at(60_000), CheckpointCollection::Skip);
    assert!(result.failure().is_none());
    assert!(
        result
            .feedback
            .iter()
            .any(|event| matches!(event, CaptureEvent::Committed(_)))
    );
    assert_eq!(fixture.intervals()[0].confirmed_mono_ms, Some(59_000));
}

#[test]
fn closed_fact_commits_within_batch_deadline_and_advances_open_confirmation() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let closed = at(100);
    let deadline = closed
        .checked_advance(limits::BATCH_WAIT)
        .expect("commit deadline");
    let parts = fixture.step(closed, foreground(2, "after close"));
    let _ = fixture.shell.accept(parts, closed);
    assert_eq!(fixture.shell.next_deadline(), Some(deadline.mono));
    let early = closed
        .checked_advance(limits::BATCH_WAIT - std::time::Duration::from_millis(1))
        .expect("before deadline");
    assert!(
        fixture
            .shell
            .flush_due(&mut fixture.store, early, CheckpointCollection::Skip)
            .feedback
            .is_empty()
    );
    assert!(fixture.intervals()[0].end_mono_ms.is_none());
    let committed =
        fixture
            .shell
            .flush_due(&mut fixture.store, deadline, CheckpointCollection::Skip);
    assert!(committed.failure().is_none(), "{:?}", committed.failure());
    assert_eq!(committed.state(), PersistenceState::Ready);
    let rows = fixture.intervals();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].end_mono_ms, Some(100));
    assert_eq!(rows[0].end_reason, Some(enums::IntervalEndReason::Switch));
    assert_eq!(rows[0].confirmed_mono_ms, None);
    assert_eq!(rows[1].start_mono_ms, 100);
    assert_eq!(rows[1].confirmed_mono_ms, Some(100));
}

#[test]
fn recovery_decode_error_identifies_the_table_column_row_and_value() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let row_id = fixture.intervals()[0].id.expect("interval id");
    diesel::update(intervals::table)
        .set(intervals::start_mono_ms.eq(-7_i64))
        .execute(&mut fixture.store._writer)
        .expect("inject invalid monotonic value");
    let error =
        Persistence::open(&mut fixture.store, metadata()).expect_err("invalid persisted time");
    let RecoverCaptureError::Load {
        source:
            recovery::LoadRecoverableRunError::Decode {
                source: DecodeCaptureRowError::Time { field, value, .. },
            },
    } = error
    else {
        panic!("expected load decode error: {error:?}");
    };
    assert_eq!(field.table, TableName::of(intervals::table));
    assert_eq!(field.column, ColumnName::of(intervals::start_mono_ms));
    assert_eq!(field.row_id, Some(row_id));
    assert_eq!(value, -7);
}

#[test]
fn startup_decode_failure_is_classified_as_startup_and_rolls_back_recovery() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let key = Capture::deadline_key(SchedulerKind::Midnight);
    diesel::insert_into(crate::store::schema::settings::table)
        .values((
            crate::store::schema::settings::key.eq(key),
            crate::store::schema::settings::value.eq("invalid"),
        ))
        .execute(&mut fixture.store._writer)
        .expect("invalid saved deadline");
    let error =
        Persistence::open(&mut fixture.store, metadata()).expect_err("invalid startup value");
    let RecoverCaptureError::Startup {
        source:
            recovery::LoadCaptureStartupError::Decode {
                source:
                    DecodeCaptureRowError::Deadline {
                        owner: actual_owner,
                        value,
                        ..
                    },
            },
    } = error
    else {
        panic!("expected startup decode error: {error:?}");
    };
    assert_eq!(actual_owner, SchedulerKind::Midnight);
    assert_eq!(value, "invalid");
    assert!(
        fixture.intervals()[0].end_utc_ms.is_none(),
        "startup failure rolls back the prior settlement writes"
    );
}

#[test]
fn execute_errors_identify_unknown_keys_and_integer_values() {
    let mut fixture = Fixture::new(at(0));
    let mut keys = CaptureKeys::default();
    let metadata = metadata();
    let missing = RowId::from_persisted(999);
    let error = fixture
        .store
        .capture_transaction(&mut keys, &metadata, |capture| {
            capture.execute_writes(
                RunId::new(1),
                &[CaptureWrite::CloseInterval {
                    id: missing,
                    at: at(1),
                    reason: IntervalEndReason::Switch,
                }],
            )
        })
        .expect_err("unknown key");
    assert!(
        matches!(error, ExecuteCaptureBatchError::MissingKey { run, key: LogicalKey::Interval(key) } if run == RunId::new(1) && key == missing)
    );
    let error = fixture
        .store
        .capture_transaction(&mut keys, &metadata, |capture| {
            capture.execute_writes(
                RunId::new(u64::MAX),
                &[CaptureWrite::OpenRun {
                    run: RunId::new(u64::MAX),
                    at: at(1),
                }],
            )
        })
        .expect_err("oversized run identifier");
    let ExecuteCaptureBatchError::Integer { field, value, .. } = error else {
        panic!("expected integer error: {error:?}");
    };
    assert_eq!(field.table, TableName::of(capture_runs::table));
    assert_eq!(field.column, ColumnName::of(capture_runs::id));
    assert_eq!(value, u128::from(u64::MAX));
}

#[test]
fn windows_session_identifier_preserves_the_entire_u32_range() {
    let mut fixture = Fixture::new(at(0));
    let mut keys = CaptureKeys::default();
    let metadata = RunMetadata {
        win_session_id: WindowsSessionId::new(u32::MAX),
        ..metadata()
    };
    fixture
        .store
        .capture_transaction(&mut keys, &metadata, |capture| {
            capture.execute_writes(
                RunId::new(2),
                &[CaptureWrite::OpenRun {
                    run: RunId::new(2),
                    at: at(1),
                }],
            )
        })
        .expect("persist full Windows session identifier");
    let stored = capture_runs::table
        .filter(capture_runs::id.eq(2_i64))
        .select(capture_runs::win_session_id)
        .first::<i64>(&mut fixture.store._writer)
        .expect("session column");
    assert_eq!(stored, i64::from(u32::MAX));
}

/// Real worker threads use this clock while scripted sources advance capture time.
struct RuntimeClock {
    origin: std::time::Instant,
    offset: std::sync::atomic::AtomicU64,
}
impl RuntimeClock {
    fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
            offset: std::sync::atomic::AtomicU64::new(0),
        }
    }
    fn advance(&self, millis: u64) {
        self.offset
            .fetch_add(millis, std::sync::atomic::Ordering::SeqCst);
    }
}
impl crate::core::sources::ClockReader for RuntimeClock {
    fn read(&self) -> Result<Timestamp, crate::core::sources::ReadClockError> {
        let elapsed = u64::try_from(self.origin.elapsed().as_millis()).expect("test duration");
        Ok(at(
            elapsed + self.offset.load(std::sync::atomic::Ordering::SeqCst)
        ))
    }
}
struct RuntimeFixture {
    directory: tempfile::TempDir,
    workers: crate::core::app::Workers,
    clock: Arc<RuntimeClock>,
}
impl RuntimeFixture {
    fn recovered() -> Self {
        // Leave a real open run behind, as a process crash would.
        let old = Fixture::new(at(0));
        let Fixture {
            _directory: directory,
            store,
            ..
        } = old;
        drop(store);
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("reopen");
        assert_eq!(
            store.installation_id("runtime-device").expect("device ID"),
            "runtime-device"
        );
        assert_eq!(
            store
                .installation_id("ignored-proposal")
                .expect("stable ID"),
            "runtime-device"
        );
        let clock = Arc::new(RuntimeClock::new());
        let workers =
            crate::core::app::Workers::start(store, metadata(), clock.clone(), Arc::new(|| {}))
                .expect("workers");
        Self {
            directory,
            workers,
            clock,
        }
    }
    fn observe(&self, event: CaptureEvent) {
        use crate::core::sources::ClockReader;
        self.workers
            .sender
            .observe(crate::core::app::ObservedEvent {
                at: self.clock.read().expect("clock"),
                event,
            })
            .expect("observation");
    }
    fn wait(&self, predicate: impl Fn(&crate::core::app::MainCommand) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let command = self
                .workers
                .commands
                .recv_deadline(deadline)
                .expect("worker response before deadline");
            if let crate::core::app::MainCommand::Fatal(worker) = command {
                panic!("{worker} worker failed");
            }
            if predicate(&command) {
                return;
            }
        }
    }
    fn reader(&self) -> SqliteConnection {
        SqliteConnection::establish(
            self.directory
                .path()
                .join("quartz.db")
                .to_str()
                .expect("path"),
        )
        .expect("inspection connection")
    }
    fn shutdown(self) -> tempfile::TempDir {
        use crate::core::sources::ClockReader;
        self.workers
            .request_shutdown(self.clock.read().expect("clock"), RunEndReason::UserExit)
            .expect("shutdown");
        self.workers.join().expect("joined and flushed");
        self.directory
    }
}

#[test]
fn real_threads_recover_gate_checkpoint_report_failure_and_flush_shutdown() {
    use crate::core::app::MainCommand;
    use diesel::connection::SimpleConnection;
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let mut reader = runtime.reader();
    let recovered = capture_runs::table
        .filter(capture_runs::id.eq(1))
        .select(CaptureRunsRow::as_select())
        .first::<CaptureRunsRow>(&mut reader)
        .expect("recovered run");
    assert_eq!(
        recovered.end_reason,
        Some(enums::RunEndReason::CrashRecovered)
    );
    runtime.observe(CaptureEvent::Session(SessionStatus::Unavailable));
    runtime.wait(|command| matches!(command, MainCommand::Effect(CaptureEffect::QuerySession)));
    runtime.observe(CaptureEvent::Foreground {
        generation: ForegroundGeneration::new(1),
        source: ObservationSource::Review,
        foreground: Foreground::None(NoForegroundReason::Empty),
    });
    runtime.observe(CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())));
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Effect(CaptureEffect::ReviewForeground)
        )
    });
    let before = intervals::table
        .count()
        .get_result::<i64>(&mut reader)
        .expect("gating query");
    assert_eq!(before, 0, "session readiness gates foreground rows");
    runtime.wait(|command| matches!(command, MainCommand::Effect(CaptureEffect::CheckIdle)));
    runtime.observe(CaptureEvent::Foreground {
        generation: ForegroundGeneration::new(2),
        source: ObservationSource::Review,
        foreground: Foreground::None(NoForegroundReason::Empty),
    });
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let confirmed_before = capture_runs::table
        .filter(capture_runs::id.eq(2))
        .select(capture_runs::confirmed_utc_ms)
        .first::<Option<i64>>(&mut reader)
        .expect("initial confirmation")
        .expect("open run");
    runtime.clock.advance(59_000);
    runtime.observe(CaptureEvent::Wake);
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Persistence(PersistenceState::Buffered { .. })
        )
    });
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let confirmed_after = capture_runs::table
        .filter(capture_runs::id.eq(2))
        .select(capture_runs::confirmed_utc_ms)
        .first::<Option<i64>>(&mut reader)
        .expect("deadline confirmation")
        .expect("open run");
    assert!(
        confirmed_after >= confirmed_before + 59_000,
        "DB timeout flushes the checkpoint"
    );
    reader.batch_execute("CREATE TRIGGER fail_runtime_write BEFORE UPDATE ON capture_runs BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").expect("inject failure");
    runtime.observe(CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)));
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Persistence(PersistenceState::Failed { .. })
        )
    });
    reader
        .batch_execute("DROP TRIGGER fail_runtime_write;")
        .expect("restore database");
    runtime.observe(CaptureEvent::Stop(StopChange::End(StopReason::Locked)));
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let directory = runtime.shutdown();
    let mut reader =
        SqliteConnection::establish(directory.path().join("quartz.db").to_str().expect("path"))
            .expect("final reader");
    let run = capture_runs::table
        .filter(capture_runs::id.eq(2))
        .select(CaptureRunsRow::as_select())
        .first::<CaptureRunsRow>(&mut reader)
        .expect("closed run");
    assert_eq!(run.end_reason, Some(enums::RunEndReason::UserExit));
    assert!(run.ended_utc_ms.is_some());
    assert_eq!(run.confirmed_utc_ms, None);
    assert_eq!(
        clock_segments::table
            .filter(clock_segments::end_utc_ms.is_null())
            .count()
            .get_result::<i64>(&mut reader)
            .expect("closed clocks"),
        0
    );
}

#[test]
fn real_threads_translate_channel_overload_into_a_durable_gap() {
    use crate::core::app::MainCommand;
    let runtime = RuntimeFixture::recovered();
    runtime.observe(CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())));
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Effect(CaptureEffect::ReviewForeground)
        )
    });
    runtime.observe(CaptureEvent::Foreground {
        generation: ForegroundGeneration::new(1),
        source: ObservationSource::Review,
        foreground: Foreground::None(NoForegroundReason::Empty),
    });
    // Main output is deliberately left undrained, bounding capture progress before flooding input.
    for _ in 0..10_000 {
        runtime.observe(CaptureEvent::Wake);
    }
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Effect(CaptureEffect::StateChanged(CaptureState::Gap(
                GapReason::EventOverload
            )))
        )
    });
    runtime.observe(CaptureEvent::Foreground {
        generation: ForegroundGeneration::new(2),
        source: ObservationSource::Review,
        foreground: Foreground::None(NoForegroundReason::Empty),
    });
    use crate::core::sources::ClockReader;
    runtime
        .workers
        .shutdown_port()
        .close(runtime.clock.read().expect("clock"))
        .expect("synchronous session settlement");
    runtime.workers.join().expect("join settled workers");
    let directory = runtime.directory;
    let mut reader =
        SqliteConnection::establish(directory.path().join("quartz.db").to_str().expect("path"))
            .expect("gap reader");
    assert!(
        capture_gaps::table
            .filter(capture_gaps::reason.eq(enums::GapReason::EventOverload))
            .count()
            .get_result::<i64>(&mut reader)
            .expect("overload gaps")
            > 0
    );
}

impl RuntimeFixture {
    fn restart_keep_timing(until: u64) -> Self {
        let mut old = Fixture::new(at(0));
        old.persist(
            at(0),
            CaptureEvent::SetTimedState(TimedState {
                policy: TimedPolicy::KeepTiming,
                started: at(0).utc,
                until: at(until).utc,
            }),
        );
        let Fixture {
            _directory: directory,
            store,
            ..
        } = old;
        drop(store);
        let store =
            Store::open(&DataDirectory::new(directory.path())).expect("reopen timed settings");
        let clock = Arc::new(RuntimeClock::new());
        clock.advance(200_000);
        let workers =
            crate::core::app::Workers::start(store, metadata(), clock.clone(), Arc::new(|| {}))
                .expect("restart workers");
        Self {
            directory,
            workers,
            clock,
        }
    }
    fn sample_idle_then_lock(&self) {
        use crate::core::app::MainCommand;
        self.observe(CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())));
        self.observe(CaptureEvent::Idle(idle_sample(200_000)));
        self.observe(CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)));
        self.wait(|command| matches!(command, MainCommand::Effect(CaptureEffect::StateChanged(CaptureState::Stopped { reasons, .. })) if reasons.contains(&StopReason::Locked)));
        self.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    }
}
fn idle_sample(tick_ms: u64) -> IdleSample {
    IdleSample {
        tick: std::time::Duration::from_millis(tick_ms),
        last_input: LastInput::Tick(0),
        policy: IdlePolicy::default(),
        exceptions: Vec::new(),
    }
}

#[test]
fn restart_with_unexpired_keep_timing_suppresses_idle_and_keeps_original_grant() {
    use crate::core::app::MainCommand;
    let runtime = RuntimeFixture::restart_keep_timing(260_000);
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    runtime.sample_idle_then_lock();
    let mut reader = runtime.reader();
    assert_eq!(
        availability_intervals::table
            .filter(availability_intervals::reason.eq(enums::AvailabilityReason::Idle))
            .count()
            .get_result::<i64>(&mut reader)
            .expect("idle rows"),
        0
    );
    let state = crate::store::schema::timed_policies::table
        .find(enums::TimedPolicyKind::KeepTiming)
        .select(crate::store::rows::TimedPoliciesRow::as_select())
        .first::<crate::store::rows::TimedPoliciesRow>(&mut reader)
        .expect("persisted grant");
    assert_eq!(state.started_utc_ms, 0);
    assert_eq!(state.until_utc_ms, 260_000);
    runtime.shutdown();
}

#[test]
fn restart_clears_keep_timing_that_expired_while_core_was_down() {
    use crate::core::app::MainCommand;
    use crate::store::schema::timed_policies;
    let runtime = RuntimeFixture::restart_keep_timing(100_000);
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let mut reader = runtime.reader();
    assert_eq!(
        timed_policies::table
            .filter(timed_policies::kind.eq(enums::TimedPolicyKind::KeepTiming))
            .count()
            .get_result::<i64>(&mut reader)
            .expect("expired policy removed"),
        0
    );
    runtime.sample_idle_then_lock();
    assert_eq!(
        availability_intervals::table
            .filter(availability_intervals::reason.eq(enums::AvailabilityReason::Idle))
            .count()
            .get_result::<i64>(&mut reader)
            .expect("idle mask resumes"),
        1
    );
    runtime.shutdown();
}

#[test]
fn timed_application_and_audible_grants_follow_foreground_scope_and_two_hour_cap() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let app = AppIdentity::Exe("c:\\app.exe".into());
    for policy in [
        TimedPolicy::AppExemption(app.clone()),
        TimedPolicy::Audible(app.clone()),
    ] {
        fixture.persist(
            at(0),
            CaptureEvent::SetTimedState(TimedState {
                policy,
                started: at(0).utc,
                until: at(10_000_000).utc,
            }),
        );
    }
    fixture.persist(at(200_000), CaptureEvent::Idle(idle_sample(200_000)));
    assert_eq!(
        availability_intervals::table
            .filter(availability_intervals::reason.eq(enums::AvailabilityReason::Idle))
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("foreground exemptions"),
        0
    );
    let mut other = foreground(2, "other");
    if let CaptureEvent::Foreground {
        foreground: Foreground::Window { facts, .. },
        ..
    } = &mut other
    {
        facts.app = AppIdentity::Exe("c:\\other.exe".into());
    }
    fixture.persist(at(200_001), other);
    fixture.persist(at(200_002), CaptureEvent::Idle(idle_sample(200_002)));
    assert_eq!(
        availability_intervals::table
            .filter(availability_intervals::reason.eq(enums::AvailabilityReason::Idle))
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("other application becomes idle"),
        1
    );
    let opened =
        Persistence::open(&mut fixture.store, metadata()).expect("restore scoped settings");
    assert_eq!(opened.startup.policies.len(), 2);
    for state in opened.startup.policies {
        assert_eq!(state.until, at(7_200_000).utc);
        assert_eq!(state.started, at(0).utc);
    }
}

#[test]
fn policy_clear_and_capture_boundary_rollback_in_one_transaction() {
    use crate::store::schema::timed_policies;
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    fixture.persist(
        at(1),
        CaptureEvent::SetTimedState(TimedState {
            policy: TimedPolicy::KeepTiming,
            started: at(1).utc,
            until: at(60_000).utc,
        }),
    );
    let mut parts = fixture.step(
        at(2),
        CaptureEvent::CancelTimedPolicy(TimedPolicyKind::KeepTiming),
    );
    let batch = parts.batch.as_mut().expect("policy boundary batch");
    assert!(batch.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseInterval {
            reason: IntervalEndReason::PolicyChanged,
            ..
        }
    )));
    batch.writes.push(CaptureWrite::OpenRun {
        run: RunId::new(1),
        at: at(2),
    });
    let _ = fixture.shell.accept(parts, at(2));
    let failed = fixture
        .shell
        .flush(&mut fixture.store, at(2), CheckpointCollection::Skip);
    assert!(failed.failure().is_some());
    assert_eq!(
        timed_policies::table
            .filter(timed_policies::kind.eq(enums::TimedPolicyKind::KeepTiming))
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("policy row rolled back"),
        1
    );
    assert!(
        fixture
            .intervals()
            .last()
            .expect("current interval")
            .end_utc_ms
            .is_none()
    );
}

#[test]
fn running_keep_timing_expiration_clears_policy_with_capture_boundary() {
    use crate::store::schema::timed_policies;
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    fixture.persist(
        at(1),
        CaptureEvent::SetTimedState(TimedState {
            policy: TimedPolicy::KeepTiming,
            started: at(1).utc,
            until: at(100).utc,
        }),
    );
    let parts = fixture.step(at(100), CaptureEvent::Wake);
    let writes = &parts.batch.as_ref().expect("expiration transaction").writes;
    assert!(writes.iter().any(|write| matches!(
        write,
        CaptureWrite::ClearTimedPolicy(TimedPolicyKind::KeepTiming)
    )));
    assert!(writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseInterval {
            reason: IntervalEndReason::PolicyChanged,
            ..
        }
    )));
    let _ = fixture.shell.accept(parts, at(100));
    assert!(
        fixture
            .shell
            .flush(&mut fixture.store, at(100), CheckpointCollection::Skip)
            .failure()
            .is_none()
    );
    assert_eq!(
        timed_policies::table
            .filter(timed_policies::kind.eq(enums::TimedPolicyKind::KeepTiming))
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("cleared policy"),
        0
    );
}

#[test]
fn restored_audible_deadline_waits_for_fresh_foreground_evidence() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let state = TimedState {
        policy: TimedPolicy::Audible(AppIdentity::Exe("c:\\app.exe".into())),
        started: at(0).utc,
        until: at(600_000).utc,
    };
    fixture.persist(at(0), CaptureEvent::RestoreTimedState(state.clone()));
    fixture.persist(at(200_000), CaptureEvent::Idle(idle_sample(200_000)));
    let idle = availability_intervals::table
        .filter(availability_intervals::reason.eq(enums::AvailabilityReason::Idle))
        .select(AvailabilityIntervalsRow::as_select())
        .first::<AvailabilityIntervalsRow>(&mut fixture.store._writer)
        .expect("old browser evidence is not reused");
    assert!(idle.end_utc_ms.is_none());
    fixture.persist(
        at(200_001),
        CaptureEvent::RestoreTimedState(TimedState {
            until: at(600_001).utc,
            ..state.clone()
        }),
    );
    fixture.persist(at(200_002), foreground(2, "reviewed browser"));
    fixture.persist(at(200_003), CaptureEvent::Idle(idle_sample(200_003)));
    let retained = availability_intervals::table
        .filter(availability_intervals::reason.eq(enums::AvailabilityReason::Idle))
        .select(AvailabilityIntervalsRow::as_select())
        .first::<AvailabilityIntervalsRow>(&mut fixture.store._writer)
        .expect("changing the deadline does not refresh browser evidence");
    assert!(retained.end_utc_ms.is_none());
    fixture.persist(at(200_004), CaptureEvent::SetTimedState(state));
    fixture.persist(at(200_005), foreground(3, "fresh browser"));
    fixture.persist(at(200_006), CaptureEvent::Idle(idle_sample(200_006)));
    let idle = availability_intervals::table
        .filter(availability_intervals::reason.eq(enums::AvailabilityReason::Idle))
        .select(AvailabilityIntervalsRow::as_select())
        .first::<AvailabilityIntervalsRow>(&mut fixture.store._writer)
        .expect("fresh audible evidence");
    assert_eq!(idle.end_reason, Some(enums::AvailabilityEndReason::Resumed));
    assert_eq!(idle.exception, Some(enums::IdleException::Audible));
}

#[test]
fn repeated_write_loss_while_stopped_does_not_emit_recursive_recovery_batches() {
    let rules = RuleRegistry::from_state(RegistryState::default())
        .expect("rules")
        .snapshots();
    let started = CaptureCore::start(RunId::new(1), at(0)).expect("core");
    let startup_batch = started.step.batch.expect("startup batch");
    let mut core = started.core;
    let _ = core
        .step(CaptureInput {
            at: at(0),
            rules: rules.load(),
            event: CaptureEvent::Session(SessionStatus::Ready(BTreeSet::from([
                StopReason::Locked,
            ]))),
        })
        .expect("locked session");
    for _ in 0..2 {
        let loss = core
            .step(CaptureInput {
                at: at(1),
                rules: rules.load(),
                event: CaptureEvent::PersistenceGap {
                    reason: GapReason::WriteQueueOverload,
                    lost_through: at(1),
                },
            })
            .expect("bounded loss feedback");
        assert!(
            loss.writes.is_empty(),
            "loss feedback cannot depend on free queue capacity"
        );
    }
    let progress = core
        .step(CaptureInput {
            at: at(1),
            rules: rules.load(),
            event: CaptureEvent::Committed(startup_batch),
        })
        .expect("durable prefix releases capacity");
    assert!(progress.writes.iter().any(|write| matches!(write, CaptureWrite::OpenAvailability(row) if row.reason == AvailabilityReason::Stopped(StopReason::Locked))));
}

#[test]
fn restored_privacy_value_applies_until_expiration_and_clears_atomically() {
    use crate::store::schema::timed_policies;
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    fixture.persist(
        at(0),
        CaptureEvent::SetTimedState(TimedState {
            policy: TimedPolicy::Privacy(PrivacyMode::NoTitles),
            started: at(0).utc,
            until: at(100).utc,
        }),
    );
    let mut keys = CaptureKeys::default();
    let metadata = metadata();
    let startup = fixture
        .store
        .capture(&mut keys, &metadata)
        .load_startup()
        .expect("load complete policy value");
    let state = startup
        .policies
        .into_iter()
        .find(|state| state.owner() == TimedPolicyKind::Privacy)
        .expect("persisted privacy mode");
    assert_eq!(state.policy, TimedPolicy::Privacy(PrivacyMode::NoTitles));
    fixture.persist(at(0), CaptureEvent::RestoreTimedState(state));
    fixture.persist(at(1), foreground(2, "withheld"));
    assert_eq!(
        fixture
            .intervals()
            .last()
            .expect("private foreground")
            .capture_level,
        enums::CaptureLevel::TitleHidden
    );
    fixture.persist(at(100), CaptureEvent::Wake);
    fixture.persist(at(101), foreground(3, "allowed"));
    assert_eq!(
        fixture
            .intervals()
            .last()
            .expect("fresh foreground")
            .capture_level,
        enums::CaptureLevel::Full
    );
    assert_eq!(
        timed_policies::table
            .filter(timed_policies::kind.eq(enums::TimedPolicyKind::Privacy))
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("expired privacy settings"),
        0
    );
}

#[test]
fn restored_pause_remains_stopped_until_its_policy_deadline() {
    use crate::store::schema::timed_policies;
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    fixture.persist(
        at(0),
        CaptureEvent::RestoreTimedState(TimedState {
            policy: TimedPolicy::Pause,
            started: at(0).utc,
            until: at(100).utc,
        }),
    );
    assert!(
        matches!(fixture.core.state(), CaptureState::Stopped { reasons, .. } if reasons.contains(&StopReason::Paused))
    );
    fixture.persist(at(100), CaptureEvent::Wake);
    fixture.persist(at(101), foreground(2, "resumed"));
    assert!(matches!(fixture.core.state(), CaptureState::Tracking(_)));
    assert_eq!(
        timed_policies::table
            .filter(timed_policies::kind.eq(enums::TimedPolicyKind::Pause))
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("expired pause settings"),
        0
    );
}

#[test]
fn policy_scope_upserts_applications_and_restores_identity_after_display_rename() {
    use crate::store::schema::timed_policies;
    let mut fixture = Fixture::new(at(0));
    let app = AppIdentity::Unresolved("TestClass".into());
    let state = TimedState {
        policy: TimedPolicy::AppExemption(app.clone()),
        started: at(0).utc,
        until: at(100).utc,
    };
    fixture.persist(at(0), CaptureEvent::SetTimedState(state.clone()));
    fixture.persist(at(1), CaptureEvent::SetTimedState(state.clone()));
    assert_eq!(
        applications::table
            .filter(applications::identity_key.eq(app.key()))
            .count()
            .get_result::<i64>(&mut fixture.store._writer)
            .expect("one application"),
        1
    );
    let scope = timed_policies::table
        .find(enums::TimedPolicyKind::AppExemption)
        .select(timed_policies::application_id)
        .first::<Option<i64>>(&mut fixture.store._writer)
        .expect("application scope")
        .expect("scoped policy");
    diesel::update(applications::table.filter(applications::id.eq(scope)))
        .set(applications::display_name.eq("Friendly name"))
        .execute(&mut fixture.store._writer)
        .expect("rename display metadata");
    let opened = Persistence::open(&mut fixture.store, metadata()).expect("restore scoped policy");
    assert_eq!(opened.startup.policies, vec![state]);
}
