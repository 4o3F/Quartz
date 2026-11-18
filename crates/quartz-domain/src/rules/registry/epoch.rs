//! Process-local publication ordering for immutable rule snapshots.

use std::fmt;

/// Publication sequence within one registry process; it restarts at zero on startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotEpoch(u64);

impl SnapshotEpoch {
    /// Starts a registry process before its first snapshot publication.
    #[must_use]
    pub fn initial() -> Self {
        Self(0)
    }

    /// Returns the publication number for diagnostics and boundary adapters.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// Returns the next publication sequence, or `None` when the counter is exhausted.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl fmt::Display for SnapshotEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SnapshotEpoch>();
};
