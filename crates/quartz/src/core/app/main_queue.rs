//! Bounded main-thread notifications with replaceable values and reserved fatal delivery.

use super::{MainCommand, SendMainCommandError};
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, Weak},
};

struct Pending {
    commands: VecDeque<MainCommand>,
    fatal: Option<super::Worker>,
    capacity: usize,
    life: super::CaptureLife,
}
pub(super) struct MainChannel {
    pub sender: MainSender,
    pub receiver: MainReceiver,
}
#[derive(Clone)]
pub(super) struct MainSender {
    pending: Weak<Mutex<Pending>>,
    ready: Sender<()>,
}
#[derive(Clone)]
pub(crate) struct MainReceiver {
    pending: Arc<Mutex<Pending>>,
    ready: Receiver<()>,
}
impl MainChannel {
    pub(super) fn new(capacity: usize) -> Self {
        let pending = Arc::new(Mutex::new(Pending {
            commands: VecDeque::new(),
            fatal: None,
            capacity,
            life: super::CaptureLife::default(),
        }));
        let (sender, receiver) = crossbeam_channel::bounded(super::CONTROL_CAPACITY);
        Self {
            sender: MainSender {
                pending: Arc::downgrade(&pending),
                ready: sender,
            },
            receiver: MainReceiver {
                pending,
                ready: receiver,
            },
        }
    }
}
#[derive(PartialEq, Eq)]
enum ReplacementKey {
    Clock,
    DayOffset,
    Policy,
    Capture,
    Persistence,
    Timer,
    Hooks,
    Review,
    FallbackReview,
    Session,
    Idle,
    Discard,
}
impl MainCommand {
    fn replacement_key(&self) -> Option<ReplacementKey> {
        use quartz_domain::capture::CaptureEffect;
        match self {
            Self::ClockChanged => Some(ReplacementKey::Clock),
            Self::DayOffsetChanged { .. } => Some(ReplacementKey::DayOffset),
            Self::PolicyChanged => Some(ReplacementKey::Policy),
            Self::CaptureSnapshot { .. } => Some(ReplacementKey::Capture),
            Self::Persistence(_) => Some(ReplacementKey::Persistence),
            Self::Effect(effect) => match effect {
                CaptureEffect::ScheduleTimer(_) => Some(ReplacementKey::Timer),
                CaptureEffect::RebindHooks { .. } => Some(ReplacementKey::Hooks),
                CaptureEffect::ReviewForeground => Some(ReplacementKey::Review),
                CaptureEffect::FallbackReviewForeground => Some(ReplacementKey::FallbackReview),
                CaptureEffect::QuerySession => Some(ReplacementKey::Session),
                CaptureEffect::CheckIdle => Some(ReplacementKey::Idle),
                CaptureEffect::DiscardQueuedObservations => Some(ReplacementKey::Discard),
                _ => None,
            },
            _ => None,
        }
    }
    fn replaces(&self, previous: &Self) -> bool {
        self.replacement_key()
            .is_some_and(|key| Some(key) == previous.replacement_key())
    }
}
impl MainSender {
    pub(super) fn send(&self, command: MainCommand) -> Result<(), SendMainCommandError> {
        let shared = self
            .pending
            .upgrade()
            .ok_or(SendMainCommandError::Disconnected)?;
        let mut pending = shared
            .lock()
            .expect("main mailbox contains bounded memory mutations only");
        if let MainCommand::Fatal(worker) = command {
            pending.life.stop();
            pending.fatal.get_or_insert(worker);
        } else {
            // Append replacements at the tail to retain order relative to intervening effects.
            if let Some(index) = pending
                .commands
                .iter()
                .position(|previous| command.replaces(previous))
            {
                pending.commands.remove(index);
            } else if pending.commands.len() == pending.capacity {
                return Err(SendMainCommandError::Full);
            }
            pending.commands.push_back(command);
        }
        drop(pending);
        match self.ready.try_send(()) {
            Ok(()) | Err(crossbeam_channel::TrySendError::Full(())) => Ok(()),
            Err(crossbeam_channel::TrySendError::Disconnected(())) => {
                Err(SendMainCommandError::Disconnected)
            }
        }
    }
}
#[cfg(test)]
use std::time::Instant;

