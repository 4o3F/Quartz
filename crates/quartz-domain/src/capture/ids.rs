//! Private-field identifiers and their single run-local allocator.

/// Shell-assigned identity of a capture run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunId(u64);
impl RunId {
    /// Returns the logical value for the persistence adapter.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
    /// Constructs an identity supplied by the run shell.
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}
/// Run-local clock-segment key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId(u64);
impl SegmentId {
    /// Reconstructs a logical key while loading persisted capture rows.
    #[must_use]
    pub fn from_persisted(value: u64) -> Self {
        Self(value)
    }
    /// Returns the logical value for the persistence adapter.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}
/// Run-local window-instance key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(u64);
impl WindowId {
    /// Reconstructs a logical key while loading persisted capture rows.
    #[must_use]
    pub fn from_persisted(value: u64) -> Self {
        Self(value)
    }
    /// Returns the logical value for the persistence adapter.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}
/// Run-local interval or availability key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(u64);
impl RowId {
    /// Reconstructs a logical key while loading persisted capture rows.
    #[must_use]
    pub fn from_persisted(value: u64) -> Self {
        Self(value)
    }
    /// Returns the logical value for the persistence adapter.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}
/// Shared key of midnight foreground pieces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionKey(u64);
impl SessionKey {
    /// Reconstructs a logical key while loading persisted capture rows.
    #[must_use]
    pub fn from_persisted(value: u64) -> Self {
        Self(value)
    }
    /// Returns the logical value for the persistence adapter.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}
/// Ordered transaction acknowledgement key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchId(u64);
impl BatchId {
    /// Returns the logical value for the persistence adapter.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct IdAllocator {
    row: u64,
    session: u64,
    window: u64,
    segment: u64,
    batch: u64,
}
impl IdAllocator {
    pub(super) fn row(&mut self) -> RowId {
        self.row = self
            .row
            .checked_add(1)
            .expect("logical identifier space exhausted");
        RowId(self.row)
    }
    pub(super) fn session(&mut self) -> SessionKey {
        self.session = self
            .session
            .checked_add(1)
            .expect("logical identifier space exhausted");
        SessionKey(self.session)
    }
    pub(super) fn window(&mut self) -> WindowId {
        self.window = self
            .window
            .checked_add(1)
            .expect("logical identifier space exhausted");
        WindowId(self.window)
    }
    pub(super) fn segment(&mut self) -> SegmentId {
        self.segment = self
            .segment
            .checked_add(1)
            .expect("logical identifier space exhausted");
        SegmentId(self.segment)
    }
    pub(super) fn batch(&mut self) -> BatchId {
        self.batch = self
            .batch
            .checked_add(1)
            .expect("logical identifier space exhausted");
        BatchId(self.batch)
    }
    pub(super) fn origin_batch() -> BatchId {
        BatchId(0)
    }
    pub(super) fn next_batch(after: BatchId) -> BatchId {
        BatchId(
            after
                .0
                .checked_add(1)
                .expect("batch identifier space exhausted"),
        )
    }
    pub(super) fn resume_row(next: RowId) -> Self {
        Self {
            row: next
                .0
                .checked_sub(1)
                .expect("recovery row keys start at one"),
            ..Self::default()
        }
    }
}
#[cfg(test)]
impl IdAllocator {
    pub(super) fn test_segmentid(value: u64) -> SegmentId {
        SegmentId(value)
    }
    pub(super) fn test_rowid(value: u64) -> RowId {
        RowId(value)
    }
    pub(super) fn test_batchid(value: u64) -> BatchId {
        BatchId(value)
    }
}
