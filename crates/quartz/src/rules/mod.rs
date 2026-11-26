//! Single-writer rule management and immutable evaluation snapshots.

pub(crate) mod registry;

pub(crate) mod read;

#[cfg(any(windows, test))]
pub(crate) mod write;

#[cfg(any(windows, test))]
mod diagnostic;
