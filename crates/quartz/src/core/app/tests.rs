//! Real worker-thread integration tests use isolated SQLite fixtures.
use crate::{
    data_directory::DataDirectory,
    persistence::PersistenceState,
    store::{
        Store,
        capture::test_support::{Fixture, Inspection, at, metadata},
    },
};
use quartz_domain::capture::*;
use std::collections::BTreeSet;

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
        let directory = old.into_directory();
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
        let workers = crate::core::app::Workers::start(
            store,
            metadata(),
            clock.clone(),
            Arc::new(|| {}),
            create_foreground(),
        )
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
    fn reader(&self) -> Inspection {
        Inspection::open(self.directory.path())
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
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let mut reader = runtime.reader();
    let recovered = reader.run(1);
    assert_eq!(recovered.end_reason, Some(RunEndReason::CrashRecovered));
    runtime.observe(CaptureEvent::Session(SessionStatus::Unavailable));
    runtime.wait(|command| matches!(command, MainCommand::Effect(CaptureEffect::QuerySession)));
    runtime.observe(CaptureEvent::Foreground {
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
    let before = reader.interval_count();
    assert_eq!(before, 0, "session readiness gates foreground rows");
    runtime.wait(|command| matches!(command, MainCommand::Effect(CaptureEffect::CheckIdle)));
    runtime.observe(CaptureEvent::Foreground {
        source: ObservationSource::Review,
        foreground: Foreground::None(NoForegroundReason::Empty),
    });
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let confirmed_before = reader.run(2).confirmed_utc_ms.expect("open run");
    runtime.clock.advance(59_000);
    runtime.observe(CaptureEvent::Wake);
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Persistence(PersistenceState::Buffered { .. })
        )
    });
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let confirmed_after = reader.run(2).confirmed_utc_ms.expect("open run");
    assert!(
        confirmed_after >= confirmed_before + 59_000,
        "DB timeout flushes the checkpoint"
    );
    reader.inject_write_failure();
    runtime.observe(CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)));
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Persistence(PersistenceState::Failed { .. })
        )
    });
    reader.restore_writes();
    runtime.observe(CaptureEvent::Stop(StopChange::End(StopReason::Locked)));
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let directory = runtime.shutdown();
    let mut reader = Inspection::open(directory.path());
    let run = reader.run(2);
    assert_eq!(run.end_reason, Some(RunEndReason::UserExit));
    assert!(run.ended_utc_ms.is_some());
    assert_eq!(run.confirmed_utc_ms, None);
    assert_eq!(reader.open_clock_count(), 0);
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
            MainCommand::CaptureSnapshot {
                state: CaptureState::Gap(GapReason::EventOverload),
                ..
            }
        )
    });
    runtime.observe(CaptureEvent::Foreground {
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
    let mut reader = Inspection::open(directory.path());
    assert!(reader.overload_gap_count() > 0);
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
        let directory = old.into_directory();
        let store =
            Store::open(&DataDirectory::new(directory.path())).expect("reopen timed settings");
        let clock = Arc::new(RuntimeClock::new());
        clock.advance(200_000);
        let workers = crate::core::app::Workers::start(
            store,
            metadata(),
            clock.clone(),
            Arc::new(|| {}),
            create_foreground(),
        )
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
        self.wait(|command| matches!(command, MainCommand::CaptureSnapshot { state: CaptureState::Stopped { reasons, .. }, .. } if reasons.contains(&StopReason::Locked)));
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
    assert_eq!(reader.idle_count(), 0);
    let state = reader.keep_timing().expect("persisted grant");
    assert_eq!(state.started_utc_ms, 0);
    assert_eq!(state.until_utc_ms, 260_000);
    runtime.shutdown();
}

#[test]
fn restart_clears_keep_timing_that_expired_while_core_was_down() {
    use crate::core::app::MainCommand;
    let runtime = RuntimeFixture::restart_keep_timing(100_000);
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let mut reader = runtime.reader();
    assert!(reader.keep_timing().is_none());
    runtime.sample_idle_then_lock();
    assert_eq!(reader.idle_count(), 1);
    runtime.shutdown();
}

#[test]
fn large_query_yields_to_capture_commits_before_the_one_second_limit() {
    use crate::core::{
        app::{MainCommand, Workers},
        sources::ClockReader,
    };
    use std::time::{Duration, Instant};
    const QUERY_FACTS: i64 = 20_000;
    const COMMIT_LIMIT: Duration = Duration::from_secs(1);
    const ADMISSION_POLL: Duration = Duration::from_millis(1);
    let fixture = crate::store::query::test_support::QueryFixture::new(QUERY_FACTS);
    let request = fixture.request_day();
    let clock = Arc::new(RuntimeClock::new());
    let workers = Workers::start(
        fixture.store,
        metadata(),
        clock.clone(),
        Arc::new(|| {}),
        create_foreground(),
    )
    .expect("real workers");
    let runtime = RuntimeFixture {
        directory: fixture.directory,
        workers,
        clock,
    };
    runtime.observe(CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())));
    runtime.observe(CaptureEvent::Foreground {
        source: ObservationSource::Review,
        foreground: Foreground::None(NoForegroundReason::Empty),
    });
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let mut reply = runtime
        .workers
        .queries
        .submit(request)
        .expect("large query admission");
    let admitted = Instant::now();
    while runtime.workers.queries.queued_requests() > 0 {
        assert!(
            admitted.elapsed() < COMMIT_LIMIT,
            "DB must start queued query"
        );
        std::thread::sleep(ADMISSION_POLL);
    }
    assert!(
        matches!(
            reply.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "large query is running when capture write is submitted"
    );
    let submitted = Instant::now();
    runtime.observe(CaptureEvent::Stop(StopChange::Begin(StopReason::Paused)));
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let latency = submitted.elapsed();
    assert!(
        latency < COMMIT_LIMIT,
        "capture commit took {latency:?} while query was running"
    );
    let mut inspection = runtime.reader();
    let pauses = inspection.pause_count();
    assert_eq!(pauses, 1);
    assert!(reply.blocking_recv().expect("query reply").is_ok());
    assert!(runtime.clock.read().is_ok());
    runtime.shutdown();
}

use super::*;
use crate::core::sources::{ClockReader, ReadClockError};
use crate::persistence::WriteQueue;
use capture_loop::{CaptureLoop, Settlement};
use crossbeam_channel::bounded;
use quartz_domain::rules::registry::{RegistryState, RuleRegistry};

struct TestClock(Timestamp);
impl ClockReader for TestClock {
    fn read(&self) -> Result<Timestamp, ReadClockError> {
        Ok(self.0)
    }
}

#[test]
fn resume_notifications_create_only_one_clock_segment() {
    let at = Timestamp {
        utc: UtcInstant::try_from(0).expect("UTC"),
        mono: MonoInstant::from_millis(0),
    };
    let started = CaptureCore::start(RunId::new(1), at).expect("core");
    let (_sender, observed) = bounded(limits::MAIN_QUEUE_CAPACITY);
    let (_feedback, replies) = bounded(limits::WRITE_QUEUE_CAPACITY);
    let (_shutdown, stop) = bounded(CONTROL_CAPACITY);
    let (database, _writes) = bounded(CONTROL_CAPACITY);
    let channel = crate::core::app::MainChannel::new(limits::MAIN_QUEUE_CAPACITY);
    let main = channel.sender;
    let _commands = channel.receiver;
    let snapshots = RuleRegistry::from_state(RegistryState::default())
        .expect("rules")
        .snapshots();
    let mut capture = CaptureLoop {
        pending_control: None,
        discard_remaining: 0,
        core: started.core,
        run: RunId::new(1),
        at,
        snapshots: snapshots.clone(),
        observed,
        replies,
        stop,
        database,
        writer: WriteQueue::new(),
        main: MainPort {
            sender: main,
            wake: Arc::new(|| {}),
        },
        overloaded: Arc::new(AtomicBool::new(false)),
        clock: Arc::new(TestClock(at)),
    };
    for event in [
        CaptureEvent::Session(SessionStatus::Ready(Default::default())),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Suspended)),
        CaptureEvent::ClockChanged(ClockChange::Resume),
        CaptureEvent::Stop(StopChange::End(StopReason::Suspended)),
    ] {
        capture
            .step(ObservedEvent { at, event }, Settlement::Deferred)
            .expect("power observation");
    }
    let ended = capture
        .core
        .step(CaptureInput {
            at,
            rules: snapshots.load(),
            event: CaptureEvent::Shutdown,
        })
        .expect("final segment");
    assert!(ended.writes.iter().any(|write| matches!(write, CaptureWrite::CloseClockSegment { segment, .. } if segment.get() == 2)));
}

pub(super) struct FakeResolver;
impl crate::core::foreground::ForegroundResolve for FakeResolver {
    fn resolve(
        &mut self,
        snapshot: crate::core::foreground::Snapshot,
    ) -> crate::core::foreground::Resolution {
        crate::core::foreground::Resolution::Event(super::ObservedEvent {
            at: snapshot.at,
            event: CaptureEvent::Foreground {
                source: snapshot.source,
                foreground: Foreground::None(NoForegroundReason::Empty),
            },
        })
    }
}
pub(super) fn create_foreground() -> crate::core::foreground::CreateForeground {
    Box::new(|| Ok(Box::new(FakeResolver)))
}

