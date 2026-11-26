use super::*;
use crate::persistence::{CheckpointCollection, Persistence, PersistenceState};
use crate::store::enums::IntegerCode;
use crate::store::identifiers::{ColumnName, TableName};
use crate::store::{
    enums,
    rows::{
        ApplicationsRow, AvailabilityIntervalsRow, CaptureGapsRow, CaptureRunsRow,
        ClockSegmentsRow, WindowInstancesRow,
    },
    schema::{
        applications, availability_intervals, capture_gaps, capture_runs, clock_segments,
        data_day_versions, intervals, titles, window_instances,
    },
};
use diesel::prelude::*;
use domain::*;
use quartz_domain::capture as domain;
use quartz_domain::rules::registry::{RegistryState, RuleRegistry};
use std::collections::BTreeSet;

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

use super::test_support::*;

#[test]
fn review_persists_resolved_content_in_new_window_generation() {
    let mut fixture = Fixture::new(at(0));
    fixture.persist(
        at(0),
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    let CaptureEvent::Foreground {
        foreground: Foreground::Window { mut facts, privacy },
        ..
    } = foreground(1, "Settings")
    else {
        panic!("window fixture")
    };
    facts.class = "ApplicationFrameWindow".into();
    facts.app = AppIdentity::Unresolved(facts.class.clone());
    facts.metadata_source = MetadataSource::Unavailable;
    facts.exe_name = None;
    facts.display_name = facts.class.clone();
    fixture.persist(
        at(0),
        CaptureEvent::Foreground {
            source: ObservationSource::Review,
            foreground: Foreground::Window {
                facts: facts.clone(),
                privacy: privacy.clone(),
            },
        },
    );
    facts.app = AppIdentity::Aumid("SettingsPackage!Settings".into());
    facts.metadata_source = MetadataSource::Process;
    facts.exe_name = Some("systemsettings.exe".into());
    facts.display_name = "Settings".into();
    for time in [100, 200] {
        fixture.persist(
            at(time),
            CaptureEvent::Foreground {
                source: ObservationSource::Review,
                foreground: Foreground::Window {
                    facts: facts.clone(),
                    privacy: privacy.clone(),
                },
            },
        );
    }
    let instances = window_instances::table
        .order(window_instances::generation)
        .select(WindowInstancesRow::as_select())
        .load(&mut fixture.store.writer)
        .expect("persisted window instances");
    assert_eq!(instances.len(), 2, "content identity needs a new instance");
    let unresolved = &instances[0];
    let resolved = &instances[1];
    assert_eq!(unresolved.generation, 0);
    assert_eq!(resolved.generation, 1);
    assert_eq!(resolved.hwnd, unresolved.hwnd);
    assert_eq!(resolved.pid, unresolved.pid);
    assert_eq!(
        resolved.process_created_utc_ms,
        unresolved.process_created_utc_ms
    );
    assert_ne!(resolved.app_id, unresolved.app_id);
    assert_eq!(unresolved.last_seen_utc_ms, 100);
    assert_eq!(unresolved.retired_utc_ms, Some(100));
    assert_eq!(resolved.first_seen_utc_ms, 100);
    assert_eq!(resolved.last_seen_utc_ms, 200);
    let app = applications::table
        .filter(applications::id.eq(resolved.app_id))
        .select(ApplicationsRow::as_select())
        .first(&mut fixture.store.writer)
        .expect("resolved content application");
    assert_eq!(app.identity_key, facts.app.key());
    assert_eq!(app.exe_name, facts.exe_name);
    assert_eq!(app.display_name, facts.display_name);

    let intervals = fixture.intervals();
    assert_eq!(intervals.len(), 2);
    assert_eq!(intervals[0].window_instance_id, unresolved.id);
    assert_eq!(intervals[0].app_id, Some(unresolved.app_id));
    assert_eq!(intervals[1].window_instance_id, resolved.id);
    assert_eq!(intervals[1].app_id, Some(resolved.app_id));
}

#[test]
fn frame_aumid_metadata_enrichment_updates_app_without_new_persisted_instance() {
    let mut fixture = Fixture::new(at(0));
    fixture.persist(
        at(0),
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    let CaptureEvent::Foreground {
        foreground: Foreground::Window { mut facts, privacy },
        ..
    } = foreground(1, "private window title")
    else {
        panic!("frame fixture")
    };
    facts.class = "ApplicationFrameWindow".into();
    facts.app = AppIdentity::Aumid("SettingsPackage!Settings".into());
    facts.metadata_source = MetadataSource::Frame;
    facts.exe_name = None;
    facts.display_name = "SettingsPackage!Settings".into();
    fixture.persist(
        at(0),
        CaptureEvent::Foreground {
            source: ObservationSource::Review,
            foreground: Foreground::Window {
                facts: facts.clone(),
                privacy: privacy.clone(),
            },
        },
    );
    facts.metadata_source = MetadataSource::Process;
    facts.exe_name = Some("systemsettings.exe".into());
    facts.display_name = "Settings".into();
    fixture.persist(
        at(10),
        CaptureEvent::Foreground {
            source: ObservationSource::Review,
            foreground: Foreground::Window {
                facts: facts.clone(),
                privacy,
            },
        },
    );
    let instances = window_instances::table
        .select(WindowInstancesRow::as_select())
        .load(&mut fixture.store.writer)
        .expect("persisted frame instance");
    assert_eq!(
        instances.len(),
        1,
        "metadata enrichment must preserve the instance"
    );
    let instance = &instances[0];
    assert_eq!(instance.generation, 0);
    let app = applications::table
        .filter(applications::id.eq(instance.app_id))
        .select(ApplicationsRow::as_select())
        .first(&mut fixture.store.writer)
        .expect("enriched application");
    assert_eq!(app.identity_key, facts.app.key());
    assert_eq!(app.exe_name, facts.exe_name);
    assert_eq!(app.display_name, facts.display_name);
    let changes = fixture.store.take_committed_versions();
    assert_eq!(
        changes.metadata_version,
        Some(1),
        "metadata enrichment invalidates historical display fields"
    );
    let intervals = fixture.intervals();
    assert_eq!(intervals.len(), 1);
    assert_eq!(intervals[0].window_instance_id, instance.id);
    assert_eq!(intervals[0].app_id, Some(instance.app_id));
}

#[test]
fn frame_observations_preserve_stored_metadata_across_new_window_instances() {
    let mut fixture = Fixture::new(at(0));
    fixture.persist(
        at(0),
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    let CaptureEvent::Foreground {
        foreground: Foreground::Window { mut facts, privacy },
        ..
    } = foreground(1, "private title")
    else {
        panic!("frame fixture")
    };
    facts.app = AppIdentity::Aumid("SettingsPackage!Settings".into());
    facts.exe_name = Some("systemsettings.exe".into());
    facts.display_name = "设置".into();
    fixture.persist(
        at(0),
        CaptureEvent::Foreground {
            source: ObservationSource::Review,
            foreground: Foreground::Window {
                facts: facts.clone(),
                privacy: privacy.clone(),
            },
        },
    );
    let process = applications::table
        .filter(applications::identity_key.eq(facts.app.key()))
        .select(ApplicationsRow::as_select())
        .first(&mut fixture.store.writer)
        .expect("stored process application");
    facts.handle = WindowHandle::new(2);
    facts.metadata_source = MetadataSource::Frame;
    facts.exe_name = None;
    facts.display_name = "SettingsPackage!Settings".into();
    fixture.persist(
        at(10),
        CaptureEvent::Foreground {
            source: ObservationSource::Review,
            foreground: Foreground::Window {
                facts: facts.clone(),
                privacy: privacy.clone(),
            },
        },
    );
    let preserved = applications::table
        .filter(applications::identity_key.eq(facts.app.key()))
        .select(ApplicationsRow::as_select())
        .first(&mut fixture.store.writer)
        .expect("application after frame observation");
    assert_eq!(preserved.id, process.id);
    assert_eq!(preserved.exe_name, process.exe_name);
    assert_eq!(preserved.display_name, process.display_name);
    facts.handle = WindowHandle::new(3);
    facts.app = AppIdentity::Aumid("NewPackage!App".into());
    facts.display_name = "NewPackage!App".into();
    fixture.persist(
        at(20),
        CaptureEvent::Foreground {
            source: ObservationSource::Review,
            foreground: Foreground::Window {
                facts: facts.clone(),
                privacy: privacy.clone(),
            },
        },
    );
    let placeholder = applications::table
        .filter(applications::identity_key.eq(facts.app.key()))
        .select(ApplicationsRow::as_select())
        .first(&mut fixture.store.writer)
        .expect("new frame-only application");
    assert!(placeholder.exe_name.is_none());
    assert_eq!(placeholder.display_name, facts.display_name);
    facts.metadata_source = MetadataSource::Process;
    facts.exe_name = Some("new-app.exe".into());
    facts.display_name = "New application".into();
    fixture.persist(
        at(30),
        CaptureEvent::Foreground {
            source: ObservationSource::Review,
            foreground: Foreground::Window {
                facts: facts.clone(),
                privacy,
            },
        },
    );
    let upgraded = applications::table
        .filter(applications::identity_key.eq(facts.app.key()))
        .select(ApplicationsRow::as_select())
        .first(&mut fixture.store.writer)
        .expect("process-enriched application");
    assert_eq!(upgraded.id, placeholder.id);
    assert_eq!(upgraded.exe_name, facts.exe_name);
    assert_eq!(upgraded.display_name, facts.display_name);
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
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("apps"),
        1
    );
    assert_eq!(
        window_instances::table
            .count()
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("windows"),
        2
    );
    assert!(
        window_instances::table
            .filter(window_instances::retired_utc_ms.is_not_null())
            .count()
            .get_result::<i64>(&mut fixture.store.writer)
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
        .load::<ClockSegmentsRow>(&mut fixture.store.writer)
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
            .load::<AvailabilityIntervalsRow>(&mut fixture.store.writer)
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
            .load::<CaptureGapsRow>(&mut fixture.store.writer)
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
            .get_result::<i64>(&mut fixture.store.writer)
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
        .execute(&mut fixture.store.writer)
        .expect("confirm availability");
    diesel::update(clock_segments::table)
        .set((
            clock_segments::confirmed_utc_ms.eq(i64::try_from(day + 1000).expect("time")),
            clock_segments::confirmed_mono_ms.eq(i64::try_from(day + 1000).expect("time")),
        ))
        .execute(&mut fixture.store.writer)
        .expect("confirm segment");
    diesel::update(capture_runs::table)
        .set(capture_runs::confirmed_utc_ms.eq(i64::try_from(day + 1000).expect("time")))
        .execute(&mut fixture.store.writer)
        .expect("confirm run");
    fixture.store.take_committed_versions();
    let opened = Persistence::open(&mut fixture.store, metadata()).expect("recover");
    let effects = fixture.store.take_committed_versions();
    assert_eq!(
        effects
            .facts
            .iter()
            .filter(|fact| fact.key.source.0 == enums::DataSource::Availability.code())
            .map(|fact| fact.key.day.0)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    let stored = fixture.store.version_state().expect("recovered versions");
    for fact in effects.facts {
        assert_eq!(stored.days.get(&fact.key), Some(&fact.version));
    }
    assert_eq!(opened.startup.next_run, RunId::new(2));
    let masks = availability_intervals::table
        .order(availability_intervals::id)
        .select(AvailabilityIntervalsRow::as_select())
        .load::<AvailabilityIntervalsRow>(&mut fixture.store.writer)
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
        .first::<CaptureRunsRow>(&mut fixture.store.writer)
        .expect("run");
    assert_eq!(run.end_reason, Some(enums::RunEndReason::CrashRecovered));
    assert!(run.confirmed_utc_ms.is_none());
    let versions = data_day_versions::table
        .filter(data_day_versions::source.eq(enums::DataSource::Availability))
        .select(data_day_versions::utc_date)
        .load::<i64>(&mut fixture.store.writer)
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
            .first::<Option<i64>>(&mut fixture.store.writer)
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
            .first::<Option<i64>>(&mut fixture.store.writer)
            .expect("idle confirmation"),
        Some(100)
    );
    assert_eq!(
        capture_runs::table
            .select(capture_runs::confirmed_utc_ms)
            .first::<Option<i64>>(&mut fixture.store.writer)
            .expect("run confirmation"),
        Some(100)
    );
    assert_eq!(
        clock_segments::table
            .select(clock_segments::confirmed_mono_ms)
            .first::<Option<i64>>(&mut fixture.store.writer)
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
    crate::store::raw::ConnectionSql::new(&mut fixture.store.writer)
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
    crate::store::raw::ConnectionSql::new(&mut fixture.store.writer)
        .resume_writes()
        .expect("writable");
    let recovered = fixture
        .shell
        .flush(&mut fixture.store, overdue, CheckpointCollection::Skip);
    assert!(recovered.failure().is_none(), "{:?}", recovered.failure());
    assert!(
        matches!(recovered.progress, crate::persistence::FlushProgress::Committed { commands } if commands > 0)
    );
    assert!(matches!(
        recovered.state(),
        PersistenceState::Buffered { .. }
    ));
    assert_eq!(
        capture_gaps::table
            .count()
            .get_result::<i64>(&mut fixture.store.writer)
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
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("recovery title is still pending"),
        0
    );
    let facts = fixture
        .shell
        .flush(&mut fixture.store, overdue, CheckpointCollection::Skip);
    assert!(facts.failure().is_none(), "{:?}", facts.failure());
    assert!(
        matches!(facts.progress, crate::persistence::FlushProgress::Committed { commands } if commands > 0)
    );
    assert_eq!(facts.state(), PersistenceState::Ready);
    let gaps = capture_gaps::table
        .order(capture_gaps::id)
        .select(CaptureGapsRow::as_select())
        .load::<CaptureGapsRow>(&mut fixture.store.writer)
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
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("dictionary"),
        2
    );
    assert_eq!(
        titles::table
            .filter(titles::text.eq("queued private title"))
            .count()
            .get_result::<i64>(&mut fixture.store.writer)
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
        .load::<AvailabilityIntervalsRow>(&mut fixture.store.writer)
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
        .load::<CaptureGapsRow>(&mut fixture.store.writer)
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
            .first::<i64>(&mut fixture.store.writer)
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
        .first::<AvailabilityIntervalsRow>(&mut fixture.store.writer)
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
        .first::<ClockSegmentsRow>(&mut fixture.store.writer)
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
        .execute(&mut fixture.store.writer)
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
    crate::store::settings::Settings::new(&mut fixture.store.writer)
        .corrupt(key, "invalid")
        .expect("invalid saved deadline");
    fixture.store.take_committed_versions();
    let versions_before = fixture
        .store
        .version_state()
        .expect("versions before recovery");
    let error =
        Persistence::open(&mut fixture.store, metadata()).expect_err("invalid startup value");
    assert!(fixture.store.take_committed_versions().facts.is_empty());
    assert_eq!(
        fixture
            .store
            .version_state()
            .expect("versions after rollback")
            .days,
        versions_before.days
    );
    let RecoverCaptureError::Startup {
        source:
            recovery::LoadCaptureStartupError::Setting {
                source:
                    crate::store::settings::AccessSettingError::Json {
                        key: actual_key, ..
                    },
            },
    } = error
    else {
        panic!("expected startup setting error: {error:?}");
    };
    assert_eq!(
        actual_key,
        crate::store::settings::SettingKey::MidnightDeadline
    );
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
        .first::<i64>(&mut fixture.store.writer)
        .expect("session column");
    assert_eq!(stored, i64::from(u32::MAX));
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
            .get_result::<i64>(&mut fixture.store.writer)
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
            .get_result::<i64>(&mut fixture.store.writer)
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
            .get_result::<i64>(&mut fixture.store.writer)
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
            .get_result::<i64>(&mut fixture.store.writer)
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
        .first::<AvailabilityIntervalsRow>(&mut fixture.store.writer)
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
        .first::<AvailabilityIntervalsRow>(&mut fixture.store.writer)
        .expect("changing the deadline does not refresh browser evidence");
    assert!(retained.end_utc_ms.is_none());
    fixture.persist(at(200_004), CaptureEvent::SetTimedState(state));
    fixture.persist(at(200_005), foreground(3, "fresh browser"));
    fixture.persist(at(200_006), CaptureEvent::Idle(idle_sample(200_006)));
    let idle = availability_intervals::table
        .filter(availability_intervals::reason.eq(enums::AvailabilityReason::Idle))
        .select(AvailabilityIntervalsRow::as_select())
        .first::<AvailabilityIntervalsRow>(&mut fixture.store.writer)
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
            .get_result::<i64>(&mut fixture.store.writer)
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
            .get_result::<i64>(&mut fixture.store.writer)
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
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("one application"),
        1
    );
    let scope = timed_policies::table
        .find(enums::TimedPolicyKind::AppExemption)
        .select(timed_policies::application_id)
        .first::<Option<i64>>(&mut fixture.store.writer)
        .expect("application scope")
        .expect("scoped policy");
    diesel::update(applications::table.filter(applications::id.eq(scope)))
        .set(applications::display_name.eq("Friendly name"))
        .execute(&mut fixture.store.writer)
        .expect("rename display metadata");
    let opened = Persistence::open(&mut fixture.store, metadata()).expect("restore scoped policy");
    assert_eq!(opened.startup.policies, vec![state]);
}

#[test]
fn clock_confirmation_invalidates_query_versions_before_first_foreground_fact() {
    let mut fixture = Fixture::new(at(0));
    let range = crate::query::input::QueryRange {
        start: 0,
        end: 60_000,
    };
    let before = fixture.store.version_state().expect("initial versions");
    fixture.persist(at(59_000), CaptureEvent::Wake);
    assert!(fixture.intervals().is_empty());
    let after = fixture.store.version_state().expect("confirmed versions");
    let before = crate::query::versions::VersionState::from_stored(
        before,
        quartz_domain::rules::registry::SnapshotHashes {
            classification: quartz_domain::rules::ContentHash::of("classification-test"),
            extraction: quartz_domain::rules::ContentHash::of("extraction-test"),
            privacy: quartz_domain::rules::ContentHash::of("privacy-test"),
        },
    )
    .expect("version state")
    .digest(range)
    .expect("digest");
    let after = crate::query::versions::VersionState::from_stored(
        after,
        quartz_domain::rules::registry::SnapshotHashes {
            classification: quartz_domain::rules::ContentHash::of("classification-test"),
            extraction: quartz_domain::rules::ContentHash::of("extraction-test"),
            privacy: quartz_domain::rules::ContentHash::of("privacy-test"),
        },
    )
    .expect("version state")
    .digest(range)
    .expect("digest");
    assert_ne!(
        before, after,
        "clock-only coverage changes invalidate cursors and day subscriptions"
    );
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
fn offline_web_privacy_never_interns_the_browser_title() {
    for field in [
        quartz_domain::rules::Field::Domain,
        quartz_domain::rules::Field::TabTitle,
        quartz_domain::rules::Field::BrowserProfile,
    ] {
        let mut fixture = Fixture::new(at(0));
        fixture.hide_web_field(field);
        fixture.persist(
            at(0),
            CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
        );
        let mut event = foreground(1, "offline database secret");
        if let CaptureEvent::Foreground {
            foreground: Foreground::Window { privacy, .. },
            ..
        } = &mut event
        {
            privacy.browser = BrowserContext::Offline;
        }
        fixture.persist(at(10), event);
        let rows = fixture.intervals();
        let browser = rows
            .iter()
            .find(|row| row.kind == enums::IntervalKind::Window)
            .expect("browser interval");
        assert_eq!(browser.start_utc_ms, 10);
        assert!(rows.iter().all(|row| row.title_id.is_none()));
        assert_eq!(
            titles::table
                .count()
                .get_result::<i64>(&mut fixture.store.writer)
                .expect("title dictionary"),
            0
        );
    }
}

#[test]
fn startup_warns_and_ignores_removed_browser_context_deadline_without_decoding_it() {
    use crate::store::settings::{SettingKey, Settings};
    let log = tempfile::NamedTempFile::new().expect("isolated log");
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log.reopen().expect("log writer")))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut fixture = Fixture::new(at(0));
        let mut settings = Settings::new(&mut fixture.store.writer);
        settings
            .corrupt(
                SettingKey::RemovedPrivacyContextDeadline,
                "obsolete, not a timestamp",
            )
            .expect("obsolete deadline");
        settings
            .put_deadline(SchedulerKind::Idle, at(123).utc)
            .expect("idle deadline");
        let mut keys = CaptureKeys::default();
        let metadata = metadata();
        let startup = fixture
            .store
            .recover_capture(&mut keys, &metadata)
            .expect("obsolete deadline permits startup");
        assert_eq!(startup.deadlines.len(), 1);
        assert_eq!(startup.deadlines[0].owner, SchedulerKind::Idle);
        assert_eq!(startup.deadlines[0].at, at(123).utc);
    });
    let logged = std::fs::read_to_string(log.path()).expect("read warning");
    assert!(logged.contains("WARN"));
    assert!(logged.contains("ignoring persisted deadline for removed browser-context deferral"));
    assert!(logged.contains(SettingKey::RemovedPrivacyContextDeadline.as_str()));
    assert!(!logged.contains("obsolete, not a timestamp"));
}

