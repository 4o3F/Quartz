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

/// A catalog message identifier, borrowed without allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageKey<'key>(&'key str);
impl<'key> MessageKey<'key> {
    /// Borrows the identifier used by the embedded catalog and rust-i18n macros.
    #[must_use]
    pub const fn new(key: &'key str) -> Self {
        Self(key)
    }
    /// Returns the identifier at a translation backend boundary.
    #[must_use]
    pub const fn as_str(self) -> &'key str {
        self.0
    }
    /// Looks up this message using the global language.
    #[must_use]
    pub fn translate(self) -> Cow<'key, str> {
        rust_i18n::t!(self.0)
    }
    /// Looks up this message in a language without changing the global language.
    #[must_use]
    pub fn in_language(self, language: Language) -> Cow<'key, str> {
        rust_i18n::t!(self.0, locale = language.code())
    }
    /// Checks the language's own catalog without English fallback.
    #[must_use]
    pub fn is_in_catalog(self, language: Language) -> bool {
        translations::_rust_i18n_backend()
            .translate(language.code(), self.0)
            .is_some()
    }
}
impl std::fmt::Display for MessageKey<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

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
    pub fn translate<'key>(self, key: MessageKey<'key>) -> Cow<'key, str> {
        key.in_language(self)
    }

    /// Whether this language's own catalog has `key`, without falling back to English.
    #[must_use]
    pub fn has_message(self, key: MessageKey<'_>) -> bool {
        key.is_in_catalog(self)
    }

    /// Formats an unfinished-command diagnostic in this language.
    #[must_use]
    pub fn not_implemented(self, operation: &str) -> String {
        rust_i18n::t!(
            "error.not_implemented",
            locale = self.code(),
            operation = operation
        )
        .into_owned()
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
                .expect("catalog placeholders have closing braces");
            assert!(!name.is_empty(), "empty placeholder in {text}");
            names.insert(name);
            rest = remaining;
        }
        names
    }

    #[test]
    fn capture_and_builtin_privacy_labels_resolve_in_both_catalogs() {
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
        let sources = [
            include_str!("../../quartz-domain/src/capture/privacy.rs"),
            include_str!("../../quartz-domain/src/rules/builtin-privacy.toml"),
        ];
        let backend = _rust_i18n_backend();
        for source in sources {
            for suffix in source.split("privacy.label.").skip(1) {
                let name: String = suffix
                    .chars()
                    .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                    .collect();
                let key = format!("privacy.label.{name}");
                for locale in ["en", "zh-CN"] {
                    let catalog = backend.messages_for_locale(locale).expect("locale catalog");
                    assert!(
                        catalog
                            .iter()
                            .any(|(candidate, value)| candidate == &key && !value.is_empty()),
                        "{locale}: {key}"
                    );
                }
            }
        }
    }

    #[test]
    fn builtin_extraction_labels_resolve_in_both_catalogs() {
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
        assert!(
            matches!(
                LOCALE_TEST_LOCK.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ),
            "catalog initialization must hold the shared locale lock"
        );
        let source = include_str!("../../quartz-domain/src/rules/builtin-extraction.toml");
        for line in source.lines() {
            let Some(key) = line.strip_prefix("label_key = \"") else {
                continue;
            };
            let key = MessageKey::new(key.strip_suffix('"').expect("quoted label key"));
            for language in [Language::English, Language::SimplifiedChinese] {
                assert!(language.has_message(key), "{language}: {key}");
                assert!(!language.translate(key).is_empty(), "{language}: {key}");
            }
        }
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
                    .expect("each advertised locale has an embedded catalog")
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
    fn translations_and_english_fallback() {
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
        assert_eq!(
            rust_i18n::t!("tray.open", locale = "zh-CN"),
            "打开 Quartz",
            "Chinese tray action"
        );
        assert_eq!(
            rust_i18n::t!("tray.exit", locale = "en"),
            "Exit Quartz",
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
            Language::English.not_implemented("rules check"),
            "not implemented yet: rules check",
            "English command diagnostic"
        );
    }

    #[test]
    fn global_locale_changes_translation_but_cli_errors_remain_english() {
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
        Language::SimplifiedChinese.set_global();
        assert_eq!(
            MessageKey::new("tray.open").translate(),
            "打开 Quartz",
            "global Chinese locale"
        );
        assert_eq!(
            Language::English.not_implemented("status"),
            "not implemented yet: status",
            "CLI diagnostic with Chinese global locale"
        );
        #[cfg(debug_assertions)]
        {
            Language::PseudoEnglish.set_global();
            assert!(
                MessageKey::new("tray.open").translate().starts_with('['),
                "pseudo-locale translation must have a start marker"
            );
        }
        Language::English.set_global();
        assert_eq!(
            MessageKey::new("tray.open").translate(),
            "Open Quartz",
            "global English locale"
        );
    }

    #[test]
    fn languages_parse_their_codes_and_translate_without_the_global_locale() {
        let _locale = LOCALE_TEST_LOCK.lock().expect("locale test lock poisoned");
        for language in Language::ALL {
            assert_eq!(language.code().parse::<Language>(), Ok(*language));
        }
        assert!("zh".parse::<Language>().is_err());
        assert!("fr".parse::<Language>().is_err());
        assert_eq!(
            MessageKey::new("tray.open").in_language(Language::SimplifiedChinese),
            "打开 Quartz"
        );
        assert_eq!(
            Language::SimplifiedChinese.translate(MessageKey::new("tray.open")),
            "打开 Quartz"
        );
        assert!(Language::SimplifiedChinese.has_message(MessageKey::new("tray.open")));
        assert!(!Language::SimplifiedChinese.has_message(MessageKey::new("no.such.key")));
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
            assert_eq!(
                Language::negotiate(&preferred).code(),
                expected,
                "{preferred:?}"
            );
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
                .expect("each English message has a generated pseudo translation");
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
            Language::negotiate(&["en-XA"]).code(),
            "en-XA",
            "explicit pseudo-locale selection"
        );
    }
}
