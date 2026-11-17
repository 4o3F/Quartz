//! Windows UI language negotiation shared by the core and UI entry paths.

use snafu::{ResultExt, Snafu};
use windows::{
    Win32::Globalization::{self, MUI_LANGUAGE_NAME},
    core::PWSTR,
};

pub(crate) fn select() -> &'static str {
    ui_language().unwrap_or_else(|error| {
        tracing::warn!(%error, "using English UI language");
        "en"
    })
}

#[derive(Debug, Snafu)]
#[snafu(display("could not read preferred Windows UI languages (GetUserPreferredUILanguages)"))]
struct ReadUiLanguagesError {
    source: windows::core::Error,
}

fn ui_language() -> Result<&'static str, ReadUiLanguagesError> {
    let mut count = 0;
    let mut length = 0;
    // SAFETY: A null output buffer queries the size of the preferred-language MULTI_SZ.
    unsafe {
        Globalization::GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut length)
    }
    .context(ReadUiLanguagesSnafu)?;
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: The buffer has the queried length; Windows writes at most that many UTF-16 units.
    unsafe {
        Globalization::GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut length,
        )
    }
    .context(ReadUiLanguagesSnafu)?;
    let languages: Vec<_> = buffer
        .split(|unit| *unit == 0)
        .filter(|name| !name.is_empty())
        .map(String::from_utf16_lossy)
        .collect();
    let preferred: Vec<_> = languages.iter().map(String::as_str).collect();
    Ok(quartz_i18n::negotiate_language(&preferred))
}
