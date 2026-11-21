//! Typed operating-system process identity used by the UI lifecycle.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ProcessId(u32);
impl ProcessId {
    pub(crate) const fn new(value: u32) -> Self {
        Self(value)
    }
    #[cfg(windows)]
    pub(crate) fn raw(self) -> u32 {
        self.0
    }
}
impl fmt::Display for ProcessId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
