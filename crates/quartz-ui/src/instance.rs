//! Window-free duplicate-instance policy; transport failures still finish a duplicate launch.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(windows)]
pub(crate) enum InstancePolicy {
    Required,
    CoreError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstanceDecision {
    Open,
    ForwardAndExit,
}
impl InstanceDecision {
    pub(crate) fn forward(
        self,
        forward: impl FnOnce() -> Result<quartz_protocol::AcceptedBody, quartz_pipe::CallCoreError>,
    ) -> Self {
        if self == Self::ForwardAndExit
            && let Err(error) = forward()
        {
            tracing::warn!(%error, "could not forward duplicate UI open request to core");
        }
        self
    }
}
#[cfg(windows)]
impl From<&quartz_pipe::AcquireUiInstance> for InstanceDecision {
    fn from(outcome: &quartz_pipe::AcquireUiInstance) -> Self {
        match outcome {
            quartz_pipe::AcquireUiInstance::Acquired(_) => Self::Open,
            quartz_pipe::AcquireUiInstance::AlreadyRunning => Self::ForwardAndExit,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_forwards_once_and_exits_even_when_core_is_unreachable() {
        for failure in [false, true] {
            let mut calls = 0;
            let result = InstanceDecision::ForwardAndExit.forward(|| {
                calls += 1;
                if failure {
                    Err(quartz_pipe::CallCoreError::Io {
                        source: std::io::Error::from(std::io::ErrorKind::NotFound),
                    })
                } else {
                    Ok(quartz_protocol::AcceptedBody)
                }
            });
            assert_eq!(calls, 1);
            assert_eq!(result, InstanceDecision::ForwardAndExit);
        }
        assert_eq!(
            InstanceDecision::Open.forward(|| panic!("primary UI must open without forwarding")),
            InstanceDecision::Open
        );
    }
    #[cfg(windows)]
    #[test]
    fn existing_mutex_selects_forwarding_without_creating_a_mutex() {
        assert_eq!(
            InstanceDecision::from(&quartz_pipe::AcquireUiInstance::AlreadyRunning),
            InstanceDecision::ForwardAndExit
        );
    }
}
