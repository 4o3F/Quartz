//! Capture work units and nonblocking submission to the shared command budget.

use super::timed::PublishTimedPolicies;
use super::{
    DatabaseCommand, Feedback, MainCommand, MainPort, Observation, ObservedEvent,
    SendMainCommandError, Shutdown,
};
use crate::core::{
    foreground::{CreateForeground, EventKind, ForegroundResolve, Resolution},
    sources::ClockReader,
};
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
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum RunCaptureLoopError {
    #[snafu(display("could not create foreground resolver"))]
    CreateResolver {
        source: Box<dyn std::error::Error + Send + Sync>,
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
/// Latest browser state retained per connected stream until M5-B2 adds linking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetainedBrowserState {
    // Read in M5-B2 when linking browser state into `web_intervals`.
    #[allow(dead_code)]
    pub stream_id: crate::store::browser::BrowserStreamRowId,
    #[allow(dead_code)]
    pub at: Timestamp,
    #[allow(dead_code)]
    pub state: quartz_protocol::BrowserStateRequest,
}

pub(super) struct CaptureLoop {
    pub core: CaptureCore,
    pub run: RunId,
    pub at: Timestamp,
    pub snapshots: SnapshotCell,
    pub observed: Receiver<Observation>,
    pub pending_control: Option<ObservedEvent>,
    pub discard_remaining: usize,
    pub browser_states: BTreeMap<crate::ipc::browser::BrowserStreamKey, RetainedBrowserState>,
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
            foreground: self.core.foreground_view(),
            timed: self.core.published_timed_policies(),
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
    pub(super) fn run(
        &mut self,
        create_foreground: CreateForeground,
    ) -> Result<(), RunCaptureLoopError> {
        let mut foreground =
            create_foreground().map_err(|source| RunCaptureLoopError::CreateResolver { source })?;
        loop {
            match self.next_work()? {
                CaptureWork::Shutdown(stop) => return self.shutdown(stop, foreground.as_mut()),
                CaptureWork::Observation(observation) => {
                    self.observe(observation, foreground.as_mut())?
                }
                CaptureWork::Feedback(feedback) => self.feedback(feedback)?,
            }
        }
    }
    fn next_work(&mut self) -> Result<CaptureWork, RunCaptureLoopError> {
        if let Ok(stop) = self.stop.try_recv() {
            return Ok(CaptureWork::Shutdown(stop));
        }
        self.drain_discarded();
        if let Some(event) = self.pending_control.take() {
            return Ok(CaptureWork::Observation(Observation::Event(event)));
        }
        select_biased! {
            recv(self.stop) -> stop => stop.map(CaptureWork::Shutdown).map_err(|_| RunCaptureLoopError::Receive { source: crossbeam_channel::RecvError }),
            recv(self.replies) -> reply => reply.map(CaptureWork::Feedback).map_err(|_| RunCaptureLoopError::Receive { source: crossbeam_channel::RecvError }),
            recv(self.observed) -> observation => observation.map(CaptureWork::Observation).map_err(|_| RunCaptureLoopError::Receive { source: crossbeam_channel::RecvError }),
        }
    }
    fn shutdown(
        &mut self,
        stop: Shutdown,
        foreground: &mut dyn ForegroundResolve,
    ) -> Result<(), RunCaptureLoopError> {
        self.drain_observations(foreground)?;
        self.step(
            ObservedEvent {
                at: match stop.at {
                    super::ShutdownTime::Sampled(at) => at,
                    super::ShutdownTime::LastObservation => self.at,
                },
                event: CaptureEvent::Shutdown,
            },
            Settlement::Final {
                reason: stop.reason,
                deadline: stop.deadline,
            },
        )?;
        self.wait_for_database(stop.deadline)
    }
    fn drain_observations(
        &mut self,
        foreground: &mut dyn ForegroundResolve,
    ) -> Result<(), RunCaptureLoopError> {
        loop {
            self.drain_discarded();
            if let Some(event) = self.pending_control.take() {
                self.step(event, Settlement::Deferred)?;
            } else if let Ok(observation) = self.observed.try_recv() {
                self.observe(observation, foreground)?;
            } else {
                break;
            }
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
    fn observe(
        &mut self,
        observation: Observation,
        foreground: &mut dyn ForegroundResolve,
    ) -> Result<(), RunCaptureLoopError> {
        if self.overloaded.swap(false, Ordering::AcqRel) {
            let observed_at = match &observation {
                Observation::Event(event) => event.at,
                Observation::Foreground(snapshot) => snapshot.at,
                Observation::Browser(input) => input.at(),
            };
            let at = if observed_at.mono < self.at.mono {
                self.at
            } else {
                observed_at
            };
            match observation {
                Observation::Event(event)
                    if matches!(
                        event.event,
                        CaptureEvent::SetTimedState(_) | CaptureEvent::CancelTimedPolicy(_)
                    ) =>
                {
                    self.pending_control = Some(event);
                }
                Observation::Browser(crate::ipc::browser::BrowserInput::StreamClosed(closed)) => {
                    self.browser_states.remove(&closed.key);
                }
                Observation::Event(_)
                | Observation::Foreground(_)
                | Observation::Browser(crate::ipc::browser::BrowserInput::State { .. }) => {}
            }
            self.discard_observations();
            self.step(
                ObservedEvent {
                    at,
                    event: CaptureEvent::EventOverload,
                },
                Settlement::Deferred,
            )
        } else {
            match observation {
                Observation::Event(event) => self.step(event, Settlement::Deferred),
                Observation::Foreground(snapshot) => {
                    let callback_at = snapshot.at.mono;
                    let kind = snapshot.kind;
                    let event_time_ms = snapshot.event_time_ms;
                    match foreground.resolve(snapshot) {
                        Resolution::Event(event) => {
                            self.step(event, Settlement::Deferred)?;
                            if let Some(event_time_ms) = event_time_ms {
                                self.trace_callback_latency(callback_at, kind, event_time_ms);
                            }
                            Ok(())
                        }
                        Resolution::Review => {
                            self.main
                                .send(MainCommand::Effect(CaptureEffect::ReviewForeground))?;
                            Ok(())
                        }
                    }
                }
                Observation::Browser(input) => {
                    self.apply_browser_input(input);
                    Ok(())
                }
            }
        }
    }
    fn apply_browser_input(&mut self, input: crate::ipc::browser::BrowserInput) {
        match input {
            crate::ipc::browser::BrowserInput::State {
                stream_id,
                key,
                at,
                state,
            } => {
                self.browser_states.insert(
                    key,
                    RetainedBrowserState {
                        stream_id,
                        at,
                        state,
                    },
                );
            }
            crate::ipc::browser::BrowserInput::StreamClosed(closed) => {
                self.browser_states.remove(&closed.key);
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
    fn discard_observations(&mut self) {
        self.discard_remaining = self.observed.len();
        self.drain_discarded();
    }
    fn drain_discarded(&mut self) {
        // Retain one control at a time; the remaining commands stay in the bounded ingress.
        while self.pending_control.is_none() && self.discard_remaining > 0 {
            self.discard_remaining -= 1;
            match self.observed.try_recv() {
                Ok(Observation::Event(event))
                    if matches!(
                        event.event,
                        CaptureEvent::SetTimedState(_) | CaptureEvent::CancelTimedPolicy(_)
                    ) =>
                {
                    self.pending_control = Some(event);
                }
                Ok(Observation::Browser(crate::ipc::browser::BrowserInput::StreamClosed(
                    closed,
                ))) => {
                    self.browser_states.remove(&closed.key);
                }
                Ok(_) | Err(_) => {}
            }
        }
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
        mut observation: ObservedEvent,
        settlement: Settlement,
    ) -> Result<(), RunCaptureLoopError> {
        // Preserve dequeue order across producers. Late work takes effect at the last
        // accepted clock pair; payload deadlines and input evidence remain unchanged.
        // Shutdown uses this same admission after draining queued observations.
        if observation.at.mono < self.at.mono {
            observation.at = self.at;
        }
        let calendar_change = match observation.event {
            CaptureEvent::ClockChanged(_) => Some(MainCommand::ClockChanged),
            _ => None,
        };
        let policy_changed = matches!(
            observation.event,
            CaptureEvent::PolicyChanged(_)
                | CaptureEvent::SetTimedState(_)
                | CaptureEvent::CancelTimedPolicy(_)
        );
        let followup = self.followup(&observation.event, observation.at);
        let event = self.normalize(observation.event);
        let step = self
            .core
            .step(CaptureInput {
                at: observation.at,
                event,
                rules: self.snapshots.load(),
            })
            .map_err(|source| RunCaptureLoopError::Step { source })?;
        self.at = observation.at;
        self.publish(step, observation.at, settlement)?;
        if let Some(command) = calendar_change {
            self.main.send(command)?;
        }
        if policy_changed {
            self.main.send(MainCommand::PolicyChanged)?;
        }
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
    fn followup(&self, event: &CaptureEvent, at: Timestamp) -> Option<CaptureEvent> {
        match event {
            CaptureEvent::Session(SessionStatus::Ready(_)) => {
                Some(CaptureEvent::RegisterDeadline {
                    owner: SchedulerKind::Idle,
                    deadline: Deadline::Mono(at.mono),
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
        if let Some(batch) = &parts.batch {
            for write in &batch.writes {
                use super::annotation::IdleAvailability;
                let change = match write {
                    CaptureWrite::OpenAvailability(row)
                        if row.reason == quartz_domain::capture::AvailabilityReason::Idle =>
                    {
                        Some(IdleAvailability::Opened {
                            id: row.id,
                            start: row.start,
                        })
                    }
                    CaptureWrite::CloseAvailability { id, at, reason } => {
                        Some(IdleAvailability::Closed {
                            id: *id,
                            at: *at,
                            reason: *reason,
                        })
                    }
                    _ => None,
                };
                if let Some(change) = change {
                    self.main.send(MainCommand::IdleAvailability(change))?;
                }
            }
        }
        if parts
            .effects
            .iter()
            .any(|effect| matches!(effect, CaptureEffect::StateChanged(_)))
            || parts.batch.as_ref().is_some_and(|batch| {
                batch.writes.iter().any(|write| {
                    matches!(
                        write,
                        CaptureWrite::OpenInterval(_)
                            | CaptureWrite::SetTimedState(_)
                            | CaptureWrite::ClearTimedPolicy(_)
                    )
                })
            })
        {
            self.main.send(MainCommand::CaptureSnapshot {
                state: self.core.state(),
                foreground: self.core.foreground_view(),
                timed: self.core.published_timed_policies(),
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
    fn publish_effect(&mut self, effect: CaptureEffect) -> Result<(), RunCaptureLoopError> {
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
            life: Default::default(),
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
        let channel = crate::core::app::MainChannel::new(limits::MAIN_QUEUE_CAPACITY);
        let main = channel.sender;
        let commands = channel.receiver;
        let mut capture = CaptureLoop {
            pending_control: None,
            discard_remaining: 0,
            browser_states: BTreeMap::new(),
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
            .observe(
                capture.observed.recv().expect("first stale event"),
                &mut super::super::tests::FakeResolver,
            )
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
        const CAPTURE_PROGRESS_WAIT: Duration = Duration::from_secs(2);
        let discarded_deadline = at(3);
        let fresh_deadline = at(4);
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
        let (database, busy_database) = bounded(CONTROL_CAPACITY);
        let channel = crate::core::app::MainChannel::new(limits::MAIN_QUEUE_CAPACITY);
        let main = channel.sender;
        let published = channel.receiver;
        let (reported, commands) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let before_discard = sender.clone();
        let rules = RuleRegistry::from_state(RegistryState::default())
            .expect("rules")
            .snapshots();
        let mut capture = CaptureLoop {
            pending_control: None,
            discard_remaining: 0,
            browser_states: BTreeMap::new(),
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
                wake: Arc::new(move || {
                    let command = published.try_recv().expect("published main command");
                    if matches!(
                        &command,
                        MainCommand::CaptureSnapshot {
                            state: CaptureState::Gap(GapReason::WriteQueueOverload),
                            ..
                        }
                    ) {
                        // A fast main-thread response to Gap still precedes queue cleanup.
                        before_discard
                            .send(Observation::Event(ObservedEvent {
                                at: at(2),
                                event: CaptureEvent::RegisterDeadline {
                                    owner: SchedulerKind::Midnight,
                                    deadline: Deadline::Utc(discarded_deadline.utc),
                                },
                            }))
                            .expect("observation before queue cleanup");
                    }
                    if matches!(
                        &command,
                        MainCommand::Effect(CaptureEffect::DiscardQueuedObservations)
                    ) {
                        assert!(
                            before_discard.is_empty(),
                            "cleanup discards the response queued after Gap"
                        );
                    }
                    reported.send(command).expect("main-thread observation");
                }),
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
                .and_then(|()| capture.run(super::super::tests::create_foreground()));
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
        let overflow_deadline = Instant::now() + CAPTURE_PROGRESS_WAIT;
        loop {
            if let MainCommand::CaptureSnapshot {
                state: CaptureState::Gap(reason),
                ..
            } = commands
                .recv_deadline(overflow_deadline)
                .expect("overflow completes without a database wait")
            {
                assert_eq!(reason, GapReason::WriteQueueOverload);
                break;
            }
        }
        let cleanup_deadline = Instant::now() + CAPTURE_PROGRESS_WAIT;
        loop {
            if let MainCommand::Effect(CaptureEffect::DiscardQueuedObservations) = commands
                .recv_deadline(cleanup_deadline)
                .expect("overflow finishes discarding queued observations")
            {
                break;
            }
        }
        assert_eq!(busy_database.len(), CONTROL_CAPACITY);
        sender
            .send(Observation::Event(ObservedEvent {
                at: at(2),
                event: CaptureEvent::RegisterDeadline {
                    owner: SchedulerKind::Midnight,
                    deadline: Deadline::Utc(fresh_deadline.utc),
                },
            }))
            .expect("fresh work after discarded observations");
        let processing_deadline = Instant::now() + CAPTURE_PROGRESS_WAIT;
        loop {
            if let MainCommand::Effect(CaptureEffect::ScheduleTimer(Timer::Arm(timer_at))) =
                commands.recv_deadline(processing_deadline).expect(
                    "capture registers fresh work while the database wakeup channel is full",
                )
            {
                assert_ne!(
                    timer_at, discarded_deadline.mono,
                    "the pre-cleanup observation was discarded"
                );
                if timer_at == fresh_deadline.mono {
                    break;
                }
            }
        }
        loop {
            if let MainCommand::Persistence(PersistenceState::Buffered { commands }) = commands
                .recv_deadline(processing_deadline)
                .expect("fresh deadline write is buffered without a database-capacity wait")
            {
                assert!(
                    commands >= 1,
                    "the fresh registration adds a buffered write"
                );
                break;
            }
        }
        assert_eq!(busy_database.len(), CONTROL_CAPACITY);
        shutdown
            .send(Shutdown::new(at(3), RunEndReason::Shutdown))
            .expect("shutdown");
        feedback
            .send(Feedback::Ended)
            .expect("scripted database completion");
        completion
            .recv_timeout(CAPTURE_PROGRESS_WAIT)
            .expect("capture exits without a database-capacity wait")
            .expect("capture result");
        worker.join().expect("capture worker");
    }
    #[test]
    fn overload_cleanup_preserves_admitted_controls_in_order_and_publishes_stopped_updates() {
        use crate::ipc::{TimedCommand, TimedDeadline};
        let run = RunId::new(1);
        let started = CaptureCore::start(run, at(0)).expect("core");
        let rules = RuleRegistry::from_state(RegistryState::default())
            .expect("rules")
            .snapshots();
        let (sender, observed) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let overloaded = Arc::new(AtomicBool::new(false));
        let ingress = CaptureSender {
            life: Default::default(),
            sender,
            overloaded: overloaded.clone(),
        };
        let port = ingress.timed_port(
            Arc::new(TestClock(at(10))),
            crate::query::calendar::DayStartOffset::MIDNIGHT,
        );
        port.call(TimedCommand::Pause(TimedDeadline::UtcMillis(100)))
            .expect("pause admitted");
        port.call(TimedCommand::Resume).expect("resume admitted");
        port.call(TimedCommand::EnablePrivacy(TimedDeadline::UtcMillis(200)))
            .expect("private admitted");
        let (_feedback, replies) = bounded(CONTROL_CAPACITY);
        let (_shutdown, stop) = bounded(CONTROL_CAPACITY);
        let (database, _writes) = bounded(CONTROL_CAPACITY);
        let channel = crate::core::app::MainChannel::new(limits::MAIN_QUEUE_CAPACITY);
        let main = channel.sender;
        let commands = channel.receiver;
        let mut capture = CaptureLoop {
            core: started.core,
            run,
            at: at(20),
            snapshots: rules,
            observed,
            pending_control: None,
            discard_remaining: 0,
            browser_states: BTreeMap::new(),
            replies,
            stop,
            database,
            writer: WriteQueue::new(),
            main: MainPort {
                sender: main,
                wake: Arc::new(|| {}),
            },
            overloaded: overloaded.clone(),
            clock: Arc::new(TestClock(at(20))),
        };
        overloaded.store(true, Ordering::Release);
        capture
            .observe(
                capture.observed.try_recv().expect("first admitted command"),
                &mut super::super::tests::FakeResolver,
            )
            .expect("overload");
        assert!(capture.pending_control.is_some());
        assert_eq!(capture.observed.len(), 2);
        // A second overload must retain the first command ahead of the remaining controls.
        overloaded.store(true, Ordering::Release);
        let CaptureWork::Observation(observation) = capture.next_work().expect("pending pause")
        else {
            panic!("expected pending control")
        };
        capture
            .observe(observation, &mut super::super::tests::FakeResolver)
            .expect("second overload");
        assert!(capture.pending_control.is_some());
        assert_eq!(capture.observed.len(), 2);
        let mut applied_policies = Vec::new();
        for _ in 0..3 {
            let CaptureWork::Observation(observation) =
                capture.next_work().expect("retained control")
            else {
                panic!("expected retained control")
            };
            capture
                .observe(observation, &mut super::super::tests::FakeResolver)
                .expect("apply retained control at current time");
            applied_policies.push(capture.core.published_timed_policies());
        }
        let policies = capture.core.published_timed_policies();
        assert_eq!(policies.pause_until_utc_ms, None);
        assert_eq!(
            policies
                .privacy_mode
                .expect("privacy retained")
                .until_utc_ms,
            200
        );
        assert!(capture.pending_control.is_none());
        let snapshots: Vec<_> = commands
            .try_iter()
            .filter_map(|command| match command {
                MainCommand::CaptureSnapshot { timed, .. } => Some(timed),
                _ => None,
            })
            .collect();
        assert!(
            applied_policies
                .iter()
                .any(|snapshot| snapshot.pause_until_utc_ms == Some(100))
        );
        assert_eq!(
            snapshots,
            [policies],
            "the main thread receives the latest replaceable state"
        );
    }
    #[test]
    fn cross_producer_observations_and_shutdown_use_the_accepted_boundary() {
        let run = RunId::new(1);
        let started = CaptureCore::start(run, at(0)).expect("core");
        let rules = RuleRegistry::from_state(RegistryState::default())
            .expect("rules")
            .snapshots();
        let (sender, observed) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let (feedback, replies) = bounded(CONTROL_CAPACITY);
        let (_shutdown, stop) = bounded(CONTROL_CAPACITY);
        let (database, _writes) = bounded(CONTROL_CAPACITY);
        let channel = crate::core::app::MainChannel::new(limits::MAIN_QUEUE_CAPACITY);
        let main = channel.sender;
        let _commands = channel.receiver;
        let mut capture = CaptureLoop {
            core: started.core,
            run,
            at: at(0),
            snapshots: rules,
            observed,
            pending_control: None,
            discard_remaining: 0,
            browser_states: BTreeMap::new(),
            replies,
            stop,
            database,
            writer: WriteQueue::new(),
            main: MainPort {
                sender: main,
                wake: Arc::new(|| {}),
            },
            overloaded: Arc::new(AtomicBool::new(false)),
            clock: Arc::new(TestClock(at(30))),
        };
        let mut resolver = super::super::tests::FakeResolver;
        capture
            .step(
                ObservedEvent {
                    at: at(20),
                    event: CaptureEvent::Wake,
                },
                Settlement::Deferred,
            )
            .expect("newer producer");
        capture
            .feedback(Feedback::RulesChanged)
            .expect("database feedback");
        capture
            .observe(
                Observation::Event(ObservedEvent {
                    at: at(10),
                    event: CaptureEvent::Session(SessionStatus::Ready(Default::default())),
                }),
                &mut resolver,
            )
            .expect("late session producer");
        capture
            .observe(
                Observation::Foreground(crate::core::foreground::Snapshot {
                    at: at(15),
                    read_at: at(15),
                    event_time_ms: None,
                    source: ObservationSource::Review,
                    kind: EventKind::Foreground,
                    window: WindowHandle::new(0),
                    title: Title::readable(""),
                    desktop: crate::core::foreground::Desktop::Observable,
                }),
                &mut resolver,
            )
            .expect("late foreground producer");
        assert_eq!(capture.at, at(20));
        sender
            .send(Observation::Event(ObservedEvent {
                at: at(30),
                event: CaptureEvent::Wake,
            }))
            .expect("queued observation newer than shutdown");
        sender
            .send(Observation::Event(ObservedEvent {
                at: at(25),
                event: CaptureEvent::Wake,
            }))
            .expect("queued late observation");
        feedback
            .send(Feedback::Ended)
            .expect("scripted database completion");
        capture
            .shutdown(Shutdown::new(at(22), RunEndReason::Shutdown), &mut resolver)
            .expect("drain observations and clamp sampled shutdown");
        assert_eq!(capture.at, at(30));
        assert_eq!(capture.core.state(), CaptureState::Ended);
        assert_eq!(
            capture.writer.shutdown().expect("final settlement").at,
            at(30)
        );
    }

    #[test]
    fn browser_state_retains_latest_per_stream_and_drops_on_stream_closed() {
        use crate::ipc::browser::{BrowserInput, BrowserStreamClosed, BrowserStreamKey};
        use crate::store::browser::BrowserStreamRowId;
        use quartz_protocol::{
            BrowserAudible, BrowserFocus, BrowserKind, BrowserStateRequest, BrowserTab,
            BrowserTitleStatus, BrowserWindow, BrowserWindowState, Uuid,
        };

        let run = RunId::new(1);
        let started = CaptureCore::start(run, at(0)).expect("core");
        let rules = RuleRegistry::from_state(RegistryState::default())
            .expect("rules")
            .snapshots();
        let (_sender, observed) = bounded(limits::MAIN_QUEUE_CAPACITY);
        let (_feedback, replies) = bounded(CONTROL_CAPACITY);
        let (_shutdown, stop) = bounded(CONTROL_CAPACITY);
        let (database, _writes) = bounded(CONTROL_CAPACITY);
        let channel = crate::core::app::MainChannel::new(limits::MAIN_QUEUE_CAPACITY);
        let mut capture = CaptureLoop {
            core: started.core,
            run,
            at: at(0),
            snapshots: rules,
            observed,
            pending_control: None,
            discard_remaining: 0,
            browser_states: BTreeMap::new(),
            replies,
            stop,
            database,
            writer: WriteQueue::new(),
            main: MainPort {
                sender: channel.sender,
                wake: Arc::new(|| {}),
            },
            overloaded: Arc::new(AtomicBool::new(false)),
            clock: Arc::new(TestClock(at(10))),
        };
        let key = BrowserStreamKey {
            browser: BrowserKind::Chrome,
            profile_instance_id: "a286737e-3855-43cb-98ee-c366344372d0"
                .parse::<Uuid>()
                .expect("uuid"),
        };
        let stream_id = BrowserStreamRowId::new(7);
        let first_state = BrowserStateRequest {
            focused: BrowserFocus::Focused,
            window: Some(BrowserWindow {
                id: 1,
                state: BrowserWindowState::Normal,
            }),
            tab: Some(BrowserTab {
                id: 10,
                domain: Some("docs.rs".to_owned()),
                title: "First".to_owned(),
                title_status: BrowserTitleStatus::Ok,
                audible: BrowserAudible::Silent,
            }),
        };
        let second_state = BrowserStateRequest {
            focused: BrowserFocus::Unfocused,
            window: None,
            tab: None,
        };
        let mut resolver = super::super::tests::FakeResolver;
        capture
            .observe(
                Observation::Browser(BrowserInput::State {
                    stream_id,
                    key: key.clone(),
                    at: at(10),
                    state: first_state,
                }),
                &mut resolver,
            )
            .expect("first browser state");
        capture
            .observe(
                Observation::Browser(BrowserInput::State {
                    stream_id,
                    key: key.clone(),
                    at: at(20),
                    state: second_state.clone(),
                }),
                &mut resolver,
            )
            .expect("second browser state replaces first");
        assert_eq!(capture.browser_states.len(), 1);
        let retained = capture.browser_states.get(&key).expect("retained stream");
        assert_eq!(retained.stream_id, stream_id);
        assert_eq!(retained.at, at(20));
        assert_eq!(retained.state, second_state);

        capture
            .observe(
                Observation::Browser(BrowserInput::StreamClosed(BrowserStreamClosed {
                    stream_id,
                    key: key.clone(),
                    at: at(30),
                })),
                &mut resolver,
            )
            .expect("stream closed drops retained state");
        assert!(capture.browser_states.is_empty());
    }
}
