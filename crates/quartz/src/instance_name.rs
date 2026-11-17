//! SID formatting and per-session core mutex naming.

use std::fmt::Write;

/// Formats the numeric components of a Windows SID in its canonical string form.
pub(super) fn format_sid(revision: u8, authority: [u8; 6], subauthorities: &[u32]) -> String {
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
    sid
}

/// Names the current user's core mutex within the caller's Windows session.
pub(super) fn core_mutex_name(sid: &str) -> String {
    format!("Local\\Quartz.{sid}.core")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_components_and_session_mutex_name() {
        let sid = format_sid(1, [0, 0, 0, 0, 0, 5], &[21, 123, 456, 789, 1001]);
        assert_eq!(sid, "S-1-5-21-123-456-789-1001");
        assert_eq!(
            core_mutex_name(&sid),
            "Local\\Quartz.S-1-5-21-123-456-789-1001.core"
        );
        assert_eq!(format_sid(1, [0; 6], &[0]), "S-1-0-0");
        assert_eq!(
            format_sid(1, [1, 2, 3, 4, 5, 6], &[u32::MAX]),
            "S-1-0x010203040506-4294967295"
        );
    }
}
