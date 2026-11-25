//! Temporary database worker for real pipe tests; no production paths are used.

use super::*;
use crate::{
    query::versions::{VersionPort, VersionPublisher},
    store::query::test_support::QueryFixture,
};
use std::thread::JoinHandle;

enum TestWrite {
    Append { reply: Sender<()> },
    Stop,
}
pub(crate) struct TestDatabase {
    pub port: QueryPort,
    pub versions: VersionPort,
    pub mutations: crate::persistence::dispatch::WritePort,
    writes: Sender<TestWrite>,
    thread: Option<JoinHandle<()>>,
}
impl TestDatabase {
    pub(crate) fn start() -> Self {
        Self::with_fixture(QueryFixture::new(4))
    }
    pub(crate) fn with_fixture(mut fixture: QueryFixture) -> Self {
        let channel = QueryChannel::new();
        let mutations = crate::persistence::dispatch::WriteChannel::new();
        let mut registry = crate::rules::registry::RegistryShell::open(
            &mut fixture.store,
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("rules");
        let snapshots = registry.snapshots();
        let crate::query::versions::VersionChannel {
            mut publisher,
            port: versions,
        } = VersionPublisher::open(&mut fixture.store, snapshots.clone()).expect("versions");
        let (writes, receive) = bounded(QUERY_CHANNEL_CAPACITY);
        let thread = std::thread::spawn(move || {
            loop {
                crossbeam_channel::select_biased! {
                    recv(receive) -> write => match write {
                        Ok(TestWrite::Append { reply }) => {
                            fixture.append_fact();
                            publisher.publish(&mut fixture.store).expect("publish versions");
                            reply.send(()).expect("write receipt");
                        }
                        Ok(TestWrite::Stop) | Err(_) => return,
                    },
                    recv(mutations.receiver) -> command => match command {
                        Ok(command) => {
                            let result = command.request.execute(&mut fixture.store, quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"), None, &mut registry);
                            publisher.publish(&mut fixture.store).expect("publish committed mutation");
                            let _ = command.reply.send(result);
                        }
                        Err(_) => return,
                    },
                    recv(publisher.requests) -> request => match request { Ok(request) => publisher.receive(request), Err(_) => return },
                    recv(channel.receiver) -> query => match query {
                        Ok(QueryCommand::Query {request, reply}) => { let result = fixture.run(request).map_err(|error| ProtocolError::new(error.kind())); let _ = reply.send(result); }
                        Ok(QueryCommand::RuleInputs {reply}) => { let result = fixture.store.rule_read_metadata().map(|metadata_version| crate::rules::read::ReadInputs {snapshot: snapshots.load(), metadata_version}).map_err(|_| ProtocolError::new(ErrorKind::Internal)); let _ = reply.send(result); }
                        Ok(command @ (QueryCommand::Explain {..} | QueryCommand::Preview {..} | QueryCommand::Controlled {..})) => {
                            let mut runner = crate::query::runner::QueryRunner::default();
                            runner.start(&mut fixture.store, command, &registry).expect("start controlled query");
                            while runner.is_running() { runner.step(&mut fixture.store).expect("step controlled query"); }
                        }
                        Err(_) => return,
                    }
                }
            }
        });
        Self {
            port: channel.port,
            versions,
            mutations: mutations.port,
            writes,
            thread: Some(thread),
        }
    }
    pub(crate) fn append(&self) {
        let (reply, received) = bounded(1);
        self.writes
            .send(TestWrite::Append { reply })
            .expect("submit write");
        received.recv().expect("write committed");
    }
}
impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = self.writes.send(TestWrite::Stop);
        self.thread
            .take()
            .expect("database thread")
            .join()
            .expect("database thread completed");
    }
}
impl QueryChannel {
    pub(crate) fn saturate(&self, request: QueryRequest) {
        for _ in 0..QUERY_CHANNEL_CAPACITY {
            let (reply, _) = oneshot::channel();
            self.port
                .sender
                .try_send(QueryCommand::Query {
                    request: request.clone(),
                    reply,
                })
                .expect("fill query queue");
        }
    }
}

impl QueryPort {
    pub(crate) fn queued_requests(&self) -> usize {
        self.sender.len()
    }
}
