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
fn shutdown_notifications_deliver_values_and_tolerate_closed_receivers() {
    let (sender, receiver) = bounded(CONTROL_CAPACITY);
    sender.send_during_shutdown(Worker::Capture);
    assert!(matches!(
        receiver.recv().expect("completion"),
        Worker::Capture
    ));
    drop(receiver);
    sender.send_during_shutdown(Worker::Database);

    let (sender, receiver) = bounded(CONTROL_CAPACITY);
    let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let main = MainPort {
        sender,
        wake: {
            let wakes = Arc::clone(&wakes);
            Arc::new(move || {
                wakes.fetch_add(1, Ordering::Relaxed);
            })
        },
    };
    main.send_during_shutdown(MainCommand::Fatal(Worker::Capture));
    assert!(matches!(
        receiver.recv().expect("fatal command"),
        MainCommand::Fatal(Worker::Capture)
    ));
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    drop(receiver);
    main.send_during_shutdown(MainCommand::Fatal(Worker::Database));
    assert_eq!(
        wakes.load(Ordering::Relaxed),
        1,
        "no wake after receiver teardown"
    );
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
    let (main, _commands) = bounded(limits::MAIN_QUEUE_CAPACITY);
    let snapshots = RuleRegistry::from_state(RegistryState::default())
        .expect("rules")
        .snapshots();
    let mut capture = CaptureLoop {
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
            runtime.ui_commands.recv().expect("UI request"),
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
fn core_runtime_drop_after_platform_start_failure_stops_pipe_before_workers() {
    run_runtime_test(|| {
        let directory = tempfile::tempdir().expect("isolated runtime database");
        let fixture = RuntimePortsFixture::new(StartTestPipe::Succeed);
        let runtime =
            super::CoreRuntime::start(&DataDirectory::new(directory.path()), fixture.ports)
                .expect("start core runtime");
        // A fallible platform step (such as Sources::start) returns while owning the runtime.
        drop(runtime);
        assert_eq!(
            fixture.log.events(),
            vec![
                LifecycleEvent::ResolverCreated,
                LifecycleEvent::PipeStopped,
                LifecycleEvent::ResolverDropped
            ]
        );
        assert_eq!(
            Inspection::open(directory.path()).run(1).end_reason,
            Some(RunEndReason::Shutdown)
        );
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

#[derive(Clone, Copy)]
enum ClockReadOutcome {
    Read,
    Fail,
}
struct FailOnceClock {
    clock: Arc<RuntimeClock>,
    caller: std::thread::ThreadId,
    next: std::sync::Mutex<ClockReadOutcome>,
}
impl ClockReader for FailOnceClock {
    fn read(&self) -> Result<Timestamp, ReadClockError> {
        if self.caller == std::thread::current().id() {
            let outcome = std::mem::replace(
                &mut *self.next.lock().expect("clock outcome"),
                ClockReadOutcome::Read,
            );
            if matches!(outcome, ClockReadOutcome::Fail) {
                return Err(ReadClockError::Interrupt {
                    source: Box::new(std::io::Error::from(std::io::ErrorKind::Interrupted)),
                });
            }
        }
        self.clock.read()
    }
}
#[test]
fn core_runtime_shutdown_clock_failure_still_settles_and_joins_workers() {
    run_runtime_test(|| {
        let directory = tempfile::tempdir().expect("isolated runtime database");
        let mut fixture = RuntimePortsFixture::new(StartTestPipe::Succeed);
        let clock = Arc::new(FailOnceClock {
            clock: Arc::clone(&fixture.clock),
            caller: std::thread::current().id(),
            next: std::sync::Mutex::new(ClockReadOutcome::Read),
        });
        fixture.ports.clock = clock.clone();
        let runtime =
            super::CoreRuntime::start(&DataDirectory::new(directory.path()), fixture.ports)
                .expect("start core runtime");
        *clock.next.lock().expect("clock outcome") = ClockReadOutcome::Fail;
        let error = runtime
            .shutdown(super::RuntimeExit::Shutdown)
            .expect_err("shutdown clock fails");
        assert!(matches!(error, super::StopCoreError::ReadClock { .. }));
        assert_eq!(
            fixture.log.events(),
            vec![
                LifecycleEvent::ResolverCreated,
                LifecycleEvent::PipeStopped,
                LifecycleEvent::ResolverDropped
            ]
        );
        let mut stored = Inspection::open(directory.path());
        assert_eq!(stored.run(1).end_reason, Some(RunEndReason::Shutdown));
        assert_eq!(stored.open_clock_count(), 0);
        drop(stored);
        std::fs::remove_dir_all(directory.path())
            .expect("clock failure leaves no open database handles");
    });
}

#[test]
fn io_write_replies_publish_committed_versions_and_rollback_keeps_versions() {
    use crate::persistence::dispatch::{TestCommit, WriteRequest};
    let runtime = RuntimeFixture::recovered();
    runtime.wait(|command| matches!(command, MainCommand::Persistence(PersistenceState::Ready)));
    let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("write-test".into()));
    let services = crate::ipc::Services {
        queries: runtime.workers.queries.clone(),
        writes: runtime.workers.writes.clone(),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
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
