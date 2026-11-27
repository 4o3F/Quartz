//! Database actor; each continuation requires observable flush progress.

use super::{
    COMMIT_HEADROOM, DatabaseCommand, Feedback, MainCommand, MainPort, SendDuringShutdown,
    SendMainCommandError,
};
use crate::{
    core::sources::{ClockReader, ReadClockError},
    persistence::{
        CheckpointCollection, FlushProgress, FlushWritesError, Persistence, PersistenceOutcome,
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
    Stalled { source: FlushWritesError },
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
    Stalled { source: FlushWritesError },
    #[snafu(display("could not acknowledge database shutdown"))]
    Feedback,
}
impl From<SendMainCommandError> for ShutdownDatabaseError {
    fn from(source: SendMainCommandError) -> Self {
        Self::Publish { source }
    }
}

/// Tracks whether the one-shot startup title-retention purge has executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StartupPurge {
    Pending,
    Done,
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
    pub startup_purge: StartupPurge,
    pub title_purge_queued: Arc<std::sync::atomic::AtomicBool>,
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
    fn version_command(
        &mut self,
        command: crate::query::versions::VersionCommand,
    ) -> Result<(), RunDatabaseLoopError> {
        if matches!(command, crate::query::versions::VersionCommand::Refresh)
            && self.store.has_committed_metadata()
        {
            self.registry
                .refresh_category_references(&mut self.store)
                .map_err(|source| RunDatabaseLoopError::RefreshReferences { source })?;
        }
        if let Some(offset) = self
            .versions
            .receive(&mut self.store, command)
            .map_err(|source| RunDatabaseLoopError::Query { source })?
        {
            self.main.send(MainCommand::DayOffsetChanged { offset })?;
        }
        Ok(())
    }
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
                self.version_command(command)?;
                continue;
            }
            if self.runner.is_running() {
                self.runner
                    .step(&mut self.store)
                    .map_err(|source| RunDatabaseLoopError::Query { source })?;
                continue;
            }
            if self.startup_purge == StartupPurge::Pending
                && self.persistence.state() == PersistenceState::Ready
            {
                self.startup_purge = StartupPurge::Done;
                self.run_startup_title_purge()?;
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
                        .start(&mut self.store, command, &self.registry)
                        .map_err(|source| RunDatabaseLoopError::Query { source })?;
                }
                DatabaseInput::Versions(command) => self.version_command(command)?,
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
    fn ensure_progress(result: &PersistenceResult) -> Result<(), FlushWritesError> {
        match (result.state(), result.progress) {
            (PersistenceState::Buffered { .. }, FlushProgress::None) => Err(FlushWritesError),
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
        if matches!(
            command.request,
            crate::persistence::dispatch::WriteRequest::PurgeExpiredTitles
        ) {
            self.title_purge_queued
                .store(false, std::sync::atomic::Ordering::Release);
        }
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
        // Finish any active read snapshot before VACUUM or WAL truncation runs on the writer.
        if command.request.requires_idle_reader() {
            while self.runner.is_running() {
                if self.persistence.writer().shutdown().is_some() {
                    let _ = command.reply.send(Err(quartz_protocol::ProtocolError::new(
                        quartz_protocol::ErrorKind::Busy,
                    )));
                    if let Some(wake) = &command.completion {
                        wake.wake();
                    }
                    return Ok(());
                }
                self.runner
                    .step(&mut self.store)
                    .map_err(|source| RunDatabaseLoopError::Query { source })?;
            }
        }
        // Lock contention must return to capture settlement within its commit headroom.
        self.store.limit_write_wait(COMMIT_HEADROOM);
        let settings_changed = matches!(
            command.request,
            crate::persistence::dispatch::WriteRequest::SettingsSet(_)
        );
        let mut result = self.execute_write(command.request, command.settings.as_ref());
        self.store.limit_write_wait(Duration::MAX);
        if result.is_ok()
            && !matches!(
                result,
                Ok(
                    crate::persistence::dispatch::WriteResponse::RuleValidation(_)
                        | crate::persistence::dispatch::WriteResponse::DataInfo(_)
                        | crate::persistence::dispatch::WriteResponse::DataBackup(_)
                )
            )
        {
            self.publish_versions()?;
            if matches!(
                result,
                Ok(crate::persistence::dispatch::WriteResponse::Rules(_))
            ) {
                self.feedback
                    .send(Feedback::RulesChanged)
                    .map_err(|_| RunDatabaseLoopError::Feedback)?;
            }
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
    fn run_startup_title_purge(&mut self) -> Result<(), RunDatabaseLoopError> {
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => {
                tracing::warn!(%error, "could not read clock for startup title retention purge");
                return Ok(());
            }
        };
        self.store.limit_write_wait(COMMIT_HEADROOM);
        let result =
            crate::persistence::data::purge_expired_titles_for_store(&mut self.store, now.utc);
        self.store.limit_write_wait(Duration::MAX);
        match result {
            Ok(_) => self.publish_versions()?,
            Err(error) => {
                tracing::error!(%error, "startup title retention purge failed");
            }
        }
        Ok(())
    }
    fn execute_write(
        &mut self,
        request: crate::persistence::dispatch::WriteRequest,
        settings: Option<&crate::persistence::dispatch::SettingsPorts>,
    ) -> Result<crate::persistence::dispatch::WriteResponse, quartz_protocol::ProtocolError> {
        let now = self.now().map_err(|_| {
            quartz_protocol::ProtocolError::new(quartz_protocol::ErrorKind::Internal)
        })?;
        request.execute(&mut self.store, now.utc, settings, &mut self.registry)
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
                self.database
                    .version_command(
                        self.database
                            .versions
                            .requests
                            .try_recv()
                            .expect("subscription request"),
                    )
                    .expect("service day subscription");
                assert!(matches!(
                    subscription.poll(&mut context),
                    std::task::Poll::Ready(Ok(_))
                ));
            }
            let mut subscription = std::pin::pin!(port.set_summary_day(date));
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(subscription.as_mut().poll(&mut context).is_pending());
            self.database
                .version_command(
                    self.database
                        .versions
                        .requests
                        .try_recv()
                        .expect("tray subscription request"),
                )
                .expect("service tray subscription");
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
                &fixture.database.registry,
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

    impl Fixture {
        fn take_rule_publication(&mut self) {
            assert!(matches!(
                self._feedback.try_recv(),
                Ok(Feedback::RulesChanged)
            ));
            assert!(
                self._feedback.is_empty(),
                "exactly one capture notification"
            );
        }
        fn rule_text(key: &str, kind: &str, parent: &str) -> String {
            format!(
                "format='quartz-rules/1'\n[set]\nkey='{key}'\nkind='{kind}'\nname='Rules'\nparent='{parent}'\n"
            )
        }
    }
    #[test]
    fn privacy_edits_save_and_take_effect_without_preview() {
        let mut fixture = Fixture::new();
        let before = fixture.database.registry.snapshots().load();
        let text = Fixture::rule_text("user.privacy", "privacy", "builtin.privacy")
            + "[[rule]]\nid='hide'\nlabel='Hide'\npreset='title_hidden'\nwhen=[{field='app.exe_name',op='equals',value='secret.exe'}]\n";
        let saved = fixture
            .ipc(
                quartz_protocol::Op::RuleSetsEdit,
                serde_json::json!({"key":"user.privacy","base_version":0,"text":text}),
            )
            .expect("privacy edit saves without receipt");
        assert_eq!(
            saved["sets"],
            serde_json::json!([{"key":"user.privacy","version":1,"deleted":false}])
        );
        fixture.take_rule_publication();
        assert!(
            fixture
                ._versions
                .updates
                .has_changed()
                .expect("version publisher")
        );
        let after = fixture.database.registry.snapshots().load();
        assert_ne!(before.hashes().privacy, after.hashes().privacy);
        let mut fields = quartz_domain::rules::FieldSnapshot::default();
        fields.text.insert(
            quartz_domain::rules::Field::ExeName,
            quartz_domain::text::NfcText::new("secret.exe"),
        );
        assert_eq!(
            after.privacy().evaluate(&fields).privacy.window_title,
            quartz_domain::rules::Action::Drop
        );
    }

    impl Fixture {
        fn rule_digest(&mut self) -> quartz_protocol::QueryVersions {
            crate::rules::read::ReadInputs {
                snapshot: self.database.registry.snapshots().load(),
                metadata_version: self.database.store.rule_read_metadata().expect("metadata"),
            }
            .versions()
        }
        fn assert_rule_failure(
            &mut self,
            op: quartz_protocol::Op,
            body: serde_json::Value,
            kind: quartz_protocol::ErrorKind,
        ) {
            let snapshot = self.database.registry.snapshots().load();
            let stored = self
                .database
                .store
                .rule_sets()
                .load()
                .expect("stored rules");
            let digest = self.rule_digest();
            assert_eq!(
                self.ipc(op, body).expect_err("rejected mutation").kind,
                kind
            );
            assert!(Arc::ptr_eq(
                &snapshot,
                &self.database.registry.snapshots().load()
            ));
            let after = self
                .database
                .store
                .rule_sets()
                .load()
                .expect("unchanged rules");
            assert_eq!(after.sets.len(), stored.sets.len());
            for (key, before) in stored.sets {
                let after = &after.sets[&key];
                assert_eq!(after.current.set, before.current.set);
                assert_eq!(after.current.version, before.current.version);
                assert_eq!(after.current.enabled, before.current.enabled);
                assert_eq!(after.current.sibling_order, before.current.sibling_order);
                assert_eq!(after.deletion, before.deletion);
                assert_eq!(after.versions, before.versions);
            }
            assert_eq!(self.rule_digest(), digest);
            assert!(self._feedback.is_empty());
            assert!(!self._versions.updates.has_changed().expect("publisher"));
        }
        fn save_rule(&mut self, key: &str, kind: &str, parent: &str) {
            self.ipc(quartz_protocol::Op::RuleSetsEdit, serde_json::json!({"key":key,"base_version":0,"text":Self::rule_text(key,kind,parent)})).expect("save set");
            self.take_rule_publication();
            self._versions.updates.borrow_and_update();
        }
    }
    #[test]
    fn failed_rule_transactions_roll_back_and_publish_nothing() {
        use diesel::{Connection, connection::SimpleConnection};
        use quartz_protocol::{ErrorKind, Op};
        let mut fixture = Fixture::new();
        fixture.save_rule("user.a", "classification", "builtin.classification");
        fixture.subscribe_category_day();
        let path = fixture._directory.path().join("quartz.db");
        let mut connection =
            diesel::sqlite::SqliteConnection::establish(path.to_str().expect("database path"))
                .expect("isolated connection");
        connection.batch_execute("CREATE TRIGGER reject_rule_revision BEFORE INSERT ON rule_set_versions WHEN NEW.set_id=(SELECT id FROM rule_sets WHERE key='user.a') BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").expect("fail revision insert");
        fixture.assert_rule_failure(
            Op::RuleSetsSetEnabled,
            serde_json::json!({"key":"user.a","base_version":1,"enabled":false}),
            ErrorKind::Internal,
        );
        connection
            .batch_execute("DROP TRIGGER reject_rule_revision")
            .expect("restore writes");
        fixture
            .ipc(
                Op::RuleSetsSetEnabled,
                serde_json::json!({"key":"user.a","base_version":1,"enabled":false}),
            )
            .expect("retry uncommitted request");
        fixture.take_rule_publication();
    }
    #[test]
    fn classification_commits_publish_once_and_toggle_effective_subtrees() {
        use quartz_domain::rules::{Field, FieldSnapshot};
        use quartz_protocol::Op;
        let mut fixture = Fixture::new();
        fixture.save_rule("user.parent", "classification", "builtin.classification");
        let text = Fixture::rule_text("user.child", "classification", "user.parent")
            + "[[rule]]\nid='test'\nlabel='Test'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='test.exe'}]\n";
        fixture.subscribe_category_day();
        let before = fixture.database.registry.snapshots().load();
        fixture
            .ipc(
                Op::RuleSetsEdit,
                serde_json::json!({"key":"user.child","base_version":0,"text":text}),
            )
            .expect("child classification");
        fixture.take_rule_publication();
        fixture.assert_category_publication();
        let snapshot = fixture.database.registry.snapshots().load();
        assert_ne!(snapshot.epoch(), before.epoch());
        assert_eq!(
            fixture
                .database
                .store
                .rule_sets()
                .load()
                .expect("commit visible")
                .sets[&"user.child".parse().expect("key")]
                .current
                .version,
            1
        );
        let fields = FieldSnapshot {
            text: std::collections::BTreeMap::from([(
                Field::ExeName,
                quartz_domain::text::NfcText::new("test.exe"),
            )]),
            domain: None,
        };
        assert!(snapshot.classification().classify(&fields).is_some());
        fixture
            .ipc(
                Op::RuleSetsSetEnabled,
                serde_json::json!({"key":"user.parent","base_version":1,"enabled":false}),
            )
            .expect("disable parent");
        fixture.take_rule_publication();
        fixture.assert_category_publication();
        assert!(
            fixture
                .database
                .registry
                .snapshots()
                .load()
                .classification()
                .classify(&fields)
                .is_none()
        );
        fixture
            .ipc(
                Op::RuleSetsSetEnabled,
                serde_json::json!({"key":"user.parent","base_version":2,"enabled":true}),
            )
            .expect("enable parent");
        fixture.take_rule_publication();
        fixture.assert_category_publication();
        assert!(
            fixture
                .database
                .registry
                .snapshots()
                .load()
                .classification()
                .classify(&fields)
                .is_some()
        );
    }

    #[test]
    fn invalid_rule_text_returns_bounded_structured_diagnostics_without_publishing() {
        use crate::rules::MAX_RULE_RESPONSE_BYTES;
        use quartz_protocol::{ErrorKind, MAX_RULE_DIAGNOSTICS, Op};
        let mut fixture = Fixture::new();
        let snapshot = fixture.database.registry.snapshots().load();
        let mut text =
            Fixture::rule_text("user.invalid", "classification", "builtin.classification");
        for index in 0..=MAX_RULE_DIAGNOSTICS {
            text.push_str(&format!("[[rule]]\nid='rule{index}'\nlabel='Rule'\ncategory='builtin:learning'\nwhen=[{{field='unavailable',op='equals',value='fixture-private-marker'}}]\n"));
        }
        let error = fixture
            .ipc(
                Op::RuleSetsEdit,
                serde_json::json!({"key":"user.invalid","base_version":0,"text":text}),
            )
            .expect_err("invalid fields");
        let wire = serde_json::to_vec(&error).expect("error frame");
        assert!(wire.len() <= MAX_RULE_RESPONSE_BYTES);
        assert!(
            !String::from_utf8(wire)
                .expect("JSON")
                .contains("fixture-private-marker")
        );
        let ErrorKind::RuleValidationFailed {
            diagnostics,
            truncated,
        } = error.kind
        else {
            panic!("structured diagnostics required")
        };
        assert_eq!(diagnostics.len(), MAX_RULE_DIAGNOSTICS);
        assert!(truncated);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code == "E-RULE-FIELD"
                    && diagnostic.location.is_some()
                    && diagnostic.params.is_empty())
        );
        assert!(Arc::ptr_eq(
            &snapshot,
            &fixture.database.registry.snapshots().load()
        ));
        assert!(fixture._feedback.is_empty());
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
                .resolve_category("builtin:development")
                .is_ok()
        );
        assert!(
            !reopened
                .core()
                .state()
                .references
                .resolve_category(CATEGORY_UUID)
                .is_ok()
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
                    startup_purge: StartupPurge::Done,
                    title_purge_queued: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

    #[test]
    fn data_ops_execute_over_ipc_and_delete_publishes_version_updates() {
        use quartz_domain::capture::limits::UTC_DAY_MS;
        use quartz_protocol::{
            BackupEntry, BackupKind, DataCompactResponse, DataDeleteRangeResponse,
            DataInfoResponse, ErrorKind, Op,
        };
        let mut fixture = Fixture::new();
        // 1970-01-04T12:00:00Z ensures 1970-01-01..1970-01-03 is fully finished in any time zone.
        fixture.database.clock = Arc::new(TestClock(Timestamp {
            utc: UtcInstant::try_from(3 * UTC_DAY_MS + 12 * 3_600_000).expect("clock"),
            mono: MonoInstant::from_millis(0),
        }));
        fixture.subscribe_category_day();

        let annotation = serde_json::json!({
            "entities": [{
                "uuid": "00000000-0000-4000-8000-000000000321",
                "kind": "note",
                "start_utc_ms": UTC_DAY_MS - 3_600_000,
                "end_utc_ms": UTC_DAY_MS + 3_600_000,
                "label": "boundary note"
            }]
        });
        fixture
            .ipc(Op::AnnotationsCreate, annotation)
            .expect("create boundary note");
        fixture.assert_category_publication();

        let backup: BackupEntry = serde_json::from_value(
            fixture
                .ipc(Op::DataBackup, serde_json::json!({}))
                .expect("manual backup"),
        )
        .expect("backup response");
        assert_eq!(backup.kind, BackupKind::Manual);
        assert_eq!(backup.file_name, "manual-19700104T120000Z.db");
        assert_eq!(
            fixture
                .ipc(Op::DataBackup, serde_json::json!({}))
                .expect_err("duplicate manual backup refused")
                .kind,
            ErrorKind::BackupExists
        );

        let info: DataInfoResponse = serde_json::from_value(
            fixture
                .ipc(Op::DataInfo, serde_json::json!({}))
                .expect("data info"),
        )
        .expect("info response");
        assert!(info.database_bytes > 0);
        assert!(info.backups.contains(&backup));

        let deleted: DataDeleteRangeResponse = serde_json::from_value(
            fixture
                .ipc(
                    Op::DataDeleteRange,
                    serde_json::json!({"start_date": "1970-01-01", "end_date": "1970-01-03"}),
                )
                .expect("delete range"),
        )
        .expect("delete response");
        assert_eq!(deleted.deleted_ms, 0);
        assert!(!deleted.versions.0.is_empty());
        fixture.assert_category_publication();

        assert_eq!(
            fixture
                .ipc(
                    Op::DataDeleteRange,
                    serde_json::json!({"start_date": "1970-01-04", "end_date": "1970-01-06"}),
                )
                .expect_err("today/future cannot be deleted")
                .kind,
            ErrorKind::BadRequest
        );

        let compact: DataCompactResponse = serde_json::from_value(
            fixture
                .ipc(Op::DataCompact, serde_json::json!({}))
                .expect("compact"),
        )
        .expect("compact response");
        assert!(compact.before_bytes > 0);
        assert!(compact.after_bytes > 0);
    }
}