#[test]
fn uninitialized_startup_discard_repairs_only_the_uncommitted_range() {
    enum LostBoundary {
        Unlock,
        Ready,
    }
    for boundary in [LostBoundary::Unlock, LostBoundary::Ready] {
        let mut fixture = Fixture::new(at(0));
        fixture.persist(
            at(10),
            CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
        );
        let lost_at;
        let settled_at;
        let mut parts = match boundary {
            LostBoundary::Unlock => {
                lost_at = at(20);
                settled_at = at(10);
                fixture.step(
                    lost_at,
                    CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
                )
            }
            LostBoundary::Ready => {
                fixture.persist(
                    at(20),
                    CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
                );
                lost_at = at(30);
                settled_at = at(20);
                fixture.step(
                    lost_at,
                    CaptureEvent::Session(SessionStatus::Ready(BTreeSet::from([
                        StopReason::Locked,
                    ]))),
                )
            }
        };
        // Exceed the command budget while retaining the real batch coverage.
        parts.batch.as_mut().expect("batch").writes.resize(
            limits::WRITE_QUEUE_CAPACITY + 1,
            CaptureWrite::ClearDeadline {
                owner: SchedulerKind::Idle,
            },
        );
        let accepted = fixture.shell.accept(parts, lost_at);
        assert!(
            accepted
                .feedback
                .iter()
                .any(|event| matches!(event, CaptureEvent::BatchDiscarded(_)))
        );
        let repaired = fixture
            .shell
            .flush(&mut fixture.store, lost_at, CheckpointCollection::Skip);
        assert!(
            repaired.failure().is_none(),
            "startup repair failed: {:?}",
            repaired.failure()
        );
        let gaps = capture_gaps::table
            .order(capture_gaps::id)
            .select(CaptureGapsRow::as_select())
            .load::<CaptureGapsRow>(&mut fixture.store.writer)
            .expect("gaps");
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0].start_mono_ms, 0);
        assert_eq!(gaps[0].end_mono_ms, 10);
        assert_eq!(
            gaps[1].start_mono_ms,
            i64::try_from(settled_at.mono).expect("settled boundary")
        );
        assert_eq!(
            gaps[1].end_mono_ms,
            i64::try_from(lost_at.mono).expect("lost boundary")
        );
        let rows = availability_intervals::table
            .select(AvailabilityIntervalsRow::as_select())
            .load::<AvailabilityIntervalsRow>(&mut fixture.store.writer)
            .expect("availability");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].start_mono_ms, 10);
        assert_eq!(
            rows[0].end_mono_ms,
            Some(i64::try_from(settled_at.mono).expect("settled boundary"))
        );
    }
}