const RUNTIME_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleEvent {
    ResolverCreated,
    SnapshotResolved,
    UiShutdown,
    PipeStopped,
    ResolverDropped,
}
#[derive(Clone, Default)]
struct LifecycleLog(Arc<std::sync::Mutex<Vec<LifecycleEvent>>>);
impl LifecycleLog {
    fn record(&self, event: LifecycleEvent) {
        self.0.lock().expect("lifecycle log").push(event);
    }
    fn events(&self) -> Vec<LifecycleEvent> {
        self.0.lock().expect("lifecycle log").clone()
    }
}
struct RecordedResolver {
    log: LifecycleLog,
    owner: std::thread::ThreadId,
    // Proves that the injected resolver need not be Send.
    _thread_local: std::rc::Rc<()>,
}
impl crate::core::foreground::ForegroundResolve for RecordedResolver {
    fn resolve(
        &mut self,
        snapshot: crate::core::foreground::Snapshot,
    ) -> crate::core::foreground::Resolution {
        assert_eq!(
            self.owner,
            std::thread::current().id(),
            "resolve on creator thread"
        );
        self.log.record(LifecycleEvent::SnapshotResolved);
        FakeResolver.resolve(snapshot)
    }
}
impl Drop for RecordedResolver {
    fn drop(&mut self) {
        assert_eq!(
            self.owner,
            std::thread::current().id(),
            "release on creator thread"
        );
        self.log.record(LifecycleEvent::ResolverDropped);
    }
}
struct TestPipe {
    log: LifecycleLog,
}
impl super::CorePipe for TestPipe {
    fn target_control(
        &self,
        _: crate::ipc::registry::ConnectionId,
        op: quartz_protocol::ControlOp,
    ) {
        self.control(op);
    }
    fn control(&self, op: quartz_protocol::ControlOp) {
        assert_eq!(op, quartz_protocol::ControlOp::UiShutdown);
        self.log.record(LifecycleEvent::UiShutdown);
    }
    fn shutdown(self: Box<Self>) -> Result<(), super::CallPlatformPortError> {
        self.log.record(LifecycleEvent::PipeStopped);
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum StartTestPipe {
    Succeed,
    Fail,
}
struct RuntimePortsFixture {
    ports: super::PlatformPorts,
    log: LifecycleLog,
    clock: Arc<RuntimeClock>,
}
impl RuntimePortsFixture {
    fn new(start: StartTestPipe) -> Self {
        let log = LifecycleLog::default();
        let capture_log = log.clone();
        let pipe_log = log.clone();
        let (started, ready) = bounded(CONTROL_CAPACITY);
        let clock = Arc::new(RuntimeClock::new());
        let ports = super::PlatformPorts {
            wake: Arc::new(|| {}),
            ui_wake: Arc::new(|_| Ok(())),
            clock: clock.clone(),
            session_id: crate::store::capture::WindowsSessionId::new(7),
            proposed_device_id: "test-device-guid".into(),
            instance_id: quartz_protocol::InstanceId("test-instance-guid".into()),
            foreground: Box::new(move || {
                assert_eq!(
                    std::thread::current().name(),
                    Some("quartz-capture"),
                    "factory called on capture thread"
                );
                capture_log.record(LifecycleEvent::ResolverCreated);
                started.send(()).expect("factory startup receiver");
                Ok(Box::new(RecordedResolver {
                    log: capture_log,
                    owner: std::thread::current().id(),
                    _thread_local: std::rc::Rc::new(()),
                }))
            }),
            pipe: Box::new(move |ports| {
                ready
                    .recv_timeout(RUNTIME_TEST_TIMEOUT)
                    .expect("resolver starts on capture thread");
                assert_eq!(ports.shutdown_timeout, super::SHUTDOWN_TIMEOUT);
                match start {
                    StartTestPipe::Succeed => {
                        // Exercise the UI port assembled by the runtime.
                        (ports.services.ui)(crate::ipc::UiCommand::Open).expect("queue UI request");
                        Ok(Box::new(TestPipe { log: pipe_log }))
                    }
                    StartTestPipe::Fail => Err(Box::new(std::io::Error::from(
                        std::io::ErrorKind::AddrInUse,
                    ))),
                }
            }),
            language: quartz_i18n::Language::English,
            locale: None,
            autostart: crate::core::autostart::test_support::in_memory_autostart(
                r"C:\Quartz\quartz.exe",
            )
            .0,
        };
        Self { ports, log, clock }
    }
}
fn run_runtime_test(run: impl FnOnce() + Send + 'static) {
    let (finished, completion) = bounded(CONTROL_CAPACITY);
    let thread = std::thread::spawn(move || {
        run();
        finished.send(()).expect("completion receiver");
    });
    completion
        .recv_timeout(RUNTIME_TEST_TIMEOUT)
        .expect("runtime joins within shutdown budget");
    thread.join().expect("runtime test thread");
}
#[test]
fn core_runtime_starts_resolves_observation_and_shuts_down_in_order() {
    run_runtime_test(|| {
        let directory = tempfile::tempdir().expect("isolated runtime database");
        let fixture = RuntimePortsFixture::new(StartTestPipe::Succeed);
        let runtime =
            super::CoreRuntime::start(&DataDirectory::new(directory.path()), fixture.ports)
                .expect("start core runtime");
        assert_eq!(
            runtime.ui_commands.try_recv().expect("UI request"),
            crate::ipc::UiCommand::Open
        );
        fixture.clock.advance(100);
        let at = fixture.clock.read().expect("observation time");
        runtime
            .sender()
            .foreground(crate::core::foreground::Snapshot {
                at,
                read_at: at,
                event_time_ms: None,
                source: ObservationSource::Hook,
                kind: crate::core::foreground::EventKind::Foreground,
                window: WindowHandle::new(0),
                title: Title::readable(""),
                desktop: crate::core::foreground::Desktop::Observable,
            })
            .expect("inject foreground observation");
        runtime
            .shutdown(super::RuntimeExit::UserExit)
            .expect("stop core runtime");
        let events = fixture.log.events();
        assert!(
            events.contains(&LifecycleEvent::SnapshotResolved),
            "accepted observation resolved before shutdown returns"
        );
        let lifecycle: Vec<_> = events
            .into_iter()
            .filter(|event| *event != LifecycleEvent::SnapshotResolved)
            .collect();
        assert_eq!(
            lifecycle,
            vec![
                LifecycleEvent::ResolverCreated,
                LifecycleEvent::UiShutdown,
                LifecycleEvent::PipeStopped,
                LifecycleEvent::ResolverDropped,
            ],
            "pipe stops before capture resources are released"
        );
        let mut stored = Inspection::open(directory.path());
        assert_eq!(stored.run(1).end_reason, Some(RunEndReason::UserExit));
        assert_eq!(
            stored.open_clock_count(),
            0,
            "final clock segment persisted"
        );
        drop(stored);
        std::fs::remove_dir_all(directory.path()).expect("all database handles released");
    });
}
#[test]
fn core_runtime_pipe_start_failure_joins_started_workers() {
    run_runtime_test(|| {
        let directory = tempfile::tempdir().expect("isolated runtime database");
        let fixture = RuntimePortsFixture::new(StartTestPipe::Fail);
        let error = super::CoreRuntime::start(&DataDirectory::new(directory.path()), fixture.ports)
            .err()
            .expect("pipe startup fails");
        assert!(matches!(error, super::StartCoreError::StartPipe { .. }));
        assert_eq!(
            fixture.log.events(),
            vec![
                LifecycleEvent::ResolverCreated,
                LifecycleEvent::ResolverDropped
            ],
            "startup error returns after capture releases resources"
        );
        let mut stored = Inspection::open(directory.path());
        assert_eq!(stored.run(1).end_reason, Some(RunEndReason::Shutdown));
        assert_eq!(stored.open_clock_count(), 0);
        drop(stored);
        std::fs::remove_dir_all(directory.path())
            .expect("failure cleanup released database handles");
    });
}

#[test]
fn core_runtime_session_end_joins_workers_already_settled_by_shutdown_port() {
    run_runtime_test(|| {
        let directory = tempfile::tempdir().expect("isolated runtime database");
        let fixture = RuntimePortsFixture::new(StartTestPipe::Succeed);
        let runtime =
            super::CoreRuntime::start(&DataDirectory::new(directory.path()), fixture.ports)
                .expect("start core runtime");
        runtime
            .shutdown_port()
            .close(fixture.clock.read().expect("session-end clock"))
            .expect("WM_ENDSESSION waits for persistence");
        assert_eq!(
            Inspection::open(directory.path()).run(1).end_reason,
            Some(RunEndReason::Shutdown)
        );
        runtime
            .shutdown(super::RuntimeExit::SessionEnded)
            .expect("join without requesting shutdown again");
        assert_eq!(
            fixture.log.events(),
            vec![
                LifecycleEvent::ResolverCreated,
                LifecycleEvent::ResolverDropped,
                LifecycleEvent::PipeStopped
            ],
            "session callback settles workers before returning to the message loop"
        );
    });
}

#[test]
fn io_write_replies_publish_committed_versions_and_rollback_keeps_versions() {
    use crate::persistence::dispatch::{TestCommit, WriteRequest};
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("write-test".into()));
    let services = crate::ipc::Services {
        summary: None,
        timed: crate::ipc::TimedPolicyPort::unavailable(),
        queries: runtime.workers.queries.clone(),
        writes: runtime.workers.writes.clone(),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
        browser_streams: status.browser_streams(),
        browser: crate::ipc::browser::BrowserCapturePort::noop(),
        ui: Arc::new(|_| Ok(())),
    };
    let mut versions = runtime.workers.versions.clone();
    let io = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("I/O runtime");
    let date = "1970-01-01".parse().expect("date");
    let before = io
        .block_on(versions.set_days(BTreeSet::from([date])))
        .expect("subscribe");
    versions.updates.borrow_and_update();
    let mut reader =
        crate::store::ReadOnlyStore::open(&DataDirectory::new(runtime.directory.path()).database())
            .expect("inspection reader");
    let metadata = reader
        .query_settings()
        .expect("initial metadata")
        .metadata_version;
    io.block_on(async {
        assert!(
            runtime
                .workers
                .writes
                .call(WriteRequest::BumpMetadata {
                    outcome: TestCommit::Rollback
                })
                .await
                .is_err()
        );
        assert!(
            !versions.updates.has_changed().expect("publisher open"),
            "rolled back transaction must not publish"
        );
        assert_eq!(
            reader
                .query_settings()
                .expect("rollback metadata")
                .metadata_version,
            metadata
        );
        services
            .writes
            .call(WriteRequest::BumpMetadata {
                outcome: TestCommit::Commit,
            })
            .await
            .expect("I/O to database reply");
        assert!(
            versions.updates.has_changed().expect("publisher open"),
            "versions must be published before success reply"
        );
        assert_ne!(
            versions.updates.borrow().days[&date].versions,
            before.days[&date].versions
        );
        assert_eq!(
            reader
                .query_settings()
                .expect("committed metadata")
                .metadata_version,
            metadata + 1,
            "reply follows SQLite commit"
        );
    });
    runtime
        .workers
        .writes
        .submit(WriteRequest::BumpMetadata {
            outcome: TestCommit::Commit,
        })
        .expect("main-thread admission")
        .blocking_recv()
        .expect("database reply")
        .expect("main-thread reply");
    assert_eq!(
        reader
            .query_settings()
            .expect("main-thread metadata")
            .metadata_version,
        metadata + 2
    );
    drop(reader);
    runtime.shutdown();
}

/// Framed memory transport connected to the real database worker and IPC registry.
struct AnnotationClient {
    frames:
        tokio_util::codec::Framed<tokio::io::DuplexStream, tokio_util::codec::LengthDelimitedCodec>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl AnnotationClient {
    async fn connect(
        services: crate::ipc::Services,
        registry: crate::ipc::registry::RegistryHandle,
        role: quartz_protocol::Role,
    ) -> Self {
        let connection = crate::ipc::connection::Connection {
            registry,
            peer_pid: crate::ipc::ProcessId::new(std::process::id()),
            services,
            timeouts: crate::ipc::connection::ConnectionTimeouts::default(),
        };
        let (server, client) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(async move {
            let (read, write) = tokio::io::split(server);
            connection
                .run(
                    read,
                    write,
                    &mut crate::ipc::ConnectionSession::new(crate::ipc::AllowedRoles::Quartz),
                    tokio::time::Instant::now() + Duration::from_secs(5),
                )
                .await
        });
        let mut client = Self {
            frames: tokio_util::codec::Framed::new(client, quartz_protocol::frame_codec()),
            task,
        };
        client.send(1, quartz_protocol::Op::Hello, serde_json::json!({"proto":{"min":1,"max":1},"role":role,"client_version":"test","ui":{"state":"starting"}})).await;
        client.success(1).await;
        client
    }
    async fn send(&mut self, id: u64, op: quartz_protocol::Op, body: serde_json::Value) {
        use futures_util::SinkExt;
        let request = quartz_protocol::Envelope::Request(quartz_protocol::Request {
            id: quartz_protocol::RequestId(id),
            op,
            body,
        });
        let bytes = quartz_protocol::encode_json(&request).expect("request JSON");
        self.frames
            .send(bytes.as_slice())
            .await
            .expect("send request");
    }
    async fn receive(&mut self) -> quartz_protocol::Envelope {
        use futures_util::StreamExt;
        // Day events are throttled after the initial subscription event.
        let deadline = crate::ipc::outbox::DAY_PUSH_INTERVAL + Duration::from_secs(5);
        let bytes = tokio::time::timeout(deadline, self.frames.next())
            .await
            .expect("IPC response deadline")
            .expect("response")
            .expect("frame");
        quartz_protocol::decode_json(&bytes).expect("envelope")
    }
    async fn response(&mut self, id: u64) -> quartz_protocol::ResponseOutcome {
        let quartz_protocol::Envelope::Response(response) = self.receive().await else {
            panic!("expected response")
        };
        assert_eq!(response.id, Some(quartz_protocol::RequestId(id)));
        response.outcome
    }
    async fn success(&mut self, id: u64) -> serde_json::Value {
        let quartz_protocol::ResponseOutcome::Ok { body } = self.response(id).await else {
            panic!("expected success")
        };
        body
    }
    async fn close(self) {
        drop(self.frames);
        self.task
            .await
            .expect("connection task")
            .expect("connection closed");
    }
}

#[test]
fn categories_ipc_retries_and_day_subscription_emit_only_real_changes() {
    use futures_util::StreamExt;
    use quartz_protocol::{Envelope, ErrorKind, Op, ProtocolError, ResponseOutcome, Role};
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId(
        "category-integration".into(),
    ));
    let services = crate::ipc::Services {
        summary: None,
        timed: crate::ipc::TimedPolicyPort::unavailable(),
        queries: runtime.workers.queries.clone(),
        writes: runtime.workers.writes.clone(),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
        browser_streams: status.browser_streams(),
        browser: crate::ipc::browser::BrowserCapturePort::noop(),
        ui: Arc::new(|_| Ok(())),
    };
    let io = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("I/O runtime");
    io.block_on(async {
        let registry = services.start_registry();
        let mut cli = AnnotationClient::connect(services.clone(), registry.clone(), Role::Cli).await;
        for op in [Op::CategoriesList, Op::CategoriesCreate, Op::CategoriesUpdate, Op::CategoriesReorder, Op::CategoriesArchive] {
            cli.send(2, op, serde_json::json!({})).await;
            assert_eq!(cli.response(2).await, ResponseOutcome::Err { error: ProtocolError::new(ErrorKind::Forbidden) });
        }
        cli.close().await;
        let mut subscriber = AnnotationClient::connect(services.clone(), registry.clone(), Role::Ui).await;
        let date: quartz_protocol::LocalDate = "2025-01-02".parse().expect("date");
        subscriber.send(2, Op::Subscribe, serde_json::json!({"topics":[format!("day:{date}")]})).await;
        subscriber.success(2).await;
        let Envelope::Event(initial) = subscriber.receive().await else { panic!("initial day event") };
        let mut ui = AnnotationClient::connect(services, registry, Role::Ui).await;
        let body = serde_json::json!({"entity":{"uuid":"00000000-0000-4000-8000-000000000123","name":"研究","parent_uuid":null,"color":"cat.8","tendency":"focus"}});
        ui.send(2, Op::CategoriesCreate, body.clone()).await;
        let created = ui.success(2).await;
        let Envelope::Event(changed) = subscriber.receive().await else { panic!("changed day event") };
        assert_eq!(changed.topic, quartz_protocol::Topic::Day(date));
        assert_ne!(changed.body, initial.body);
        assert!(changed.seq > initial.seq);
        ui.send(3, Op::CategoriesCreate, body.clone()).await;
        assert_eq!(ui.success(3).await, created);
        ui.send(4, Op::CategoriesUpdate, body.clone()).await;
        assert_eq!(ui.success(4).await, created);
        let mut invalid = body.clone();
        invalid["entity"]["color"] = serde_json::json!("invalid");
        ui.send(5, Op::CategoriesUpdate, invalid).await;
        assert_eq!(ui.response(5).await, ResponseOutcome::Err { error: ProtocolError::new(ErrorKind::BadRequest) });
        // Observe beyond the existing per-day throttle to detect delayed duplicate events.
        let quiet = crate::ipc::outbox::DAY_PUSH_INTERVAL + crate::ipc::outbox::STATE_PUSH_INTERVAL;
        assert!(tokio::time::timeout(quiet, subscriber.frames.next()).await.is_err());
        let mut renamed = body;
        renamed["entity"]["name"] = serde_json::json!("阅读");
        ui.send(6, Op::CategoriesUpdate, renamed).await;
        assert_eq!(ui.success(6).await["entity"]["name"], "阅读");
        let Envelope::Event(renamed) = subscriber.receive().await else { panic!("renamed day event") };
        assert_eq!(renamed.topic, quartz_protocol::Topic::Day(date));
        assert_ne!(renamed.body, changed.body);
        assert!(renamed.seq > changed.seq);
        assert!(tokio::time::timeout(quiet, subscriber.frames.next()).await.is_err());
        ui.close().await;
        subscriber.close().await;
    });
    runtime.shutdown();
}

#[test]
fn annotations_ipc_writes_retry_permissions_busy_and_day_push() {
    use quartz_protocol::{ErrorKind, Op, ProtocolError, ResponseOutcome};
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId(
        "annotation-integration".into(),
    ));
    let services = crate::ipc::Services {
        summary: None,
        timed: crate::ipc::TimedPolicyPort::unavailable(),
        queries: runtime.workers.queries.clone(),
        writes: runtime.workers.writes.clone(),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
        browser_streams: status.browser_streams(),
        browser: crate::ipc::browser::BrowserCapturePort::noop(),
        ui: Arc::new(|_| Ok(())),
    };
    let io = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("I/O runtime");
    io.block_on(async {
        let registry = services.start_registry();
        let uuid = "00000000-0000-4000-8000-000000000001";
        let date: quartz_protocol::LocalDate = "2025-01-02".parse().expect("date");
        let range = crate::query::calendar::DayCalendar::new(
            date,
            crate::query::calendar::DayStartOffset::MIDNIGHT,
        )
        .expect("day")
        .range;
        let body = serde_json::json!({
            "entities": [{
                "uuid": uuid,
                "kind": "note",
                "start_utc_ms": range.start,
                "end_utc_ms": range.end,
                "label_preset": "rest"
            }]
        });
        let mut cli = AnnotationClient::connect(
            services.clone(),
            registry.clone(),
            quartz_protocol::Role::Cli,
        )
        .await;
        for op in [
            Op::AnnotationsCreate,
            Op::AnnotationsUpdate,
            Op::AnnotationsDelete,
        ] {
            cli.send(2, op, body.clone()).await;
            assert_eq!(
                cli.response(2).await,
                ResponseOutcome::Err {
                    error: ProtocolError::new(ErrorKind::Forbidden)
                }
            );
        }
        cli.close().await;
        let mut subscriber = AnnotationClient::connect(
            services.clone(),
            registry.clone(),
            quartz_protocol::Role::Ui,
        )
        .await;
        subscriber
            .send(
                2,
                Op::Subscribe,
                serde_json::json!({"topics": [format!("day:{date}")]}),
            )
            .await;
        subscriber.success(2).await;
        let quartz_protocol::Envelope::Event(initial) = subscriber.receive().await else {
            panic!("initial day event")
        };
        let initial: quartz_protocol::DayChanged =
            serde_json::from_value(initial.body).expect("initial version");
        let mut ui = AnnotationClient::connect(
            services.clone(),
            registry.clone(),
            quartz_protocol::Role::Ui,
        )
        .await;
        ui.send(2, Op::AnnotationsCreate, body.clone()).await;
        let created = ui.success(2).await;
        let entities: quartz_protocol::AnnotationsResponse =
            serde_json::from_value(created.clone()).expect("typed annotation response");
        assert_eq!(entities.entities.len(), 1);
        assert_eq!(entities.entities[0].uuid.0.to_string(), uuid);
        assert_eq!(
            entities.entities[0].created_utc_ms,
            entities.entities[0].updated_utc_ms
        );
        assert_eq!(entities.entities[0].values.label, None);
        assert_eq!(
            entities.entities[0].values.label_preset,
            Some(quartz_protocol::AnnotationLabelPreset::Rest)
        );
        let quartz_protocol::Envelope::Event(changed) = subscriber.receive().await else {
            panic!("changed day event")
        };
        assert_eq!(changed.topic, quartz_protocol::Topic::Day(date));
        let changed: quartz_protocol::DayChanged =
            serde_json::from_value(changed.body).expect("changed version");
        assert_ne!(changed.versions, initial.versions);
        ui.send(3, Op::AnnotationsCreate, body).await;
        assert_eq!(ui.success(3).await, created);
        // The final values clear omitted optional fields and preserve the stored kind.
        ui.send(
            4,
            Op::AnnotationsUpdate,
            serde_json::json!({
                "entities": [{
                    "uuid": uuid,
                    "start_utc_ms": range.start,
                    "end_utc_ms": range.end,
                    "note": "text"
                }]
            }),
        )
        .await;
        let updated = ui.success(4).await;
        assert!(updated["entities"][0]["label"].is_null());
        assert_eq!(
            updated["entities"][0].get("label_preset"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(updated["entities"][0]["note"], "text");
        ui.send(
            5,
            Op::AnnotationsUpdate,
            serde_json::json!({
                "entities": [{
                    "uuid": uuid,
                    "kind": "active_override",
                    "start_utc_ms": range.start,
                    "end_utc_ms": range.end
                }]
            }),
        )
        .await;
        assert_eq!(
            ui.response(5).await,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::BadRequest)
            }
        );
        ui.send(
            6,
            Op::AnnotationsCreate,
            serde_json::json!({
                "entities": [{
                    "uuid": "00000000-0000-4000-8000-000000000002",
                    "kind": "note",
                    "start_utc_ms": 0,
                    "end_utc_ms": 0,
                    "label": "invalid"
                }]
            }),
        )
        .await;
        assert_eq!(
            ui.response(6).await,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::InvalidRange)
            }
        );
        for request_id in [7, 8] {
            ui.send(
                request_id,
                Op::AnnotationsDelete,
                serde_json::json!({"uuids": [uuid]}),
            )
            .await;
            assert_eq!(
                ui.success(request_id).await,
                serde_json::json!({"accepted": true})
            );
        }
        let channel = crate::ipc::WriteChannel::new();
        let mut pending = Vec::new();
        for _ in 0..crate::persistence::dispatch::WRITE_CHANNEL_CAPACITY {
            let request = crate::persistence::dispatch::WriteRequest::Annotations(
                crate::persistence::dispatch::AnnotationWrite::Delete(
                    quartz_protocol::AnnotationsDeleteRequest { uuids: Vec::new() },
                ),
            );
            pending.push(channel.port.submit(request).expect("fill write channel"));
        }
        let mut busy_services = services;
        busy_services.writes = channel.port;
        let mut busy =
            AnnotationClient::connect(busy_services, registry, quartz_protocol::Role::Ui).await;
        busy.send(
            2,
            Op::AnnotationsCreate,
            serde_json::json!({"entities": []}),
        )
        .await;
        assert_eq!(
            busy.response(2).await,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::Busy)
            }
        );
        assert_eq!(
            channel.receiver.len(),
            crate::persistence::dispatch::WRITE_CHANNEL_CAPACITY
        );
        busy.close().await;
        ui.close().await;
        subscriber.close().await;
    });
    runtime.shutdown();
}

