//! Canonical user SID supplied by the Windows process token.
use std::fmt;

/// User identity used in pipe naming and the server's explicit DACL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserSid(String);
impl UserSid {
    #[cfg(windows)]
    pub(crate) fn from_token_string(value: String) -> Self {
        Self(value)
    }
}
impl fmt::Display for UserSid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
