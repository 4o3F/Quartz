//! Embedded translations shared by the core and user interface.

use std::borrow::Cow;

use snafu::{OptionExt as _, Snafu};

#[cfg(debug_assertions)]
mod pseudo;

mod translations {
    #[cfg(debug_assertions)]
    rust_i18n::i18n!("locales", fallback = "en", backend = super::pseudo::Backend);
    #[cfg(not(debug_assertions))]
    rust_i18n::i18n!("locales", fallback = "en");
}

use translations::{_rust_i18n_t, _rust_i18n_try_translate};

/// An interface language with a translation catalog (10 section 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Language {
    /// `en`, also the fallback for missing translations.
    #[default]
    English,
    /// `zh-CN`
    SimplifiedChinese,
    /// `en-XA`: English lengthened by about 40 percent with start and end markers, for
    /// screenshot review of truncation and hard-coded text (10 section 2.3).
    #[cfg(debug_assertions)]
    PseudoEnglish,
}

/// A language code has no translation catalog.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
#[snafu(display("no translation catalog for language {code:?}"))]
pub struct ParseLanguageError {
    code: String,
}

impl Language {
    /// Every language with a catalog in this build.
    pub const ALL: &[Self] = &[
        Self::English,
        Self::SimplifiedChinese,
        #[cfg(debug_assertions)]
        Self::PseudoEnglish,
    ];

    /// The locale code rust-i18n and gpui-component use, such as `zh-CN`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
            #[cfg(debug_assertions)]
            Self::PseudoEnglish => "en-XA",
        }
    }

    /// The first supported language of `preferred`, in order; English when none is supported.
    #[must_use]
    pub fn negotiate(preferred: &[&str]) -> Self {
        for language in preferred {
            #[cfg(debug_assertions)]
            if language.eq_ignore_ascii_case("en-XA") {
                return Self::PseudoEnglish;
            }
            match language.split('-').next() {
                Some(base) if base.eq_ignore_ascii_case("zh") => return Self::SimplifiedChinese,
                Some(base) if base.eq_ignore_ascii_case("en") => return Self::English,
                _ => {}
            }
        }
        Self::English
    }

    /// Looks up a template in this language without changing the global language; English
    /// fills in missing translations, and an unknown key comes back unchanged.
    #[must_use]
    pub fn translate(self, key: &str) -> Cow<'_, str> {
        rust_i18n::t!(key, locale = self.code())
    }

    /// Whether this language's own catalog has `key`, without falling back to English.
    #[must_use]
    pub fn has_message(self, key: &str) -> bool {
        translations::_rust_i18n_backend()
            .translate(self.code(), key)
            .is_some()
    }

    /// Makes this the global language shared with rust-i18n consumers such as gpui-component.
    pub fn set_global(self) {
        rust_i18n::set_locale(self.code());
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::str::FromStr for Language {
    type Err = ParseLanguageError;

    /// Parses an exact locale code of [`Language::code`].
    fn from_str(code: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|language| language.code() == code)
            .context(ParseLanguageSnafu { code })
    }
}

/// Selects the first supported preferred language, falling back to English.
///
/// Returns the locale code for callers that still pass languages as text; new code uses
/// [`Language::negotiate`].
#[must_use]
pub fn negotiate_language(preferred: &[&str]) -> &'static str {
    Language::negotiate(preferred).code()
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
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Mutex,
    };

    // Backend initialization can touch the global locale as well as set_locale.
    static LOCALE_TEST_LOCK: Mutex<()> = Mutex::new(());

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
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
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
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
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
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
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
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
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
    fn languages_parse_their_codes_and_translate_without_the_global_locale() {
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
        for language in Language::ALL {
            assert_eq!(language.code().parse::<Language>(), Ok(*language));
        }
        assert!("zh".parse::<Language>().is_err());
        assert!("fr".parse::<Language>().is_err());
        assert_eq!(Language::SimplifiedChinese.translate("tray.open"), "打开");
        assert!(Language::SimplifiedChinese.has_message("tray.open"));
        assert!(!Language::SimplifiedChinese.has_message("no.such.key"));
        assert_eq!(
            Language::negotiate(&["fr", "zh-TW"]),
            Language::SimplifiedChinese
        );
    }

    #[test]
    fn preferred_languages_are_matched_in_order() {
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
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
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
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
