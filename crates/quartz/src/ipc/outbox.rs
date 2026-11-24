//! One writer actor per connection, with byte admission before mailbox insertion.

use futures_util::SinkExt;
use quartz_protocol::{
    ControlBody, Envelope, Event, EventSeq, Response, StateSnapshot, Topic, encode_json,
    frame_codec,
};
use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncWrite,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::codec::FramedWrite;

/// Per-connection encoded-byte budget, including headers and in-flight writes (04 §7).
pub(crate) const SEND_BUFFER_LIMIT: usize = 1024 * 1024;
/// Minimum spacing of completed state writes per connection (04 §7).
pub(crate) const STATE_PUSH_INTERVAL: Duration = Duration::from_millis(250);
/// Minimum spacing of completed day writes per date (04 §7).
pub(crate) const DAY_PUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Measures state spacing from completed writes.
#[derive(Debug, Default)]
struct StatePushClock {
    next: Option<tokio::time::Instant>,
}
impl StatePushClock {
    fn ready(&self, now: tokio::time::Instant) -> bool {
        self.next.is_none_or(|next| now >= next)
    }
    fn sent(&mut self, now: tokio::time::Instant) {
        self.next = Some(now + STATE_PUSH_INTERVAL);
    }
}

/// Every encoded frame includes a four-byte length field (04 §2).
const FRAME_HEADER_LEN: usize = std::mem::size_of::<u32>();
/// Byte admission permits at most this many frames; one extra entry is for Finish (04 §7).
const OUTBOX_MAILBOX_CAPACITY: usize = SEND_BUFFER_LIMIT / FRAME_HEADER_LEN + 1;

#[derive(Debug, Default)]
struct SendBudget {
    queued: usize,
    peak: usize,
}
impl SendBudget {
    fn reserve(&mut self, bytes: usize) -> Result<(), QueueFrameError> {
        let total = self
            .queued
            .checked_add(bytes)
            .filter(|total| *total <= SEND_BUFFER_LIMIT)
            .ok_or(QueueFrameError::Budget)?;
        self.queued = total;
        self.peak = self.peak.max(total);
        Ok(())
    }
    fn release(&mut self, bytes: usize) {
        self.queued -= bytes;
    }
}

/// Releases accounting on successful write, cancellation, or mailbox destruction.
struct Reservation {
    budget: Arc<Mutex<SendBudget>>,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget
            .lock()
            .expect("outbox budget lock poisoned")
            .release(self.bytes);
    }
}

