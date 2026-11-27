//! Shared progress state for multi-day queries that exceed the 250 ms progress threshold.

use num_traits::ToPrimitive as _;

/// Per-day progress reported by the core for an in-flight multi-day query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryProgress {
    /// Completed logical days so far.
    pub done: u32,
    /// Total logical days in the query.
    pub total: u32,
}

impl QueryProgress {
    /// Completed days clamped so display never exceeds `total`.
    #[must_use]
    pub const fn clamped_done(self) -> u32 {
        if self.done > self.total {
            self.total
        } else {
            self.done
        }
    }

    /// Completion fraction in `0.0..=1.0` for rendering a determinate progress bar.
    #[must_use]
    pub fn fraction(self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (f64::from(self.clamped_done()) / f64::from(self.total))
            .to_f32()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    }
}

impl From<quartz_protocol::Progress> for QueryProgress {
    fn from(progress: quartz_protocol::Progress) -> Self {
        Self {
            done: progress.done.min(progress.total),
            total: progress.total,
        }
    }
}

/// Progressive loading state for a multi-day query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LoadProgress {
    /// Query is in flight before the core sends its first progress frame.
    #[default]
    Waiting,
    /// Core has emitted at least one per-day progress update.
    Counting { done: u32, total: u32 },
}

impl LoadProgress {
    /// Records an incoming progress update, clamping `done` to `total` and ignoring zero totals.
    pub(crate) fn record(&mut self, progress: QueryProgress) {
        if progress.total == 0 {
            return;
        }
        *self = Self::Counting {
            done: progress.clamped_done(),
            total: progress.total,
        };
    }

    /// Returns the clamped progress once counting has started, or `None` while waiting.
    #[must_use]
    pub(crate) const fn current(self) -> Option<QueryProgress> {
        match self {
            Self::Waiting => None,
            Self::Counting { done, total } => {
                if total == 0 {
                    return None;
                }
                let progress = QueryProgress { done, total };
                Some(QueryProgress {
                    done: progress.clamped_done(),
                    total,
                })
            }
        }
    }
}

/// Whether an incoming progress update applied to the active loading generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgressUpdate {
    Applied,
    Ignored,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_stays_hidden_while_waiting_and_clamps_done_to_total() {
        let mut state = LoadProgress::default();
        assert_eq!(state, LoadProgress::Waiting);
        assert_eq!(state.current(), None);

        state.record(QueryProgress { done: 3, total: 0 });
        assert_eq!(
            state,
            LoadProgress::Waiting,
            "zero-total progress updates are ignored"
        );

        state.record(QueryProgress {
            done: 120,
            total: 365,
        });
        assert_eq!(
            state.current(),
            Some(QueryProgress {
                done: 120,
                total: 365,
            })
        );

        state.record(QueryProgress { done: 12, total: 7 });
        assert_eq!(
            state.current(),
            Some(QueryProgress { done: 7, total: 7 }),
            "done never exceeds total"
        );

        let unclamped = LoadProgress::Counting { done: 99, total: 7 };
        assert_eq!(
            unclamped.current(),
            Some(QueryProgress { done: 7, total: 7 })
        );
    }

    #[test]
    fn progress_fraction_stays_within_unit_interval() {
        assert!(QueryProgress { done: 0, total: 0 }.fraction().abs() < f32::EPSILON);
        assert!(QueryProgress { done: 0, total: 7 }.fraction().abs() < f32::EPSILON);
        assert!((QueryProgress { done: 1, total: 4 }.fraction() - 0.25).abs() < f32::EPSILON);
        assert!((QueryProgress { done: 10, total: 7 }.fraction() - 1.0).abs() < f32::EPSILON);
    }
}
