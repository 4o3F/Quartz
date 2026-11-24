//! UTS #46 domain keys for equality, storage and domain-suffix rules.

use std::fmt;

use idna::uts46::{AsciiDenyList, DnsLength, Hyphens, Uts46};
use snafu::{ResultExt, Snafu};

/// Failure to normalize a domain, without retaining private domain text.
#[derive(Debug, Snafu)]
pub enum NormalizeDomainError {
    /// Fragment normalization removed every input character.
    #[snafu(display("normalized domain fragment is empty"))]
    EmptyFragment,
    /// The input fails UTS #46, URL host character or DNS length validation.
    #[snafu(display("invalid domain name"))]
    InvalidDomain {
        /// The IDNA validation failure, which contains no input text.
        source: idna::Errors,
    },
}

/// A domain substring with UTS #46 labels and all separators preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainFragment(String);
impl DomainFragment {
    /// Normalizes each nonempty label while retaining leading, trailing and repeated dots.
    ///
    /// # Errors
    /// Returns a typed empty-fragment error or the IDNA error for invalid spellings.
    pub fn parse(value: &str) -> Result<Self, NormalizeDomainError> {
        let mut normalized = String::new();
        for (index, label) in value
            .split(['.', '\u{3002}', '\u{ff0e}', '\u{ff61}'])
            .enumerate()
        {
            if index != 0 {
                normalized.push('.');
            }
            if !label.is_empty() {
                let ascii = Uts46::new()
                    .to_ascii(
                        label.as_bytes(),
                        AsciiDenyList::URL,
                        Hyphens::Allow,
                        DnsLength::Ignore,
                    )
                    .context(InvalidDomainSnafu)?;
                normalized.push_str(&ascii);
            }
        }
        if normalized.is_empty() {
            return Err(NormalizeDomainError::EmptyFragment);
        }
        Ok(Self(normalized))
    }
    /// Borrows the ASCII substring used by literal comparison.
    pub fn as_ascii(&self) -> &str {
        &self.0
    }
}

/// A validated lowercase ASCII domain key with Punycode labels where needed.
///
/// Uses non-transitional UTS #46, URL host character restrictions and DNS length
/// validation. Hyphens follow browser URL handling. A trailing root dot is removed
/// so absolute and relative spellings compare equally. No DNS lookup is performed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DomainName(String);

impl DomainName {
    /// Normalizes a Unicode or ASCII domain without a scheme, port or path.
    ///
    /// # Errors
    /// Returns [`NormalizeDomainError::InvalidDomain`] for invalid IDN input,
    /// forbidden URL host characters, empty labels or excessive DNS lengths.
    pub fn parse(domain: &str) -> Result<Self, NormalizeDomainError> {
        let ascii = Uts46::new()
            .to_ascii(
                domain.as_bytes(),
                AsciiDenyList::URL,
                Hyphens::Allow,
                DnsLength::VerifyAllowRootDot,
            )
            .context(InvalidDomainSnafu)?;
        Ok(Self(ascii.trim_end_matches('.').to_owned()))
    }

    /// Borrows the ASCII storage and comparison key.
    #[must_use]
    pub fn as_ascii(&self) -> &str {
        &self.0
    }

    /// Decodes this validated key to its normalized Unicode spelling.
    #[must_use]
    pub fn to_unicode(&self) -> String {
        // Construction validates this key with the same UTS #46 options.
        Uts46::new()
            .to_unicode(self.0.as_bytes(), AsciiDenyList::URL, Hyphens::Allow)
            .0
            .into_owned()
    }

    /// Matches this domain or a subdomain at a complete label boundary.
    #[must_use]
    pub fn matches_suffix(&self, suffix: &Self) -> bool {
        self == suffix
            || self
                .0
                .strip_suffix(suffix.as_ascii())
                .is_some_and(|prefix| prefix.ends_with('.'))
    }
}

