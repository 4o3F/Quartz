//! Regional settings for state subscriptions, independent of the UI catalog.

use quartz_protocol::{HourCycle, LocaleSettings, Weekday};
use windows::{Win32::Globalization, core::PCWSTR};

#[derive(Debug, Clone, Copy)]
pub(crate) enum LocaleSetting {
    TimeFormat,
    FirstDayOfWeek,
}
impl LocaleSetting {
    fn lctype(self) -> u32 {
        match self {
            Self::TimeFormat => Globalization::LOCALE_STIMEFORMAT,
            Self::FirstDayOfWeek => Globalization::LOCALE_IFIRSTDAYOFWEEK,
        }
    }
}

#[derive(Debug, snafu::Snafu)]
pub(crate) enum ReadLocaleError {
    #[snafu(display("could not read Windows regional setting {setting:?}"))]
    Windows {
        setting: LocaleSetting,
        source: windows::core::Error,
    },
    #[snafu(display("Windows regional setting {setting:?} has an unsupported value"))]
    Value { setting: LocaleSetting },
    #[snafu(display("Windows regional setting is not valid UTF-16"))]
    Encoding { source: std::string::FromUtf16Error },
    #[snafu(display("Windows regional setting buffer size is invalid"))]
    Size { source: std::num::TryFromIntError },
}

pub(crate) struct UserLocale;
impl UserLocale {
    pub(crate) fn read() -> Result<LocaleSettings, ReadLocaleError> {
        let format = Self::text(LocaleSetting::TimeFormat)?;
        let day = Self::text(LocaleSetting::FirstDayOfWeek)?;
        Ok(LocaleSettings {
            hour_cycle: Self::hour_cycle(&format).ok_or(ReadLocaleError::Value {
                setting: LocaleSetting::TimeFormat,
            })?,
            first_day_of_week: match day.as_str() {
                "0" => Weekday::Monday,
                "1" => Weekday::Tuesday,
                "2" => Weekday::Wednesday,
                "3" => Weekday::Thursday,
                "4" => Weekday::Friday,
                "5" => Weekday::Saturday,
                "6" => Weekday::Sunday,
                _ => {
                    return Err(ReadLocaleError::Value {
                        setting: LocaleSetting::FirstDayOfWeek,
                    });
                }
            },
        })
    }
    fn text(setting: LocaleSetting) -> Result<String, ReadLocaleError> {
        // SAFETY: A null locale name selects the current user's locale; no output buffer queries its size.
        let length =
            unsafe { Globalization::GetLocaleInfoEx(PCWSTR::null(), setting.lctype(), None) };
        if length == 0 {
            return Err(ReadLocaleError::Windows {
                setting,
                source: windows::core::Error::from_thread(),
            });
        }
        let mut buffer =
            vec![0; usize::try_from(length).map_err(|source| ReadLocaleError::Size { source })?];
        // SAFETY: The buffer has the queried number of UTF-16 units, including its terminating null.
        if unsafe {
            Globalization::GetLocaleInfoEx(PCWSTR::null(), setting.lctype(), Some(&mut buffer))
        } == 0
        {
            return Err(ReadLocaleError::Windows {
                setting,
                source: windows::core::Error::from_thread(),
            });
        }
        let end = buffer
            .iter()
            .position(|unit| *unit == 0)
            .ok_or(ReadLocaleError::Value { setting })?;
        String::from_utf16(&buffer[..end]).map_err(|source| ReadLocaleError::Encoding { source })
    }
    fn hour_cycle(format: &str) -> Option<HourCycle> {
        let mut quoted = QuoteState::Outside;
        for ch in format.chars() {
            match ch {
                '\'' => {
                    quoted = match quoted {
                        QuoteState::Outside => QuoteState::Inside,
                        QuoteState::Inside => QuoteState::Outside,
                    }
                }
                'H' if quoted == QuoteState::Outside => return Some(HourCycle::H24),
                'h' if quoted == QuoteState::Outside => return Some(HourCycle::H12),
                _ => {}
            }
        }
        None
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    Outside,
    Inside,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patterns_ignore_quoted_hour_letters() {
        assert_eq!(UserLocale::hour_cycle("'h' HH:mm:ss"), Some(HourCycle::H24));
        assert_eq!(UserLocale::hour_cycle("'H' h:mm tt"), Some(HourCycle::H12));
        assert_eq!(UserLocale::hour_cycle("'HH' mm:ss"), None);
        assert_eq!(
            UserLocale::hour_cycle("h 'o''clock' tt"),
            Some(HourCycle::H12)
        );
    }
    #[test]
    fn windows_user_locale_resolves_without_defaults() {
        UserLocale::read().expect("Windows user regional settings");
    }
}
