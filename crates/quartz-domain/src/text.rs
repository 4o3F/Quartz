//! Locale-independent comparison keys for rule values and captured text.
//!
//! Keep captured titles unchanged. Cache these keys separately and normalize rule
//! values once when loading rules. Regex matching uses [`NfcText::as_str`].

use std::fmt;

use icu_casemap::CaseMapper;
use icu_normalizer::ComposingNormalizer;

/// NFC text for case-sensitive equality, substring matching and regex input.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NfcText(String);

impl NfcText {
    /// Composes canonically equivalent sequences without folding case or width.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self(ComposingNormalizer::new_nfc().normalize(text).into_owned())
    }

    /// Borrows the NFC text, including for a regex engine's own case handling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Tests for a normalized code point subsequence without word segmentation.
    #[must_use]
    pub fn contains(&self, pattern: &Self) -> bool {
        self.0.contains(pattern.as_str())
    }

    /// Tests whether this text begins with the normalized pattern.
    #[must_use]
    pub fn starts_with(&self, pattern: &Self) -> bool {
        self.0.starts_with(pattern.as_str())
    }

    /// Tests whether this text ends with the normalized pattern.
    #[must_use]
    pub fn ends_with(&self, pattern: &Self) -> bool {
        self.0.ends_with(pattern.as_str())
    }
}

impl fmt::Display for NfcText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An NFKC → default full case fold → NFKC key for case-insensitive matching.
///
/// Folding is non-Turkic and independent of the operating system and UI language.
/// These keys are comparison data; retain the original text for storage and display.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FoldedKey(String);

impl FoldedKey {
    /// Folds compatibility characters, width and case using compiled Unicode data.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let nfkc = ComposingNormalizer::new_nfkc();
        let normalized = nfkc.normalize(text);
        let folded = CaseMapper::new().fold_string(&normalized);
        Self(nfkc.normalize(&folded).into_owned())
    }

    /// Borrows the folded code point sequence.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Tests for a folded code point subsequence without word segmentation.
    #[must_use]
    pub fn contains(&self, pattern: &Self) -> bool {
        self.0.contains(pattern.as_str())
    }

    /// Tests whether this key begins with the folded pattern.
    #[must_use]
    pub fn starts_with(&self, pattern: &Self) -> bool {
        self.0.starts_with(pattern.as_str())
    }

    /// Tests whether this key ends with the folded pattern.
    #[must_use]
    pub fn ends_with(&self, pattern: &Self) -> bool {
        self.0.ends_with(pattern.as_str())
    }
}

impl fmt::Display for FoldedKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Encodes bytes as lowercase hexadecimal digits.
#[must_use]
pub fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_preserves_case_width_and_compatibility() {
        assert_eq!(NfcText::new("é"), NfcText::new("e\u{301}"));
        for (left, right) in [("A", "a"), ("Ａ", "A"), ("ﬁ", "fi"), ("ｶ", "カ")] {
            assert_ne!(
                NfcText::new(left),
                NfcText::new(right),
                "{left:?}, {right:?}"
            );
        }
        assert_eq!(NfcText::new("中文标题").as_str(), "中文标题");
    }

    #[test]
    fn full_folding_handles_multilingual_equivalences() {
        for (input, expected) in [
            ("Straße", "strasse"),
            ("STRAẞE", "strasse"),
            ("ßẞ", "ssss"),
            ("I", "i"),
            ("İ", "i\u{307}"),
            ("ı", "ı"),
            ("Σσς", "σσσ"),
            ("ＶＳ１２３", "vs123"),
            ("VS123", "vs123"),
            ("ｶﾀｶﾅ", "カタカナ"),
            ("ｶﾞ", "ガ"),
            ("ﬁ", "fi"),
            ("中文标题", "中文标题"),
            ("É", "é"),
            ("e\u{301}", "é"),
            // Full folding decomposes this character; the final NFKC recomposes it.
            ("ǰ", "ǰ"),
            ("Привет МИР", "привет мир"),
            ("नमस्ते दुनिया", "नमस्ते दुनिया"),
        ] {
            let key = FoldedKey::new(input);
            assert_eq!(key.as_str(), expected, "input {input:?}");
            assert_eq!(FoldedKey::new(key.as_str()), key, "idempotence: {input:?}");
        }
        assert_ne!(FoldedKey::new("I"), FoldedKey::new("ı"));
        assert_ne!(FoldedKey::new("İ"), FoldedKey::new("i"));
        assert_ne!(FoldedKey::new("é"), FoldedKey::new("e"));
    }

    #[test]
    fn normalized_operators_match_code_point_sequences() {
        let text = NfcText::new("e\u{301}中的文档 - Quartz");
        assert!(text.starts_with(&NfcText::new("é中")));
        assert!(!text.contains(&NfcText::new("中文档")));
        assert!(text.contains(&NfcText::new("文档")));
        assert!(text.ends_with(&NfcText::new("Quartz")));
        assert!(!text.ends_with(&NfcText::new("quartz")));
        assert_eq!(NfcText::new(text.as_str()), text);

        let text = FoldedKey::new("ＳＴＲＡẞＥ 中的文档 - QUARTZ");
        assert!(text.starts_with(&FoldedKey::new("strasse")));
        assert!(text.contains(&FoldedKey::new("文档")));
        assert!(text.ends_with(&FoldedKey::new("quartz")));
        assert!(!text.contains(&FoldedKey::new("其他")));
        assert!(!text.starts_with(&FoldedKey::new("quartz")));
        assert!(!text.ends_with(&FoldedKey::new("strasse")));
    }

    #[test]
    fn empty_patterns_follow_string_operator_semantics() {
        let empty = NfcText::new("");
        assert!(empty.contains(&empty));
        assert!(empty.starts_with(&empty));
        assert!(empty.ends_with(&empty));
        let empty = FoldedKey::new("");
        assert!(empty.contains(&empty));
        assert!(empty.starts_with(&empty));
        assert!(empty.ends_with(&empty));
    }
}
