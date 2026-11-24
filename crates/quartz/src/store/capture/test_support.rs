//! Isolated capture fixtures and persisted-state inspection.
use super::*;
use crate::{
    data_directory::DataDirectory,
    persistence::{CheckpointCollection, Persistence},
    store::{
        Store, enums,
        rows::{CaptureRunsRow, IntervalsRow},
        schema::{
            availability_intervals, capture_gaps, capture_runs, clock_segments, intervals,
            timed_policies,
        },
    },
};
use diesel::prelude::*;
use quartz_domain::capture::*;
use quartz_domain::rules::registry::{RegistryState, RuleRegistry};
use std::{collections::BTreeSet, sync::Arc};
pub(crate) fn at(ms: u64) -> Timestamp {
    Timestamp {
        utc: UtcInstant::try_from(i64::try_from(ms).expect("test milliseconds")).expect("UTC"),
        mono: MonoInstant::from_millis(ms),
    }
}
pub(crate) fn metadata() -> RunMetadata {
    RunMetadata {
        device_id: "test-device".into(),
        win_session_id: WindowsSessionId::new(7),
        app_version: "test".into(),
    }
}
pub(crate) struct Fixture {
    pub(crate) _directory: tempfile::TempDir,
    pub(crate) store: Store,
    pub(crate) shell: Persistence,
    pub(crate) core: CaptureCore,
    pub(crate) rules: Arc<quartz_domain::rules::registry::RuleSnapshot>,
}
impl Fixture {
    pub(crate) fn new(start: Timestamp) -> Self {
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
    pub(crate) fn step(&mut self, now: Timestamp, event: CaptureEvent) -> CaptureParts {
        self.core
            .step(CaptureInput {
                at: now,
                rules: self.rules.clone(),
                event,
            })
            .expect("scripted event")
            .into_parts(RunId::new(1))
    }
    pub(crate) fn persist(&mut self, now: Timestamp, event: CaptureEvent) {
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
    pub(crate) fn discard_window_observation(&mut self, handle: u64) -> Timestamp {
        let parts = self.step(at(10), foreground(handle, "discarded observation"));
        assert!(self.shell.accept(parts, at(10)).feedback.is_empty());
        let now = at(10)
            .checked_advance(limits::BACKLOG_RETENTION)
            .expect("retention");
        let expired = self
            .shell
            .flush(&mut self.store, now, CheckpointCollection::Skip);
        assert!(expired.failure().is_none());
        assert!(
            expired
                .feedback
                .iter()
                .any(|event| matches!(event, CaptureEvent::PersistenceGap { .. }))
        );
        for feedback in expired.feedback {
            let parts = self.step(now, feedback);
            assert!(self.shell.accept(parts, now).feedback.is_empty());
        }
        now
    }
    pub(crate) fn hide_web_field(&mut self, field: quartz_domain::rules::Field) {
        use quartz_domain::rules::registry::RegistryCommand;
        let mut registry = RuleRegistry::from_state(RegistryState::default()).expect("rules");
        let field = field.as_str();
        let decision = registry.decide(RegistryCommand::Apply {
            key: "user.capture".parse().expect("rule set key"),
            file: "capture-test.toml".into(),
            text: format!("format='quartz-rules/1'\n[set]\nkey='user.capture'\nkind='privacy'\nname='Privacy tests'\n[[rule]]\nid='hide'\nlabel='Sensitive'\npreset='title_hidden'\nwhen=[{{field='{field}',op='equals',value='secret.example'}}]\n"),
            base_version: 0,
        }).into_parts();
        decision.reply.expect("valid privacy rule");
        registry
            .committed(decision.outcome.expect("rule commit token"))
            .expect("install privacy rule");
        self.rules = registry.snapshots().load();
        assert!(self.rules.privacy().requires_web_privacy());
    }
    pub(crate) fn ready(&mut self, now: Timestamp) {
        self.persist(
            now,
            CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
        );
        self.persist(now, foreground(1, "first"));
    }
    pub(crate) fn recovered_foreground(&mut self) -> IntervalFact {
        let mut keys = CaptureKeys::default();
        let metadata = metadata();
        let mut runs = self
            .store
            .capture(&mut keys, &metadata)
            .load_recoverable()
            .expect("load persisted intervals");
        assert_eq!(runs.len(), 1, "one recoverable run");
        let mut run = runs.pop().expect("recoverable run");
        assert_eq!(run.intervals.len(), 1, "one open foreground interval");
        run.intervals.pop().expect("open interval").row.fact
    }
    pub(in crate::store) fn intervals(&mut self) -> Vec<IntervalsRow> {
        intervals::table
            .order(intervals::id)
            .select(IntervalsRow::as_select())
            .load(&mut self.store.writer)
            .expect("interval rows")
    }
}
pub(crate) fn foreground(handle: u64, title: &str) -> CaptureEvent {
    CaptureEvent::Foreground {
        source: ObservationSource::Review,
        foreground: Foreground::Window {
            facts: Box::new(WindowFacts {
                handle: WindowHandle::new(handle),
                process: ProcessId::new(7),
                process_created_utc: Some(at(0).utc),
                class: "TestClass".into(),
                app: AppIdentity::Exe("c:\\app.exe".into()),
                metadata_source: MetadataSource::Process,
                exe_name: Some("app.exe".into()),
                display_name: "App".into(),
                title: Title::readable(title),
            }),
            privacy: PrivacyContext::default(),
        },
    }
}

impl Fixture {
    pub(crate) fn into_directory(self) -> tempfile::TempDir {
        self._directory
    }
}
pub(crate) struct Inspection {
    connection: SqliteConnection,
}
pub(crate) struct RunInspection {
    pub end_reason: Option<RunEndReason>,
    pub ended_utc_ms: Option<i64>,
    pub confirmed_utc_ms: Option<i64>,
}
pub(crate) struct TimedInspection {
    pub started_utc_ms: i64,
    pub until_utc_ms: i64,
}
impl Inspection {
    pub(crate) fn open(directory: &std::path::Path) -> Self {
        Self {
            connection: SqliteConnection::establish(
                directory.join("quartz.db").to_str().expect("path"),
            )
            .expect("inspection connection"),
        }
    }
    pub(crate) fn run(&mut self, id: i64) -> RunInspection {
        let row = capture_runs::table
            .filter(capture_runs::id.eq(id))
            .select(CaptureRunsRow::as_select())
            .first::<CaptureRunsRow>(&mut self.connection)
            .expect("run");
        let end_reason = row.end_reason.map(|reason| match reason {
            enums::RunEndReason::Shutdown => RunEndReason::Shutdown,
            enums::RunEndReason::UserExit => RunEndReason::UserExit,
            enums::RunEndReason::CrashRecovered => RunEndReason::CrashRecovered,
            enums::RunEndReason::Update => RunEndReason::Update,
        });
        RunInspection {
            end_reason,
            ended_utc_ms: row.ended_utc_ms,
            confirmed_utc_ms: row.confirmed_utc_ms,
        }
    }
    pub(crate) fn interval_count(&mut self) -> i64 {
        intervals::table
            .count()
            .get_result(&mut self.connection)
            .expect("interval count")
    }
    pub(crate) fn open_clock_count(&mut self) -> i64 {
        clock_segments::table
            .filter(clock_segments::end_utc_ms.is_null())
            .count()
            .get_result(&mut self.connection)
            .expect("open clock count")
    }
    pub(crate) fn overload_gap_count(&mut self) -> i64 {
        capture_gaps::table
            .filter(capture_gaps::reason.eq(enums::GapReason::EventOverload))
            .count()
            .get_result(&mut self.connection)
            .expect("overload gap count")
    }
    pub(crate) fn policy_boundary_count(&mut self) -> i64 {
        intervals::table
            .filter(intervals::end_reason.eq(enums::IntervalEndReason::PolicyChanged))
            .count()
            .get_result(&mut self.connection)
            .expect("policy boundary count")
    }
    pub(crate) fn idle_count(&mut self) -> i64 {
        self.availability_count(enums::AvailabilityReason::Idle)
    }
    pub(crate) fn pause_count(&mut self) -> i64 {
        self.availability_count(enums::AvailabilityReason::Paused)
    }
    fn availability_count(&mut self, reason: enums::AvailabilityReason) -> i64 {
        availability_intervals::table
            .filter(availability_intervals::reason.eq(reason))
            .count()
            .get_result(&mut self.connection)
            .expect("availability count")
    }
    pub(crate) fn keep_timing(&mut self) -> Option<TimedInspection> {
        timed_policies::table
            .find(enums::TimedPolicyKind::KeepTiming)
            .select(crate::store::rows::TimedPoliciesRow::as_select())
            .first::<crate::store::rows::TimedPoliciesRow>(&mut self.connection)
            .optional()
            .expect("timed policy")
            .map(|row| TimedInspection {
                started_utc_ms: row.started_utc_ms,
                until_utc_ms: row.until_utc_ms,
            })
    }
    pub(crate) fn inject_write_failure(&mut self) {
        use diesel::connection::SimpleConnection;
        self.connection.batch_execute("CREATE TRIGGER fail_runtime_write BEFORE UPDATE ON capture_runs BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").expect("inject failure");
    }
    pub(crate) fn restore_writes(&mut self) {
        use diesel::connection::SimpleConnection;
        self.connection
            .batch_execute("DROP TRIGGER fail_runtime_write;")
            .expect("restore writes");
    }
}
