//! One writer actor per connection, with byte admission before mailbox insertion.

use futures_util::SinkExt;
use quartz_protocol::{
    ControlBody, Envelope, Event, EventSeq, Response, StateSnapshot, Topic, encode_json,
    frame_codec,
};
use std::{
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

/// Actual writer progress can lag registry publication, so each writer enforces spacing too.
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
    async fn deadline(&self) {
        match self.next {
            Some(next) => tokio::time::sleep_until(next).await,
            None => std::future::pending().await,
        }
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

/// A bounded latest-value slot; replacement discards the previous unsent value.
#[derive(Debug, Clone)]
struct LatestSlot<T>(Option<T>);
impl<T> Default for LatestSlot<T> {
    fn default() -> Self {
        Self(None)
    }
}
impl<T> LatestSlot<T> {
    fn replace(&mut self, value: T) {
        self.0 = Some(value);
    }
    #[cfg(test)]
    fn take(&mut self) -> Option<T> {
        self.0.take()
    }
}

#[derive(Debug, Clone, Default)]
struct LatestEvents {
    state: LatestSlot<StateSnapshot>,
    // TODO(M2-C): one latest slot per subscribed day, keyed by date, carrying the fact version.
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
    pub(crate) fn state(&self, snapshot: StateSnapshot) {
        self.latest
            .send_modify(|latest| latest.state.replace(snapshot));
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
    task: JoinHandle<io::Result<()>>,
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
        Self { handle, task }
    }
    pub(crate) async fn finish(mut self) -> io::Result<()> {
        self.handle
            .enqueue(OutboxMessage::Finish)
            .map_err(io::Error::other)?;
        (&mut self.task).await.map_err(io::Error::other)?
    }
}
impl Drop for OutboxTask {
    fn drop(&mut self) {
        self.handle.close();
        self.task.abort();
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
    fn next_sequence(&mut self) -> io::Result<EventSeq> {
        self.sequence = EventSeq(
            self.sequence
                .0
                .checked_add(1)
                .ok_or_else(|| io::Error::other("event sequence exhausted"))?,
        );
        Ok(self.sequence)
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
            let state_pending = self.snapshots.has_changed().map_err(io::Error::other)?;
            if state_pending && self.state_clock.ready(tokio::time::Instant::now()) {
                let state = self.snapshots.borrow_and_update().state.0.clone();
                if let Some(state) = state {
                    let seq = self.next_sequence()?;
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
            tokio::select! {
                message = self.receiver.recv() => {
                    let Some(message) = message else { return Ok(()); };
                    if self.reliable(message, &mut writer).await? == WriteProgress::Finished { return Ok(()); }
                }
                () = self.state_clock.deadline(), if state_pending => {},
                changed = self.snapshots.changed(), if !state_pending => {
                    changed.map_err(io::Error::other)?;
                    self.snapshots.mark_changed();
                }
            }
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
                let seq = self.next_sequence()?;
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
    fn latest_slot_replaces_instead_of_growing() {
        let mut slot = LatestSlot::default();
        for value in 0..1000 {
            slot.replace(value);
        }
        assert_eq!(slot.take(), Some(999));
        assert_eq!(slot.take(), None);
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
            // The first response holds up the state past the registry's publication deadline.
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
}
