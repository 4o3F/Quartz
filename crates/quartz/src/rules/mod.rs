//! Single-writer rule management and immutable evaluation snapshots.

#[expect(
    dead_code,
    reason = "the registry command queue is wired to IPC in M1-D/M2"
)]
pub(crate) mod registry;