#[test]
fn settings_ipc_commits_before_reply_and_updates_tray_and_state_through_ui_port() {
    use crate::core::autostart::test_support::{FakeRegistryFault, in_memory_autostart};
    use quartz_protocol::{
        AutostartEnabled, EffectiveUiSettings, HourCycle, LocaleSettings, Op, Role,
        SettingsResponse, UiLanguage, Weekday,
    };
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId(
        "settings-integration".into(),
    ));
    let system = EffectiveUiSettings {
        resident: Default::default(),
        prewarm: Default::default(),
        ui_language: UiLanguage::English,
        locale: Some(LocaleSettings {
            hour_cycle: HourCycle::H24,
            first_day_of_week: Weekday::Monday,
        }),
    };
    let (autostart_port, autostart_registry) =
        in_memory_autostart(r"C:\Program Files\Quartz\quartz.exe");
    let mut channel = crate::ipc::UiCommands::channel(Arc::new(|_| Ok(())));
    let services = crate::ipc::Services {
        summary: None,
        timed: crate::ipc::TimedPolicyPort::unavailable(),
        queries: runtime.workers.queries.clone(),
        writes: runtime.workers.writes.clone().with_settings(
            system,
            Arc::clone(&channel.port),
            autostart_port,
        ),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
        browser_streams: status.browser_streams(),
        browser: crate::ipc::browser::BrowserCapturePort::noop(),
        ui: Arc::clone(&channel.port),
    };
    let io = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("settings I/O runtime");
    io.block_on(async {
        let registry = services.start_registry();
        let mut ui = AnnotationClient::connect(services.clone(), registry.clone(), Role::Ui).await;
        let mut subscriber = AnnotationClient::connect(services.clone(), registry.clone(), Role::Ui).await;
        subscriber.send(2, Op::Subscribe, serde_json::json!({"topics":["state"]})).await;
        subscriber.success(2).await;
        let quartz_protocol::Envelope::Event(initial) = subscriber.receive().await else { panic!("initial state event"); };
        assert_eq!(initial.body["ui_language"], "en");
        ui.send(2, Op::SettingsGet, serde_json::json!({})).await;
        let initial: SettingsResponse = serde_json::from_value(ui.success(2).await).expect("typed defaults");
        assert_eq!(initial.ui, quartz_protocol::UiSettings::default());
        assert_eq!(initial.effective, system);
        assert_eq!(initial.autostart, AutostartEnabled::Disabled);
        ui.send(8, Op::SettingsSet, serde_json::json!({"autostart": true})).await;
        let enabled: SettingsResponse = serde_json::from_value(ui.success(8).await).expect("autostart enabled");
        assert_eq!(enabled.autostart, AutostartEnabled::Enabled);
        assert_eq!(
            autostart_registry.raw().as_deref(),
            Some(r#""C:\Program Files\Quartz\quartz.exe" --autostart"#)
        );
        ui.send(9, Op::SettingsSet, serde_json::json!({"autostart": false})).await;
        let disabled: SettingsResponse = serde_json::from_value(ui.success(9).await).expect("autostart disabled");
        assert_eq!(disabled.autostart, AutostartEnabled::Disabled);
        assert_eq!(autostart_registry.raw(), None);
        let _ = channel.commands.take_settings_change();
        autostart_registry.set_fault(FakeRegistryFault::FailRead);
        ui.send(10, Op::SettingsGet, serde_json::json!({})).await;
        assert_eq!(
            ui.response(10).await,
            quartz_protocol::ResponseOutcome::Err {
                error: quartz_protocol::ProtocolError::new(quartz_protocol::ErrorKind::Internal)
            }
        );
        autostart_registry.set_fault(FakeRegistryFault::None);
        ui.send(3, Op::SettingsSet, serde_json::json!({"language":"zh-CN", "hour_cycle":"h12", "first_day_of_week":"sunday"})).await;
        let updated: SettingsResponse = serde_json::from_value(ui.success(3).await).expect("typed updated settings");
        assert_eq!(updated.effective.ui_language, UiLanguage::SimplifiedChinese);
        assert_eq!(updated.effective.locale.expect("locale").hour_cycle, HourCycle::H12);
        ui.send(4, Op::SettingsGet, serde_json::json!({})).await;
        assert_eq!(serde_json::from_value::<SettingsResponse>(ui.success(4).await).expect("committed settings"), updated);
        let changed = channel.commands.take_settings_change().expect("main-thread notification after commit");
        let mut tray_language = None;
        status.apply_ui_settings(changed, |language| tray_language = Some(language));
        assert_eq!(tray_language, Some(UiLanguage::SimplifiedChinese));
        let quartz_protocol::Envelope::Event(event) = subscriber.receive().await else { panic!("first state push after set"); };
        assert_eq!(event.topic, quartz_protocol::Topic::State);
        let state: quartz_protocol::StateSnapshot = serde_json::from_value(event.body).expect("typed state");
        assert_eq!(state.ui_language, updated.effective.ui_language);
        assert_eq!(state.locale, updated.effective.locale);
        assert_eq!(channel.commands.take_settings_change(), None);
        ui.send(5, Op::SettingsSet, serde_json::json!({"language":"auto"})).await;
        let automatic: SettingsResponse = serde_json::from_value(ui.success(5).await).expect("automatic language");
        assert_eq!(automatic.effective.ui_language, system.ui_language);
        assert_eq!(automatic.ui.hour_cycle, updated.ui.hour_cycle);
        assert_eq!(automatic.ui.first_day_of_week, updated.ui.first_day_of_week);
        for body in [serde_json::json!({"language":"fr"}), serde_json::json!({"hour_cycle":null})] {
            ui.send(6, Op::SettingsSet, body).await;
            assert_eq!(ui.response(6).await, quartz_protocol::ResponseOutcome::Err { error: quartz_protocol::ProtocolError::new(quartz_protocol::ErrorKind::BadRequest) });
        }
        let mut cli = AnnotationClient::connect(services.clone(), registry.clone(), Role::Cli).await;
        cli.send(7, Op::SettingsSet, serde_json::json!({"prewarm":{"enabled":false,"on_battery":true}})).await;
        let prewarm: SettingsResponse = serde_json::from_value(cli.success(7).await).expect("CLI prewarm update");
        assert_eq!(prewarm.ui.prewarm.enabled, quartz_protocol::PrewarmEnabled::Disabled);
        assert_eq!(prewarm.effective.prewarm.on_battery, quartz_protocol::PrewarmOnBattery::Allowed);
        let effective = channel.commands.take_settings_change().expect("committed prewarm notification");
        status.apply_ui_settings(effective, |_| {});
        assert_eq!(status.ui_settings().prewarm, prewarm.ui.prewarm);
        cli.close().await;
        ui.close().await;
        subscriber.close().await;
    });
    drop(services);
    let directory = runtime.shutdown();
    let mut reopened =
        Store::open(&DataDirectory::new(directory.path())).expect("reopen committed settings");
    let settings = reopened.ui_settings().expect("stored UI settings");
    assert_eq!(
        settings.prewarm,
        quartz_protocol::PrewarmSettings {
            enabled: quartz_protocol::PrewarmEnabled::Disabled,
            on_battery: quartz_protocol::PrewarmOnBattery::Allowed
        }
    );
    assert_eq!(settings.language, quartz_protocol::UiLanguageSetting::Auto);
    assert_eq!(settings.hour_cycle, quartz_protocol::HourCycleSetting::H12);
    assert_eq!(
        settings.first_day_of_week,
        quartz_protocol::FirstDayOfWeekSetting::Sunday
    );
}

