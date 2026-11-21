//! Typed production and isolated test pipe names.

use crate::UserSid;
use std::ffi::OsStr;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Versioned per-user endpoint, or an isolated GUID endpoint in tests.
pub struct PipeName(String);
impl PipeName {
    pub(super) fn readiness_name(&self) -> std::ffi::OsString {
        let endpoint = self
            .0
            .strip_prefix(r"\\.\pipe\")
            .expect("pipe names are constructed with the Windows pipe prefix");
        format!("Local\\{endpoint}.ready").into()
    }
    pub(super) fn from_sid(sid: &UserSid) -> Self {
        Self(format!(r"\\.\pipe\Quartz.{sid}.v1"))
    }
    /// Creates an isolated name from a fresh GUID without changing production naming.
    #[cfg(any(test, feature = "test-util"))]
    pub fn for_test(suffix: windows::core::GUID) -> Self {
        Self(format!(r"\\.\pipe\Quartz.test.{suffix:?}"))
    }
}
impl AsRef<OsStr> for PipeName {
    fn as_ref(&self) -> &OsStr {
        self.0.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_name_uses_canonical_sid_and_tests_are_isolated() {
        let sid = UserSid::from_token_string("S-1-5-21-1001".into());
        let production = PipeName::from_sid(&sid);
        assert_eq!(
            production.as_ref(),
            OsStr::new(r"\\.\pipe\Quartz.S-1-5-21-1001.v1")
        );
        let test = PipeName::for_test(windows::core::GUID::new().expect("unique test pipe"));
        assert_ne!(production, test);
    }
}
