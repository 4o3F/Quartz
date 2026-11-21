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
    writes: Sender<TestWrite>,
    thread: Option<JoinHandle<()>>,
}
impl TestDatabase {
    pub(crate) fn start() -> Self {
        Self::with_fixture(QueryFixture::new(4))
    }
    pub(crate) fn with_fixture(mut fixture: QueryFixture) -> Self {
        let channel = QueryChannel::new();
        let snapshots = crate::rules::registry::RegistryShell::open(
            &mut fixture.store,
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("rules")
        .snapshots();
        let crate::query::versions::VersionChannel {
            mut publisher,
            port: versions,
        } = VersionPublisher::open(&mut fixture.store, snapshots).expect("versions");
        let (writes, receive) = bounded(QUERY_CHANNEL_CAPACITY);
        let thread = std::thread::spawn(move || {
            loop {
                crossbeam_channel::select_biased! {
                    recv(receive) -> write => match write {
                        Ok(TestWrite::Append { reply }) => {
                            fixture.append_fact();
                            publisher.publish(&mut fixture.store);
                            reply.send(()).expect("write receipt");
                        }
                        Ok(TestWrite::Stop) | Err(_) => return,
                    },
                    recv(publisher.requests) -> request => match request { Ok(request) => publisher.receive(request), Err(_) => return },
                    recv(channel.receiver) -> query => match query {
                        Ok(query) => { let result = fixture.run(query.request).map_err(|error| ProtocolError::new(error.kind())); let _ = query.reply.send(result); }
                        Err(_) => return,
                    }
                }
            }
        });
        Self {
            port: channel.port,
            versions,
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
                .try_send(QueryCommand {
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
