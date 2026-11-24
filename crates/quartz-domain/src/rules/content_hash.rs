use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use snafu::Snafu;
use std::{fmt, str::FromStr};

/// SHA-256 encodes 256 bits as 64 hexadecimal digits.
const SHA256_HEX_DIGITS: usize = 64;

/// Lowercase hexadecimal SHA-256 of canonical content or an effective rule list.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    /// Hashes the exact UTF-8 bytes of the supplied content.
    pub fn of(content: &str) -> Self {
        Self(crate::text::encode_hex(&Sha256::digest(content.as_bytes())))
    }

    /// Borrows the 64 lowercase hexadecimal digits without allocating.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stored hash does not contain exactly 64 lowercase hexadecimal digits.
#[derive(Debug, Snafu)]
#[snafu(display("content hash must contain exactly 64 lowercase hexadecimal digits"))]
pub struct ParseContentHashError;

impl TryFrom<String> for ContentHash {
    type Error = ParseContentHashError;

    /// Validates an owned hash read from storage.
    ///
    /// # Errors
    /// Returns an error unless the value contains exactly 64 lowercase hexadecimal digits.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != SHA256_HEX_DIGITS
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ParseContentHashError);
        }
        Ok(Self(value))
    }
}

impl FromStr for ContentHash {
    type Err = ParseContentHashError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digest_and_string_serialization_round_trip() {
        let hash = ContentHash::of("abc");
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(hash.as_str(), expected);
        assert_eq!(hash.to_string(), expected);
        assert_eq!(hash, expected.parse().expect("canonical hash"));
        let json = serde_json::to_string(&hash).expect("hash serializes as a string");
        assert_eq!(json, format!("\"{expected}\""));
        assert_eq!(hash, serde_json::from_str(&json).expect("hash round trip"));
    }

    #[test]
    fn malformed_stored_hashes_are_rejected() {
        let hash = ContentHash::of("abc");
        for invalid in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            "g".repeat(64),
            hash.to_string().to_uppercase(),
            "é".repeat(32),
        ] {
            assert!(invalid.parse::<ContentHash>().is_err(), "{invalid:?}");
            let json = serde_json::to_string(&invalid).expect("invalid text is JSON-safe");
            assert!(serde_json::from_str::<ContentHash>(&json).is_err());
        }
    }
}