impl Fixture {
    fn query_utc(&mut self, request: crate::query::QueryRequest) -> crate::query::QueryResponse {
        let mut reader = self.store.take_query_reader();
        let mut job = crate::query::QueryJob::start_in_zone(
            &mut reader,
            request,
            self.rules.clone(),
            jiff::tz::TimeZone::UTC,
        )
        .expect("start UTC query");
        let response = loop {
            if let Some(response) = job.advance().expect("derive persisted capture") {
                break response;
            }
        };
        self.store
            .return_query_reader(job.finish().expect("finish query"));
        response
    }
    fn query_day_utc(&mut self, date: &str) -> quartz_protocol::QueryDayResponse {
        let response = self.query_utc(crate::query::QueryRequest::Day(
            quartz_protocol::QueryDayRequest {
                local_date: date.parse().expect("date"),
            },
        ));
        let crate::query::QueryResponse::Day(day) = response else {
            panic!("day response")
        };
        day
    }
}

fn sampled(utc: &str, mono: u64) -> Timestamp {
    Timestamp {
        utc: UtcInstant::try_from(
            utc.parse::<jiff::Timestamp>()
                .expect("UTC")
                .as_millisecond(),
        )
        .expect("capture UTC"),
        mono: MonoInstant::from_millis(mono),
    }
}

