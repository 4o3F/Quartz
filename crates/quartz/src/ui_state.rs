//! UI child registration until the message loop observes process exit.

/// Owns the UI process until the message loop observes its exit.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) enum UiState<Process> {
    #[default]
    None,
    Starting(Process),
}

impl<Process> UiState<Process> {
    /// Starts at most one child, leaving the state unchanged when creation fails.
    ///
    /// # Errors
    /// Returns the launch failure without registering a child.
    pub(super) fn open<E>(&mut self, launch: impl FnOnce() -> Result<Process, E>) -> Result<(), E> {
        if matches!(self, Self::None) {
            *self = Self::Starting(launch()?);
        }
        Ok(())
    }

    /// Releases the process registration after its handle becomes signaled.
    pub(super) fn exited(&mut self) {
        *self = Self::None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_open_waits_for_exit_before_launching_again() {
        let mut state = UiState::None;
        state.open(|| Ok::<_, ()>(1)).expect("first launch");
        state
            .open::<()>(|| panic!("a starting child must not launch again"))
            .expect("repeated open");
        assert_eq!(state, UiState::Starting(1));
        state.exited();
        assert_eq!(state, UiState::None);
        state.open(|| Ok::<_, ()>(2)).expect("launch after exit");
        assert_eq!(state, UiState::Starting(2));
    }

    #[test]
    fn failed_launch_allows_retry() {
        let mut state = UiState::None;
        assert_eq!(
            state.open(|| Err::<u32, _>("creation failed")),
            Err("creation failed")
        );
        assert_eq!(state, UiState::None);
        state.open(|| Ok::<_, ()>(3)).expect("retry after failure");
        assert_eq!(state, UiState::Starting(3));
    }
}
