//! Debug-only translations expanded from the embedded English catalog.

use std::borrow::Cow;

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
        // English bypasses this extension, so the shared backend does not recurse.
        super::translations::_rust_i18n_backend()
            .translate("en", key)
            .map(|text| Cow::Owned(expand(&text)))
    }

    // rust-i18n requires key/value tuples at its backend boundary.
    fn messages_for_locale(&self, locale: &str) -> Option<Vec<(Cow<'_, str>, Cow<'_, str>)>> {
        self.messages(locale).map(|messages| {
            messages
                .into_iter()
                .map(|message| (message.key, message.text))
                .collect()
        })
    }
}
struct CatalogMessage<'message> {
    key: Cow<'message, str>,
    text: Cow<'message, str>,
}
impl Backend {
    fn messages(&self, locale: &str) -> Option<Vec<CatalogMessage<'_>>> {
        if locale != "en-XA" {
            return None;
        }
        super::translations::_rust_i18n_backend()
            .messages_for_locale("en")
            .map(|messages| {
                messages
                    .into_iter()
                    .map(|(key, text)| CatalogMessage {
                        key,
                        text: Cow::Owned(expand(&text)),
                    })
                    .collect()
            })
    }
}

fn expand(text: &str) -> String {
    let padding = (text.chars().count() * 2).div_ceil(5);
    format!("[{text}{}]", "~".repeat(padding))
}