#[test]
fn timed_ipc_admits_typed_capture_events_and_reports_busy_and_invalid_deadlines() {
    use crate::ipc::{TimedCommand, TimedDeadline};
    use quartz_protocol::{ErrorKind, Op, ProtocolError, ResponseOutcome, Role};
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("timed-wire".into()));
    let (sender, observed) = crossbeam_channel::bounded(1);
    let overloaded = Arc::new(AtomicBool::new(false));
    let ingress = CaptureSender {
        life: Default::default(),
        sender,
        overloaded: overloaded.clone(),
    };
    let services = crate::ipc::Services {
        summary: None,
        timed: ingress.timed_port(
            Arc::new(TestClock(at(10))),
            crate::query::calendar::DayStartOffset::MIDNIGHT,
        ),
        queries: runtime.workers.queries.clone(),
        writes: runtime.workers.writes.clone(),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
        browser_streams: status.browser_streams(),
        browser: crate::ipc::browser::BrowserCapturePort::noop(),
        ui: Arc::new(|_| Ok(())),
    };
    let io = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("I/O runtime");
    io.block_on(async {
        let registry = services.start_registry();
        let port = services.timed.clone();
        let mut ui = AnnotationClient::connect(services, registry.clone(), Role::Ui).await;
        ui.send(2, Op::TrackingPause, serde_json::json!({"until_utc_ms":10})).await;
        assert_eq!(ui.response(2).await, ResponseOutcome::Err { error: ProtocolError::new(ErrorKind::BadRequest) });
        ui.send(3, Op::TrackingPause, serde_json::json!({"until_utc_ms":100})).await;
        assert_eq!(ui.success(3).await, serde_json::json!({"accepted":true,"requested":{"policy":"pause","until_utc_ms":100}}));
        ui.send(4, Op::TrackingResume, serde_json::json!({})).await;
        assert_eq!(ui.response(4).await, ResponseOutcome::Err { error: ProtocolError::new(ErrorKind::Busy) });
        assert!(matches!(observed.try_recv().expect("pause event"), Observation::Event(ObservedEvent { at: timestamp, event: CaptureEvent::SetTimedState(TimedState { policy: TimedPolicy::Pause, until, .. }) }) if timestamp == at(10) && until == at(100).utc));
        ui.send(5, Op::TrackingResume, serde_json::json!({})).await;
        assert_eq!(ui.success(5).await, serde_json::json!({"accepted":true,"requested":{"policy":"pause","until_utc_ms":null}}));
        assert!(matches!(observed.try_recv().expect("resume event"), Observation::Event(ObservedEvent { event: CaptureEvent::CancelTimedPolicy(TimedPolicyKind::Pause), .. })));
        ui.send(6, Op::TrackingKeepAlive, serde_json::json!({"until_utc_ms":200})).await;
        assert_eq!(ui.success(6).await, serde_json::json!({"accepted":true,"requested":{"policy":"keep_timing","until_utc_ms":200}}));
        assert!(matches!(observed.try_recv().expect("keep timing event"), Observation::Event(ObservedEvent { event: CaptureEvent::SetTimedState(TimedState { policy: TimedPolicy::KeepTiming, .. }), .. })));
        ui.send(7, Op::PrivacyModeSet, serde_json::json!({"enabled":true})).await;
        assert_eq!(ui.response(7).await, ResponseOutcome::Err { error: ProtocolError::new(ErrorKind::BadRequest) });
        ui.send(8, Op::PrivacyModeSet, serde_json::json!({"enabled":true,"until_utc_ms":300})).await;
        assert_eq!(ui.success(8).await, serde_json::json!({"accepted":true,"requested":{"policy":"privacy","mode":{"mode":"private","until_utc_ms":300}}}));
        assert!(matches!(observed.try_recv().expect("private event"), Observation::Event(ObservedEvent { event: CaptureEvent::SetTimedState(TimedState { policy: TimedPolicy::Privacy(PrivacyMode::Private), .. }), .. })));
        ui.send(9, Op::PrivacyModeSet, serde_json::json!({"enabled":false})).await;
        assert_eq!(ui.success(9).await, serde_json::json!({"accepted":true,"requested":{"policy":"privacy","mode":null}}));
        assert!(matches!(observed.try_recv().expect("privacy cancellation"), Observation::Event(ObservedEvent { event: CaptureEvent::CancelTimedPolicy(TimedPolicyKind::Privacy), .. })));
        port.call(TimedCommand::Pause(TimedDeadline::UtcMillis(100))).expect("tray uses same admission");
        assert_eq!(observed.len(), 1);
        assert!(!overloaded.load(Ordering::Acquire));
        ui.close().await;
    });
    runtime.shutdown();
}