#[test]
fn backward_jump_query_preserves_closed_segments_and_repeated_hour_totals() {
    const HOUR_MS: u64 = 3_600_000;
    let start = sampled("2025-01-02T10:00:00Z", 0);
    let mut fixture = Fixture::new(start);
    fixture.ready(start);
    let rollback = sampled("2025-01-02T09:30:00Z", HOUR_MS);
    fixture.persist(
        rollback,
        CaptureEvent::ClockChanged(ClockChange::TimeChange),
    );
    fixture.persist(rollback, foreground(1, "after rollback"));
    fixture.persist(
        sampled("2025-01-02T10:30:00Z", HOUR_MS * 2),
        CaptureEvent::Shutdown,
    );
    let day = fixture.query_day_utc("2025-01-02");
    assert_eq!(
        day.metrics.active_ms,
        i64::try_from(HOUR_MS * 2).expect("duration")
    );
    let repeated = day
        .hours
        .into_iter()
        .find(|hour| hour.local_start.starts_with("2025-01-02T10:"))
        .expect("repeated hour");
    assert_eq!(
        repeated
            .categories
            .iter()
            .map(|part| part.duration_ms)
            .sum::<i64>(),
        i64::try_from(HOUR_MS + HOUR_MS / 2).expect("duration")
    );
}

