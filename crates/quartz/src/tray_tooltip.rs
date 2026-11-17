//! UTF-16 encoding for the fixed Windows tray tooltip buffer.

use snafu::Snafu;

/// A tooltip that cannot fit together with its terminating NUL.
#[derive(Debug, Snafu)]
#[snafu(display("tray tooltip needs {units} UTF-16 units; at most 127 are available"))]
pub(super) struct FormatTrayTooltipError {
    units: usize,
}

/// Encodes a complete tooltip into the fixed NOTIFYICONDATAW buffer.
///
/// # Errors
/// Returns an error when the text needs more than 127 UTF-16 units.
pub(super) fn encode(text: &str) -> Result<[u16; 128], FormatTrayTooltipError> {
    let units = text.encode_utf16().count();
    if units >= 128 {
        return FormatTrayTooltipSnafu { units }.fail();
    }
    let mut buffer = [0; 128];
    for (slot, unit) in buffer.iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_counts_utf16_units_and_reserves_a_terminator() {
        let text = format!("{}a", "😀".repeat(63));
        let buffer = encode(&text).expect("127 UTF-16 units fit");
        assert_eq!(
            String::from_utf16(&buffer[..127]).expect("complete UTF-16"),
            text
        );
        assert_eq!(buffer[127], 0);
        assert!(
            encode(&"😀".repeat(64)).is_err(),
            "128 units leave no terminator"
        );
        assert_eq!(encode("").expect("empty tooltip"), [0; 128]);
    }
}