/// Allows the restarted core's clock to differ from the original create time.
const RESTART_CLOCK_ADVANCE_MS: u64 = 1000;
const ANNOTATION_COMMIT_POLL: std::time::Duration = std::time::Duration::from_millis(1);
#[derive(Clone, Copy, PartialEq, Eq)]
enum RestartPhase {
    BeforeRestart,
    AfterRestart,
}

/// Reads through the storage snapshot API so integration assertions do not use Diesel.
struct RestartAnnotations {
    rows: Vec<crate::store::query::QueryAnnotation>,
}
impl RestartAnnotations {
    fn read(directory: &DataDirectory) -> Self {
        let mut snapshot = crate::store::ReadOnlyStore::open(&directory.database())
            .expect("annotation reader")
            .snapshot();
        snapshot.begin().expect("pin annotation snapshot");
        let mut rows = Vec::new();
        while let Some(batch) = snapshot
            .read_batch(quartz_domain::facts::QueryRange {
                start: 0,
                end: 1000,
            })
            .expect("annotation batch")
        {
            rows.extend(batch.into_iter().filter_map(|row| match row {
                crate::store::query::InputRow::Annotation(annotation) => Some(annotation),
                _ => None,
            }));
        }
        Self { rows }
    }
    async fn wait(directory: &DataDirectory, count: usize) -> Self {
        let started = std::time::Instant::now();
        loop {
            let snapshot = Self::read(directory);
            if snapshot.rows.len() == count {
                return snapshot;
            }
            assert!(
                started.elapsed() < RUNTIME_TEST_TIMEOUT,
                "annotation commit deadline"
            );
            tokio::time::sleep(ANNOTATION_COMMIT_POLL).await;
        }
    }
}

#[test]
fn annotations_lost_response_retries_after_core_restart_without_duplicate_or_second_remainder() {
    use quartz_protocol::{
        AnnotationId, AnnotationKind, AnnotationValues, AnnotationsCreateRequest,
        AnnotationsResponse, CreateAnnotation, Op, Role,
    };
    // Separate databases prove both the one-row create and the three-row split case.
    for kind in [AnnotationKind::Note, AnnotationKind::ActiveOverride] {
        let temporary = tempfile::tempdir().expect("restart database");
        let directory = DataDirectory::new(temporary.path());
        let writer = CreateAnnotation {
            uuid: AnnotationId(
                "00000000-0000-4000-8000-000000000002"
                    .parse()
                    .expect("writer UUID"),
            ),
            kind,
            values: AnnotationValues {
                start_utc_ms: 20,
                end_utc_ms: 80,
                category: None,
                label_preset: None,
                label: Some("restart annotation".into()),
                note: None,
            },
        };
        let request = AnnotationsCreateRequest {
            entities: vec![writer.clone()],
        };
        let io = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("restart I/O");
        let mut before = None;
        for phase in [RestartPhase::BeforeRestart, RestartPhase::AfterRestart] {
            let mut fixture = RuntimePortsFixture::new(StartTestPipe::Succeed);
            fixture.ports.foreground = create_foreground();
            if phase == RestartPhase::AfterRestart {
                fixture.clock.advance(RESTART_CLOCK_ADVANCE_MS);
            }
            let (send, receive) = bounded(CONTROL_CAPACITY);
            fixture.ports.pipe = Box::new(move |ports| {
                send.send(ports.services).expect("receive runtime services");
                Ok(Box::new(TestPipe {
                    log: LifecycleLog::default(),
                }))
            });
            let runtime = CoreRuntime::start(&directory, fixture.ports)
                .expect("start core on same directory");
            let services = receive
                .recv_timeout(RUNTIME_TEST_TIMEOUT)
                .expect("runtime services");
            io.block_on(async {
                let registry = services.start_registry();
                let mut ui = AnnotationClient::connect(services, registry, Role::Ui).await;
                if phase == RestartPhase::BeforeRestart && kind == AnnotationKind::ActiveOverride {
                    let mut original = writer.clone();
                    original.uuid = AnnotationId(
                        "00000000-0000-4000-8000-000000000001"
                            .parse()
                            .expect("original UUID"),
                    );
                    original.values.start_utc_ms = 0;
                    original.values.end_utc_ms = 100;
                    ui.send(
                        2,
                        Op::AnnotationsCreate,
                        serde_json::to_value(AnnotationsCreateRequest {
                            entities: vec![original],
                        })
                        .expect("original body"),
                    )
                    .await;
                    ui.success(2).await;
                }
                ui.send(
                    3,
                    Op::AnnotationsCreate,
                    serde_json::to_value(&request).expect("create body"),
                )
                .await;
                let expected_count = if kind == AnnotationKind::Note { 1 } else { 3 };
                if phase == RestartPhase::BeforeRestart {
                    // Observe the SQLite commit independently, then discard the unread response.
                    before = Some(RestartAnnotations::wait(&directory, expected_count).await);
                } else {
                    let response: AnnotationsResponse =
                        serde_json::from_value(ui.success(3).await).expect("retry entity");
                    assert_eq!(response.entities.len(), 1);
                    assert_eq!(response.entities[0].uuid, writer.uuid);
                    assert_eq!(response.entities[0].kind, writer.kind);
                    assert_eq!(response.entities[0].values, writer.values);
                    let after = RestartAnnotations::read(&directory);
                    let before = before.as_ref().expect("committed pre-restart snapshot");
                    assert_eq!(after.rows.len(), expected_count);
                    for (index, old) in before.rows.iter().enumerate() {
                        let new = &after.rows[index];
                        assert_eq!(old.id, new.id, "remainder identity survives retry");
                        assert_eq!(old.range, new.range, "retry preserves clipped ranges");
                        assert_eq!(old.kind, new.kind);
                    }
                }
                ui.close().await;
            });
            runtime
                .shutdown(RuntimeExit::UserExit)
                .expect("real core shutdown before restart");
        }
    }
}

