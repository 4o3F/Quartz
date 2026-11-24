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

    let channel = MainChannel::new(CONTROL_CAPACITY);
    let sender = channel.sender;
    let receiver = channel.receiver;
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
        summary: None,
        timed: crate::ipc::TimedPolicyPort::unavailable(),
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
    use quartz_protocol::{
        EffectiveUiSettings, HourCycle, LocaleSettings, Op, Role, SettingsResponse, UiLanguage,
        Weekday,
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
    let mut channel = crate::ipc::UiCommands::channel(Arc::new(|_| Ok(())));
    let services = crate::ipc::Services {
        summary: None,
        timed: crate::ipc::TimedPolicyPort::unavailable(),
        queries: runtime.workers.queries.clone(),
        writes: runtime
            .workers
            .writes
            .clone()
            .with_settings(system, Arc::clone(&channel.port)),
        versions: runtime.workers.versions.clone(),
        status: status.subscribe(),
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
        for body in [serde_json::json!({"language":"fr"}), serde_json::json!({"hour_cycle":null}), serde_json::json!({"first_day_of_week":"yesterday"})] {
            ui.send(6, Op::SettingsSet, body).await;
            assert_eq!(ui.response(6).await, quartz_protocol::ResponseOutcome::Err { error: quartz_protocol::ProtocolError::new(quartz_protocol::ErrorKind::BadRequest) });
        }
        let mut cli = AnnotationClient::connect(services.clone(), registry.clone(), Role::Cli).await;
        for op in [Op::SettingsGet, Op::SettingsSet] {
            cli.send(2, op, serde_json::json!({})).await;
            let response: SettingsResponse = serde_json::from_value(cli.success(2).await).expect("CLI settings response");
            assert_eq!(response.ui, automatic.ui);
        }
        cli.send(7, Op::SettingsSet, serde_json::json!({"prewarm":{"enabled":false,"on_battery":true}})).await;
        let prewarm: SettingsResponse = serde_json::from_value(cli.success(7).await).expect("CLI prewarm update");
        assert_eq!(prewarm.ui.prewarm.enabled, quartz_protocol::PrewarmEnabled::Disabled);
        assert_eq!(prewarm.effective.prewarm.on_battery, quartz_protocol::PrewarmOnBattery::Allowed);
        let effective = channel.commands.take_settings_change().expect("committed prewarm notification");
        status.apply_ui_settings(effective, |_| {});
        assert_eq!(status.ui_settings().prewarm, prewarm.ui.prewarm);
        let quartz_protocol::Envelope::Event(event) = subscriber.receive().await else {
            panic!("state push after prewarm update");
        };
        let mut previous_state: quartz_protocol::StateSnapshot =
            serde_json::from_value(event.body).expect("prewarm state");
        assert_eq!(previous_state.ui_language, UiLanguage::English);
        for (request_id, preference, expected) in [
            (
                8,
                quartz_protocol::UiLanguageSetting::ZhCn,
                UiLanguage::SimplifiedChinese,
            ),
            (9, quartz_protocol::UiLanguageSetting::Auto, system.ui_language),
        ] {
            let update = quartz_protocol::SettingsSetRequest {
                language: Some(preference),
                ..Default::default()
            };
            cli.send(
                request_id,
                Op::SettingsSet,
                serde_json::to_value(update).expect("CLI language update"),
            ).await;
            let response: SettingsResponse = serde_json::from_value(cli.success(request_id).await)
                .expect("CLI language response");
            assert_eq!(response.ui.language, preference);
            assert_eq!(response.effective.ui_language, expected);
            let effective = channel.commands.take_settings_change()
                .expect("CLI language notification after commit");
            let mut tray_language = None;
            status.apply_ui_settings(effective, |language| tray_language = Some(language));
            assert_eq!(tray_language, Some(expected));
            let quartz_protocol::Envelope::Event(event) = subscriber.receive().await else {
                panic!("state push after CLI language update");
            };
            assert_eq!(event.topic, quartz_protocol::Topic::State);
            let state: quartz_protocol::StateSnapshot =
                serde_json::from_value(event.body).expect("CLI language state");
            previous_state.ui_language = expected;
            assert_eq!(
                state, previous_state,
                "language updates preserve other state fields"
            );
            assert_eq!(channel.commands.take_settings_change(), None);
        }
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
fn core_runtime_starts_with_stored_preferences_overriding_platform_values() {
    run_runtime_test(|| {
        let directory = tempfile::tempdir().expect("isolated startup settings");
        let data = DataDirectory::new(directory.path());
        let mut store = Store::open(&data).expect("store");
        let expected = store
            .update_ui_settings(quartz_protocol::SettingsSetRequest {
                resident: None,
                language: Some(quartz_protocol::UiLanguageSetting::ZhCn),
                hour_cycle: Some(quartz_protocol::HourCycleSetting::H12),
                first_day_of_week: Some(quartz_protocol::FirstDayOfWeekSetting::Sunday),
                prewarm: Some(quartz_protocol::PrewarmUpdate {
                    enabled: Some(quartz_protocol::PrewarmEnabled::Disabled),
                    on_battery: Some(quartz_protocol::PrewarmOnBattery::Allowed),
                }),
            })
            .expect("saved preferences");
        drop(store);
        let fixture = RuntimePortsFixture::new(StartTestPipe::Succeed);
        let runtime = super::CoreRuntime::start(&data, fixture.ports).expect("runtime");
        assert_eq!(
            runtime.status.ui_settings(),
            expected.resolve(quartz_protocol::EffectiveUiSettings {
                resident: Default::default(),
                prewarm: Default::default(),
                ui_language: quartz_protocol::UiLanguage::English,
                locale: None
            })
        );
        runtime
            .shutdown(super::RuntimeExit::UserExit)
            .expect("shutdown");
    });
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
