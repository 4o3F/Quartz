//! Single-writer rule management and immutable evaluation snapshots.

pub(crate) mod registry;

pub(crate) mod read;

#[cfg(any(windows, test))]
pub(crate) mod write;

#[cfg(any(windows, test))]
mod diagnostic;

/// Maximum encoded JSON bytes for rule diagnostics and mutation responses (200 KiB, 04 §6.3.1, §6.3.3).
#[cfg(any(windows, test))]
pub(crate) const MAX_RULE_RESPONSE_BYTES: usize = 200 * 1024;
