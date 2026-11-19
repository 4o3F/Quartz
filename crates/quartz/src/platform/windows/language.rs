//! Windows UI language negotiation shared by the core and UI entry paths.

use snafu::{ResultExt, Snafu};
use windows::{
    Win32::Globalization::{self, MUI_LANGUAGE_NAME},
    core::PWSTR,
};

/// Supported UI language selected from Windows preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiLanguage(quartz_i18n::Language);

impl UiLanguage {
    /// Detects the preferred supported language, warning and falling back to English on failure.
    pub(crate) fn detect() -> Self {
        Self(
            PreferredUiLanguages::read()
                .map(|languages| languages.negotiate())
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "using English UI language");
                    quartz_i18n::Language::English
                }),
        )
    }

    /// Returns the locale identifier accepted by the translation and UI APIs.
    pub(crate) fn as_str(self) -> &'static str {
        self.0.code()
    }
    pub(crate) fn language(self) -> quartz_i18n::Language {
        self.0
    }
}

#[derive(Debug, Snafu)]
enum ReadUiLanguagesError {
    #[snafu(display(
        "could not read preferred Windows UI languages (GetUserPreferredUILanguages)"
    ))]
    Read { source: windows::core::Error },
    #[snafu(display("preferred UI language buffer exceeds addressable memory"))]
    BufferSize { source: std::num::TryFromIntError },
}

#[derive(Debug)]
struct PreferredUiLanguages {
    languages: Vec<String>,
}
impl PreferredUiLanguages {
    fn read() -> Result<Self, ReadUiLanguagesError> {
        let mut count = 0;
        let mut length = 0;
        // SAFETY: A null output buffer queries the size of the preferred-language MULTI_SZ.
        unsafe {
            Globalization::GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                None,
                &mut length,
            )
        }
        .context(ReadSnafu)?;
        let mut buffer = vec![0u16; usize::try_from(length).context(BufferSizeSnafu)?];
        // SAFETY: The buffer has the queried length; Windows writes at most that many UTF-16 units.
        unsafe {
            Globalization::GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                Some(PWSTR(buffer.as_mut_ptr())),
                &mut length,
            )
        }
        .context(ReadSnafu)?;
        let languages: Vec<_> = buffer
            .split(|unit| *unit == 0)
            .filter(|name| !name.is_empty())
            .map(String::from_utf16_lossy)
            .collect();
        Ok(Self { languages })
    }
    fn negotiate(&self) -> quartz_i18n::Language {
        let preferred: Vec<_> = self.languages.iter().map(String::as_str).collect();
        quartz_i18n::Language::negotiate(&preferred)
    }
}