impl fmt::Display for DomainName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_ascii())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_reject_empty_normalized_output() {
        for value in ["", "\u{00ad}"] {
            assert!(
                matches!(
                    DomainFragment::parse(value),
                    Err(NormalizeDomainError::EmptyFragment)
                ),
                "{value:?}"
            );
        }
    }

    #[test]
    fn whole_domains_reject_empty_and_ignored_only_values() {
        for value in ["", "\u{00ad}"] {
            assert!(
                matches!(
                    DomainName::parse(value),
                    Err(NormalizeDomainError::InvalidDomain { .. })
                ),
                "{value:?}"
            );
        }
    }

    #[test]
    fn fragments_match_whole_domain_normalization_and_preserve_dot_boundaries() {
        for value in [
            "EXAMPLE.COM",
            "例子.测试",
            "faß.de",
            "MU\u{308}NCHEN.DE",
            "ＥＸＡＭＰＬＥ。ＣＯＭ",
            "例子．测试",
            "例子｡测试",
            "localhost",
        ] {
            assert_eq!(
                DomainFragment::parse(value).expect("fragment").as_ascii(),
                DomainName::parse(value).expect("whole domain").as_ascii(),
                "{value:?}"
            );
        }
        for value in [
            ".EXAMPLE.COM.",
            "。EXAMPLE。COM。",
            "．EXAMPLE．COM．",
            "｡EXAMPLE｡COM｡",
        ] {
            assert_eq!(
                DomainFragment::parse(value).expect("separators").as_ascii(),
                ".example.com."
            );
        }
    }

    #[test]
    fn fragments_reject_invalid_url_characters_and_punycode() {
        for value in [
            "bad domain",
            "example:443",
            "example/path",
            "xn--",
            "xn--.com",
            "a\0.com",
        ] {
            assert!(
                matches!(
                    DomainFragment::parse(value),
                    Err(NormalizeDomainError::InvalidDomain { .. })
                ),
                "{value:?}"
            );
        }
    }

    #[test]
    fn idn_ascii_unicode_and_punycode_round_trip() {
        for (input, ascii, unicode) in [
            ("例子.测试", "xn--fsqu00a.xn--0zwm56d", "例子.测试"),
            ("münchen.de", "xn--mnchen-3ya.de", "münchen.de"),
            ("XN--MNCHEN-3YA.DE", "xn--mnchen-3ya.de", "münchen.de"),
            ("MU\u{308}NCHEN.DE", "xn--mnchen-3ya.de", "münchen.de"),
            ("ＥＸＡＭＰＬＥ。ＣＯＭ.", "example.com", "example.com"),
            ("faß.de", "xn--fa-hia.de", "faß.de"),
            ("ab--cd.github.io", "ab--cd.github.io", "ab--cd.github.io"),
            ("localhost", "localhost", "localhost"),
        ] {
            let domain = DomainName::parse(input).expect("valid domain");
            assert_eq!(domain.as_ascii(), ascii, "input {input:?}");
            assert_eq!(domain.to_unicode(), unicode, "input {input:?}");
            assert_eq!(DomainName::parse(ascii).expect("ASCII round trip"), domain);
            assert_eq!(
                DomainName::parse(unicode).expect("Unicode round trip"),
                domain
            );
        }
        assert_ne!(
            DomainName::parse("faß.de").expect("valid IDN"),
            DomainName::parse("fass.de").expect("valid ASCII domain")
        );
    }

    #[test]
    fn suffix_matching_checks_label_boundaries() {
        let suffix = DomainName::parse("münchen.de").expect("valid suffix");
        for (input, expected) in [
            ("münchen.de", true),
            ("WWW.XN--MNCHEN-3YA.DE.", true),
            ("a.b.münchen.de", true),
            ("notmünchen.de", false),
            ("münchen.de.evil.example", false),
            ("de", false),
        ] {
            assert_eq!(
                DomainName::parse(input)
                    .expect("valid domain")
                    .matches_suffix(&suffix),
                expected,
                "input {input:?}"
            );
        }
    }

    #[test]
    fn invalid_domains_return_structured_private_errors() {
        let long_label = format!("{}.com", "a".repeat(64));
        let long_domain = std::array::from_fn::<_, 4, _>(|_| "a".repeat(63)).join(".");
        for input in [
            "",
            ".",
            "..",
            "a..com",
            ".example.com",
            "example.com..",
            "bad domain.com",
            "example.com:443",
            "https://example.com",
            "example.com/path",
            "a\0.com",
            "\u{fffd}.com",
            "\u{301}a.com",
            "a\u{200d}.com",
            "אa.com",
            "xn--.com",
            "xn--invalid-.com",
            &long_label,
            &long_domain,
        ] {
            let error = DomainName::parse(input).expect_err("invalid domain");
            assert!(
                matches!(error, NormalizeDomainError::InvalidDomain { .. }),
                "input {input:?}"
            );
            assert_eq!(error.to_string(), "invalid domain name");
            assert!(std::error::Error::source(&error).is_some());
        }
        let error =
            DomainName::parse("private-example.invalid/path").expect_err("path is forbidden");
        assert!(!format!("{error:?}").contains("private-example"));
        assert!(!error.to_string().contains("private-example"));
    }
}
