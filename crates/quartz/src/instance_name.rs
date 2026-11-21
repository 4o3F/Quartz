//! SID formatting and per-session core mutex naming.

use std::fmt::{self, Write};

/// Canonical Windows user SID constructed from its numeric components.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UserSid(String);

impl UserSid {
    /// Constructs the canonical SID, including hexadecimal authorities above 32 bits.
    pub(crate) fn from_components(
        revision: u8,
        authority: [u8; 6],
        subauthorities: &[u32],
    ) -> Self {
        let authority = u64::from_be_bytes([
            0,
            0,
            authority[0],
            authority[1],
            authority[2],
            authority[3],
            authority[4],
            authority[5],
        ]);
        let mut sid = if authority <= u64::from(u32::MAX) {
            format!("S-{revision}-{authority}")
        } else {
            format!("S-{revision}-0x{authority:012x}")
        };
        for value in subauthorities {
            write!(sid, "-{value}").expect("writing to a String cannot fail");
        }
        Self(sid)
    }

    /// Names this user's core mutex within the caller's Windows session.
    pub(crate) fn core_mutex_name(&self) -> String {
        format!("Local\\Quartz.{self}.core")
    }
}

impl fmt::Display for UserSid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_components_and_session_mutex_name() {
        let sid = UserSid::from_components(1, [0, 0, 0, 0, 0, 5], &[21, 123, 456, 789, 1001]);
        assert_eq!(sid.to_string(), "S-1-5-21-123-456-789-1001");
        assert_eq!(
            sid.core_mutex_name(),
            "Local\\Quartz.S-1-5-21-123-456-789-1001.core"
        );
        assert_eq!(
            UserSid::from_components(1, [0; 6], &[0]).to_string(),
            "S-1-0-0"
        );
        assert_eq!(
            UserSid::from_components(1, [1, 2, 3, 4, 5, 6], &[u32::MAX]).to_string(),
            "S-1-0x010203040506-4294967295"
        );
    }
}