#[test]
fn suspended_intervening_day_loads_owning_clock_and_counts_offline_wall_time() {
    const HOUR_MS: u64 = 3_600_000;
    let start = sampled("2025-01-01T22:00:00Z", 0);
    let mut fixture = Fixture::new(start);
    fixture.ready(start);
    fixture.persist(
        sampled("2025-01-01T23:00:00Z", HOUR_MS),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    fixture.persist(
        sampled("2025-01-01T23:00:00Z", HOUR_MS),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Suspended)),
    );
    let resume = sampled("2025-01-03T07:00:00Z", HOUR_MS);
    fixture.persist(resume, CaptureEvent::ClockChanged(ClockChange::Resume));
    fixture.persist(
        resume,
        CaptureEvent::Stop(StopChange::End(StopReason::Suspended)),
    );
    fixture.persist(
        resume,
        CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
    );
    fixture.persist(resume, foreground(1, "resumed"));
    fixture.persist(
        sampled("2025-01-03T08:00:00Z", HOUR_MS * 2),
        CaptureEvent::Shutdown,
    );
    fixture
        .store
        .create_annotations(
            quartz_protocol::AnnotationsCreateRequest {
                entities: vec![quartz_protocol::CreateAnnotation {
                    uuid: quartz_protocol::AnnotationId(
                        "abcdef00-0000-4000-8000-000000000001"
                            .parse()
                            .expect("UUID"),
                    ),
                    kind: quartz_protocol::AnnotationKind::OfflineActivity,
                    values: quartz_protocol::AnnotationValues {
                        start_utc_ms: sampled("2025-01-02T12:00:00Z", 0)
                            .utc
                            .get()
                            .as_millisecond(),
                        end_utc_ms: sampled("2025-01-02T14:00:00Z", 0)
                            .utc
                            .get()
                            .as_millisecond(),
                        category: None,
                        label_preset: None,
                        label: None,
                        note: None,
                    },
                }],
            },
            resume.utc,
        )
        .expect("offline activity");
    let day = fixture.query_day_utc("2025-01-02");
    assert_eq!(
        day.metrics.active_ms,
        i64::try_from(HOUR_MS * 2).expect("duration")
    );
    let response = fixture.query_utc(crate::query::QueryRequest::SegmentDetail(
        quartz_protocol::QuerySegmentDetailRequest {
            start_utc_ms: sampled("2025-01-01T22:00:00Z", 0)
                .utc
                .get()
                .as_millisecond(),
            end_utc_ms: sampled("2025-01-03T08:00:00Z", 0)
                .utc
                .get()
                .as_millisecond(),
            app: None,
            window_instance: None,
        },
    ));
    let crate::query::QueryResponse::SegmentDetail(detail) = response else {
        panic!("detail response")
    };
    assert!(detail.intervals.iter().any(|segment| matches!(&segment.activity, quartz_protocol::Activity::Stopped { reasons } if reasons.contains(&quartz_protocol::QueryStopReason::Suspended) && reasons.contains(&quartz_protocol::QueryStopReason::Locked))));
    assert_eq!(
        detail
            .intervals
            .iter()
            .filter(|segment| matches!(
                segment.activity,
                quartz_protocol::Activity::OfflineActivity { .. }
            ))
            .map(|segment| segment.duration_ms)
            .sum::<i64>(),
        i64::try_from(HOUR_MS * 2).expect("wall duration")
    );
}

