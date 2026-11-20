//! Typed production and isolated test pipe names.

use crate::instance_name::UserSid;
use std::ffi::OsStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PipeName(String);
impl PipeName {
    pub(super) fn from_sid(sid: &UserSid) -> Self {
        Self(format!(r"\\.\pipe\Quartz.{sid}.v1"))
    }
    /// Creates an isolated name from a fresh GUID without changing production naming.
    #[cfg(test)]
    pub(crate) fn for_test(suffix: windows::core::GUID) -> Self {
        Self(format!(r"\\.\pipe\Quartz.test.{suffix:?}"))
    }
}
impl AsRef<OsStr> for PipeName {
    fn as_ref(&self) -> &OsStr {
        self.0.as_ref()
    }
}