#[derive(Debug, snafu::Snafu)]
pub(crate) enum QueueFrameError {
    #[snafu(display("connection send budget exhausted"))]
    Budget,
    #[snafu(display("connection outbox is closed"))]
    Closed,
    #[snafu(display("could not encode outgoing envelope"))]
    Encode {
        source: quartz_protocol::EncodeFrameError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutboxLife {
    Open,
    Closed,
}

#[derive(Debug, Clone, Default)]
struct LatestEvents {
    state: Option<StateSnapshot>,
    days: BTreeMap<quartz_protocol::LocalDate, quartz_protocol::DayChanged>,
    day_clocks: BTreeMap<quartz_protocol::LocalDate, Option<tokio::time::Instant>>,
}

/// Reliable mailbox entries own their byte reservations before enqueueing.
enum OutboxMessage {
    Response {
        payload: Vec<u8>,
        reservation: Reservation,
    },
    Control {
        body: ControlBody,
        reservation: Reservation,
    },
    Finish,
}

#[derive(Clone)]
pub(crate) struct OutboxHandle {
    mailbox: mpsc::Sender<OutboxMessage>,
    latest: watch::Sender<LatestEvents>,
    life: watch::Sender<OutboxLife>,
    budget: Arc<Mutex<SendBudget>>,
}
impl OutboxHandle {
    fn reserve(&self, payload_len: usize) -> Result<Reservation, QueueFrameError> {
        if self.is_closed() {
            return Err(QueueFrameError::Closed);
        }
        let bytes = payload_len + FRAME_HEADER_LEN;
        let result = self
            .budget
            .lock()
            .expect("outbox budget lock poisoned")
            .reserve(bytes);
        if result.is_err() {
            self.close();
        }
        result?;
        Ok(Reservation {
            budget: Arc::clone(&self.budget),
            bytes,
        })
    }
    fn enqueue(&self, message: OutboxMessage) -> Result<(), QueueFrameError> {
        self.mailbox.try_send(message).map_err(|error| {
            tracing::debug!(%error, "outbox mailbox unavailable; closing connection");
            self.close();
            QueueFrameError::Closed
        })
    }
    pub(crate) fn response(&self, response: Response) -> Result<(), QueueFrameError> {
        let payload = encode_json(&Envelope::Response(response))
            .map_err(|source| QueueFrameError::Encode { source })?;
        let reservation = self.reserve(payload.len())?;
        self.enqueue(OutboxMessage::Response {
            payload,
            reservation,
        })
    }
    pub(crate) fn control(&self, body: ControlBody) -> Result<(), QueueFrameError> {
        // Reserve the maximum sequence width; actual wire bytes can only be smaller.
        let payload = Self::event(Topic::Control, EventSeq(u64::MAX), &body)?;
        let reservation = self.reserve(payload.len())?;
        self.enqueue(OutboxMessage::Control { body, reservation })
    }
    pub(crate) fn unsubscribe(&self, topic: Topic) {
        self.latest.send_modify(|latest| match topic {
            Topic::State => latest.state = None,
            Topic::Day(date) => {
                latest.days.remove(&date);
                latest.day_clocks.remove(&date);
            }
            Topic::Control => {}
        });
    }
    pub(crate) fn state(&self, snapshot: StateSnapshot) {
        self.latest
            .send_modify(|latest| latest.state = Some(snapshot));
    }
    pub(crate) fn day(&self, changed: quartz_protocol::DayChanged) {
        self.latest.send_modify(|latest| {
            latest.day_clocks.entry(changed.date).or_default();
            latest.days.insert(changed.date, changed);
        });
    }
    pub(crate) fn close(&self) {
        self.life.send_replace(OutboxLife::Closed);
    }
    pub(crate) fn is_closed(&self) -> bool {
        *self.life.borrow() == OutboxLife::Closed
    }
    pub(crate) async fn closed(&self) {
        let mut life = self.life.subscribe();
        if *life.borrow_and_update() == OutboxLife::Closed {
            return;
        }
        if life.changed().await.is_err() {
            tracing::debug!("outbox lifetime sender disappeared");
        }
    }
    pub(crate) fn queued_bytes(&self) -> usize {
        self.budget
            .lock()
            .expect("outbox budget lock poisoned")
            .queued
    }
    #[cfg(test)]
    pub(crate) fn peak_bytes(&self) -> usize {
        self.budget
            .lock()
            .expect("outbox budget lock poisoned")
            .peak
    }
    fn event(
        topic: Topic,
        seq: EventSeq,
        body: &impl serde::Serialize,
    ) -> Result<Vec<u8>, QueueFrameError> {
        let body = serde_json::to_value(body).map_err(|source| QueueFrameError::Encode {
            source: quartz_protocol::EncodeFrameError::Json { source },
        })?;
        encode_json(&Envelope::Event(Event { topic, seq, body }))
            .map_err(|source| QueueFrameError::Encode { source })
    }
}

pub(crate) struct OutboxTask {
    pub(crate) handle: OutboxHandle,
    task: Option<JoinHandle<io::Result<()>>>,
}
impl OutboxTask {
    pub(crate) fn spawn(write: impl AsyncWrite + Unpin + Send + 'static) -> Self {
        let (mailbox, receiver) = mpsc::channel(OUTBOX_MAILBOX_CAPACITY);
        let (latest, snapshots) = watch::channel(LatestEvents::default());
        let (life, _) = watch::channel(OutboxLife::Open);
        let handle = OutboxHandle {
            mailbox,
            latest,
            life,
            budget: Arc::new(Mutex::new(SendBudget::default())),
        };
        let actor = OutboxActor {
            receiver,
            snapshots,
            handle: handle.clone(),
            sequence: EventSeq(0),
            state_clock: StatePushClock::default(),
        };
        let task = tokio::spawn(actor.run(write));
        Self {
            handle,
            task: Some(task),
        }
    }
    pub(crate) async fn finish(&mut self) -> io::Result<()> {
        self.handle
            .enqueue(OutboxMessage::Finish)
            .map_err(io::Error::other)?;
        let result = self.task.as_mut().expect("outbox has not finished").await;
        self.task.take();
        result.map_err(io::Error::other)?
    }
    /// Waits for cancellation to release the transport before its slot is replaced.
    pub(crate) async fn stop(&mut self) -> io::Result<()> {
        self.handle.close();
        if let Some(task) = self.task.take() {
            task.abort();
            match task.await {
                Ok(result) => result?,
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        Ok(())
    }
}
impl Drop for OutboxTask {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct OutboxActor {
    receiver: mpsc::Receiver<OutboxMessage>,
    snapshots: watch::Receiver<LatestEvents>,
    handle: OutboxHandle,
    sequence: EventSeq,
    state_clock: StatePushClock,
}
impl OutboxActor {
    fn next_sequence(&mut self) -> EventSeq {
        self.sequence = EventSeq(
            self.sequence
                .0
                .checked_add(1)
                .expect("u64 counter cannot overflow in practice"),
        );
        self.sequence
    }
    async fn run(mut self, write: impl AsyncWrite + Unpin) -> io::Result<()> {
        let result = self.write(write).await;
        self.handle.close();
        result
    }
    async fn write(&mut self, write: impl AsyncWrite + Unpin) -> io::Result<()> {
        let mut writer = FramedWrite::new(write, frame_codec());
        loop {
            // Reliable frames always take precedence. Latest values remain unencoded until FIFO is empty.
            match self.receiver.try_recv() {
                Ok(message) => {
                    if self.reliable(message, &mut writer).await? == WriteProgress::Finished {
                        return Ok(());
                    }
                    continue;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            let pending = self.snapshots.borrow_and_update().clone();
            let now = tokio::time::Instant::now();
            if pending.state.is_some() && self.state_clock.ready(now) {
                let mut state = None;
                self.handle.latest.send_if_modified(|latest| {
                    state = latest.state.take();
                    state.is_some()
                });
                if let Some(state) = state {
                    let seq = self.next_sequence();
                    let payload =
                        OutboxHandle::event(Topic::State, seq, &state).map_err(io::Error::other)?;
                    let reservation = self
                        .handle
                        .reserve(payload.len())
                        .map_err(io::Error::other)?;
                    writer.send(payload.as_slice()).await?;
                    self.state_clock.sent(tokio::time::Instant::now());
                    drop(reservation);
                }
                continue;
            }
            let due_date = pending
                .days
                .keys()
                .find(|date| {
                    pending
                        .day_clocks
                        .get(date)
                        .copied()
                        .flatten()
                        .is_none_or(|next| now >= next)
                })
                .copied();
            if let Some(date) = due_date {
                let mut changed = None;
                self.handle.latest.send_if_modified(|latest| {
                    changed = latest.days.remove(&date);
                    changed.is_some()
                });
                if let Some(changed) = changed {
                    let seq = self.next_sequence();
                    let payload = OutboxHandle::event(Topic::Day(date), seq, &changed)
                        .map_err(io::Error::other)?;
                    let reservation = self
                        .handle
                        .reserve(payload.len())
                        .map_err(io::Error::other)?;
                    let scheduled = tokio::time::Instant::now() + DAY_PUSH_INTERVAL;
                    self.handle.latest.send_modify(|latest| {
                        latest.day_clocks.insert(date, Some(scheduled));
                    });
                    writer.send(payload.as_slice()).await?;
                    self.handle.latest.send_modify(|latest| {
                        if latest.day_clocks.get(&date) == Some(&Some(scheduled)) {
                            latest.day_clocks.insert(
                                date,
                                Some(tokio::time::Instant::now() + DAY_PUSH_INTERVAL),
                            );
                        }
                    });
                    drop(reservation);
                }
                continue;
            }
            let deadline = pending
                .day_clocks
                .iter()
                .filter(|entry| pending.days.contains_key(entry.0))
                .filter_map(|entry| *entry.1)
                .chain(pending.state.as_ref().and(self.state_clock.next))
                .min();
            tokio::select! {
                message = self.receiver.recv() => {
                    let Some(message) = message else { return Ok(()); };
                    if self.reliable(message, &mut writer).await? == WriteProgress::Finished { return Ok(()); }
                }
                () = Self::deadline(deadline) => {},
                changed = self.snapshots.changed() => { changed.map_err(io::Error::other)?; }
            }
        }
    }
    async fn deadline(deadline: Option<tokio::time::Instant>) {
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    }
    async fn reliable<W: AsyncWrite + Unpin>(
        &mut self,
        message: OutboxMessage,
        writer: &mut FramedWrite<W, tokio_util::codec::LengthDelimitedCodec>,
    ) -> io::Result<WriteProgress> {
        match message {
            OutboxMessage::Response {
                payload,
                reservation,
            } => {
                writer.send(payload.as_slice()).await?;
                drop(reservation);
            }
            OutboxMessage::Control { body, reservation } => {
                let seq = self.next_sequence();
                let payload =
                    OutboxHandle::event(Topic::Control, seq, &body).map_err(io::Error::other)?;
                writer.send(payload.as_slice()).await?;
                drop(reservation);
            }
            OutboxMessage::Finish => return Ok(WriteProgress::Finished),
        }
        Ok(WriteProgress::Continue)
    }
}
#[derive(Debug, PartialEq, Eq)]
enum WriteProgress {
    Continue,
    Finished,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn unsubscribe_discards_throttle_and_resubscribe_pushes_immediately() {
        use futures_util::StreamExt;
        let (write, read) = tokio::io::duplex(4096);
        let writer = OutboxTask::spawn(write);
        let date = "2025-01-02".parse().expect("date");
        let changed = quartz_protocol::DayChanged {
            date,
            versions: quartz_protocol::QueryVersions("initial".into()),
        };
        let mut reader = tokio_util::codec::FramedRead::new(read, frame_codec());
        writer.handle.day(changed.clone());
        reader.next().await.expect("first event").expect("frame");
        tokio::task::yield_now().await;
        assert!(writer.handle.latest.borrow().day_clocks.contains_key(&date));
        writer.handle.unsubscribe(Topic::Day(date));
        assert!(
            writer.handle.latest.borrow().day_clocks.is_empty(),
            "unsubscribed dates retain no throttle state"
        );
        let before = tokio::time::Instant::now();
        writer.handle.day(changed);
        reader
            .next()
            .await
            .expect("new subscription snapshot")
            .expect("frame");
        assert_eq!(
            tokio::time::Instant::now(),
            before,
            "new subscription does not inherit old throttle"
        );
    }

    #[test]
    fn exact_budget_fits_and_one_more_byte_is_rejected_without_growth() {
        let mut budget = SendBudget::default();
        budget.reserve(SEND_BUFFER_LIMIT).expect("exact limit");
        assert!(matches!(budget.reserve(1), Err(QueueFrameError::Budget)));
        assert_eq!(budget.queued, SEND_BUFFER_LIMIT);
        budget.release(SEND_BUFFER_LIMIT);
        assert_eq!(budget.queued, 0);
    }
    #[test]
    fn responses_and_controls_reserve_before_mailbox_and_close_only_their_outbox() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let stalled = OutboxTask::spawn(tokio::io::sink());
            let healthy = OutboxTask::spawn(tokio::io::sink());
            let empty = Response {
                id: Some(quartz_protocol::RequestId(1)),
                outcome: quartz_protocol::ResponseOutcome::Ok {
                    body: serde_json::json!(""),
                },
            };
            let overhead = encode_json(&Envelope::Response(empty))
                .expect("empty response")
                .len();
            let payload_len = SEND_BUFFER_LIMIT / 4 - FRAME_HEADER_LEN;
            for _ in 0..4 {
                stalled
                    .handle
                    .response(Response {
                        id: Some(quartz_protocol::RequestId(1)),
                        outcome: quartz_protocol::ResponseOutcome::Ok {
                            body: serde_json::json!("x".repeat(payload_len - overhead)),
                        },
                    })
                    .expect("fits exactly");
            }
            assert_eq!(stalled.handle.queued_bytes(), SEND_BUFFER_LIMIT);
            let control = ControlBody {
                op: quartz_protocol::ControlOp::UiShutdown,
                fields: serde_json::Map::new(),
            };
            assert!(matches!(
                stalled.handle.control(control.clone()),
                Err(QueueFrameError::Budget)
            ));
            assert!(stalled.handle.is_closed());
            assert!(!healthy.handle.is_closed());
            healthy.handle.control(control).expect("other outbox works");
            assert_eq!(stalled.handle.peak_bytes(), SEND_BUFFER_LIMIT);
            let handle = stalled.handle.clone();
            drop(stalled);
            tokio::task::yield_now().await;
            assert_eq!(
                handle.queued_bytes(),
                0,
                "cancellation releases every reservation"
            );
        });
    }
    #[test]
    fn delayed_state_write_does_not_burst_and_pending_state_keeps_only_the_latest() {
        use futures_util::StreamExt;
        use tokio_util::codec::FramedRead;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            tokio::time::pause();
            let (write, read) = tokio::io::duplex(1);
            let writer = OutboxTask::spawn(write);
            let publisher = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId(
                "outbox-clock".into(),
            ));
            let snapshot = publisher.subscribe().borrow().snapshot();
            writer
                .handle
                .response(Response {
                    id: Some(quartz_protocol::RequestId(1)),
                    outcome: quartz_protocol::ResponseOutcome::Ok {
                        body: serde_json::json!({}),
                    },
                })
                .expect("response before state");
            writer.handle.state(snapshot.clone());
            tokio::task::yield_now().await;
            // The first response delays the initial state write.
            tokio::time::advance(STATE_PUSH_INTERVAL).await;
            let mut reader = FramedRead::new(read, frame_codec());
            let response = reader
                .next()
                .await
                .expect("response")
                .expect("response frame");
            assert!(matches!(
                quartz_protocol::decode_json::<Envelope>(&response).expect("response JSON"),
                Envelope::Response(_)
            ));
            let first = reader
                .next()
                .await
                .expect("first state")
                .expect("state frame");
            let Envelope::Event(first) =
                quartz_protocol::decode_json::<Envelope>(&first).expect("state JSON")
            else {
                panic!("state event");
            };
            let completed = tokio::time::Instant::now();
            let mut latest = snapshot;
            latest.stop_reason = Some(quartz_protocol::StopReason::Locked);
            writer.handle.state(latest.clone());
            latest.stop_reason = Some(quartz_protocol::StopReason::Paused);
            writer.handle.state(latest.clone());
            assert!(
                tokio::time::timeout(STATE_PUSH_INTERVAL / 2, reader.next())
                    .await
                    .is_err(),
                "late writer must not emit two states together"
            );
            let last = reader
                .next()
                .await
                .expect("latest state")
                .expect("state frame");
            let Envelope::Event(last) =
                quartz_protocol::decode_json::<Envelope>(&last).expect("state JSON")
            else {
                panic!("state event");
            };
            assert!(tokio::time::Instant::now() - completed >= STATE_PUSH_INTERVAL);
            assert!(last.seq > first.seq);
            assert_eq!(
                serde_json::from_value::<StateSnapshot>(last.body).expect("state body"),
                latest
            );
            assert!(writer.handle.peak_bytes() <= SEND_BUFFER_LIMIT);
        });
    }
    #[test]
    fn delayed_day_writes_keep_latest_versions_and_throttle_each_date_independently() {
        use futures_util::StreamExt;
        use quartz_protocol::{DayChanged, QueryVersions};
        use tokio_util::codec::FramedRead;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            tokio::time::pause();
            let (write, read) = tokio::io::duplex(1);
            let writer = OutboxTask::spawn(write);
            let date = "2025-01-02".parse().expect("first date");
            let other_date = "2025-01-03".parse().expect("other date");
            let initial = DayChanged {
                date,
                versions: QueryVersions("initial".into()),
            };
            writer.handle.day(initial.clone());
            tokio::task::yield_now().await;
            tokio::time::advance(DAY_PUSH_INTERVAL).await;
            let mut reader = FramedRead::new(read, frame_codec());
            let first = reader
                .next()
                .await
                .expect("initial day")
                .expect("day frame");
            let Envelope::Event(first) =
                quartz_protocol::decode_json::<Envelope>(&first).expect("day JSON")
            else {
                panic!("day event");
            };
            assert_eq!(
                serde_json::from_value::<DayChanged>(first.body).expect("day body"),
                initial
            );
            tokio::task::yield_now().await;
            let completed = tokio::time::Instant::now();
            writer.handle.day(DayChanged {
                date,
                versions: QueryVersions("intermediate".into()),
            });
            let latest = DayChanged {
                date,
                versions: QueryVersions("latest".into()),
            };
            writer.handle.day(latest.clone());
            writer.handle.day(DayChanged {
                date: other_date,
                versions: QueryVersions("other".into()),
            });
            let other = reader
                .next()
                .await
                .expect("independent date")
                .expect("day frame");
            let Envelope::Event(other) =
                quartz_protocol::decode_json::<Envelope>(&other).expect("day JSON")
            else {
                panic!("day event");
            };
            assert_eq!(other.topic, Topic::Day(other_date));
            assert_eq!(
                tokio::time::Instant::now(),
                completed,
                "one date must not delay another"
            );
            assert!(
                tokio::time::timeout(DAY_PUSH_INTERVAL / 2, reader.next())
                    .await
                    .is_err(),
                "spacing starts when the delayed write completes"
            );
            let last = reader.next().await.expect("latest day").expect("day frame");
            let Envelope::Event(last) =
                quartz_protocol::decode_json::<Envelope>(&last).expect("day JSON")
            else {
                panic!("day event");
            };
            assert!(tokio::time::Instant::now() - completed >= DAY_PUSH_INTERVAL);
            assert!(first.seq < other.seq && other.seq < last.seq);
            assert_eq!(
                serde_json::from_value::<DayChanged>(last.body).expect("day body"),
                latest
            );
            assert!(
                tokio::time::timeout(DAY_PUSH_INTERVAL, reader.next())
                    .await
                    .is_err(),
                "intermediate versions are discarded"
            );
        });
    }
}
