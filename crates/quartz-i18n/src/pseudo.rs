//! Debug-only translations expanded from the embedded English catalog.

use std::borrow::Cow;

mod catalog {
    rust_i18n::i18n!("locales", fallback = "en");
}

/// Generates expanded translations for the debug pseudo-locale.
#[derive(Debug)]
pub(super) struct Backend;

impl rust_i18n::Backend for Backend {
    fn available_locales(&self) -> Vec<Cow<'_, str>> {
        vec![Cow::Borrowed("en-XA")]
    }

    fn translate(&self, locale: &str, key: &str) -> Option<Cow<'_, str>> {
        if locale != "en-XA" {
            return None;
        }
        catalog::_rust_i18n_backend()
            .translate("en", key)
            .map(|text| Cow::Owned(expand(&text)))
    }

    fn messages_for_locale(&self, locale: &str) -> Option<Vec<(Cow<'_, str>, Cow<'_, str>)>> {
        if locale != "en-XA" {
            return None;
        }
        catalog::_rust_i18n_backend()
            .messages_for_locale("en")
            .map(|messages| {
                messages
                    .into_iter()
                    .map(|(key, text)| (key, Cow::Owned(expand(&text))))
                    .collect()
            })
    }
}

fn expand(text: &str) -> String {
    let padding = (text.chars().count() * 2).div_ceil(5);
    format!("[{text}{}]", "~".repeat(padding))
}