#[test]
fn backward_jump_before_old_segment_start_remains_queryable() {
    const HOUR_MS: u64 = 3_600_000;
    let start = sampled("2025-01-02T10:00:00Z", 0);
    let mut fixture = Fixture::new(start);
    fixture.ready(start);
    let rollback = sampled("2025-01-02T09:00:00Z", HOUR_MS);
    fixture.persist(
        rollback,
        CaptureEvent::ClockChanged(ClockChange::TimeChange),
    );
    fixture.persist(rollback, foreground(1, "after rollback"));
    fixture.persist(
        sampled("2025-01-02T09:30:00Z", HOUR_MS + HOUR_MS / 2),
        CaptureEvent::Shutdown,
    );
    let day = fixture.query_day_utc("2025-01-02");
    assert_eq!(
        day.metrics.active_ms,
        i64::try_from(HOUR_MS + HOUR_MS / 2).expect("duration")
    );
    let response = fixture.query_utc(crate::query::QueryRequest::SegmentDetail(
        quartz_protocol::QuerySegmentDetailRequest {
            start_utc_ms: start.utc.get().as_millisecond(),
            end_utc_ms: sampled("2025-01-02T11:00:00Z", 0)
                .utc
                .get()
                .as_millisecond(),
            app: None,
            window_instance: None,
        },
    ));
    let crate::query::QueryResponse::SegmentDetail(detail) = response else {
        panic!("detail response")
    };
    let active: Vec<_> = detail
        .intervals
        .iter()
        .filter(|segment| segment.activity == quartz_protocol::Activity::Active)
        .collect();
    assert_eq!(active.len(), 1);
    let old = active.first().expect("old segment");
    assert_eq!(
        old.clock_segment_id,
        Some(quartz_protocol::ClockSegmentId(1))
    );
    assert_eq!(old.duration_ms, i64::try_from(HOUR_MS).expect("hour"));
}