#[test]
fn stopped_capture_suppresses_queued_timer_system_and_foreground_observations() {
    let (sender, observed) = bounded(1);
    let ingress = CaptureSender {
        sender,
        overloaded: Arc::default(),
        life: Default::default(),
    };
    drop(observed);
    assert!(
        ingress
            .observe(ObservedEvent {
                at: at(0),
                event: CaptureEvent::Wake
            })
            .is_err()
    );
    ingress.stop();
    for event in [
        CaptureEvent::Wake,
        CaptureEvent::ClockChanged(ClockChange::Resume),
        CaptureEvent::Session(SessionStatus::Ready(Default::default())),
    ] {
        ingress
            .observe(ObservedEvent { at: at(0), event })
            .expect("late observation suppressed after settlement");
    }
}

#[test]
fn failed_persistence_recovers_without_any_new_capture_observation() {
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let mut reader = runtime.reader();
    reader.inject_write_failure();
    runtime.observe(CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)));
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Persistence(PersistenceState::Failed { .. })
        )
    });
    reader.restore_writes();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    assert!(
        reader.run(2).confirmed_utc_ms.is_some(),
        "retry persisted the retained capture boundary"
    );
    let _directory = runtime.shutdown();
}

#[test]
fn committed_classification_edit_immediately_splits_capture_without_another_observation() {
    use crate::core::app::MainCommand;
    use crate::persistence::dispatch::WriteRequest;
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    runtime.observe(CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())));
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Effect(CaptureEffect::ReviewForeground)
        )
    });
    runtime.observe(crate::store::capture::test_support::foreground(
        1,
        "fixture title",
    ));
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::CaptureSnapshot {
                state: CaptureState::Tracking(_),
                foreground: Some(_),
                ..
            }
        )
    });
    runtime.clock.advance(100);
    runtime.observe(crate::store::capture::test_support::foreground(
        1,
        "fixture title",
    ));
    let text = "format='quartz-rules/1'\n[set]\nkey='user.capture'\nkind='classification'\nname='Classification'\nparent='builtin.classification'\n[[rule]]\nid='test'\nlabel='Test'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='app.exe'}]\n";
    let request = WriteRequest::decode(
        quartz_protocol::Op::RuleSetsEdit,
        serde_json::json!({"key":"user.capture","base_version":0,"text":text}),
    )
    .expect("typed write");
    let reply = runtime
        .workers
        .writes
        .submit(request)
        .expect("mutation admission");
    runtime.wait(|command| matches!(command, MainCommand::PolicyChanged));
    let response = reply
        .blocking_recv()
        .expect("reply")
        .expect("committed edit");
    assert!(matches!(
        response,
        crate::persistence::dispatch::WriteResponse::Rules(_)
    ));
    runtime.observe(CaptureEvent::Stop(StopChange::Begin(StopReason::Paused)));
    runtime.wait(|command| matches!(command,MainCommand::CaptureSnapshot {state:CaptureState::Stopped {reasons,..},..} if reasons.contains(&StopReason::Paused)));
    let directory = runtime.shutdown();
    assert_eq!(
        Inspection::open(directory.path()).policy_boundary_count(),
        1,
        "one immediate boundary for the classification commit"
    );
}

#[test]
fn rule_validation_dispatch_returns_diagnostics_without_publishing_or_saving_a_revision() {
    use crate::core::app::MainCommand;
    use crate::persistence::dispatch::{WriteRequest, WriteResponse};
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let request = WriteRequest::decode(quartz_protocol::Op::RulesTextValidate, serde_json::json!({
        "key":"user.validation", "base_version":0,
        "text":"format='quartz-rules/1'\n[set]\nkey='user.validation'\nkind='privacy'\nname='Draft'\n"
    })).expect("typed validation");
    let response = runtime
        .workers
        .writes
        .submit(request)
        .expect("admission")
        .blocking_recv()
        .expect("reply")
        .expect("dry run");
    assert!(
        matches!(response,WriteResponse::RuleValidation(body) if body.valid && body.diagnostics.is_empty())
    );
    let directory = runtime.shutdown();
    let state = crate::store::ReadOnlyStore::open(&directory.path().join("quartz.db"))
        .expect("read only store")
        .rule_sets()
        .load()
        .expect("rules");
    assert!(
        !state
            .sets
            .contains_key(&"user.validation".parse().expect("key"))
    );
    assert_eq!(
        Inspection::open(directory.path()).policy_boundary_count(),
        0
    );
}

#[test]
fn changing_title_retention_setting_triggers_immediate_purge_and_publishes_versions() {
    use crate::core::app::MainCommand;
    use crate::core::autostart::test_support::in_memory_autostart;
    use crate::query::calendar::{DayCalendar, DayStartOffset};
    use crate::store::ReadOnlyStore;
    use quartz_domain::capture::limits::UTC_DAY_MS;
    use quartz_protocol::{
        EffectiveUiSettings, ErrorKind, HourCycle, LocaleSettings, Op, ProtocolError,
        ResponseOutcome, Role, SettingsResponse, TitleRetention, UiLanguage, Weekday,
        facts::{FactFields, FactTitleStatus},
    };

    let day_ms = u64::try_from(UTC_DAY_MS).expect("day ms");
    let old_start = 10 * day_ms;
    let now_start = 100 * day_ms;

    // Seed a closed historical interval well before the 30-day cutoff.
    let mut old = Fixture::new(at(old_start));
    old.persist(
        at(old_start),
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    old.persist(
        at(old_start),
        crate::store::capture::test_support::foreground(1, "expired core title"),
    );
    old.persist(at(old_start + 60_000), CaptureEvent::Shutdown);
    let directory = old.into_directory();

    let store = Store::open(&DataDirectory::new(directory.path())).expect("reopen store");
    let clock = Arc::new(RuntimeClock::new());
    clock.advance(now_start);
    let workers = crate::core::app::Workers::start(
        store,
        metadata(),
        clock.clone(),
        Arc::new(|| {}),
        create_foreground(),
    )
    .expect("workers");
    let runtime = RuntimeFixture {
        directory,
        workers,
        clock,
    };
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));

    // Record a current open interval whose title must be retained after the purge.
    runtime.observe(CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())));
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::Effect(CaptureEffect::ReviewForeground)
        )
    });
    runtime.observe(crate::store::capture::test_support::foreground(
        2,
        "retained open title",
    ));
    runtime.wait(|command| {
        matches!(
            command,
            MainCommand::CaptureSnapshot {
                state: CaptureState::Tracking(_),
                foreground: Some(_),
                ..
            }
        )
    });
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));

    let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId(
        "retention-integration".into(),
    ));
    let system = EffectiveUiSettings {
        resident: Default::default(),
        prewarm: Default::default(),
        ui_language: UiLanguage::English,
        locale: Some(LocaleSettings {
            hour_cycle: HourCycle::H24,
            first_day_of_week: Weekday::Monday,
        }),
    };
    let (autostart_port, _autostart_registry) =
        in_memory_autostart(r"C:\Program Files\Quartz\quartz.exe");
    let channel = crate::ipc::UiCommands::channel(Arc::new(|_| Ok(())));
    let services = crate::ipc::Services {
        summary: None,
        timed: crate::ipc::TimedPolicyPort::unavailable(),
        queries: runtime.workers.queries.clone(),
        writes: runtime.workers.writes.clone().with_settings(
            system,
            Arc::clone(&channel.port),
            autostart_port,
        ),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
        browser_streams: status.browser_streams(),
        browser: crate::ipc::browser::BrowserCapturePort::noop(),
        ui: Arc::clone(&channel.port),
    };

    let old_date = DayCalendar::current_date(at(old_start).utc.get(), DayStartOffset::MIDNIGHT)
        .expect("old logical date");
    let db_path = runtime.directory.path().join("quartz.db");

    let io = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("retention I/O runtime");
    io.block_on(async {
        let registry = services.start_registry();
        let mut subscriber =
            AnnotationClient::connect(services.clone(), registry.clone(), Role::Ui).await;
        subscriber
            .send(
                2,
                Op::Subscribe,
                serde_json::json!({"topics": [format!("day:{old_date}")]}),
            )
            .await;
        subscriber.success(2).await;
        let quartz_protocol::Envelope::Event(initial_day) = subscriber.receive().await else {
            panic!("initial day event");
        };
        assert_eq!(initial_day.topic, quartz_protocol::Topic::Day(old_date));
        let initial_day: quartz_protocol::DayChanged =
            serde_json::from_value(initial_day.body).expect("initial day version");

        let mut ui = AnnotationClient::connect(services.clone(), registry.clone(), Role::Ui).await;
        ui.send(2, Op::SettingsGet, serde_json::json!({})).await;
        let initial: SettingsResponse =
            serde_json::from_value(ui.success(2).await).expect("initial settings");
        assert_eq!(initial.title_retention, TitleRetention::Forever);

        // Explicit null is rejected.
        ui.send(
            3,
            Op::SettingsSet,
            serde_json::json!({"title_retention": null}),
        )
        .await;
        assert_eq!(
            ui.response(3).await,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::BadRequest)
            }
        );

        // Changing retention to 30d commits the setting, purges expired titles, and publishes day versions.
        ui.send(
            4,
            Op::SettingsSet,
            serde_json::json!({"title_retention": "30d"}),
        )
        .await;
        let updated: SettingsResponse =
            serde_json::from_value(ui.success(4).await).expect("updated retention");
        assert_eq!(updated.title_retention, TitleRetention::Days30);

        let quartz_protocol::Envelope::Event(changed_day) = subscriber.receive().await else {
            panic!("changed day event after title purge");
        };
        assert_eq!(changed_day.topic, quartz_protocol::Topic::Day(old_date));
        let changed_day: quartz_protocol::DayChanged =
            serde_json::from_value(changed_day.body).expect("changed day version");
        assert_ne!(
            changed_day.versions, initial_day.versions,
            "purge must publish updated day versions to subscribers"
        );

        let mut reader = ReadOnlyStore::open(&db_path).expect("reader");
        let old_facts = reader
            .dump_range(
                i64::try_from(old_start).expect("old start"),
                i64::try_from(old_start + 120_000).expect("old end"),
            )
            .expect("dump expired range");
        let expired_intervals: Vec<_> = old_facts
            .iter()
            .filter_map(|fact| match fact.record().expect("record").fields {
                FactFields::Foreground { interval } => Some(interval),
                _ => None,
            })
            .collect();
        assert_eq!(expired_intervals.len(), 1);
        assert_eq!(expired_intervals[0].title_status, FactTitleStatus::Purged);
        assert_eq!(expired_intervals[0].title, None);
        assert_eq!(
            reader.row_counts().expect("counts").titles,
            1,
            "only the retained open interval title should remain"
        );

        // Nonblocking rollover purge trigger executes cleanly on the database loop.
        runtime.workers.writes.request_title_purge();
        ui.send(5, Op::SettingsGet, serde_json::json!({})).await;
        let after_rollover: SettingsResponse =
            serde_json::from_value(ui.success(5).await).expect("settings after rollover purge");
        assert_eq!(after_rollover.title_retention, TitleRetention::Days30);

        ui.close().await;
        subscriber.close().await;
    });

    drop(services);
    let directory = runtime.shutdown();
    let mut reopened =
        Store::open(&DataDirectory::new(directory.path())).expect("reopen retention");
    assert_eq!(
        reopened.title_retention().expect("persisted retention"),
        TitleRetention::Days30
    );
}

