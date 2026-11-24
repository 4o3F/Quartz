//! Native pipe draining with explicit cancellation and completion before reuse.

use super::win32_error::Win32ErrorCode as _;
use std::{
    fmt, io,
    os::windows::io::AsRawHandle as _,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    thread::JoinHandle,
    time::Duration,
};
use tokio::{
    net::windows::named_pipe::NamedPipeServer,
    sync::{Semaphore, watch},
};
use windows::Win32::{
    Foundation::{ERROR_NO_DATA, ERROR_NOT_FOUND, ERROR_PIPE_NOT_CONNECTED, HANDLE},
    Storage::FileSystem::FlushFileBuffers,
    System::IO::CancelSynchronousIo,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FlushPhase {
    #[default]
    Dormant,
    Waiting,
    Running,
    Cancelling,
    CancellingCompleted,
    Cancelled,
    Completed,
}
type UnblockFlush = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;
struct FlushWork {
    run: Box<dyn FnOnce() -> io::Result<()> + Send>,
    unblock: UnblockFlush,
}
struct CancelFlush {
    worker: Arc<JoinHandle<()>>,
    unblock: UnblockFlush,
}
impl CancelFlush {
    fn cancel(&self) -> io::Result<()> {
        let unblocked = (self.unblock)();
        // SAFETY: The JoinHandle keeps this dedicated worker's native thread handle live.
        // The worker performs only one synchronous I/O call, so another operation cannot be cancelled.
        let cancelled = unsafe { CancelSynchronousIo(HANDLE(self.worker.as_raw_handle())) };
        let cancelled = match cancelled {
            Ok(()) => Ok(()),
            Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_NOT_FOUND.0) => {
                Ok(())
            }
            Err(error) => Err(io::Error::other(error)),
        };
        unblocked.and(cancelled)
    }
}
#[derive(Default)]
struct FlushState {
    phase: FlushPhase,
    waiting: Option<FlushWork>,
    cancel: Option<CancelFlush>,
}
impl fmt::Debug for FlushState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FlushState")
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, snafu::Snafu)]
pub(super) enum WaitPipeFlushError {
    #[snafu(display(
        "native pipe flush did not release its resources before the cleanup deadline"
    ))]
    Deadline { source: tokio::time::error::Elapsed },
    #[snafu(display("native pipe flush completion publisher stopped"))]
    Completion { source: watch::error::RecvError },
}
/// A completed phase means both the worker and its cancellation hook released their handles.
#[derive(Debug, Clone)]
pub(super) struct PipeFlush {
    state: Arc<Mutex<FlushState>>,
    capacity: Arc<Semaphore>,
    completed: watch::Sender<()>,
}
impl PipeFlush {
    pub(super) fn new(capacity: Arc<Semaphore>) -> Self {
        let (completed, _receiver) = watch::channel(());
        Self {
            state: Default::default(),
            capacity,
            completed,
        }
    }
    pub(super) fn reusable(&self) -> bool {
        matches!(
            self.state.lock().expect("pipe flush lock poisoned").phase,
            FlushPhase::Dormant | FlushPhase::Completed
        )
    }
    pub(super) async fn wait_reusable(&self, timeout: Duration) -> Result<(), WaitPipeFlushError> {
        tokio::time::timeout(timeout, self.wait_completion())
            .await
            .map_err(|source| WaitPipeFlushError::Deadline { source })?
    }
    pub(super) async fn wait_completion(&self) -> Result<(), WaitPipeFlushError> {
        let mut completed = self.completed.subscribe();
        while !self.reusable() {
            completed
                .changed()
                .await
                .map_err(|source| WaitPipeFlushError::Completion { source })?;
        }
        Ok(())
    }
    pub(super) fn start(&self, pipe: Arc<NamedPipeServer>) -> PendingFlush {
        let cancel_pipe = Arc::clone(&pipe);
        self.start_with_unblock(
            move || {
                // SAFETY: This closure owns the connected pipe throughout the synchronous flush.
                unsafe { FlushFileBuffers(HANDLE(pipe.as_raw_handle())) }.map_err(io::Error::other)
            },
            move || {
                // Disconnect also prevents a cancelled worker that has not entered FlushFileBuffers
                // from starting a new wait between the cancellation check and the native call.
                match cancel_pipe.disconnect() {
                    Err(error)
                        if error.is_win32(ERROR_PIPE_NOT_CONNECTED)
                            || error.is_win32(ERROR_NO_DATA) =>
                    {
                        Ok(())
                    }
                    result => result,
                }
            },
        )
    }
    #[cfg(test)]
    pub(super) fn start_with(
        &self,
        flush: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> PendingFlush {
        self.start_with_unblock(flush, || Ok(()))
    }
    fn start_with_unblock(
        &self,
        flush: impl FnOnce() -> io::Result<()> + Send + 'static,
        unblock: impl Fn() -> io::Result<()> + Send + Sync + 'static,
    ) -> PendingFlush {
        {
            let mut state = self.state.lock().expect("pipe flush lock poisoned");
            state.phase = FlushPhase::Waiting;
            state.waiting = Some(FlushWork {
                run: Box::new(flush),
                unblock: Arc::new(unblock),
            });
        }
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let permit = match Arc::clone(&owner.capacity).acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    owner.cancel();
                    return Err(io::Error::other(error));
                }
            };
            let (reply, received) = tokio::sync::oneshot::channel();
            {
                // Keep the transition locked until the cancellation hook has a live thread handle.
                let mut state = owner.state.lock().expect("pipe flush lock poisoned");
                let Some(work) = state.waiting.take() else {
                    return Err(io::ErrorKind::Interrupted.into());
                };
                let worker_owner = owner.clone();
                let started = std::thread::Builder::new()
                    .name("quartz-pipe-flush".into())
                    .spawn(move || {
                        let cancelled = matches!(
                            worker_owner
                                .state
                                .lock()
                                .expect("pipe flush lock poisoned")
                                .phase,
                            FlushPhase::Cancelling
                                | FlushPhase::CancellingCompleted
                                | FlushPhase::Cancelled
                        );
                        let result = if cancelled {
                            drop(work.run);
                            Err(io::ErrorKind::Interrupted.into())
                        } else {
                            (work.run)()
                        };
                        // Release capacity and the operation's captured pipe before publishing completion.
                        drop(permit);
                        worker_owner.finish_worker();
                        let _ = reply.send(result);
                    });
                match started {
                    Ok(worker) => {
                        state.cancel = Some(CancelFlush {
                            worker: Arc::new(worker),
                            unblock: work.unblock,
                        });
                        state.phase = FlushPhase::Running;
                    }
                    Err(error) => {
                        drop(work.unblock);
                        state.phase = FlushPhase::Completed;
                        owner.completed.send_replace(());
                        return Err(error);
                    }
                }
            }
            received.await.map_err(io::Error::other)?
        });
        PendingFlush {
            task,
            owner: self.clone(),
        }
    }
    fn finish_worker(&self) {
        let mut state = self.state.lock().expect("pipe flush lock poisoned");
        state.cancel.take();
        state.phase = match state.phase {
            FlushPhase::Running | FlushPhase::Cancelled => FlushPhase::Completed,
            FlushPhase::Cancelling => FlushPhase::CancellingCompleted,
            phase => unreachable!("worker cannot finish in phase {phase:?}"),
        };
        if state.phase == FlushPhase::Completed {
            self.completed.send_replace(());
        }
    }
    fn cancel(&self) {
        let cancel = {
            let mut state = self.state.lock().expect("pipe flush lock poisoned");
            match state.phase {
                FlushPhase::Waiting => {
                    state.waiting.take();
                    state.phase = FlushPhase::Completed;
                    self.completed.send_replace(());
                    return;
                }
                FlushPhase::Running => {
                    state.phase = FlushPhase::Cancelling;
                    state
                        .cancel
                        .take()
                        .expect("running worker has cancellation hook")
                }
                FlushPhase::Dormant
                | FlushPhase::Completed
                | FlushPhase::Cancelling
                | FlushPhase::CancellingCompleted
                | FlushPhase::Cancelled => return,
            }
        };
        if let Err(error) = cancel.cancel() {
            tracing::warn!(%error, "could not cancel native pipe flush");
        }
        drop(cancel);
        let mut state = self.state.lock().expect("pipe flush lock poisoned");
        state.phase = match state.phase {
            FlushPhase::Cancelling => FlushPhase::Cancelled,
            FlushPhase::CancellingCompleted => FlushPhase::Completed,
            phase => unreachable!("cancellation cannot finish in phase {phase:?}"),
        };
        if state.phase == FlushPhase::Completed {
            self.completed.send_replace(());
        }
    }
}
#[derive(Debug)]
pub(super) struct PendingFlush {
    task: tokio::task::JoinHandle<io::Result<()>>,
    owner: PipeFlush,
}
impl PendingFlush {
    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        use std::future::Future as _;
        Pin::new(&mut self.task)
            .poll(cx)
            .map(|result| result.map_err(io::Error::other).and_then(|result| result))
    }
}
impl Drop for PendingFlush {
    fn drop(&mut self) {
        self.task.abort();
        self.owner.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;
    #[tokio::test]
    async fn flush_completion_releases_the_transport_as_soon_as_backend_finishes() {
        let flush = PipeFlush::new(Arc::new(Semaphore::new(1)));
        let (started, running) = oneshot::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let mut pending = flush.start_with(move || {
            let _ = started.send(());
            waiting.recv().map_err(io::Error::other)
        });
        let task = tokio::spawn(async move { std::future::poll_fn(|cx| pending.poll(cx)).await });
        running.await.expect("fake flush started");
        assert!(!task.is_finished());
        assert!(!flush.reusable());
        release.send(()).expect("finish fake flush");
        task.await.expect("task").expect("flush");
        assert!(flush.reusable());
    }
    #[tokio::test]
    async fn cancelled_flush_keeps_the_old_instance_until_backend_completion() {
        let flush = PipeFlush::new(Arc::new(Semaphore::new(1)));
        let (started, running) = oneshot::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let pending = flush.start_with(move || {
            let _ = started.send(());
            waiting.recv().map_err(io::Error::other)
        });
        running.await.expect("fake flush started");
        drop(pending);
        assert!(
            !flush.reusable(),
            "cancelled native call cannot touch a new client"
        );
        release.send(()).expect("unblock old backend");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !flush.reusable() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old backend finishes");
    }
    #[tokio::test]
    async fn cancellation_while_waiting_for_flush_capacity_never_calls_backend() {
        let flush = PipeFlush::new(Arc::new(Semaphore::new(0)));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let resource = Arc::new(());
        let retained = Arc::downgrade(&resource);
        let pending = flush.start_with(move || {
            drop(resource);
            observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        });
        tokio::task::yield_now().await;
        drop(pending);
        assert!(
            retained.upgrade().is_none(),
            "waiting cancellation releases captured handles synchronously"
        );
        assert!(flush.reusable());
        flush.capacity.add_permits(1);
        tokio::task::yield_now().await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(
            flush.reusable(),
            "pre-spawn cancellation must leave a reusable instance"
        );
    }
    #[tokio::test]
    async fn cancelling_running_flush_unblocks_backend_and_releases_capacity() {
        let flush = PipeFlush::new(Arc::new(Semaphore::new(1)));
        let (started, running) = oneshot::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let fallback_release = release.clone();
        let unblocks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = unblocks.clone();
        let pending = flush.start_with_unblock(
            move || {
                let _ = started.send(());
                waiting.recv().map_err(io::Error::other)
            },
            move || {
                observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                release.send(()).map_err(io::Error::other)
            },
        );
        running.await.expect("backend started");
        drop(pending);
        let completed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !flush.reusable() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let _ = fallback_release.send(());
        assert!(completed.is_ok(), "cancellation must unblock the backend");
        assert_eq!(unblocks.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(flush.capacity.available_permits(), 1);
    }
    #[tokio::test]
    async fn cancelled_flush_waits_for_worker_release_before_pipe_reuse() {
        let flush = PipeFlush::new(Arc::new(Semaphore::new(1)));
        let (started, running) = oneshot::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let pending = flush.start_with(move || {
            let _ = started.send(());
            waiting.recv().map_err(io::Error::other)
        });
        running.await.expect("backend started");
        drop(pending);
        let waiting_flush = flush.clone();
        let settlement = tokio::spawn(async move {
            waiting_flush
                .wait_reusable(std::time::Duration::from_secs(1))
                .await
        });
        tokio::task::yield_now().await;
        let premature = settlement.is_finished();
        release.send(()).expect("backend completion");
        settlement
            .await
            .expect("settlement task")
            .expect("settlement");
        assert!(
            !premature,
            "a slot must wait for the old worker to release its handle"
        );
        assert!(flush.reusable());
    }
}
