//! Subscription topics with validated calendar dates (04 §7).

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use snafu::Snafu;

/// Calendar date used for logical-day subscriptions.
pub use jiff::civil::Date as LocalDate;

/// Topics supported by the core; unknown subscriptions must return `bad_request`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Topic {
    /// Current recording and application state.
    State,
    /// Data-change notifications for a logical local date.
    Day(LocalDate),
    /// UI and browser control notifications.
    Control,
}

/// Topic parsing failed because the name or calendar date is invalid.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum ParseTopicError {
    /// Unknown topic or noncanonical date spelling.
    #[snafu(display("unknown or malformed subscription topic: {value}"))]
    Name {
        /// Received topic text.
        value: String,
    },
    /// A correctly shaped date is not a valid calendar date.
    #[snafu(display("invalid subscription date: {source}"))]
    Date {
        /// Calendar validation failure.
        source: jiff::Error,
    },
}

/// `YYYY-MM-DD` is ten ASCII bytes with two hyphens.
const DATE_TEXT_LEN: usize = 10;
/// Separators in a four-digit-year ISO calendar date.
const DATE_HYPHENS: [usize; 2] = [4, 7];

impl FromStr for Topic {
    type Err = ParseTopicError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "state" => Ok(Self::State),
            "control" => Ok(Self::Control),
            _ => {
                let date = value
                    .strip_prefix("day:")
                    .filter(|date| {
                        date.len() == DATE_TEXT_LEN
                            && date.bytes().enumerate().all(|(index, byte)| {
                                if DATE_HYPHENS.contains(&index) {
                                    byte == b'-'
                                } else {
                                    byte.is_ascii_digit()
                                }
                            })
                    })
                    .ok_or_else(|| ParseTopicError::Name {
                        value: value.to_owned(),
                    })?;
                date.parse::<LocalDate>()
                    .map(Self::Day)
                    .map_err(|source| ParseTopicError::Date { source })
            }
        }
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State => f.write_str("state"),
            Self::Day(date) => write!(f, "day:{date}"),
            Self::Control => f.write_str("control"),
        }
    }
}

impl Serialize for Topic {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Self::Day(date) = self
            && date.year() < 0
        {
            return Err(serde::ser::Error::custom(
                "day topics require a four-digit unsigned year",
            ));
        }
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Topic {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Typed body for `subscribe`, rejecting unknown topics before authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribe {
    /// Requested state, logical-day, or control subscriptions.
    pub topics: Vec<Topic>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_round_trip_with_calendar_validation() {
        for text in ["state", "day:2026-11-16", "control", "day:2024-02-29"] {
            let topic: Topic = text.parse().expect("known topic");
            assert_eq!(topic.to_string(), text);
            assert_eq!(
                serde_json::to_value(&topic).expect("serialize"),
                serde_json::json!(text)
            );
            assert_eq!(
                serde_json::from_value::<Topic>(serde_json::json!(text)).expect("deserialize"),
                topic
            );
        }
        for text in [
            "day:2026-13-01",
            "weather",
            "day:2026-02-29",
            "day:2026-1-01",
            "day:2026-11-16T00:00:00",
            "day:",
        ] {
            assert!(text.parse::<Topic>().is_err(), "{text}");
            assert!(
                serde_json::from_value::<Subscribe>(serde_json::json!({"topics":[text]})).is_err(),
                "{text}"
            );
        }
    }
}