/// Framed memory transport authenticating as `quartz-host.exe` (`AllowedRoles::Host`).
struct BrowserTestClient {
    frames:
        tokio_util::codec::Framed<tokio::io::DuplexStream, tokio_util::codec::LengthDelimitedCodec>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl BrowserTestClient {
    async fn connect(
        services: crate::ipc::Services,
        registry: crate::ipc::registry::RegistryHandle,
        browser: quartz_protocol::BrowserKind,
        profile_instance_id: quartz_protocol::Uuid,
    ) -> Self {
        let connection = crate::ipc::connection::Connection {
            registry,
            peer_pid: crate::ipc::ProcessId::new(std::process::id()),
            services,
            timeouts: crate::ipc::connection::ConnectionTimeouts::default(),
        };
        let (server, client) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(async move {
            let (read, write) = tokio::io::split(server);
            connection
                .run(
                    read,
                    write,
                    &mut crate::ipc::ConnectionSession::new(crate::ipc::AllowedRoles::Host),
                    tokio::time::Instant::now() + Duration::from_secs(5),
                )
                .await
        });
        let mut client = Self {
            frames: tokio_util::codec::Framed::new(client, quartz_protocol::frame_codec()),
            task,
        };
        client
            .send(
                1,
                quartz_protocol::Op::Hello,
                serde_json::json!({
                    "proto": {"min": 1, "max": 1},
                    "role": "browser",
                    "client_version": "test-host",
                    "browser": {
                        "browser": browser,
                        "profile_instance_id": profile_instance_id
                    }
                }),
            )
            .await;
        let body = client.success(1).await;
        let _: quartz_protocol::HelloResponse =
            serde_json::from_value(body).expect("typed browser hello response");
        client
    }

    async fn send(&mut self, id: u64, op: quartz_protocol::Op, body: serde_json::Value) {
        use futures_util::SinkExt;
        let request = quartz_protocol::Envelope::Request(quartz_protocol::Request {
            id: quartz_protocol::RequestId(id),
            op,
            body,
        });
        let bytes = quartz_protocol::encode_json(&request).expect("browser request JSON");
        self.frames
            .send(bytes.as_slice())
            .await
            .expect("send browser request");
    }

    async fn response(&mut self, id: u64) -> quartz_protocol::ResponseOutcome {
        use futures_util::StreamExt;
        let bytes = tokio::time::timeout(Duration::from_secs(5), self.frames.next())
            .await
            .expect("browser response deadline")
            .expect("browser response frame")
            .expect("browser frame decode");
        let quartz_protocol::Envelope::Response(response) =
            quartz_protocol::decode_json(&bytes).expect("browser response envelope")
        else {
            panic!("expected browser response envelope")
        };
        assert_eq!(response.id, Some(quartz_protocol::RequestId(id)));
        response.outcome
    }

    async fn success(&mut self, id: u64) -> serde_json::Value {
        let quartz_protocol::ResponseOutcome::Ok { body } = self.response(id).await else {
            panic!("expected browser success response")
        };
        body
    }

    async fn wait_closed(mut self) {
        use futures_util::StreamExt;
        let next = tokio::time::timeout(Duration::from_secs(5), self.frames.next())
            .await
            .expect("replaced browser connection closes promptly");
        assert!(next.is_none(), "expected server EOF, got {next:?}");
        drop(self.frames);
        self.task
            .await
            .expect("browser connection task")
            .expect("browser connection closed cleanly");
    }

    async fn close(self) {
        drop(self.frames);
        self.task
            .await
            .expect("browser connection task")
            .expect("browser connection closed");
    }
}

#[test]
fn browser_hello_persists_stream_lists_in_status_forwards_state_and_disconnect_enqueues_close() {
    use crate::core::app::{CaptureSender, MainCommand, Observation};
    use crate::ipc::browser::{BrowserInput, BrowserStreamKey};
    use quartz_protocol::{
        BrowserKind, BrowserStream, BrowserStreamId, CoreStatusResponse, Envelope, ErrorKind, Op,
        ProtocolError, ResponseOutcome, Role, StateSnapshot, Topic, Uuid,
    };

    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let status =
        crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("browser-integration".into()));
    let (capture_tx, capture_rx) = crossbeam_channel::bounded(16);
    let capture_sender = CaptureSender {
        life: Default::default(),
        sender: capture_tx,
        overloaded: Arc::new(AtomicBool::new(false)),
    };
    let services = crate::ipc::Services {
        summary: None,
        timed: crate::ipc::TimedPolicyPort::unavailable(),
        queries: runtime.workers.queries.clone(),
        writes: runtime.workers.writes.clone(),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
        browser_streams: status.browser_streams(),
        browser: capture_sender.browser_port(runtime.clock.clone()),
        ui: Arc::new(|_| Ok(())),
    };
    let profile: Uuid = "a286737e-3855-43cb-98ee-c366344372d0"
        .parse()
        .expect("profile uuid");
    let expected_key = BrowserStreamKey {
        browser: BrowserKind::Chrome,
        profile_instance_id: profile.clone(),
    };
    let expected_stream = BrowserStream {
        browser_stream: BrowserStreamId(profile.to_string()),
        browser: BrowserKind::Chrome,
    };
    let db_path = DataDirectory::new(runtime.directory.path()).database();

    let io = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("browser I/O runtime");
    io.block_on(async {
        let registry = services.start_registry();
        let mut ui = AnnotationClient::connect(services.clone(), registry.clone(), Role::Ui).await;
        let mut subscriber =
            AnnotationClient::connect(services.clone(), registry.clone(), Role::Ui).await;
        subscriber
            .send(2, Op::Subscribe, serde_json::json!({"topics": ["state"]}))
            .await;
        subscriber.success(2).await;
        let Envelope::Event(initial_event) = subscriber.receive().await else {
            panic!("initial state event")
        };
        assert_eq!(initial_event.topic, Topic::State);
        let initial_state: StateSnapshot =
            serde_json::from_value(initial_event.body).expect("initial state");
        assert!(initial_state.browser_streams.is_empty());

        let mut browser = BrowserTestClient::connect(
            services.clone(),
            registry.clone(),
            BrowserKind::Chrome,
            profile.clone(),
        )
        .await;

        let mut reader = crate::store::ReadOnlyStore::open(&db_path).expect("reader");
        let persisted = reader
            .browser_stream(&profile)
            .expect("query browser_streams")
            .expect("browser stream row created");
        assert_eq!(persisted.browser, "chrome");
        assert_eq!(
            persisted.extension_id,
            crate::store::browser::CHROMIUM_EXTENSION_ID
        );
        assert_eq!(persisted.first_seen_utc_ms, persisted.last_seen_utc_ms);

        let Envelope::Event(connected_event) = subscriber.receive().await else {
            panic!("connected state event")
        };
        assert_eq!(connected_event.topic, Topic::State);
        let connected_state: StateSnapshot =
            serde_json::from_value(connected_event.body).expect("connected state");
        assert_eq!(
            connected_state.browser_streams,
            vec![expected_stream.clone()]
        );

        ui.send(2, Op::CoreStatus, serde_json::json!({})).await;
        let core_status: CoreStatusResponse =
            serde_json::from_value(ui.success(2).await).expect("core status");
        assert_eq!(core_status.browser_streams, vec![expected_stream.clone()]);

        browser
            .send(
                2,
                Op::BrowserState,
                serde_json::json!({
                    "focused": true,
                    "window": {"id": 10, "state": "normal"},
                    "tab": {
                        "id": 99,
                        "domain": "docs.rs",
                        "title": "Quartz Docs",
                        "title_status": "ok",
                        "audible": false
                    }
                }),
            )
            .await;
        assert_eq!(
            browser.success(2).await,
            serde_json::json!({"accepted": true})
        );
        let queued = capture_rx.try_recv().expect("queued browser state");
        assert!(matches!(
            queued,
            Observation::Browser(BrowserInput::State {
                stream_id,
                ref key,
                ..
            }) if stream_id == persisted.id && key == &expected_key
        ));

        browser
            .send(
                3,
                Op::BrowserState,
                serde_json::json!({
                    "focused": true,
                    "window": null,
                    "tab": {
                        "id": 99,
                        "domain": "https://invalid.example",
                        "title": "Bad",
                        "title_status": "ok",
                        "audible": false
                    }
                }),
            )
            .await;
        assert_eq!(
            browser.response(3).await,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::BadRequest)
            }
        );

