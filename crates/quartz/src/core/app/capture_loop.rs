//! Capture work units and nonblocking submission to the shared command budget.

use super::{
    DatabaseCommand, Feedback, MainCommand, MainPort, Observation, ObservedEvent,
    SendMainCommandError, Shutdown,
};
use crate::core::{foreground::EventKind, sources::ClockReader};
use crate::persistence::WriteQueue;
use crate::store::capture::CaptureStartup;
use crossbeam_channel::{Receiver, Sender, TrySendError, select_biased};
use quartz_domain::{
    capture::{
        CaptureCore, CaptureEffect, CaptureEvent, CaptureInput, CaptureParts, CaptureState,
        CaptureStep, CaptureWrite, ClockChange, Deadline, MonoInstant, PolicyObservation,
        RunEndReason, RunId, SchedulerKind, SessionStatus, StepCaptureError, StopChange,
        StopReason, Timestamp,
    },
    rules::registry::SnapshotCell,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum RunCaptureLoopError {
    #[cfg(windows)]
    #[snafu(display("could not initialize capture worker COM"))]
    InitializeCom {
        source: crate::platform::InitializeCaptureComError,
    },
    #[snafu(display("capture input channel disconnected"))]
    Receive {
        source: crossbeam_channel::RecvError,
    },
    #[snafu(display("could not publish capture effects"))]
    Publish { source: SendMainCommandError },
    #[snafu(display("could not advance capture"))]
    Step { source: StepCaptureError },
    #[snafu(display("could not notify database"))]
    Notify,
    #[snafu(display("database ended before capture shutdown"))]
    UnexpectedDatabaseEnd,
    #[snafu(display("could not await database shutdown"))]
    Shutdown {
        source: crossbeam_channel::RecvTimeoutError,
    },
}
impl From<SendMainCommandError> for RunCaptureLoopError {
    fn from(source: SendMainCommandError) -> Self {
        Self::Publish { source }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Settlement {
    Deferred,
    Final {
        reason: RunEndReason,
        deadline: Instant,
    },
}
enum CaptureWork {
    Shutdown(Shutdown),
    Observation(Observation),
    Feedback(Feedback),
}
pub(super) struct CaptureLoop {
    #[cfg(windows)]
    pub foreground: crate::platform::ForegroundResolver,
    pub core: CaptureCore,
    pub run: RunId,
    pub at: Timestamp,
    pub snapshots: SnapshotCell,
    pub observed: Receiver<Observation>,
    pub replies: Receiver<Feedback>,
    pub stop: Receiver<Shutdown>,
    pub database: Sender<DatabaseCommand>,
    pub writer: WriteQueue,
    pub main: MainPort,
    pub overloaded: Arc<AtomicBool>,
    pub clock: Arc<dyn ClockReader>,
}
impl CaptureLoop {
    pub(super) fn initialize(
        &mut self,
        step: CaptureStep,
        startup: CaptureStartup,
    ) -> Result<(), RunCaptureLoopError> {
        self.main.send(MainCommand::CaptureSnapshot {
            state: self.core.state(),
            interval: self.core.current_interval(),
        })?;
        self.publish(step, self.at, Settlement::Deferred)?;
        for deadline in startup.deadlines {
            let event = CaptureEvent::RegisterDeadline {
                owner: deadline.owner,
                deadline: Deadline::Utc(deadline.at),
            };
            self.step(ObservedEvent { at: self.at, event }, Settlement::Deferred)?;
        }
        for state in startup.policies {
            self.step(
                ObservedEvent {
                    at: self.at,
                    event: CaptureEvent::RestoreTimedState(state),
                },
                Settlement::Deferred,
            )?;
        }
        Ok(())
    }
    pub(super) fn run(&mut self) -> Result<(), RunCaptureLoopError> {
        #[cfg(windows)]
        let _apartment = crate::platform::CaptureApartment::initialize()
            .map_err(|source| RunCaptureLoopError::InitializeCom { source })?;
        loop {
            match self.next_work()? {
                CaptureWork::Shutdown(stop) => return self.shutdown(stop),
                CaptureWork::Observation(observation) => self.observe(observation)?,
                CaptureWork::Feedback(feedback) => self.feedback(feedback)?,
            }
        }
    }
    fn next_work(&self) -> Result<CaptureWork, RunCaptureLoopError> {
        select_biased! {
            recv(self.stop) -> stop => stop.map(CaptureWork::Shutdown).map_err(|_| RunCaptureLoopError::Receive { source: crossbeam_channel::RecvError }),
            recv(self.replies) -> reply => reply.map(CaptureWork::Feedback).map_err(|_| RunCaptureLoopError::Receive { source: crossbeam_channel::RecvError }),
            recv(self.observed) -> observation => observation.map(CaptureWork::Observation).map_err(|_| RunCaptureLoopError::Receive { source: crossbeam_channel::RecvError }),
        }
    }
    fn shutdown(&mut self, stop: Shutdown) -> Result<(), RunCaptureLoopError> {
        self.drain_observations()?;
        self.step(
            ObservedEvent {
                at: stop.at,
                event: CaptureEvent::Shutdown,
            },
            Settlement::Final {
                reason: stop.reason,
                deadline: stop.deadline,
            },
        )?;
        self.wait_for_database(stop.deadline)
    }
    fn drain_observations(&mut self) -> Result<(), RunCaptureLoopError> {
        while let Ok(observation) = self.observed.try_recv() {
            self.observe(observation)?;
        }
        Ok(())
    }
    fn wait_for_database(&self, deadline: Instant) -> Result<(), RunCaptureLoopError> {
        loop {
            match self
                .replies
                .recv_deadline(deadline)
                .map_err(|source| RunCaptureLoopError::Shutdown { source })?
            {
                Feedback::Ended => return Ok(()),
                Feedback::Events(_) | Feedback::RulesChanged => {}
            }
        }
    }
    fn observe(&mut self, observation: Observation) -> Result<(), RunCaptureLoopError> {
        if self.overloaded.swap(false, Ordering::AcqRel) {
            self.discard_observations();
            self.step(
                ObservedEvent {
                    at: match &observation {
                        Observation::Event(event) => event.at,
                        #[cfg(windows)]
                        Observation::Foreground(snapshot) => snapshot.at,
                    },
                    event: CaptureEvent::EventOverload,
                },
                Settlement::Deferred,
            )
        } else {
            match observation {
                Observation::Event(event) => self.step(event, Settlement::Deferred),
                #[cfg(windows)]
                Observation::Foreground(snapshot) => {
                    let callback_at = snapshot.at.mono;
                    let kind = snapshot.kind;
                    let event_time_ms = snapshot.event_time_ms;
                    match self.foreground.resolve(snapshot) {
                        crate::platform::ForegroundResolution::Event(event) => {
                            self.step(event, Settlement::Deferred)?;
                            if let Some(event_time_ms) = event_time_ms {
                                self.trace_callback_latency(callback_at, kind, event_time_ms);
                            }
                            Ok(())
                        }
                        crate::platform::ForegroundResolution::Review => {
                            self.main
                                .send(MainCommand::Effect(CaptureEffect::ReviewForeground))?;
                            Ok(())
                        }
                    }
                }
            }
        }
    }
    fn trace_callback_latency(
        &self,
        callback_at: MonoInstant,
        kind: EventKind,
        event_time_ms: u32,
    ) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }
        let completed = match self.clock.read() {
            Ok(at) => at.mono,
            Err(error) => {
                tracing::debug!(%error, "could not sample foreground completion clock");
                return;
            }
        };
        match completed.duration_since(callback_at) {
            Ok(elapsed) => tracing::debug!(
                ?kind,
                event_time_ms,
                callback_to_fact_ms = %elapsed.as_millis(),
                "foreground callback processed"
            ),
            Err(error) => tracing::debug!(%error, "could not measure foreground callback latency"),
        }
    }
    fn discard_observations(&self) {
        while self.observed.try_recv().is_ok() {}
    }
    fn feedback(&mut self, feedback: Feedback) -> Result<(), RunCaptureLoopError> {
        let events = match feedback {
            Feedback::Events(events) => events,
            Feedback::RulesChanged => vec![CaptureEvent::PolicyChanged(PolicyObservation::Refresh)],
            Feedback::Ended => return Err(RunCaptureLoopError::UnexpectedDatabaseEnd),
        };
        for event in events {
            self.step(ObservedEvent { at: self.at, event }, Settlement::Deferred)?;
        }
        Ok(())
    }
    pub(super) fn step(
        &mut self,
        observation: ObservedEvent,
        settlement: Settlement,
    ) -> Result<(), RunCaptureLoopError> {
        self.at = observation.at;
        let followup = self.followup(&observation.event);
        let event = self.normalize(observation.event);
        let step = self
            .core
            .step(CaptureInput {
                at: observation.at,
                event,
                rules: self.snapshots.load(),
            })
            .map_err(|source| RunCaptureLoopError::Step { source })?;
        self.publish(step, observation.at, settlement)?;
        if let Some(event) = followup {
            self.step(
                ObservedEvent {
                    at: observation.at,
                    event,
                },
                Settlement::Deferred,
            )?;
        }
        Ok(())
    }
    fn followup(&self, event: &CaptureEvent) -> Option<CaptureEvent> {
        match event {
            CaptureEvent::Session(SessionStatus::Ready(_)) => {
                Some(CaptureEvent::RegisterDeadline {
                    owner: SchedulerKind::Idle,
                    deadline: Deadline::Mono(self.at.mono),
                })
            }
            _ => None,
        }
    }
    fn normalize(&self, event: CaptureEvent) -> CaptureEvent {
        match event {
            // Ending suspension already creates the resume clock segment in CaptureCore.
            CaptureEvent::ClockChanged(ClockChange::Resume) if matches!(self.core.state(), CaptureState::Stopped { ref reasons, .. } if reasons.contains(&StopReason::Suspended)) => {
                CaptureEvent::Stop(StopChange::End(StopReason::Suspended))
            }
            event => event,
        }
    }
    fn notify_database(&self) -> Result<(), RunCaptureLoopError> {
        match self.database.try_send(DatabaseCommand::Drain) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => Err(RunCaptureLoopError::Notify),
        }
    }
    fn publish(
        &mut self,
        step: CaptureStep,
        at: Timestamp,
        settlement: Settlement,
    ) -> Result<(), RunCaptureLoopError> {
        let mut parts = step.into_parts(self.run);
        if parts
            .effects
            .iter()
            .any(|effect| matches!(effect, CaptureEffect::StateChanged(_)))
            || parts.batch.as_ref().is_some_and(|batch| {
                batch
                    .writes
                    .iter()
                    .any(|write| matches!(write, CaptureWrite::OpenInterval(_)))
            })
        {
            self.main.send(MainCommand::CaptureSnapshot {
                state: self.core.state(),
                interval: self.core.current_interval(),
            })?;
        }
        for effect in std::mem::take(&mut parts.effects) {
            self.publish_effect(effect)?;
        }
        Self::set_end_reason(&mut parts, settlement);
        if parts.batch.is_some() {
            self.submit(parts, at)?;
        }
        if let Settlement::Final { deadline, .. } = settlement {
            self.writer
                .finish(crate::persistence::QueueShutdown { at, deadline });
            self.notify_database()?;
        }
        Ok(())
    }
    fn publish_effect(&self, effect: CaptureEffect) -> Result<(), RunCaptureLoopError> {
        if matches!(effect, CaptureEffect::StateChanged(_)) {
            return Ok(());
        }
        if effect == CaptureEffect::DiscardQueuedObservations {
            self.discard_observations();
        }
        self.main
            .send(MainCommand::Effect(effect))
            .map_err(RunCaptureLoopError::from)
    }
    fn set_end_reason(parts: &mut CaptureParts, settlement: Settlement) {
        if let Settlement::Final { reason, .. } = settlement
            && let Some(batch) = &mut parts.batch
        {
            for write in &mut batch.writes {
                if let CaptureWrite::CloseRun { reason: end, .. } = write {
                    *end = reason;
                }
            }
        }
    }
    fn submit(&mut self, parts: CaptureParts, at: Timestamp) -> Result<(), RunCaptureLoopError> {
        let result = self.writer.accept(parts, at);
        self.main.send(MainCommand::Persistence(result.state()))?;
        self.notify_database()?;
        self.feedback(Feedback::Events(result.feedback))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::app::{CONTROL_CAPACITY, CaptureSender};
    use crate::core::sources::ReadClockError;
    use crate::persistence::PersistenceState;
    use crossbeam_channel::bounded;
    use quartz_domain::capture::*;
    use quartz_domain::rules::registry::{RegistryState, RuleRegistry};
    use std::{thread, time::Duration};

    struct TestClock(Timestamp);
    impl ClockReader for TestClock {
        fn read(&self) -> Result<Timestamp, ReadClockError> {
            Ok(self.0)
        }
    }

    fn at(millis: u64) -> Timestamp {
        Timestamp {
            utc: UtcInstant::try_from(i64::try_from(millis).expect("test time")).expect("UTC"),
            mono: MonoInstant::from_millis(millis),
        }
    }
    #[test]
    fn main_queue_overload_discards_stale_events_and_keeps_exact_gap_until_review() {
        let run = RunId::new(1);
        let mut started = CaptureCore::start(run, at(0)).expect("core");
        let rules = RuleRegistry::from_state(RegistryState::default())
            .expect("rules")
            .snapshots();
        for event in [
            CaptureEvent::Session(SessionStatus::Ready(Default::default())),
            CaptureEvent::Foreground {
                source: ObservationSource::Review,
                foreground: Foreground::None(NoForegroundReason::Empty),
            },
        ] {
            // Seed the core's trusted foreground state; this test examines later overload writes.
            let _ = started
                .core
                .step(CaptureInput {
                    at: at(10),
                    rules: rules.load(),
                    event,
                })
                .expect("trusted foreground");
        }
        let (sender, observed) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let overloaded = Arc::new(AtomicBool::new(false));
        let ingress = CaptureSender {
            sender,
            overloaded: Arc::clone(&overloaded),
        };
        let (done, completion) = bounded(CONTROL_CAPACITY);
        let producer = thread::spawn(move || {
            for _ in 0..=limits::MAIN_QUEUE_CAPACITY {
                ingress
                    .observe(ObservedEvent {
                        at: at(20),
                        event: CaptureEvent::Wake,
                    })
                    .expect("nonblocking observation");
            }
            done.send(()).expect("producer completion");
        });
        completion
            .recv_timeout(Duration::from_secs(2))
            .expect("full queue cannot block the main thread");
        producer.join().expect("producer");
        assert_eq!(observed.len(), limits::MAIN_QUEUE_CAPACITY);
        assert!(overloaded.load(Ordering::Acquire));
        let (_feedback, replies) = bounded(CONTROL_CAPACITY);
        let (_shutdown, stop) = bounded(CONTROL_CAPACITY);
        let (database, _writes) = bounded(CONTROL_CAPACITY);
        let (main, commands) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let mut capture = CaptureLoop {
            #[cfg(windows)]
            foreground: crate::platform::ForegroundResolver::new(),
            core: started.core,
            run,
            at: at(10),
            snapshots: rules.clone(),
            observed,
            replies,
            stop,
            database,
            writer: WriteQueue::new(),
            main: MainPort {
                sender: main,
                wake: Arc::new(|| {}),
            },
            overloaded,
            clock: Arc::new(TestClock(at(30))),
        };
        capture
            .observe(capture.observed.recv().expect("first stale event"))
            .expect("consume overload");
        assert!(
            capture.observed.is_empty(),
            "queued events lose trust together"
        );
        assert!(!capture.overloaded.load(Ordering::Acquire));
        assert_eq!(
            capture.core.state(),
            CaptureState::Gap(GapReason::EventOverload)
        );
        let commands: Vec<_> = commands.try_iter().collect();
        assert!(commands.iter().any(|command| matches!(
            command,
            MainCommand::Effect(CaptureEffect::ReviewForeground)
        )));
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(
                    command,
                    MainCommand::CaptureSnapshot {
                        state: CaptureState::Gap(GapReason::EventOverload),
                        ..
                    }
                ))
                .count(),
            1,
            "one main-thread snapshot per state change"
        );
        assert!(
            !commands.iter().any(|command| matches!(
                command,
                MainCommand::Effect(CaptureEffect::StateChanged(_))
            )),
            "state changes use CaptureSnapshot exclusively"
        );
        let review = capture
            .core
            .step(CaptureInput {
                at: at(30),
                rules: rules.load(),
                event: CaptureEvent::Foreground {
                    source: ObservationSource::Review,
                    foreground: Foreground::None(NoForegroundReason::Empty),
                },
            })
            .expect("fresh foreground review");
        assert!(review.writes.iter().any(|write| matches!(write,
            CaptureWrite::RecordGap(gap)
                if gap.reason == GapReason::EventOverload
                    // The overload checkpoint already settled the gap through at(20).
                    && gap.range == TimeRange { start: at(20), end: at(30) }
        )));
        assert!(review.writes.iter().any(|write| matches!(write,
            CaptureWrite::OpenInterval(row) if row.start == at(30)
        )));
    }
    #[test]
    fn capture_loop_continues_after_command_overflow_with_database_wakeup_channel_full() {
        let run = RunId::new(1);
        let mut started = CaptureCore::start(run, at(0)).expect("core");
        let extra = limits::WRITE_QUEUE_CAPACITY - started.step.writes.len();
        started.step.writes.extend(vec![
            CaptureWrite::RetireWindow {
                instance: WindowId::from_persisted(99),
                at: at(0)
            };
            extra
        ]);
        let (sender, observed) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let (feedback, replies) = bounded(CONTROL_CAPACITY);
        let (shutdown, stop) = bounded(CONTROL_CAPACITY);
        // Keep this receiver alive without reading it, like a database busy with SQLite.
        let (database, _busy_database) = bounded(CONTROL_CAPACITY);
        let (main, commands) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let rules = RuleRegistry::from_state(RegistryState::default())
            .expect("rules")
            .snapshots();
        let mut capture = CaptureLoop {
            #[cfg(windows)]
            foreground: crate::platform::ForegroundResolver::new(),
            core: started.core,
            run,
            at: at(0),
            snapshots: rules,
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
            clock: Arc::new(TestClock(at(0))),
        };
        let (finished, completion) = bounded(CONTROL_CAPACITY);
        let worker = thread::spawn(move || {
            let result = capture
                .initialize(
                    started.step,
                    CaptureStartup {
                        next_run: run,
                        last_segment: None,
                        deadlines: Vec::new(),
                        policies: Vec::new(),
                    },
                )
                .and_then(|()| capture.run());
            finished.send(result).expect("capture result");
        });
        sender
            .send(Observation::Event(ObservedEvent {
                at: at(0),
                event: CaptureEvent::Session(SessionStatus::Ready(Default::default())),
            }))
            .expect("session");
        sender
            .send(Observation::Event(ObservedEvent {
                at: at(1),
                event: CaptureEvent::Foreground {
                    source: ObservationSource::Review,
                    foreground: Foreground::None(NoForegroundReason::Empty),
                },
            }))
            .expect("overflow boundary");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let MainCommand::CaptureSnapshot {
                state: CaptureState::Gap(reason),
                ..
            } = commands
                .recv_deadline(deadline)
                .expect("overflow completes without a database wait")
            {
                assert_eq!(reason, GapReason::WriteQueueOverload);
                break;
            }
        }
        sender
            .send(Observation::Event(ObservedEvent {
                at: at(2),
                event: CaptureEvent::RegisterDeadline {
                    owner: SchedulerKind::Midnight,
                    deadline: Deadline::Utc(at(86_400_000).utc),
                },
            }))
            .expect("fresh work after discarded observations");
        loop {
            if let MainCommand::Persistence(PersistenceState::Buffered { commands: 1 }) = commands
                .recv_deadline(deadline)
                .expect("capture continues while the database wakeup channel is full")
            {
                break;
            }
        }
        shutdown
            .send(Shutdown::new(at(3), RunEndReason::Shutdown))
            .expect("shutdown");
        feedback
            .send(Feedback::Ended)
            .expect("scripted database completion");
        completion
            .recv_deadline(deadline)
            .expect("capture exits without a database-capacity wait")
            .expect("capture result");
        worker.join().expect("capture worker");
    }
}