impl MainReceiver {
    #[cfg(test)]
    pub(crate) fn fatal_for_test(worker: super::Worker) -> Self {
        let channel = MainChannel::new(1);
        channel
            .sender
            .send(MainCommand::Fatal(worker))
            .expect("test fatal delivery");
        channel.receiver
    }
    pub(super) fn capture_life(&self) -> super::CaptureLife {
        self.pending.lock().expect("main mailbox lock").life.clone()
    }
    pub(crate) fn has_fatal(&self) -> bool {
        self.pending
            .lock()
            .expect("main mailbox lock")
            .fatal
            .is_some()
    }
    pub(crate) fn take_fatal(&self) -> Option<super::Worker> {
        self.pending.lock().expect("main mailbox lock").fatal.take()
    }
    pub(crate) fn try_recv(&self) -> Result<MainCommand, TryRecvError> {
        let signal = self.ready.try_recv();
        let mut pending = self
            .pending
            .lock()
            .expect("main mailbox contains bounded memory mutations only");
        if let Some(worker) = pending.fatal.take() {
            return Ok(MainCommand::Fatal(worker));
        }
        if let Some(command) = pending.commands.pop_front() {
            return Ok(command);
        }
        if matches!(signal, Err(TryRecvError::Disconnected)) {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }
    #[cfg(test)]
    pub(super) fn recv(&self) -> Result<MainCommand, crossbeam_channel::RecvTimeoutError> {
        self.recv_deadline(Instant::now() + super::SHUTDOWN_TIMEOUT)
    }
    #[cfg(test)]
    pub(super) fn recv_timeout(
        &self,
        wait: std::time::Duration,
    ) -> Result<MainCommand, crossbeam_channel::RecvTimeoutError> {
        self.recv_deadline(Instant::now() + wait)
    }
    #[cfg(test)]
    pub(super) fn recv_deadline(
        &self,
        deadline: Instant,
    ) -> Result<MainCommand, crossbeam_channel::RecvTimeoutError> {
        loop {
            match self.try_recv() {
                Ok(command) => return Ok(command),
                Err(TryRecvError::Disconnected) => {
                    return Err(crossbeam_channel::RecvTimeoutError::Disconnected);
                }
                Err(TryRecvError::Empty) => self.ready.recv_deadline(deadline)?,
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn try_iter(&self) -> impl Iterator<Item = MainCommand> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        let pending = self.pending.lock().expect("main mailbox lock");
        pending.fatal.is_none() && pending.commands.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::app::{MainPort, Worker};
    use quartz_domain::capture::{CaptureEffect, Timer};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn replaceable_notifications_coalesce_without_blocking_workers() {
        let channel = MainChannel::new(2);
        let port = MainPort {
            sender: channel.sender,
            wake: Arc::new(|| {}),
        };
        for _ in 0..super::super::MAIN_NOTIFICATION_CAPACITY {
            port.send(MainCommand::PolicyChanged)
                .expect("coalesced policy");
            port.send(MainCommand::Effect(CaptureEffect::ScheduleTimer(
                Timer::Cancel,
            )))
            .expect("coalesced timer");
        }
        assert!(matches!(
            channel.receiver.recv().expect("policy"),
            MainCommand::PolicyChanged
        ));
        assert!(matches!(
            channel.receiver.recv().expect("timer"),
            MainCommand::Effect(CaptureEffect::ScheduleTimer(Timer::Cancel))
        ));
        assert!(channel.receiver.is_empty());
    }

    #[test]
    fn fatal_delivery_has_reserved_capacity_and_wakes_a_stalled_main_thread() {
        let channel = MainChannel::new(1);
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&wakes);
        let port = MainPort {
            sender: channel.sender,
            wake: Arc::new(move || {
                count.fetch_add(1, Ordering::Relaxed);
            }),
        };
        port.send(MainCommand::PolicyChanged)
            .expect("full ordinary queue");
        let (finished, completed) = crossbeam_channel::bounded(super::super::CONTROL_CAPACITY);
        let worker = std::thread::spawn(move || {
            assert!(matches!(
                port.send(MainCommand::ClockChanged),
                Err(SendMainCommandError::Full)
            ));
            port.send(MainCommand::Fatal(Worker::Capture))
                .expect("reserved fatal slot");
            finished.send(()).expect("worker completed");
        });
        // No main queue draining until the worker has completed its sends.
        completed
            .recv_timeout(super::super::SHUTDOWN_TIMEOUT)
            .expect("worker cannot wait for a stalled main thread");
        worker
            .join()
            .expect("worker never waits for queue capacity");
        assert!(channel.receiver.capture_life().is_stopped());
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
        assert!(matches!(
            channel
                .receiver
                .recv_timeout(std::time::Duration::ZERO)
                .expect("fatal first"),
            MainCommand::Fatal(Worker::Capture)
        ));
        assert!(matches!(
            channel
                .receiver
                .try_recv()
                .expect("ordinary command retained"),
            MainCommand::PolicyChanged
        ));
    }
}