struct LockBeforeSleep {
    start: &'static str,
    locked: &'static str,
    suspended: &'static str,
    resumed: &'static str,
    shutdown: &'static str,
}
impl LockBeforeSleep {
    const HALF_HOUR_MS: u64 = 30 * 60 * 1000;
    fn assert_exact_durations(self) {
        use quartz_protocol::{Activity, QueryStopReason, TimelineSegment};
        let start = sampled(self.start, 0);
        let locked = sampled(self.locked, Self::HALF_HOUR_MS);
        let suspended = sampled(self.suspended, Self::HALF_HOUR_MS * 2);
        let resumed = sampled(self.resumed, Self::HALF_HOUR_MS * 2);
        let shutdown = sampled(self.shutdown, Self::HALF_HOUR_MS * 3);
        let mut fixture = Fixture::new(start);
        fixture.ready(start);
        fixture.persist(
            locked,
            CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
        );
        fixture.persist(
            suspended,
            CaptureEvent::Stop(StopChange::Begin(StopReason::Suspended)),
        );
        fixture.persist(resumed, CaptureEvent::ClockChanged(ClockChange::Resume));
        fixture.persist(
            resumed,
            CaptureEvent::Stop(StopChange::End(StopReason::Suspended)),
        );
        fixture.persist(
            resumed,
            CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
        );
        fixture.persist(resumed, foreground(1, "resumed"));
        fixture.persist(shutdown, CaptureEvent::Shutdown);
        let request =
            crate::query::QueryRequest::SegmentDetail(quartz_protocol::QuerySegmentDetailRequest {
                start_utc_ms: start.utc.get().as_millisecond(),
                end_utc_ms: shutdown.utc.get().as_millisecond(),
                app: None,
                window_instance: None,
            });
        let crate::query::QueryResponse::SegmentDetail(stopped) =
            fixture.query_utc(request.clone())
        else {
            panic!("detail")
        };
        let pre_sleep = stopped
            .intervals
            .iter()
            .find(|segment| {
                segment.start_utc_ms == locked.utc.get().as_millisecond()
                    && segment.end_utc_ms == suspended.utc.get().as_millisecond()
            })
            .expect("pre-sleep locked slice");
        assert_eq!(
            pre_sleep.activity,
            Activity::Stopped {
                reasons: vec![QueryStopReason::Locked]
            }
        );
        let half_hour = i64::try_from(Self::HALF_HOUR_MS).expect("duration");
        assert_eq!(
            pre_sleep.duration_ms, half_hour,
            "pre-sleep lock retains monotonic duration"
        );
        let sleeping: Vec<&TimelineSegment> = stopped.intervals.iter().filter(|segment| matches!(&segment.activity, Activity::Stopped { reasons } if reasons.contains(&QueryStopReason::Suspended))).collect();
        let sleep_ms = resumed.utc.get().as_millisecond() - suspended.utc.get().as_millisecond();
        assert_eq!(
            sleeping
                .iter()
                .map(|segment| segment.duration_ms)
                .sum::<i64>(),
            sleep_ms
        );
        assert!(sleeping.iter().all(|segment| matches!(&segment.activity, Activity::Stopped { reasons } if reasons.contains(&QueryStopReason::Locked))));
        let wall_ms = shutdown.utc.get().as_millisecond() - start.utc.get().as_millisecond();
        assert_eq!(
            stopped
                .intervals
                .iter()
                .map(|segment| segment.duration_ms)
                .sum::<i64>(),
            wall_ms,
            "unannotated duration conservation"
        );
        fixture
            .store
            .create_annotations(
                quartz_protocol::AnnotationsCreateRequest {
                    entities: vec![quartz_protocol::CreateAnnotation {
                        uuid: quartz_protocol::AnnotationId(
                            "abcdef00-0000-4000-8000-000000000002"
                                .parse()
                                .expect("UUID"),
                        ),
                        kind: quartz_protocol::AnnotationKind::OfflineActivity,
                        values: quartz_protocol::AnnotationValues {
                            start_utc_ms: locked.utc.get().as_millisecond(),
                            end_utc_ms: resumed.utc.get().as_millisecond(),
                            category: None,
                            label_preset: None,
                            label: None,
                            note: None,
                        },
                    }],
                },
                resumed.utc,
            )
            .expect("offline activity covering lock and sleep");
        let crate::query::QueryResponse::SegmentDetail(annotated) = fixture.query_utc(request)
        else {
            panic!("detail")
        };
        let offline: Vec<_> = annotated
            .intervals
            .iter()
            .filter(|segment| matches!(segment.activity, Activity::OfflineActivity { .. }))
            .collect();
        assert_eq!(
            offline
                .iter()
                .map(|segment| segment.duration_ms)
                .sum::<i64>(),
            half_hour + sleep_ms
        );
        assert_eq!(
            offline
                .iter()
                .filter(|segment| segment.clock_segment_id.is_some())
                .map(|segment| segment.duration_ms)
                .sum::<i64>(),
            half_hour
        );
        assert_eq!(
            annotated
                .intervals
                .iter()
                .map(|segment| segment.duration_ms)
                .sum::<i64>(),
            wall_ms,
            "annotated duration conservation"
        );
        let start_date = start
            .utc
            .get()
            .to_zoned_datetime(jiff::tz::TimeZone::UTC)
            .date();
        let end_date = shutdown
            .utc
            .get()
            .to_zoned_datetime(jiff::tz::TimeZone::UTC)
            .date();
        let mut date = start_date;
        let mut active_ms = 0;
        loop {
            let calendar = crate::query::calendar::DayCalendar::in_zone(
                date,
                crate::query::calendar::DayStartOffset::MIDNIGHT,
                jiff::tz::TimeZone::UTC,
            )
            .expect("calendar");
            let expected = calendar.range.end.min(shutdown.utc.get().as_millisecond())
                - calendar.range.start.max(start.utc.get().as_millisecond());
            let day = fixture.query_day_utc(&date.to_string());
            let metrics = day.metrics;
            assert_eq!(metrics.active_ms, expected, "day {date}");
            assert_eq!(
                day.hours
                    .iter()
                    .flat_map(|hour| &hour.categories)
                    .map(|part| part.duration_ms)
                    .sum::<i64>(),
                expected
            );
            active_ms += metrics.active_ms;
            if date == end_date {
                break;
            }
            date = date.tomorrow().expect("next date");
        }
        assert_eq!(
            active_ms, wall_ms,
            "daily aggregates conserve the complete run"
        );
    }
}

#[test]
fn lock_before_sleep_same_day_conserves_stopped_and_offline_durations() {
    LockBeforeSleep {
        start: "2025-01-02T10:00:00Z",
        locked: "2025-01-02T10:30:00Z",
        suspended: "2025-01-02T11:00:00Z",
        resumed: "2025-01-02T15:00:00Z",
        shutdown: "2025-01-02T15:30:00Z",
    }
    .assert_exact_durations();
}

#[test]
fn lock_before_sleep_across_midnight_conserves_stopped_and_offline_durations() {
    LockBeforeSleep {
        start: "2025-01-01T22:30:00Z",
        locked: "2025-01-01T23:00:00Z",
        suspended: "2025-01-01T23:30:00Z",
        resumed: "2025-01-02T07:30:00Z",
        shutdown: "2025-01-02T08:00:00Z",
    }
    .assert_exact_durations();
}

