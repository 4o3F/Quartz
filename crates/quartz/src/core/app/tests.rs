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
    let workers = Workers::start(fixture.store, metadata(), clock.clone(), Arc::new(|| {}))
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
        #[cfg(windows)]
        foreground: crate::platform::ForegroundResolver::new(),
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