        browser.send(4, Op::CoreStatus, serde_json::json!({})).await;
        assert_eq!(
            browser.response(4).await,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::Forbidden)
            }
        );

        browser.close().await;
        registry.outboxes().await.expect("disconnect barrier");
        let closed = capture_rx.try_recv().expect("queued browser close");
        assert!(matches!(
            closed,
            Observation::Browser(BrowserInput::StreamClosed(ref notice))
                if notice.stream_id == persisted.id && notice.key == expected_key
        ));

        let Envelope::Event(disconnected_event) = subscriber.receive().await else {
            panic!("disconnected state event")
        };
        assert_eq!(disconnected_event.topic, Topic::State);
        let disconnected_state: StateSnapshot =
            serde_json::from_value(disconnected_event.body).expect("disconnected state");
        assert!(disconnected_state.browser_streams.is_empty());

        ui.send(3, Op::CoreStatus, serde_json::json!({})).await;
        let after_disconnect: CoreStatusResponse =
            serde_json::from_value(ui.success(3).await).expect("status after disconnect");
        assert!(after_disconnect.browser_streams.is_empty());

        runtime.clock.advance(5_000);
        let reconnected = BrowserTestClient::connect(
            services.clone(),
            registry.clone(),
            BrowserKind::Chrome,
            profile.clone(),
        )
        .await;
        let updated = reader
            .browser_stream(&profile)
            .expect("query updated browser_streams")
            .expect("browser stream row preserved");
        assert_eq!(updated.id, persisted.id);
        assert_eq!(updated.first_seen_utc_ms, persisted.first_seen_utc_ms);
        assert!(
            updated.last_seen_utc_ms > persisted.last_seen_utc_ms,
            "reconnect must advance last_seen_utc_ms"
        );

        reconnected.close().await;
        subscriber.close().await;
        ui.close().await;
    });
    drop(services);
    runtime.shutdown();
}

#[test]
fn browser_second_connection_for_same_profile_replaces_and_closes_first() {
    use crate::core::app::{CaptureSender, MainCommand, Observation};
    use crate::ipc::browser::{BrowserInput, BrowserStreamKey};
    use quartz_protocol::{
        BrowserKind, BrowserStream, BrowserStreamId, CoreStatusResponse, Op, Role, Uuid,
    };

    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let status =
        crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("browser-replacement".into()));
    let (capture_tx, capture_rx) = crossbeam_channel::bounded(16);
    let capture_sender = CaptureSender {
        life: Default::default(),
        sender: capture_tx,
        overloaded: Arc::new(AtomicBool::new(false)),
    };
    let services = crate::ipc::Services {
        summary: None,
        timed: crate::ipc::TimedPolicyPort::unavailable(),
        queries: runtime.workers.queries.clone(),
        writes: runtime.workers.writes.clone(),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
        browser_streams: status.browser_streams(),
        browser: capture_sender.browser_port(runtime.clock.clone()),
        ui: Arc::new(|_| Ok(())),
    };
    let profile: Uuid = "b286737e-3855-43cb-98ee-c366344372d1"
        .parse()
        .expect("profile uuid");
    let expected_key = BrowserStreamKey {
        browser: BrowserKind::Firefox,
        profile_instance_id: profile.clone(),
    };
    let db_path = DataDirectory::new(runtime.directory.path()).database();

    let io = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("browser replacement I/O runtime");
    io.block_on(async {
        let registry = services.start_registry();
        let mut ui = AnnotationClient::connect(services.clone(), registry.clone(), Role::Ui).await;

        let first = BrowserTestClient::connect(
            services.clone(),
            registry.clone(),
            BrowserKind::Firefox,
            profile.clone(),
        )
        .await;
        let mut reader = crate::store::ReadOnlyStore::open(&db_path).expect("reader");
        let row = reader
            .browser_stream(&profile)
            .expect("query firefox stream")
            .expect("firefox stream row");
        assert_eq!(row.browser, "firefox");
        assert_eq!(
            row.extension_id,
            crate::store::browser::FIREFOX_EXTENSION_ID
        );

        let mut second = BrowserTestClient::connect(
            services.clone(),
            registry.clone(),
            BrowserKind::Firefox,
            profile.clone(),
        )
        .await;
        first.wait_closed().await;
        registry.outboxes().await.expect("replacement barrier");

        let replaced_close = capture_rx
            .try_recv()
            .expect("replaced stream emits StreamClosed");
        assert!(matches!(
            replaced_close,
            Observation::Browser(BrowserInput::StreamClosed(ref notice))
                if notice.stream_id == row.id && notice.key == expected_key
        ));
        assert!(
            capture_rx.try_recv().is_err(),
            "old connection cleanup must not emit a duplicate StreamClosed"
        );

        ui.send(2, Op::CoreStatus, serde_json::json!({})).await;
        let status_body: CoreStatusResponse =
            serde_json::from_value(ui.success(2).await).expect("status after replacement");
        assert_eq!(
            status_body.browser_streams,
            vec![BrowserStream {
                browser_stream: BrowserStreamId(profile.to_string()),
                browser: BrowserKind::Firefox,
            }]
        );

        second
            .send(
                2,
                Op::BrowserState,
                serde_json::json!({"focused": false, "window": null, "tab": null}),
            )
            .await;
        assert_eq!(
            second.success(2).await,
            serde_json::json!({"accepted": true})
        );
        assert!(matches!(
            capture_rx.try_recv().expect("replacement browser state"),
            Observation::Browser(BrowserInput::State { stream_id, ref key, .. })
                if stream_id == row.id && key == &expected_key
        ));

        second.close().await;
        registry.outboxes().await.expect("final disconnect barrier");
        assert!(matches!(
            capture_rx.try_recv().expect("final stream close"),
            Observation::Browser(BrowserInput::StreamClosed(ref notice))
                if notice.stream_id == row.id && notice.key == expected_key
        ));

        ui.send(3, Op::CoreStatus, serde_json::json!({})).await;
        let final_status: CoreStatusResponse =
            serde_json::from_value(ui.success(3).await).expect("status after final close");
        assert!(final_status.browser_streams.is_empty());

        ui.close().await;
    });
    drop(services);
    runtime.shutdown();
}

#[test]
fn browser_state_and_matching_foreground_persist_web_interval_end_to_end() {
    use crate::ipc::browser::BrowserStreamKey as WireStreamKey;
    use quartz_protocol as wire;

    let directory = tempfile::tempdir().expect("isolated browser runtime");
    let mut store = Store::open(&DataDirectory::new(directory.path())).expect("open store");
    let profile: wire::Uuid = "a286737e-3855-43cb-98ee-c366344372d0"
        .parse()
        .expect("profile uuid");
    let stream_id = store
        .upsert_browser_stream(wire::BrowserKind::Chrome, &profile, at(0).utc)
        .expect("upsert browser stream");
    let clock = Arc::new(RuntimeClock::new());
    let workers = crate::core::app::Workers::start(
        store,
        metadata(),
        clock.clone(),
        Arc::new(|| {}),
        create_foreground(),
    )
    .expect("workers");
    let runtime = RuntimeFixture {
        directory,
        workers,
        clock,
    };
    let browser = runtime.workers.sender.browser_port(runtime.clock.clone());
    runtime.observe(CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())));
    runtime.clock.advance(100);
    runtime.observe(CaptureEvent::Foreground {
        source: ObservationSource::Review,
        foreground: Foreground::Window {
            facts: Box::new(WindowFacts {
                handle: WindowHandle::new(1),
                process: ProcessId::new(7),
                process_created_utc: Some(at(0).utc),
                class: "Chrome_WidgetWin_1".into(),
                app: AppIdentity::Exe("c:\\chrome.exe".into()),
                metadata_source: MetadataSource::Process,
                exe_name: Some("chrome.exe".into()),
                display_name: "Google Chrome".into(),
                title: Title::readable("Rust Docs - Google Chrome"),
            }),
            privacy: PrivacyContext::default(),
        },
    });
    runtime.clock.advance(100);
    browser
        .send_state(
            stream_id,
            WireStreamKey {
                browser: wire::BrowserKind::Chrome,
                profile_instance_id: profile,
            },
            wire::BrowserStateRequest {
                focused: wire::BrowserFocus::Focused,
                window: Some(wire::BrowserWindow {
                    id: 7,
                    state: wire::BrowserWindowState::Normal,
                }),
                tab: Some(wire::BrowserTab {
                    id: 91,
                    domain: Some("docs.rs".to_owned()),
                    title: "Rust Docs".to_owned(),
                    title_status: wire::BrowserTitleStatus::Ok,
                    audible: wire::BrowserAudible::Audible,
                }),
            },
        )
        .expect("enqueue browser state");
    runtime.clock.advance(100);
    let directory = runtime.shutdown();
    let mut inspection = Inspection::open(directory.path());
    let web_rows = inspection.web_intervals();
    assert_eq!(web_rows.len(), 1);
    let row = &web_rows[0];
    assert_eq!(row.stream_id, stream_id.get());
    assert_eq!(row.domain.as_deref(), Some("docs.rs"));
    assert_eq!(row.tab_title.as_deref(), Some("Rust Docs"));
    assert_eq!(row.tab_title_status, TitleStatus::Ok);
    assert_eq!(row.tab_id, Some(91));
    assert_eq!(row.browser_window_id, Some(7));
    assert_eq!(row.audible, 1);
    assert_eq!(row.link_status, 1);
    assert_eq!(row.web_level, WebLevel::Full.code());
    assert!(row.end_utc_ms.is_some());
    assert_eq!(row.confirmed_utc_ms, None);
    assert_eq!(row.end_reason, Some(WebEndReason::Shutdown.code()));
}
