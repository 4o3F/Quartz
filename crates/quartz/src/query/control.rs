//! Connection-owned cancellation and admission-relative query deadlines.

use super::RunQueryError;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelState {
    Live,
    Cancelled,
}
#[derive(Debug, Clone)]
pub(crate) struct QueryControl {
    cancelled: tokio::sync::watch::Sender<CancelState>,
    deadline: Option<Instant>,
}
impl Default for QueryControl {
    fn default() -> Self {
        Self {
            cancelled: tokio::sync::watch::channel(CancelState::Live).0,
            deadline: None,
        }
    }
}
impl QueryControl {
    #[cfg(test)]
    pub(crate) fn expired() -> Self {
        Self {
            deadline: Some(Instant::now()),
            ..Self::default()
        }
    }

    pub(crate) fn with_deadline(milliseconds: u32) -> Self {
        Self {
            deadline: Some(Instant::now() + Duration::from_millis(u64::from(milliseconds))),
            ..Self::default()
        }
    }
    pub(crate) fn cancel(&self) {
        self.cancelled.send_replace(CancelState::Cancelled);
    }
    pub(crate) fn check(&self) -> Result<(), RunQueryError> {
        if *self.cancelled.borrow() == CancelState::Cancelled {
            Err(RunQueryError::Cancelled)
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Err(RunQueryError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }
    pub(crate) async fn stopped(&self) -> quartz_protocol::ErrorKind {
        let mut receiver = self.cancelled.subscribe();
        loop {
            if let Err(error) = self.check() {
                return error.kind();
            }
            tokio::select! {
                result = receiver.changed() => { result.expect("control retains cancellation sender"); }
                () = async {
                    match self.deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
        }
    }
}
