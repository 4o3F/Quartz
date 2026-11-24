//! Database actor; each continuation requires observable flush progress.

use super::{
    COMMIT_HEADROOM, DatabaseCommand, Feedback, MainCommand, MainPort, SendDuringShutdown,
    SendMainCommandError,
};
use crate::{
    core::sources::{ClockReader, ReadClockError},
    persistence::{
        CheckpointCollection, FlushProgress, FlushStalledError, Persistence, PersistenceOutcome,
        PersistenceResult, PersistenceState, QueueShutdown,
    },
    rules::registry::{RegistryResult, RegistryShell, RunRegistryCommandError},
    store::Store,
};
use crossbeam_channel::{Receiver, Sender};
use quartz_domain::{
    capture::{AdvanceTimeError, MonoInstant, Timestamp},
    rules::registry::RegistryCommand,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum RunDatabaseLoopError {
    #[snafu(display("could not refresh committed category references"))]
    RefreshReferences {
        source: crate::store::rules::LoadRulesError,
    },
    #[snafu(display("could not read database worker clock"))]
    Clock { source: ReadClockError },
    #[snafu(display("database command channel disconnected"))]
    Receive,
    #[snafu(display("could not publish database state"))]
    Publish { source: SendMainCommandError },
    #[snafu(display("capture feedback channel disconnected"))]
    Feedback,
    #[snafu(display("{source}"))]
    Stalled { source: FlushStalledError },
    #[snafu(display("query connection could not be returned to the database loop"))]
    Query { source: crate::query::RunQueryError },
    #[snafu(display("could not shut down database worker"))]
    Shutdown { source: Box<ShutdownDatabaseError> },
}
impl From<SendMainCommandError> for RunDatabaseLoopError {
    fn from(source: SendMainCommandError) -> Self {
        Self::Publish { source }
    }
}
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum ShutdownDatabaseError {
    #[snafu(display("database shutdown exceeded its deadline"))]
    Deadline,
    #[snafu(display("could not read shutdown clock"))]
    Clock { source: ReadClockError },
    #[snafu(display("shutdown clock precedes the observed shutdown event"))]
    Time { source: AdvanceTimeError },
    #[snafu(display("could not publish shutdown state"))]
    Publish { source: SendMainCommandError },
    #[snafu(display("database final transaction failed"))]
    Flush {
        source: crate::store::capture::ExecuteCaptureBatchError,
    },
    #[snafu(display("database retained failed writes at shutdown"))]
    RetainedWrites,
    #[snafu(display("{source}"))]
    Stalled { source: FlushStalledError },
    #[snafu(display("could not acknowledge database shutdown"))]
    Feedback,
}
impl From<SendMainCommandError> for ShutdownDatabaseError {
    fn from(source: SendMainCommandError) -> Self {
        Self::Publish { source }
    }
}

pub(super) struct DatabaseLoop {
    // Release any active read snapshot before closing the writer connection.
    pub runner: crate::query::runner::QueryRunner,
    pub store: Store,
    pub registry: RegistryShell,
    pub persistence: Persistence,
    pub writes: Receiver<DatabaseCommand>,
    pub user_writes: Receiver<crate::persistence::dispatch::WriteCommand>,
    pub queries: Receiver<crate::query::dispatch::QueryCommand>,
    pub versions: crate::query::versions::VersionPublisher,
    pub feedback: Sender<Feedback>,
    pub main: MainPort,
    pub clock: Arc<dyn ClockReader>,
}
enum DatabaseInput {
    Write(Option<DatabaseCommand>),
    Query(crate::query::dispatch::QueryCommand),
    UserWrite(crate::persistence::dispatch::WriteCommand),
    Versions(crate::query::versions::VersionCommand),
}
enum DatabaseProgress {
    Continue,
    Ended,
}
enum DrainProgress {
    Drained,
    Waiting,
}
impl DatabaseLoop {
    fn publish_versions(&mut self) -> Result<(), RunDatabaseLoopError> {
        if self.store.has_committed_metadata() {
            self.registry
                .refresh_category_references(&mut self.store)
                .map_err(|source| RunDatabaseLoopError::RefreshReferences { source })?;
        }
        if let Some(offset) = self
            .versions
            .publish(&mut self.store)
            .map_err(|source| RunDatabaseLoopError::Query { source })?
        {
            self.main.send(MainCommand::DayOffsetChanged { offset })?;
        }
        Ok(())
    }
    fn checkpoint_collection(&self) -> CheckpointCollection {
        if tracing::enabled!(tracing::Level::DEBUG) {
            CheckpointCollection::Collect
        } else {
            CheckpointCollection::Skip
        }
    }
    fn now(&self) -> Result<Timestamp, ReadClockError> {
        self.clock.read()
    }
    fn publish(&mut self, result: PersistenceResult) -> Result<(), RunDatabaseLoopError> {
        self.trace_commit_latency(&result);
        self.publish_versions()?;
        if let Some(error) = result.failure() {
            tracing::error!(%error, "capture write failed");
        }
        self.main.send(MainCommand::Persistence(result.state()))?;
        if !result.feedback.is_empty() {
            self.feedback
                .send(Feedback::Events(result.feedback))
                .map_err(|_| RunDatabaseLoopError::Feedback)?;
        }
        Ok(())
    }
    fn trace_commit_latency(&self, result: &PersistenceResult) {
        if !tracing::enabled!(tracing::Level::DEBUG) || result.checkpoints.is_empty() {
            return;
        }
        let committed = match self.now() {
            Ok(at) => at.mono,
            Err(error) => {
                tracing::debug!(%error, "could not sample capture commit clock");
                return;
            }
        };
        for point in &result.checkpoints {
            match point.latency_at(committed) {
                Ok(elapsed) => tracing::debug!(
                    run = point.run.get(),
                    batch = point.batch.get(),
                    output_to_commit_ms = %elapsed.as_millis(),
                    confirmed_mono_ms = point.at.mono.get(),
                    commit_by_mono_ms = point.commit_by_mono.get(),
                    committed_mono_ms = committed.get(),
                    "capture batch committed"
                ),
                Err(error) => {
                    tracing::debug!(%error, "could not measure capture commit latency")
                }
            }
        }
    }
    pub(super) fn run(&mut self) -> Result<(), RunDatabaseLoopError> {
        loop {
            if let Some(request) = self.persistence.writer().shutdown() {
                return self.shutdown(request);
            }
            if let DrainProgress::Drained = self.drain_ready()? {
                continue;
            }
            if let Ok(command) = self.writes.try_recv() {
                if let DatabaseProgress::Ended = self.handle_command(Some(command))? {
                    return Ok(());
                }
                continue;
            }
            if let Some(deadline) = self.persistence.next_deadline()
                && self.wait_duration(deadline)?.is_zero()
            {
                if let DatabaseProgress::Ended = self.handle_command(None)? {
                    return Ok(());
                }
                continue;
            }
            if let Ok(command) = self.user_writes.try_recv() {
                self.write_command(command)?;
                continue;
            }
            if let Ok(command) = self.versions.requests.try_recv() {
                self.versions.receive(command);
                continue;
            }
            if self.runner.is_running() {
                self.runner
                    .step(&mut self.store)
                    .map_err(|source| RunDatabaseLoopError::Query { source })?;
                continue;
            }
            match self.wait_for_input()? {
                DatabaseInput::Write(command) => {
                    if let DatabaseProgress::Ended = self.handle_command(command)? {
                        return Ok(());
                    }
                }
                DatabaseInput::Query(command) => {
                    self.runner
                        .start(&mut self.store, command, self.registry.snapshots().load())
                        .map_err(|source| RunDatabaseLoopError::Query { source })?;
                }
                DatabaseInput::Versions(command) => self.versions.receive(command),
                DatabaseInput::UserWrite(command) => self.write_command(command)?,
            }
        }
    }
    fn drain_ready(&mut self) -> Result<DrainProgress, RunDatabaseLoopError> {
        if matches!(self.persistence.state(), PersistenceState::Ready) {
            return Ok(DrainProgress::Waiting);
        }
        let now = self
            .now()
            .map_err(|source| RunDatabaseLoopError::Clock { source })?;
        if !self.persistence.is_due(now) {
            return Ok(DrainProgress::Waiting);
        }
        self.flush_and_publish(now)?;
        Ok(
            if matches!(self.persistence.state(), PersistenceState::Failed { .. }) {
                DrainProgress::Waiting
            } else {
                DrainProgress::Drained
            },
        )
    }
    fn flush_and_publish(&mut self, now: Timestamp) -> Result<(), RunDatabaseLoopError> {
        let checkpoints = self.checkpoint_collection();
        let result = self
            .persistence
            .flush_with_clock(&mut self.store, now, checkpoints, || self.clock.read())
            .map_err(|source| RunDatabaseLoopError::Clock { source })?;
        Self::ensure_progress(&result)
            .map_err(|source| RunDatabaseLoopError::Stalled { source })?;
        self.publish(result)
    }
    fn ensure_progress(result: &PersistenceResult) -> Result<(), FlushStalledError> {
        match (result.state(), result.progress) {
            (PersistenceState::Buffered { .. }, FlushProgress::None) => Err(FlushStalledError),
            _ => Ok(()),
        }
    }
    fn wait_for_input(&self) -> Result<DatabaseInput, RunDatabaseLoopError> {
        let wait = match self.persistence.next_deadline() {
            Some(deadline) => self.wait_duration(deadline)?,
            None => Duration::MAX,
        };
        crossbeam_channel::select_biased! {
            recv(self.writes) -> command => command.map(|command| DatabaseInput::Write(Some(command))).map_err(|_| RunDatabaseLoopError::Receive),
            recv(self.user_writes) -> command => command.map(DatabaseInput::UserWrite).map_err(|_| RunDatabaseLoopError::Receive),
            recv(self.versions.requests) -> command => command.map(DatabaseInput::Versions).map_err(|_| RunDatabaseLoopError::Receive),
            recv(self.queries) -> command => match command { Ok(command) => Ok(DatabaseInput::Query(command)), Err(_) => self.wait_for_writes() },
            default(wait) => Ok(DatabaseInput::Write(None)),
        }
    }
    fn wait_for_writes(&self) -> Result<DatabaseInput, RunDatabaseLoopError> {
        let wait = match self.persistence.next_deadline() {
            Some(deadline) => self.wait_duration(deadline)?,
            None => Duration::MAX,
        };
        crossbeam_channel::select_biased! {
            recv(self.writes) -> command => command.map(|command| DatabaseInput::Write(Some(command))).map_err(|_| RunDatabaseLoopError::Receive),
            recv(self.user_writes) -> command => command.map(DatabaseInput::UserWrite).map_err(|_| RunDatabaseLoopError::Receive),
            recv(self.versions.requests) -> command => command.map(DatabaseInput::Versions).map_err(|_| RunDatabaseLoopError::Receive),
            default(wait) => Ok(DatabaseInput::Write(None)),
        }
    }
    fn wait_duration(&self, deadline: MonoInstant) -> Result<Duration, RunDatabaseLoopError> {
        let now = self
            .now()
            .map_err(|source| RunDatabaseLoopError::Clock { source })?;
        let remaining = Duration::from_millis(deadline.get().saturating_sub(now.mono.get()));
        Ok(match self.persistence.state() {
            PersistenceState::Failed { .. } => remaining,
            _ => remaining.saturating_sub(COMMIT_HEADROOM),
        })
    }
    fn handle_command(
        &mut self,
        command: Option<DatabaseCommand>,
    ) -> Result<DatabaseProgress, RunDatabaseLoopError> {
        if let Some(request) = self.persistence.writer().shutdown() {
            self.shutdown(request)?;
            return Ok(DatabaseProgress::Ended);
        }
        match command {
            Some(DatabaseCommand::Drain) => {
                let now = self
                    .now()
                    .map_err(|source| RunDatabaseLoopError::Clock { source })?;
                let checkpoints = self.checkpoint_collection();
                if self.persistence.is_due(now) {
                    let result = self
                        .persistence
                        .flush_with_clock(&mut self.store, now, checkpoints, || self.clock.read())
                        .map_err(|source| RunDatabaseLoopError::Clock { source })?;
                    self.publish(result)?;
                }
            }
            Some(DatabaseCommand::Registry { command, reply }) => {
                self.registry_command(*command, reply)?
            }
            None => {
                let now = self
                    .now()
                    .map_err(|source| RunDatabaseLoopError::Clock { source })?;
                self.flush_and_publish(now)?;
            }
        }
        Ok(DatabaseProgress::Continue)
    }
    fn write_command(
        &mut self,
        command: crate::persistence::dispatch::WriteCommand,
    ) -> Result<(), RunDatabaseLoopError> {
        // Commit queued capture prefixes, checking shutdown between transactions.
        loop {
            if self.persistence.writer().shutdown().is_some()
                || matches!(self.persistence.state(), PersistenceState::Failed { .. })
            {
                let _ = command.reply.send(Err(quartz_protocol::ProtocolError::new(
                    quartz_protocol::ErrorKind::Busy,
                )));
                if let Some(wake) = &command.completion {
                    wake.wake();
                }
                return Ok(());
            }
            if self.persistence.state() == PersistenceState::Ready {
                break;
            }
            let now = self
                .now()
                .map_err(|source| RunDatabaseLoopError::Clock { source })?;
            self.flush_and_publish(now)?;
        }
        // Lock contention must return to capture settlement within its commit headroom.
        self.store.limit_write_wait(COMMIT_HEADROOM);
        let settings_changed = matches!(
            command.request,
            crate::persistence::dispatch::WriteRequest::SettingsSet(_)
        );
        let system = command.settings.as_ref().map(|ports| ports.system);
        let mut result = self.execute_write(command.request, system);
        self.store.limit_write_wait(Duration::MAX);
        if result.is_ok() {
            self.publish_versions()?;
        }
        if settings_changed
            && let Ok(crate::persistence::dispatch::WriteResponse::Settings(response)) = &result
            && let Some(ports) = command.settings
            && let Err(error) =
                (ports.notify)(crate::ipc::UiCommand::SettingsChanged(response.effective))
        {
            tracing::error!(%error, "could not notify main thread of committed settings");
            result = Err(quartz_protocol::ProtocolError::new(
                quartz_protocol::ErrorKind::Internal,
            ));
        }
        let _ = command.reply.send(result);
        if let Some(wake) = &command.completion {
            wake.wake();
        }
        Ok(())
    }
    fn execute_write(
        &mut self,
        request: crate::persistence::dispatch::WriteRequest,
        system: Option<quartz_protocol::EffectiveUiSettings>,
    ) -> Result<crate::persistence::dispatch::WriteResponse, quartz_protocol::ProtocolError> {
        let now = self.now().map_err(|_| {
            quartz_protocol::ProtocolError::new(quartz_protocol::ErrorKind::Internal)
        })?;
        request.execute(&mut self.store, now.utc, system)
    }
    fn registry_command(
        &mut self,
        command: RegistryCommand,
        reply: Sender<Result<RegistryResult, RunRegistryCommandError>>,
    ) -> Result<(), RunDatabaseLoopError> {
        let now = self
            .now()
            .map_err(|source| RunDatabaseLoopError::Clock { source })?;
        let result = self.registry.run(&mut self.store, now.utc, command);
        self.publish_versions()?;
        if result.as_ref().is_ok_and(|result| result.event.is_some()) {
            self.feedback
                .send(Feedback::RulesChanged)
                .map_err(|_| RunDatabaseLoopError::Feedback)?;
        }
        reply.send_during_shutdown(result);
        Ok(())
    }
    fn shutdown(&mut self, request: QueueShutdown) -> Result<(), RunDatabaseLoopError> {
        self.flush_shutdown(request)
            .map_err(|source| RunDatabaseLoopError::Shutdown {
                source: Box::new(source),
            })
    }
    fn flush_shutdown(&mut self, request: QueueShutdown) -> Result<(), ShutdownDatabaseError> {
        let clock = Arc::clone(&self.clock);
        self.flush_shutdown_with(request, |persistence, store, now, checkpoints| {
            persistence
                .flush_with_clock(store, now, checkpoints, || clock.read())
                .map_err(|source| ShutdownDatabaseError::Clock { source })
        })
    }
    fn flush_shutdown_with(
        &mut self,
        request: QueueShutdown,
        mut flush: impl FnMut(
            &mut Persistence,
            &mut Store,
            Timestamp,
            CheckpointCollection,
        ) -> Result<PersistenceResult, ShutdownDatabaseError>,
    ) -> Result<(), ShutdownDatabaseError> {
        loop {
            if Instant::now() >= request.deadline {
                return Err(ShutdownDatabaseError::Deadline);
            }
            let now = self
                .now()
                .map_err(|source| ShutdownDatabaseError::Clock { source })?;
            now.mono
                .duration_since(request.at.mono)
                .map_err(|source| ShutdownDatabaseError::Time { source })?;
            self.store
                .limit_write_wait(request.deadline.saturating_duration_since(Instant::now()));
            let checkpoints = self.checkpoint_collection();
            let result = flush(&mut self.persistence, &mut self.store, now, checkpoints)?;
            Self::ensure_progress(&result)
                .map_err(|source| ShutdownDatabaseError::Stalled { source })?;
            if let DatabaseProgress::Ended = self.settle_shutdown(result, request.deadline)? {
                return Ok(());
            }
        }
    }
    fn settle_shutdown(
        &self,
        result: PersistenceResult,
        deadline: Instant,
    ) -> Result<DatabaseProgress, ShutdownDatabaseError> {
        self.trace_commit_latency(&result);
        self.main
            .send_during_shutdown(MainCommand::Persistence(result.state()));
        if Instant::now() >= deadline {
            return Err(ShutdownDatabaseError::Deadline);
        }
        match result.outcome {
            PersistenceOutcome::Ready => {
                self.feedback
                    .send_deadline(Feedback::Ended, deadline)
                    .map_err(|error| match error {
                        crossbeam_channel::SendTimeoutError::Timeout(_) => {
                            ShutdownDatabaseError::Deadline
                        }
                        crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                            ShutdownDatabaseError::Feedback
                        }
                    })?;
                Ok(DatabaseProgress::Ended)
            }
            PersistenceOutcome::Buffered { .. } => Ok(DatabaseProgress::Continue),
            PersistenceOutcome::Failed { error, .. } => {
                Err(ShutdownDatabaseError::Flush { source: error })
            }
            PersistenceOutcome::RetainedFailure { .. } => {
                Err(ShutdownDatabaseError::RetainedWrites)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::app::{CONTROL_CAPACITY, SHUTDOWN_TIMEOUT},
        data_directory::DataDirectory,
        store::capture::{RunMetadata, WindowsSessionId},
    };
    use crossbeam_channel::bounded;
    use quartz_domain::capture::UtcInstant;

    struct TestClock(Timestamp);
    impl ClockReader for TestClock {
        fn read(&self) -> Result<Timestamp, ReadClockError> {
            Ok(self.0)
        }
    }
    struct Fixture {
        _versions: crate::query::versions::VersionPort,
        database: DatabaseLoop,
        _directory: tempfile::TempDir,
        _commands: super::super::MainReceiver,
        _feedback: Receiver<Feedback>,
    }
    const CATEGORY_UUID: &str = "00000000-0000-4000-8000-000000000123";
    const MISSING_CATEGORY_UUID: &str = "00000000-0000-4000-8000-000000000124";

    impl Fixture {
        fn category_body(&self) -> serde_json::Value {
            serde_json::json!({"entity":{"uuid":CATEGORY_UUID,"parent_uuid":null,"name":"研究","color":"cat.8","tendency":"focus"}})
        }
        fn ipc(
            &mut self,
            op: quartz_protocol::Op,
            body: serde_json::Value,
        ) -> Result<serde_json::Value, quartz_protocol::ProtocolError> {
            use crate::ipc::{Action, AllowedRoles, ConnectionSession, StatusPublisher};
            use quartz_protocol::{Envelope, Request, RequestId, ResponseOutcome};
            let status = StatusPublisher::new(quartz_protocol::InstanceId("category-test".into()));
            let status = status.subscribe();
            let mut session = ConnectionSession::new(AllowedRoles::Quartz);
            let hello = quartz_protocol::Hello {
                proto: quartz_protocol::ProtoRange::new(
                    quartz_protocol::PROTOCOL_VERSION,
                    quartz_protocol::PROTOCOL_VERSION,
                )
                .expect("protocol"),
                client_version: quartz_protocol::ClientVersion("test".into()),
                role: quartz_protocol::HelloRole::Ui {
                    ui: quartz_protocol::UiHello {
                        state: quartz_protocol::UiState::Visible,
                    },
                },
            };
            assert!(matches!(
                session.receive(
                    Envelope::Request(Request {
                        id: RequestId(1),
                        op: quartz_protocol::Op::Hello,
                        body: serde_json::to_value(hello).expect("hello")
                    }),
                    &status.borrow().status
                ),
                Action::Reply(quartz_protocol::Response {
                    outcome: ResponseOutcome::Ok { .. },
                    ..
                })
            ));
            let envelope = Envelope::Request(Request {
                id: RequestId(2),
                op,
                body,
            });
            let bytes = quartz_protocol::encode_json(&envelope).expect("encode request");
            let request = quartz_protocol::decode_json(&bytes).expect("decode request");
            let action = session.receive(request, &status.borrow().status);
            let result = match action {
                Action::Write { id, request } => {
                    assert_eq!(id, RequestId(2));
                    let channel = crate::persistence::dispatch::WriteChannel::new();
                    let mut reply = channel
                        .port
                        .submit(request)
                        .expect("admit category request");
                    self.database
                        .write_command(channel.receiver.try_recv().expect("database request"))
                        .expect("database loop");
                    reply
                        .try_recv()
                        .expect("database reply")
                        .map(|response| serde_json::to_value(response).expect("wire response"))
                }
                Action::Reply(response) => match response.outcome {
                    ResponseOutcome::Err { error } => Err(error),
                    ResponseOutcome::Ok { body } => Ok(body),
                },
                _ => panic!("expected category or annotation dispatch"),
            };
            let response = quartz_protocol::Response {
                id: Some(RequestId(2)),
                outcome: match result {
                    Ok(body) => ResponseOutcome::Ok { body },
                    Err(error) => ResponseOutcome::Err { error },
                },
            };
            let bytes = quartz_protocol::encode_json(&response).expect("encode response");
            let response: quartz_protocol::Response =
                quartz_protocol::decode_json(&bytes).expect("decode response");
            match response.outcome {
                ResponseOutcome::Ok { body } => Ok(body),
                ResponseOutcome::Err { error } => Err(error),
            }
        }
        fn import_category(
            &mut self,
            key: &str,
            reference: &str,
        ) -> Result<RegistryResult, RunRegistryCommandError> {
            let text = format!(
                "format='quartz-rules/1'\n[set]\nkey='{key}'\nkind='classification'\nname='Test rules'\nparent='builtin.classification'\n[[rule]]\nid='test'\nlabel='Test rule'\ncategory='{reference}'\nwhen=[{{field='app.exe_name',op='equals',value='test.exe'}}]\n"
            );
            let (reply, received) = bounded(1);
            self.database
                .registry_command(
                    RegistryCommand::Import {
                        key: key.parse().expect("key"),
                        file: "test.toml".into(),
                        text,
                        base_version: 0,
                        preview_hash: None,
                    },
                    reply,
                )
                .expect("registry dispatch");
            let result = received.recv().expect("registry response");
            if result.as_ref().is_ok_and(|result| result.event.is_some()) {
                assert!(matches!(
                    self._feedback.try_recv(),
                    Ok(Feedback::RulesChanged)
                ));
            }
            result
        }
        fn subscribe_category_day(&mut self) {
            use std::future::Future;
            let port = self._versions.clone();
            let date = "1970-01-01".parse().expect("date");
            {
                let subscription = port.set_days(std::collections::BTreeSet::from([date]));
                let mut subscription = std::pin::pin!(subscription);
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(subscription.as_mut().poll(&mut context).is_pending());
                self.database.versions.receive(
                    self.database
                        .versions
                        .requests
                        .try_recv()
                        .expect("subscription request"),
                );
                assert!(matches!(
                    subscription.poll(&mut context),
                    std::task::Poll::Ready(Ok(_))
                ));
            }
            let mut subscription = std::pin::pin!(port.set_summary_day(date));
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(subscription.as_mut().poll(&mut context).is_pending());
            self.database.versions.receive(
                self.database
                    .versions
                    .requests
                    .try_recv()
                    .expect("tray subscription request"),
            );
            assert!(matches!(
                subscription.poll(&mut context),
                std::task::Poll::Ready(Ok(_))
            ));
            self._versions.updates.borrow_and_update();
        }
        fn assert_category_publication(&mut self) {
            assert!(
                self._versions
                    .updates
                    .has_changed()
                    .expect("publisher open"),
                "committed change reaches day and tray input"
            );
            self._versions.updates.borrow_and_update();
            self.database
                .publish_versions()
                .expect("repeat publication");
            assert!(
                !self
                    ._versions
                    .updates
                    .has_changed()
                    .expect("publisher open"),
                "committed change is consumed once"
            );
        }
    }

    #[test]
    fn shutdown_acknowledges_capture_when_the_main_notification_queue_is_full() {
        let fixture = Fixture::new();
        fixture
            .database
            .main
            .send(MainCommand::PolicyChanged)
            .expect("saturate main queue");
        let mut result = stalled_result();
        result.outcome = PersistenceOutcome::Ready;
        assert!(matches!(
            fixture
                .database
                .settle_shutdown(result, Instant::now() + SHUTDOWN_TIMEOUT),
            Ok(DatabaseProgress::Ended)
        ));
        assert!(matches!(
            fixture
                ._feedback
                .try_recv()
                .expect("capture shutdown acknowledgement"),
            Feedback::Ended
        ));
    }

    #[test]
    fn slow_failure_uses_completion_clock_and_leaves_other_channels_serviceable() {
        use crate::store::capture::test_support::{Inspection, at};
        use quartz_domain::capture::{
            CaptureCore, CaptureEvent, CaptureInput, RunId, SessionStatus, StopChange, StopReason,
        };
        struct TransactionClock {
            reads: std::sync::atomic::AtomicUsize,
        }
        impl ClockReader for TransactionClock {
            fn read(&self) -> Result<Timestamp, ReadClockError> {
                Ok(at(
                    if self
                        .reads
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        == 0
                    {
                        1000
                    } else {
                        3000
                    },
                ))
            }
        }
        let mut fixture = Fixture::new();
        let mut started = CaptureCore::start(RunId::new(1), at(0)).expect("capture");
        let _ = fixture
            .database
            .persistence
            .accept(started.step.into_parts(RunId::new(1)), at(0));
        let initial = fixture.database.persistence.flush(
            &mut fixture.database.store,
            at(0),
            CheckpointCollection::Skip,
        );
        assert!(initial.failure().is_none());
        let ready = started
            .core
            .step(CaptureInput {
                at: at(0),
                rules: fixture.database.registry.snapshots().load(),
                event: CaptureEvent::Session(SessionStatus::Ready(Default::default())),
            })
            .expect("session");
        let _ = fixture
            .database
            .persistence
            .accept(ready.into_parts(RunId::new(1)), at(0));
        let initial = fixture.database.persistence.flush(
            &mut fixture.database.store,
            at(0),
            CheckpointCollection::Skip,
        );
        assert!(initial.failure().is_none());
        let stopped = started
            .core
            .step(CaptureInput {
                at: at(1),
                rules: fixture.database.registry.snapshots().load(),
                event: CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
            })
            .expect("stopped boundary");
        let _ = fixture
            .database
            .persistence
            .accept(stopped.into_parts(RunId::new(1)), at(1));
        fixture.database.writes = crossbeam_channel::never();
        let mut inspection = Inspection::open(fixture._directory.path());
        inspection.inject_write_failure();
        fixture.database.clock = Arc::new(TransactionClock {
            reads: Default::default(),
        });
        assert!(
            matches!(fixture.database.drain_ready(), Ok(DrainProgress::Waiting)),
            "failure must return to channel servicing"
        );
        assert_eq!(
            fixture.database.persistence.next_deadline(),
            Some(at(3100).mono)
        );
        assert!(
            !fixture.database.persistence.is_due(at(3000)),
            "no back-to-back failure retries"
        );
        let writes = crate::ipc::WriteChannel::new();
        let mut response = writes
            .port
            .submit(crate::persistence::dispatch::WriteRequest::BumpMetadata {
                outcome: crate::persistence::dispatch::TestCommit::Commit,
            })
            .expect("admit user write");
        fixture
            .database
            .write_command(writes.receiver.try_recv().expect("write"))
            .expect("service write");
        assert_eq!(
            response
                .try_recv()
                .expect("prompt reply")
                .expect_err("failed persistence is busy")
                .kind,
            quartz_protocol::ErrorKind::Busy
        );
        let queries = crate::query::dispatch::QueryChannel::new();
        fixture.database.queries = queries.receiver;
        let reply = queries
            .port
            .submit_rule_read(crate::rules::read::ReadRequest::Tree(
                quartz_protocol::RuleSetsTreeRequest {
                    kind: quartz_protocol::RuleKind::Privacy,
                    cursor: None,
                },
            ))
            .expect("admit read");
        let DatabaseInput::Query(command) = fixture
            .database
            .wait_for_input()
            .expect("read remains serviceable")
        else {
            panic!("query must precede future retry");
        };
        fixture
            .database
            .runner
            .start(
                &mut fixture.database.store,
                command,
                fixture.database.registry.snapshots().load(),
            )
            .expect("service read");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("reply runtime");
        runtime
            .block_on(reply.receive())
            .expect("read-only rule query still completes");
        inspection.restore_writes();
    }

    #[test]
    fn category_ipc_retries_and_each_real_edit_publish_once() {
        use quartz_protocol::Op;
        let mut fixture = Fixture::new();
        fixture
            .database
            .publish_versions()
            .expect("startup publication");
        fixture.subscribe_category_day();
        let first = fixture
            .ipc(Op::CategoriesCreate, fixture.category_body())
            .expect("create over IPC");
        fixture.assert_category_publication();
        let references = &fixture
            .database
            .registry
            .core()
            .state()
            .references
            .categories;
        assert_eq!(
            references.get("研究").map(String::as_str),
            Some(CATEGORY_UUID)
        );
        let mut retry = fixture.category_body();
        retry["entity"]["name"] = serde_json::json!(" ");
        retry["entity"]["color"] = serde_json::json!("invalid");
        retry["entity"]["parent_uuid"] = serde_json::json!(MISSING_CATEGORY_UUID);
        assert_eq!(
            fixture
                .ipc(Op::CategoriesCreate, retry)
                .expect("UUID retry returns stored entity"),
            first
        );
        assert!(!fixture._versions.updates.has_changed().expect("publisher"));
        assert_eq!(
            fixture
                .ipc(Op::CategoriesUpdate, fixture.category_body())
                .expect("same final values"),
            first
        );
        assert!(!fixture._versions.updates.has_changed().expect("publisher"));
        let mut rename = fixture.category_body();
        rename["entity"]["name"] = serde_json::json!("阅读");
        fixture
            .ipc(Op::CategoriesUpdate, rename.clone())
            .expect("rename");
        fixture.assert_category_publication();
        let references = &fixture
            .database
            .registry
            .core()
            .state()
            .references
            .categories;
        assert!(!references.contains_key("研究"));
        assert_eq!(
            references.get("阅读").map(String::as_str),
            Some(CATEGORY_UUID)
        );
        let listing = fixture
            .ipc(
                Op::CategoriesList,
                serde_json::json!({"include_archived":true}),
            )
            .expect("siblings");
        let mut uuids: Vec<_> = listing["entities"]
            .as_array()
            .expect("entities")
            .iter()
            .map(|entity| entity["uuid"].clone())
            .collect();
        uuids.reverse();
        let reorder = serde_json::json!({"parent_uuid":null,"uuids":uuids});
        assert_eq!(
            fixture
                .ipc(Op::CategoriesReorder, reorder.clone())
                .expect("reorder"),
            serde_json::json!({"accepted":true})
        );
        fixture.assert_category_publication();
        fixture
            .ipc(Op::CategoriesReorder, reorder)
            .expect("same order");
        assert!(!fixture._versions.updates.has_changed().expect("publisher"));
        for archived in [true, false] {
            let body = serde_json::json!({"uuid":CATEGORY_UUID,"archived":archived});
            fixture
                .ipc(Op::CategoriesArchive, body.clone())
                .expect("archive final state");
            fixture.assert_category_publication();
            fixture
                .ipc(Op::CategoriesArchive, body)
                .expect("same archive state");
            assert!(!fixture._versions.updates.has_changed().expect("publisher"));
        }
        fixture
            .ipc(Op::CategoriesUpdate, rename)
            .expect("same renamed values");
        assert!(!fixture._versions.updates.has_changed().expect("publisher"));
    }

    #[test]
    fn category_ipc_validation_keeps_rows_versions_and_references_unchanged() {
        use quartz_protocol::{ErrorKind, Op};
        let mut fixture = Fixture::new();
        fixture
            .ipc(Op::CategoriesCreate, fixture.category_body())
            .expect("create");
        fixture.subscribe_category_day();
        let before = fixture
            .ipc(
                Op::CategoriesList,
                serde_json::json!({"include_archived":true}),
            )
            .expect("before");
        struct Case {
            op: Op,
            body: serde_json::Value,
        }
        let mut cases = vec![
            Case {
                op: Op::CategoriesArchive,
                body: serde_json::json!({"uuid":MISSING_CATEGORY_UUID,"archived":true}),
            },
            Case {
                op: Op::CategoriesReorder,
                body: serde_json::json!({"parent_uuid":null,"uuids":[CATEGORY_UUID]}),
            },
        ];
        for name in [
            serde_json::json!(" "),
            serde_json::json!(
                "🦀".repeat(quartz_domain::categories::MAX_CATEGORY_NAME_SCALARS + 1)
            ),
            serde_json::Value::Null,
        ] {
            let mut body = fixture.category_body();
            body["entity"]["name"] = name;
            cases.push(Case {
                op: Op::CategoriesUpdate,
                body,
            });
        }
        for field in ["color", "tendency", "parent_uuid", "uuid", "builtin_key"] {
            let mut body = fixture.category_body();
            body["entity"][field] = serde_json::json!(if matches!(field, "uuid" | "parent_uuid") {
                MISSING_CATEGORY_UUID
            } else {
                "invalid"
            });
            cases.push(Case {
                op: Op::CategoriesUpdate,
                body,
            });
        }
        let mut cycle = fixture.category_body();
        cycle["entity"]["parent_uuid"] = serde_json::json!(CATEGORY_UUID);
        cases.push(Case {
            op: Op::CategoriesUpdate,
            body: cycle,
        });
        for case in cases {
            assert_eq!(
                fixture
                    .ipc(case.op, case.body)
                    .expect_err("invalid category request")
                    .kind,
                ErrorKind::BadRequest
            );
            assert_eq!(
                fixture
                    .ipc(
                        Op::CategoriesList,
                        serde_json::json!({"include_archived":true})
                    )
                    .expect("unchanged list"),
                before
            );
            assert!(!fixture._versions.updates.has_changed().expect("publisher"));
            assert_eq!(
                fixture
                    .database
                    .registry
                    .core()
                    .state()
                    .references
                    .categories
                    .get("研究")
                    .map(String::as_str),
                Some(CATEGORY_UUID)
            );
        }
    }

    #[test]
    fn committed_category_is_immediately_usable_and_archive_retains_existing_references() {
        use quartz_protocol::{ErrorKind, Op};
        let mut fixture = Fixture::new();
        fixture
            .ipc(Op::CategoriesCreate, fixture.category_body())
            .expect("create");
        fixture
            .import_category("user.by-name", "研究")
            .expect("import new name immediately");
        fixture
            .import_category("user.by-uuid", CATEGORY_UUID)
            .expect("import new UUID immediately");
        let annotation = serde_json::json!({"entities":[{"uuid":"00000000-0000-4000-8000-000000000200","kind":"offline_activity","start_utc_ms":0,"end_utc_ms":1000,"category":{"kind":"uuid","uuid":CATEGORY_UUID}}]});
        let saved = fixture
            .ipc(Op::AnnotationsCreate, annotation.clone())
            .expect("annotate new category immediately");
        let rule_hashes = fixture
            .database
            .registry
            .snapshots()
            .load()
            .hashes()
            .clone();
        fixture
            .ipc(
                Op::CategoriesArchive,
                serde_json::json!({"uuid":CATEGORY_UUID,"archived":true}),
            )
            .expect("archive");
        assert_eq!(
            fixture.database.registry.snapshots().load().hashes(),
            &rule_hashes
        );
        fixture
            .import_category("user.archived-name", "研究")
            .expect_err("new rule rejects archived name");
        fixture
            .import_category("user.archived-uuid", CATEGORY_UUID)
            .expect_err("new rule rejects archived UUID");
        let mut rejected = annotation.clone();
        rejected["entities"][0]["uuid"] = serde_json::json!("00000000-0000-4000-8000-000000000201");
        assert_eq!(
            fixture
                .ipc(Op::AnnotationsCreate, rejected)
                .expect_err("new annotation rejects archive")
                .kind,
            ErrorKind::BadRequest
        );
        assert_eq!(
            fixture
                .ipc(Op::AnnotationsCreate, annotation)
                .expect("load original annotation by retry"),
            saved
        );
        let stored = fixture
            .database
            .store
            .rule_sets()
            .load()
            .expect("retained rule history");
        assert!(
            stored
                .sets
                .contains_key(&"user.by-name".parse().expect("key"))
        );
        assert!(
            stored
                .sets
                .contains_key(&"user.by-uuid".parse().expect("key"))
        );
        let builtin = quartz_domain::categories::BuiltinCategory::Development.uuid();
        fixture
            .ipc(
                Op::CategoriesArchive,
                serde_json::json!({"uuid":builtin.0,"archived":true}),
            )
            .expect("archive builtin");
        let reopened = RegistryShell::open(
            &mut fixture.database.store,
            UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("restart retains rules including archived builtin references");
        assert_eq!(reopened.snapshots().load().hashes(), &rule_hashes);
        assert!(
            !reopened
                .core()
                .state()
                .references
                .categories
                .contains_key("builtin:development")
        );
        assert!(
            !reopened
                .core()
                .state()
                .references
                .categories
                .contains_key(CATEGORY_UUID)
        );
        fixture
            .ipc(
                Op::CategoriesArchive,
                serde_json::json!({"uuid":CATEGORY_UUID,"archived":false}),
            )
            .expect("restore");
        fixture
            .import_category("user.restored", "研究")
            .expect("restored name immediately available");
    }

    #[test]
    fn category_ipc_pages_are_bounded_and_metadata_changes_stale_the_cursor() {
        use quartz_domain::categories::{CategoryValues, CreateCategory};
        use quartz_protocol::{CategoriesListResponse, ErrorKind, Op};
        // Maximum-length four-byte names force multiple pages with a small hierarchy.
        const CATEGORY_COUNT: usize = 220;
        let mut fixture = Fixture::new();
        for index in 0..CATEGORY_COUNT {
            fixture
                .database
                .store
                .create_category(CreateCategory {
                    uuid: quartz_domain::categories::CategoryUuid(format!(
                        "00000000-0000-4000-8000-{index:012}"
                    )),
                    values: CategoryValues {
                        parent_uuid: None,
                        name: Some(
                            "🦀"
                                .repeat(quartz_domain::categories::MAX_CATEGORY_NAME_SCALARS)
                                .parse()
                                .expect("maximum name"),
                        ),
                        color: quartz_domain::categories::CategoryColor::Neutral,
                        tendency: quartz_domain::categories::CategoryTendency::Neutral,
                    },
                })
                .expect("create paging fixture");
        }
        let first_wire = fixture
            .ipc(Op::CategoriesList, serde_json::json!({}))
            .expect("first page");
        assert!(
            serde_json::to_vec(&first_wire).expect("page bytes").len()
                <= quartz_protocol::QUERY_PAGE_BYTES
        );
        let first: CategoriesListResponse =
            serde_json::from_value(first_wire).expect("typed first page");
        let cursor = first.next_cursor.clone().expect("continuation");
        let mut entities = first.entities;
        let mut next = Some(cursor.clone());
        while let Some(cursor) = next {
            let wire = fixture
                .ipc(Op::CategoriesList, serde_json::json!({"cursor":cursor}))
                .expect("next page");
            assert!(
                serde_json::to_vec(&wire).expect("page bytes").len()
                    <= quartz_protocol::QUERY_PAGE_BYTES
            );
            let page: CategoriesListResponse = serde_json::from_value(wire).expect("typed page");
            assert_eq!(page.versions, first.versions);
            entities.extend(page.entities);
            next = page.next_cursor;
        }
        assert_eq!(
            entities.len(),
            CATEGORY_COUNT + quartz_domain::categories::BuiltinCategory::ALL.len()
        );
        let expected = fixture
            .database
            .store
            .list_categories(quartz_domain::categories::IncludeArchivedCategories::Exclude)
            .expect("hierarchy");
        assert!(
            entities
                .iter()
                .map(|entity| entity.uuid.to_string())
                .eq(expected.into_iter().map(|entity| entity.uuid.0))
        );
        assert_eq!(
            fixture
                .ipc(
                    Op::CategoriesList,
                    serde_json::json!({"include_archived":true,"cursor":cursor})
                )
                .expect_err("filter differs")
                .kind,
            ErrorKind::BadRequest
        );
        let mut update = fixture.category_body();
        update["entity"]["name"] = serde_json::json!("Changed");
        fixture
            .ipc(Op::CategoriesUpdate, update)
            .expect("metadata change");
        assert_eq!(
            fixture
                .ipc(Op::CategoriesList, serde_json::json!({"cursor":cursor}))
                .expect_err("stale cursor")
                .kind,
            ErrorKind::StaleCursor
        );
    }

    #[test]
    fn committed_day_offset_wakes_main_once_and_updates_query_versions() {
        let mut fixture = Fixture::new();
        fixture
            .database
            .publish_versions()
            .expect("initial versions");
        fixture
            .database
            .store
            .test_day_offset_write(240)
            .expect("committed offset");
        fixture
            .database
            .publish_versions()
            .expect("publish changed offset");
        assert!(matches!(
            fixture._commands.try_recv().expect("main notified"),
            MainCommand::DayOffsetChanged { offset } if offset.minutes() == 240
        ));
        fixture
            .database
            .publish_versions()
            .expect("unchanged versions");
        assert!(
            fixture._commands.try_recv().is_err(),
            "unchanged offset does not notify main"
        );
        fixture
            .database
            .store
            .test_day_offset_write(0)
            .expect("restore midnight");
        fixture
            .database
            .publish_versions()
            .expect("publish restored offset");
        assert!(matches!(
            fixture._commands.try_recv().expect("main notified"),
            MainCommand::DayOffsetChanged { offset } if offset == crate::query::calendar::DayStartOffset::MIDNIGHT
        ));
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("data directory");
            let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
            let opened = Persistence::open(
                &mut store,
                RunMetadata {
                    device_id: "test-device".into(),
                    win_session_id: WindowsSessionId::new(7),
                    app_version: "test".into(),
                },
            )
            .expect("persistence");
            let (_, writes) = bounded(CONTROL_CAPACITY);
            let (feedback, received_feedback) = bounded(CONTROL_CAPACITY);
            let channel = crate::core::app::MainChannel::new(CONTROL_CAPACITY);
            let sender = channel.sender;
            let commands = channel.receiver;
            store.seed_categories().expect("builtin categories");
            let registry = RegistryShell::open(&mut store, UtcInstant::try_from(0).expect("UTC"))
                .expect("registry");
            let crate::query::versions::VersionChannel {
                publisher: versions,
                port,
            } = crate::query::versions::VersionPublisher::open(&mut store, registry.snapshots())
                .expect("versions");
            Self {
                _versions: port,
                database: DatabaseLoop {
                    registry,
                    queries: crossbeam_channel::never(),
                    user_writes: crossbeam_channel::never(),
                    versions,
                    store,
                    runner: Default::default(),
                    persistence: opened.persistence,
                    writes,
                    feedback,
                    main: MainPort {
                        sender,
                        wake: Arc::new(|| {}),
                    },
                    clock: Arc::new(TestClock(Timestamp {
                        utc: UtcInstant::try_from(0).expect("UTC"),
                        mono: MonoInstant::from_millis(0),
                    })),
                },
                _directory: directory,
                _commands: commands,
                _feedback: received_feedback,
            }
        }
        fn shutdown_request(&self, deadline: Instant) -> QueueShutdown {
            QueueShutdown {
                at: self.database.now().expect("clock"),
                deadline,
            }
        }
    }
    fn stalled_result() -> PersistenceResult {
        PersistenceResult {
            feedback: Vec::new(),
            effects: Vec::new(),
            outcome: PersistenceOutcome::Buffered { commands: 1 },
            progress: FlushProgress::None,
            checkpoints: Vec::new(),
        }
    }
    #[test]
    fn inconsistent_shutdown_plan_returns_stalled_after_one_attempt() {
        let mut fixture = Fixture::new();
        let request = fixture.shutdown_request(Instant::now() + SHUTDOWN_TIMEOUT);
        let mut attempts = 0;
        let result = fixture.database.flush_shutdown_with(request, |_, _, _, _| {
            attempts += 1;
            Ok(stalled_result())
        });
        assert!(matches!(result, Err(ShutdownDatabaseError::Stalled { .. })));
        assert_eq!(attempts, 1);
    }
    #[test]
    fn shutdown_deadline_prevents_another_flush() {
        let mut fixture = Fixture::new();
        let request = fixture.shutdown_request(Instant::now());
        let result = fixture.database.flush_shutdown_with(request, |_, _, _, _| {
            panic!("expired shutdown cannot flush")
        });
        assert!(matches!(result, Err(ShutdownDatabaseError::Deadline)));
    }
    #[test]
    fn stalled_drain_cannot_continue_the_event_loop() {
        assert!(DatabaseLoop::ensure_progress(&stalled_result()).is_err());
        let mut committed = stalled_result();
        committed.progress = FlushProgress::Committed { commands: 1 };
        assert!(DatabaseLoop::ensure_progress(&committed).is_ok());
    }

    #[test]
    fn commit_wait_reserves_headroom_and_never_waits_past_the_deadline() {
        let fixture = Fixture::new();
        let now = fixture.database.now().expect("scripted clock");
        let deadline = now
            .mono
            .checked_add(quartz_domain::capture::limits::BATCH_WAIT)
            .expect("commit deadline");
        assert_eq!(
            fixture
                .database
                .wait_duration(deadline)
                .expect("commit wait"),
            quartz_domain::capture::limits::BATCH_WAIT - COMMIT_HEADROOM
        );
        assert_eq!(
            fixture
                .database
                .wait_duration(now.mono)
                .expect("due commit"),
            Duration::ZERO
        );
        let near_deadline = now
            .mono
            .checked_add(COMMIT_HEADROOM - Duration::from_millis(1))
            .expect("near deadline");
        assert_eq!(
            fixture
                .database
                .wait_duration(near_deadline)
                .expect("headroom already reached"),
            Duration::ZERO
        );
    }

    #[test]
    fn committed_checkpoint_latency_uses_the_injected_post_flush_clock() {
        use quartz_domain::{
            capture::{
                CaptureCore, CaptureEvent, CaptureInput, Foreground, NoForegroundReason,
                ObservationSource, RunId,
            },
            rules::registry::{RegistryState, RuleRegistry},
        };
        for checkpoints in [CheckpointCollection::Skip, CheckpointCollection::Collect] {
            let mut fixture = Fixture::new();
            let start = fixture.database.now().expect("scripted startup clock");
            let run = RunId::new(1);
            let mut started = CaptureCore::start(run, start).expect("capture core");
            let rules = RuleRegistry::from_state(RegistryState::default())
                .expect("rules")
                .snapshots();
            let _ = fixture
                .database
                .persistence
                .accept(started.step.into_parts(run), start);
            for event in [
                CaptureEvent::Session(quartz_domain::capture::SessionStatus::Ready(
                    Default::default(),
                )),
                CaptureEvent::Foreground {
                    source: ObservationSource::Review,
                    foreground: Foreground::None(NoForegroundReason::Empty),
                },
            ] {
                let parts = started
                    .core
                    .step(CaptureInput {
                        at: start,
                        rules: rules.load(),
                        event,
                    })
                    .expect("startup observation")
                    .into_parts(run);
                let _ = fixture.database.persistence.accept(parts, start);
            }
            let initial = fixture.database.persistence.flush(
                &mut fixture.database.store,
                start,
                CheckpointCollection::Skip,
            );
            assert!(initial.failure().is_none(), "{:?}", initial.failure());
            let output = start
                .checked_advance(quartz_domain::capture::limits::CHECKPOINT_PERIOD)
                .expect("checkpoint time");
            let parts = started
                .core
                .step(CaptureInput {
                    at: output,
                    rules: rules.load(),
                    event: CaptureEvent::Wake,
                })
                .expect("checkpoint observation")
                .into_parts(run);
            let _ = fixture.database.persistence.accept(parts, output);
            let queued = Duration::from_millis(100);
            let transaction = Duration::from_millis(25);
            let flush_at = output.checked_advance(queued).expect("flush clock");
            let committed_at = flush_at
                .checked_advance(transaction)
                .expect("post-flush clock");
            fixture.database.clock = Arc::new(TestClock(committed_at));
            let level = match checkpoints {
                CheckpointCollection::Skip => tracing::Level::INFO,
                CheckpointCollection::Collect => tracing::Level::DEBUG,
            };
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(level)
                .with_writer(std::io::sink)
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                assert_eq!(fixture.database.checkpoint_collection(), checkpoints);
                let result = fixture.database.persistence.flush(
                    &mut fixture.database.store,
                    flush_at,
                    checkpoints,
                );
                assert!(result.failure().is_none(), "{:?}", result.failure());
                assert_eq!(result.state(), PersistenceState::Ready);
                assert!(matches!(result.progress, FlushProgress::Committed { .. }));
                match checkpoints {
                    CheckpointCollection::Skip => assert!(result.checkpoints.is_empty()),
                    CheckpointCollection::Collect => {
                        assert_eq!(result.checkpoints.len(), 1);
                        let point = &result.checkpoints[0];
                        assert_eq!(point.at, output);
                        assert_eq!(
                            point.commit_by_mono,
                            output
                                .checked_advance(quartz_domain::capture::limits::BATCH_WAIT)
                                .expect("deadline")
                                .mono
                        );
                        let committed = fixture.database.now().expect("injected commit clock").mono;
                        assert_eq!(
                            point.latency_at(committed).expect("commit latency"),
                            queued + transaction
                        );
                    }
                }
                fixture
                    .database
                    .publish(result)
                    .expect("publish successful commit");
            });
        }
    }
    #[test]
    fn user_write_flushes_capture_before_commit_and_reply() {
        use crate::persistence::dispatch::{TestCommit, WriteChannel, WriteRequest};
        use quartz_domain::capture::{CaptureCore, RunId};
        let mut fixture = Fixture::new();
        let at = fixture.database.now().expect("clock");
        let run = RunId::new(1);
        let started = CaptureCore::start(run, at).expect("capture");
        let _ = fixture
            .database
            .persistence
            .accept(started.step.into_parts(run), at);
        assert!(matches!(
            fixture.database.persistence.state(),
            PersistenceState::Buffered { .. }
        ));
        let channel = WriteChannel::new();
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let notify = Arc::clone(&wakes);
        let port = channel.port.with_completion_wake(Arc::new(move || {
            notify.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }));
        let received = port
            .submit(WriteRequest::BumpMetadata {
                outcome: TestCommit::Commit,
            })
            .expect("admit write");
        fixture
            .database
            .write_command(channel.receiver.try_recv().expect("queued write"))
            .expect("dispatch write");
        assert_eq!(
            wakes.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "wake main after reply"
        );
        received
            .blocking_recv()
            .expect("reply channel")
            .expect("committed");
        assert_eq!(
            fixture.database.persistence.state(),
            PersistenceState::Ready,
            "capture commits before user reply"
        );
        assert!(matches!(
            fixture
                ._commands
                .try_recv()
                .expect("capture commit published"),
            MainCommand::Persistence(PersistenceState::Ready)
        ));
    }
    #[test]
    fn shutdown_rejects_user_write_before_draining_capture() {
        use crate::persistence::dispatch::{TestCommit, WriteCommand, WriteRequest};
        use quartz_domain::capture::{CaptureCore, RunId};
        let mut fixture = Fixture::new();
        let at = fixture.database.now().expect("clock");
        let run = RunId::new(1);
        let started = CaptureCore::start(run, at).expect("capture");
        let _ = fixture
            .database
            .persistence
            .accept(started.step.into_parts(run), at);
        let request = fixture.shutdown_request(Instant::now() + SHUTDOWN_TIMEOUT);
        fixture.database.persistence.writer().finish(request);
        let (reply, received) = tokio::sync::oneshot::channel();
        fixture
            .database
            .write_command(WriteCommand {
                completion: None,
                settings: None,
                request: WriteRequest::BumpMetadata {
                    outcome: TestCommit::Commit,
                },
                reply,
            })
            .expect("reject write");
        assert_eq!(
            received
                .blocking_recv()
                .expect("reply")
                .expect_err("shutdown wins")
                .kind,
            quartz_protocol::ErrorKind::Busy
        );
        assert!(
            matches!(
                fixture.database.persistence.state(),
                PersistenceState::Buffered { .. }
            ),
            "shutdown owns the final capture drain"
        );
        assert!(fixture._commands.is_empty());
    }
}
