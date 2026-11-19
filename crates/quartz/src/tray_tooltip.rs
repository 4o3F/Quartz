//! UTF-16 encoding for the fixed Windows tray tooltip buffer.

use snafu::Snafu;

/// All shell tooltip templates, shared with catalog verification on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayTooltip {
    Ready,
    Opening,
    Upgrading,
    PersistenceFailed,
    SessionUnavailable,
    CaptureGap,
}
impl TrayTooltip {
    #[cfg(test)]
    pub(crate) const ALL: &[Self] = &[
        Self::Ready,
        Self::Opening,
        Self::Upgrading,
        Self::PersistenceFailed,
        Self::SessionUnavailable,
        Self::CaptureGap,
    ];
    pub(crate) fn key(self) -> quartz_i18n::MessageKey<'static> {
        quartz_i18n::MessageKey::new(match self {
            Self::Ready => "app.name",
            Self::Opening => "tray.opening",
            Self::Upgrading => "tray.upgrading",
            Self::PersistenceFailed => "tray.persistence_failed",
            Self::SessionUnavailable => "tray.session_unavailable",
            Self::CaptureGap => "tray.capture_gap",
        })
    }
}

/// A tooltip that cannot fit together with its terminating NUL.
#[derive(Debug, Snafu)]
#[snafu(display("tray tooltip needs {units} UTF-16 units; at most 127 are available"))]
pub(super) struct FormatTrayTooltipError {
    units: usize,
}

/// Complete UTF-16 tooltip with space reserved for its terminating NUL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrayTooltipText([u16; 128]);

impl TrayTooltipText {
    /// Transfers the fixed buffer to NOTIFYICONDATAW.
    pub(crate) fn into_buffer(self) -> [u16; 128] {
        self.0
    }
}

impl TryFrom<&str> for TrayTooltipText {
    type Error = FormatTrayTooltipError;

    /// Encodes text without truncation, reserving one UTF-16 unit for the terminator.
    ///
    /// # Errors
    /// Returns an error when the text needs more than 127 UTF-16 units.
    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let units = text.encode_utf16().count();
        if units >= 128 {
            return FormatTrayTooltipSnafu { units }.fail();
        }
        let mut buffer = [0; 128];
        for (slot, unit) in buffer.iter_mut().zip(text.encode_utf16()) {
            *slot = unit;
        }
        Ok(Self(buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tray_text_key_resolves_in_both_catalogs() {
        for locale in [
            quartz_i18n::Language::English,
            quartz_i18n::Language::SimplifiedChinese,
        ] {
            for key in TrayTooltip::ALL.iter().map(|tooltip| tooltip.key()).chain([
                quartz_i18n::MessageKey::new("tray.open"),
                quartz_i18n::MessageKey::new("tray.exit"),
            ]) {
                assert!(key.is_in_catalog(locale), "{locale}: {key}");
                assert_ne!(key.in_language(locale), key.as_str());
            }
        }
    }

    #[test]
    fn tooltip_counts_utf16_units_and_reserves_a_terminator() {
        let text = format!("{}a", "😀".repeat(63));
        let buffer = TrayTooltipText::try_from(text.as_str())
            .expect("127 UTF-16 units fit")
            .into_buffer();
        assert_eq!(
            String::from_utf16(&buffer[..127]).expect("complete UTF-16"),
            text
        );
        assert_eq!(buffer[127], 0);
        assert!(
            TrayTooltipText::try_from("😀".repeat(64).as_str()).is_err(),
            "128 units leave no terminator"
        );
        assert_eq!(
            TrayTooltipText::try_from("")
                .expect("empty tooltip")
                .into_buffer(),
            [0; 128]
        );
    }
}
