//! Temporary database worker for real pipe tests; no production paths are used.

use super::*;
use crate::{query::versions::VersionState, store::query::test_support::QueryFixture};
use std::thread::JoinHandle;
use tokio::sync::watch;

enum TestWrite {
    Append { reply: Sender<()> },
    Stop,
}
pub(crate) struct TestDatabase {
    pub port: QueryPort,
    pub versions: watch::Receiver<VersionState>,
    writes: Sender<TestWrite>,
    thread: Option<JoinHandle<()>>,
}
impl TestDatabase {
    pub(crate) fn start() -> Self {
        let mut fixture = QueryFixture::new(4);
        let channel = QueryChannel::new();
        let state = fixture
            .store
            .version_state("fixture-classification-v1")
            .expect("initial versions");
        let (publisher, versions) = watch::channel(state);
        let (writes, receive) = bounded(QUERY_CHANNEL_CAPACITY);
        let thread = std::thread::spawn(move || {
            loop {
                crossbeam_channel::select_biased! {
                    recv(receive) -> write => match write {
                        Ok(TestWrite::Append { reply }) => {
                            fixture.append_fact();
                            let state = fixture.store.version_state("fixture-classification-v1").expect("committed versions");
                            publisher.send_replace(state);
                            reply.send(()).expect("write receipt");
                        }
                        Ok(TestWrite::Stop) | Err(_) => return,
                    },
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
