//! Typed nonblocking port to capture; assembly supplies clock and queue ownership.

use quartz_protocol::{ErrorKind, Op, ProtocolError, TimedPolicyResponse};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimedDeadline {
    UtcMillis(i64),
    After(Duration),
    NextLogicalDay,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimedCommand {
    Pause(TimedDeadline),
    Resume,
    KeepTiming(TimedDeadline),
    EnablePrivacy(TimedDeadline),
    DisablePrivacy,
}
impl TimedCommand {
    pub(crate) fn decode(op: Op, body: serde_json::Value) -> Result<Self, ProtocolError> {
        use quartz_protocol::{
            PrivacyModeSetRequest, TrackingKeepAliveRequest, TrackingPauseRequest,
            TrackingResumeRequest,
        };
        let invalid = || ProtocolError::new(ErrorKind::BadRequest);
        Ok(match op {
            Op::TrackingPause => {
                let request: TrackingPauseRequest =
                    serde_json::from_value(body).map_err(|_| invalid())?;
                Self::Pause(TimedDeadline::UtcMillis(request.until_utc_ms))
            }
            Op::TrackingResume => {
                let _: TrackingResumeRequest =
                    serde_json::from_value(body).map_err(|_| invalid())?;
                Self::Resume
            }
            Op::TrackingKeepAlive => {
                let request: TrackingKeepAliveRequest =
                    serde_json::from_value(body).map_err(|_| invalid())?;
                Self::KeepTiming(TimedDeadline::UtcMillis(request.until_utc_ms))
            }
            Op::PrivacyModeSet => {
                let request: PrivacyModeSetRequest =
                    serde_json::from_value(body).map_err(|_| invalid())?;
                if request.enabled {
                    Self::EnablePrivacy(TimedDeadline::UtcMillis(
                        request.until_utc_ms.ok_or_else(invalid)?,
                    ))
                } else {
                    Self::DisablePrivacy
                }
            }
            _ => return Err(invalid()),
        })
    }
}
#[derive(Clone)]
pub(crate) struct TimedPolicyPort {
    submit: Arc<dyn Fn(TimedCommand) -> Result<TimedPolicyResponse, ProtocolError> + Send + Sync>,
}
impl TimedPolicyPort {
    pub(crate) fn new(
        submit: impl Fn(TimedCommand) -> Result<TimedPolicyResponse, ProtocolError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            submit: Arc::new(submit),
        }
    }
    pub(crate) fn call(&self, command: TimedCommand) -> Result<TimedPolicyResponse, ProtocolError> {
        (self.submit)(command)
    }
    #[cfg(test)]
    pub(crate) fn unavailable() -> Self {
        Self::new(|_| Err(ProtocolError::new(ErrorKind::Internal)))
    }
}
