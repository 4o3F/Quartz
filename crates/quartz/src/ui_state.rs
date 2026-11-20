//! Main-thread UI registration and first-frame acknowledgement.

#[derive(Debug, Default, PartialEq, Eq)]
enum UiPhase<Process> {
    #[default]
    None,
    Starting(Process),
    Visible(Process),
}

/// Owns a single child and tracks its first frame.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct UiState<Process> {
    phase: UiPhase<Process>,
}
impl<Process> Default for UiState<Process> {
    fn default() -> Self {
        Self {
            phase: UiPhase::None,
        }
    }
}
impl<Process> UiState<Process> {
    /// Starts at most one child.
    ///
    /// # Errors
    /// Returns a launch failure without registering a child.
    pub(super) fn open<E>(&mut self, launch: impl FnOnce() -> Result<Process, E>) -> Result<(), E> {
        if matches!(self.phase, UiPhase::None) {
            self.phase = UiPhase::Starting(launch()?);
        }
        // TODO(M3): Replay ui.show/ui.activate on the registered UI pipe connection.
        Ok(())
    }
    pub(super) fn process(&self) -> Option<&Process> {
        match &self.phase {
            UiPhase::None => None,
            UiPhase::Starting(process) | UiPhase::Visible(process) => Some(process),
        }
    }
    pub(super) fn first_frame(&mut self) {
        self.phase = match std::mem::replace(&mut self.phase, UiPhase::None) {
            UiPhase::Starting(process) | UiPhase::Visible(process) => UiPhase::Visible(process),
            UiPhase::None => UiPhase::None,
        };
    }
    pub(super) fn exited(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_open_waits_for_exit_before_launching_again() {
        let mut state = UiState::default();
        state.open(|| Ok::<_, ()>(1)).expect("first launch");
        state
            .open::<()>(|| panic!("a starting child must not launch again"))
            .expect("repeated open");
        assert_eq!(state.phase, UiPhase::Starting(1));
        state.exited();
        assert_eq!(state, UiState::default());
        state.open(|| Ok::<_, ()>(2)).expect("launch after exit");
        assert_eq!(state.phase, UiPhase::Starting(2));
    }

    #[test]
    fn first_frame_marks_visible() {
        let mut state = UiState::default();
        state.first_frame();
        assert_eq!(state.process(), None);
        state.open(|| Ok::<_, ()>(7)).expect("launch");
        state.first_frame();
        assert_eq!(state.phase, UiPhase::Visible(7));
        state
            .open::<()>(|| panic!("visible UI must not launch again"))
            .expect("activate");
        state.first_frame();
        assert_eq!(state.process(), Some(&7));
        state.exited();
        assert_eq!(state.process(), None);
    }

    #[test]
    fn failed_launch_allows_retry() {
        let mut state = UiState::default();
        assert_eq!(
            state.open(|| Err::<u32, _>("creation failed")),
            Err("creation failed")
        );
        assert_eq!(state, UiState::default());
        state.open(|| Ok::<_, ()>(3)).expect("retry after failure");
        assert_eq!(state.phase, UiPhase::Starting(3));
    }
}