#[test]
fn confirmations_only_invalidate_new_availability_coverage() {
    use crate::store::versions::{UtcDay, VersionKey, VersionSource};
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    fixture.persist(
        at(10),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    const DAY: u64 = 86_400_000;
    fixture.persist(at(DAY + 10), CaptureEvent::Wake);
    fixture.store.take_committed_versions();
    fixture.persist(at(2 * DAY + 10), CaptureEvent::Wake);
    let changes = fixture.store.take_committed_versions();
    let historical = VersionKey {
        source: VersionSource(enums::DataSource::Availability.code()),
        day: UtcDay(0),
    };
    assert!(
        !changes
            .facts
            .iter()
            .any(|version| version.key == historical),
        "already confirmed day remains valid"
    );
    assert!(
        changes
            .facts
            .iter()
            .any(|version| version.key.day == UtcDay(1) && version.key.source == historical.source)
    );
    assert!(
        changes
            .facts
            .iter()
            .any(|version| version.key.day == UtcDay(2) && version.key.source == historical.source)
    );
}

#[test]
fn closed_segment_mappings_are_retired_after_backlog_repair() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    for boundary in 1..=128 {
        fixture.persist(
            at(boundary * 1000),
            CaptureEvent::ClockChanged(ClockChange::Resume),
        );
        assert!(
            fixture.shell.segment_count() <= 1,
            "closed mappings must not grow with residence"
        );
    }
    let mut inspection = Inspection::open(fixture._directory.path());
    inspection.inject_write_failure();
    let parts = fixture.step(at(130_000), CaptureEvent::ClockChanged(ClockChange::Resume));
    let _ = fixture.shell.accept(parts, at(130_000));
    let failed = fixture
        .shell
        .flush(&mut fixture.store, at(130_000), CheckpointCollection::Skip);
    assert!(failed.failure().is_some());
    assert_eq!(
        fixture.shell.segment_count(),
        1,
        "failed mapping changes roll back"
    );
    let parts = fixture.step(at(131_000), CaptureEvent::ClockChanged(ClockChange::Resume));
    let _ = fixture.shell.accept(parts, at(131_000));
    use diesel::connection::SimpleConnection;
    fixture.store.writer.batch_execute("CREATE TRIGGER fail_repair BEFORE INSERT ON capture_gaps BEGIN SELECT RAISE(ABORT, 'injected gap failure'); END;").expect("fail repair");
    let failed = fixture
        .shell
        .flush(&mut fixture.store, at(800_000), CheckpointCollection::Skip);
    assert!(failed.failure().is_some());
    inspection.restore_writes();
    fixture
        .store
        .writer
        .batch_execute("DROP TRIGGER fail_repair;")
        .expect("restore repair");
    for _ in 0..10 {
        let repaired =
            fixture
                .shell
                .flush(&mut fixture.store, at(800_000), CheckpointCollection::Skip);
        assert!(repaired.failure().is_none(), "{:?}", repaired.failure());
        if matches!(fixture.shell.state(), PersistenceState::Ready) {
            break;
        }
    }
    assert!(matches!(fixture.shell.state(), PersistenceState::Ready));
    assert!(fixture.shell.segment_count() <= 1);
}

#[test]
fn evicted_reused_handle_persists_a_distinct_window_row_and_new_application() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    for handle in
        2..=u64::try_from(domain::limits::WINDOW_INSTANCE_CAPACITY + 1).expect("test handles")
    {
        fixture.persist(at(handle), foreground(handle, "window"));
    }
    let CaptureEvent::Foreground {
        foreground: Foreground::Window { mut facts, privacy },
        ..
    } = foreground(1, "new application")
    else {
        panic!("window")
    };
    facts.app = AppIdentity::Exe("c:\\new.exe".into());
    facts.exe_name = Some("new.exe".into());
    facts.display_name = "New".into();
    fixture.persist(
        at(10_000),
        CaptureEvent::Foreground {
            source: ObservationSource::Review,
            foreground: Foreground::Window {
                facts: facts.clone(),
                privacy,
            },
        },
    );
    let instances = window_instances::table
        .filter(window_instances::hwnd.eq(1))
        .order(window_instances::generation)
        .select(WindowInstancesRow::as_select())
        .load(&mut fixture.store.writer)
        .expect("HWND history");
    assert_eq!(
        instances.len(),
        2,
        "reused evicted HWND needs a new database row"
    );
    assert_ne!(instances[0].app_id, instances[1].app_id);
    assert!(instances[1].generation > instances[0].generation);
    assert!(instances[0].retired_utc_ms.is_some());
    let app = applications::table
        .filter(applications::id.eq(instances[1].app_id))
        .select(ApplicationsRow::as_select())
        .first(&mut fixture.store.writer)
        .expect("new application");
    assert_eq!(app.identity_key, facts.app.key());
    let intervals = fixture.intervals();
    let last = intervals.last().expect("new interval");
    assert_eq!(last.window_instance_id, instances[1].id);
    assert_eq!(last.app_id, Some(instances[1].app_id));
    assert!(fixture.shell.window_count() <= domain::limits::WINDOW_INSTANCE_CAPACITY);
}

#[test]
fn destroying_discarded_window_commits_without_a_secondary_database_gap() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let now = fixture.discard_window_observation(2);
    fixture.persist(
        now,
        CaptureEvent::DestroyWindow {
            handle: WindowHandle::new(2),
        },
    );
    assert_eq!(
        window_instances::table
            .filter(window_instances::hwnd.eq(2))
            .count()
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("window count"),
        0
    );
    assert_eq!(
        capture_gaps::table
            .filter(capture_gaps::reason.eq(enums::GapReason::DatabaseError))
            .count()
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("secondary gaps"),
        0
    );
}
#[test]
fn evicting_discarded_window_commits_without_a_secondary_database_gap() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let now = fixture.discard_window_observation(2);
    for handle in 3..=u64::try_from(limits::WINDOW_INSTANCE_CAPACITY + 3).expect("test handles") {
        fixture.persist(now, foreground(handle, "healthy observation"));
    }
    assert!(fixture.shell.window_count() <= limits::WINDOW_INSTANCE_CAPACITY);
    assert_eq!(
        capture_gaps::table
            .filter(capture_gaps::reason.eq(enums::GapReason::DatabaseError))
            .count()
            .get_result::<i64>(&mut fixture.store.writer)
            .expect("secondary gaps"),
        0
    );
}
#[test]
fn refocusing_discarded_window_registers_it_before_persisting_the_interval() {
    let mut fixture = Fixture::new(at(0));
    fixture.ready(at(0));
    let now = fixture.discard_window_observation(2);
    fixture.persist(now, foreground(2, "fresh observation"));
    let instance = window_instances::table
        .filter(window_instances::hwnd.eq(2))
        .select(WindowInstancesRow::as_select())
        .first(&mut fixture.store.writer)
        .expect("registered window");
    let rows = fixture.intervals();
    assert_eq!(
        rows.last().expect("new interval").window_instance_id,
        instance.id
    );
    assert_eq!(
        rows.last().expect("new interval").app_id,
        Some(instance.app_id)
    );
}
