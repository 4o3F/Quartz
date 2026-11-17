//! Embedded translations shared by the core and user interface.

use std::borrow::Cow;

#[cfg(debug_assertions)]
mod pseudo;

mod translations {
    #[cfg(debug_assertions)]
    rust_i18n::i18n!("locales", fallback = "en", backend = super::pseudo::Backend);
    #[cfg(not(debug_assertions))]
    rust_i18n::i18n!("locales", fallback = "en");
}

use translations::{_rust_i18n_t, _rust_i18n_try_translate};

/// Selects the first supported preferred language, falling back to English.
#[must_use]
pub fn negotiate_language(preferred: &[&str]) -> &'static str {
    for language in preferred {
        #[cfg(debug_assertions)]
        if language.eq_ignore_ascii_case("en-XA") {
            return "en-XA";
        }
        match language.split('-').next() {
            Some(base) if base.eq_ignore_ascii_case("zh") => return "zh-CN",
            Some(base) if base.eq_ignore_ascii_case("en") => return "en",
            _ => {}
        }
    }
    "en"
}

/// Changes the global translation locale shared with rust-i18n consumers.
pub fn set_locale(locale: &str) {
    rust_i18n::set_locale(locale);
}

/// Looks up a translation template in the current locale.
#[must_use]
pub fn translate(key: &str) -> Cow<'_, str> {
    rust_i18n::t!(key)
}

/// Formats the English diagnostic used by unfinished commands.
#[must_use]
pub fn not_implemented(operation: &str) -> String {
    rust_i18n::t!(
        "error.not_implemented",
        locale = "en",
        operation = operation
    )
    .into_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::translations::_rust_i18n_backend;
    use super::*;

    fn placeholders(text: &str) -> BTreeSet<&str> {
        let mut names = BTreeSet::new();
        let mut rest = text;
        while let Some((_, after)) = rest.split_once("%{") {
            let (name, remaining) = after
                .split_once('}')
                .unwrap_or_else(|| panic!("unclosed placeholder in {text}"));
            assert!(!name.is_empty(), "empty placeholder in {text}");
            names.insert(name);
            rest = remaining;
        }
        names
    }

    #[test]
    fn every_locale_has_every_key_and_matching_placeholders() {
        // Read the backend directly so fallback cannot hide missing keys.
        let backend = _rust_i18n_backend();
        let catalogs: BTreeMap<_, BTreeMap<_, _>> = backend
            .available_locales()
            .into_iter()
            .map(|locale| {
                let messages = backend
                    .messages_for_locale(&locale)
                    .unwrap_or_else(|| panic!("missing translation catalog for {locale}"))
                    .into_iter()
                    .collect();
                (locale, messages)
            })
            .collect();
        assert!(catalogs.contains_key("en"), "missing English catalog");
        assert!(catalogs.contains_key("zh-CN"), "missing Chinese catalog");
        #[cfg(debug_assertions)]
        assert!(
            catalogs.contains_key("en-XA"),
            "missing debug pseudo-locale catalog"
        );
        let keys: BTreeSet<_> = catalogs
            .values()
            .flat_map(|catalog| catalog.keys())
            .collect();
        assert!(!keys.is_empty(), "translation catalogs have no keys");
        for (locale, catalog) in &catalogs {
            assert_eq!(
                catalog.keys().collect::<BTreeSet<_>>(),
                keys,
                "keys for {locale}"
            );
            for key in &keys {
                assert_eq!(
                    placeholders(&catalog[*key]),
                    placeholders(&catalogs["en"][*key]),
                    "{locale}: {key}"
                );
            }
        }
    }

    #[test]
    fn tray_text_fits_utf16_tooltip_buffer() {
        let backend = _rust_i18n_backend();
        for locale in backend.available_locales() {
            for (key, text) in backend
                .messages_for_locale(&locale)
                .unwrap_or_else(|| panic!("missing translation catalog for {locale}"))
            {
                if key.starts_with("tray.") || key == "app.name" {
                    // M0 tray keys have no placeholders; add maximum substitutions with new keys.
                    assert!(
                        placeholders(&text).is_empty(),
                        "provide maximum placeholder values for {key}"
                    );
                    assert!(text.encode_utf16().count() < 128, "{locale}: {key}");
                }
            }
        }
    }

    #[test]
    fn translations_and_english_fallback() {
        assert_eq!(
            rust_i18n::t!("tray.open", locale = "zh-CN"),
            "打开",
            "Chinese tray action"
        );
        assert_eq!(
            rust_i18n::t!("tray.exit", locale = "en"),
            "Exit",
            "English tray action"
        );
        assert_eq!(
            rust_i18n::t!("app.name", locale = "fr"),
            "Quartz",
            "English fallback for French"
        );
        assert_eq!(
            rust_i18n::t!(
                "error.not_implemented",
                locale = "zh-CN",
                operation = "status"
            ),
            "尚未实现：status",
            "Chinese operation substitution"
        );
        assert_eq!(
            not_implemented("rules check"),
            "not implemented yet: rules check",
            "English command diagnostic"
        );
    }

    #[test]
    fn global_locale_changes_translation_but_cli_errors_remain_english() {
        set_locale("zh-CN");
        assert_eq!(translate("tray.open"), "打开", "global Chinese locale");
        assert_eq!(
            not_implemented("status"),
            "not implemented yet: status",
            "CLI diagnostic with Chinese global locale"
        );
        #[cfg(debug_assertions)]
        {
            set_locale("en-XA");
            assert!(
                translate("tray.open").starts_with('['),
                "pseudo-locale translation must have a start marker"
            );
        }
        set_locale("en");
        assert_eq!(translate("tray.open"), "Open", "global English locale");
    }

    #[test]
    fn preferred_languages_are_matched_in_order() {
        for (preferred, expected) in [
            (vec![], "en"),
            (vec!["fr"], "en"),
            (vec!["fr", "zh-TW", "en"], "zh-CN"),
            (vec!["en-US", "zh-CN"], "en"),
            (vec!["ZH-cn"], "zh-CN"),
        ] {
            assert_eq!(negotiate_language(&preferred), expected, "{preferred:?}");
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn pseudo_locale_is_expanded_and_preserves_substitutions() {
        let backend = _rust_i18n_backend();
        for (key, english) in backend.messages_for_locale("en").expect("English catalog") {
            let pseudo = backend
                .translate("en-XA", &key)
                .unwrap_or_else(|| panic!("missing pseudo translation for {key}"));
            assert!(pseudo.starts_with('[') && pseudo.ends_with(']'), "{key}");
            assert!(
                pseudo.chars().count() >= english.chars().count() * 14 / 10,
                "{key}"
            );
            assert_eq!(placeholders(&pseudo), placeholders(&english), "{key}");
        }
        let message = rust_i18n::t!(
            "error.not_implemented",
            locale = "en-XA",
            operation = "status"
        );
        assert!(message.contains("status"), "missing operation in {message}");
        assert!(
            !message.contains("%{"),
            "unresolved placeholder in {message}"
        );
        assert_eq!(
            negotiate_language(&["en-XA"]),
            "en-XA",
            "explicit pseudo-locale selection"
        );
    }
}
