//! UTF-16 encoding for the fixed Windows tray tooltip buffer.

use snafu::Snafu;

/// NOTIFYICONDATAW::szTip contains 128 UTF-16 units, including its NUL terminator.
pub(crate) const TOOLTIP_BUFFER_UNITS: usize = 128;
/// The final szTip unit is reserved for NUL (07 §1.2).
pub(crate) const TOOLTIP_TEXT_UNITS: usize = TOOLTIP_BUFFER_UNITS - 1;

/// Clock periods each span twelve civil hours.
const HOURS_PER_PERIOD: i8 = 12;

/// All shell tooltip templates, shared with catalog verification on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayTooltip {
    Ready,
    Opening,
    Upgrading,
    PersistenceFailed,
    SessionUnavailable,
    CaptureGap,
    Paused {
        until_utc_ms: i64,
        hour_cycle: Option<quartz_protocol::HourCycle>,
    },
}
impl TrayTooltip {
    #[cfg(test)]
    pub(crate) const ALL: &[Self] = &[
        Self::Ready,
        Self::Opening,
        Self::Upgrading,
        Self::PersistenceFailed,
        Self::SessionUnavailable,
        Self::CaptureGap,
    ];
    pub(crate) fn text(self, language: quartz_i18n::Language) -> String {
        self.text_with_zone(language, jiff::tz::TimeZone::try_system)
    }
    fn text_with_zone(
        self,
        language: quartz_i18n::Language,
        resolve: impl FnOnce() -> Result<jiff::tz::TimeZone, jiff::Error>,
    ) -> String {
        if let Self::Paused {
            until_utc_ms,
            hour_cycle,
        } = self
        {
            let time = Self::resume_time(until_utc_ms, resolve);
            let time = match time {
                Ok(time) => time,
                Err(error) => {
                    tracing::warn!(%error, "could not resolve pause resume time");
                    return quartz_i18n::MessageKey::new("tray.paused_unknown_time")
                        .in_language(language)
                        .into_owned();
                }
            };
            return self.paused_text(language, time, hour_cycle);
        }
        self.key().in_language(language).into_owned()
    }
    fn resume_time(
        until_utc_ms: i64,
        resolve: impl FnOnce() -> Result<jiff::tz::TimeZone, jiff::Error>,
    ) -> Result<jiff::civil::Time, ResolveResumeTimeError> {
        let zone = resolve().map_err(|source| ResolveResumeTimeError::TimeZone { source })?;
        let timestamp = jiff::Timestamp::from_millisecond(until_utc_ms)
            .map_err(|source| ResolveResumeTimeError::Timestamp { source })?;
        Ok(timestamp.to_zoned_datetime(zone).time())
    }
    fn paused_text(
        self,
        language: quartz_i18n::Language,
        time: jiff::civil::Time,
        hour_cycle: Option<quartz_protocol::HourCycle>,
    ) -> String {
        let template = match hour_cycle {
            Some(quartz_protocol::HourCycle::H24) => "tray.time_24",
            Some(quartz_protocol::HourCycle::H12) => "tray.time_12",
            None => {
                return quartz_i18n::MessageKey::new("tray.paused_unknown_time")
                    .in_language(language)
                    .into_owned();
            }
        };
        let hour = match hour_cycle {
            Some(quartz_protocol::HourCycle::H12) => {
                let hour = time.hour() % HOURS_PER_PERIOD;
                if hour == 0 { HOURS_PER_PERIOD } else { hour }
            }
            _ => time.hour(),
        };
        let period = quartz_i18n::MessageKey::new(if time.hour() < HOURS_PER_PERIOD {
            "tray.am"
        } else {
            "tray.pm"
        })
        .in_language(language);
        let formatted = quartz_i18n::MessageKey::new(template)
            .in_language(language)
            .replace("%{hour}", &format!("{hour:02}"))
            .replace("%{minute}", &format!("{:02}", time.minute()))
            .replace("%{period}", &period);
        self.key()
            .in_language(language)
            .replace("%{time}", &formatted)
    }
    pub(crate) fn key(self) -> quartz_i18n::MessageKey<'static> {
        quartz_i18n::MessageKey::new(match self {
            Self::Ready => "app.name",
            Self::Opening => "tray.opening",
            Self::Upgrading => "tray.upgrading",
            Self::PersistenceFailed => "tray.persistence_failed",
            Self::SessionUnavailable => "tray.session_unavailable",
            Self::CaptureGap => "tray.capture_gap",
            Self::Paused { .. } => "tray.paused",
        })
    }
}

#[derive(Debug, Snafu)]
enum ResolveResumeTimeError {
    #[snafu(display("could not resolve system time zone: {source}"))]
    TimeZone { source: jiff::Error },
    #[snafu(display("invalid pause deadline: {source}"))]
    Timestamp { source: jiff::Error },
}

/// A tooltip that cannot fit together with its terminating NUL.
#[derive(Debug, Snafu)]
#[snafu(display(
    "tray tooltip needs {units} UTF-16 units; at most {TOOLTIP_TEXT_UNITS} are available"
))]
pub(super) struct FormatTrayTooltipError {
    units: usize,
}

/// Complete UTF-16 tooltip with space reserved for its terminating NUL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrayTooltipText([u16; TOOLTIP_BUFFER_UNITS]);

impl TrayTooltipText {
    /// Transfers the fixed buffer to NOTIFYICONDATAW.
    pub(crate) fn into_buffer(self) -> [u16; TOOLTIP_BUFFER_UNITS] {
        self.0
    }
}

impl TryFrom<&str> for TrayTooltipText {
    type Error = FormatTrayTooltipError;

    /// Encodes text without truncation, reserving one UTF-16 unit for the terminator.
    ///
    /// # Errors
    /// Returns an error when the text needs more than 127 UTF-16 units.
    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let units = text.encode_utf16().count();
        if units > TOOLTIP_TEXT_UNITS {
            return FormatTrayTooltipSnafu { units }.fail();
        }
        let mut buffer = [0; TOOLTIP_BUFFER_UNITS];
        for (slot, unit) in buffer.iter_mut().zip(text.encode_utf16()) {
            *slot = unit;
        }
        Ok(Self(buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_resume_timezone_displays_localized_unknown_time() {
        for &language in quartz_i18n::Language::ALL {
            let tooltip = TrayTooltip::Paused {
                until_utc_ms: 0,
                hour_cycle: Some(quartz_protocol::HourCycle::H24),
            };
            assert_eq!(
                tooltip.text_with_zone(language, || jiff::tz::TimeZone::get("Quartz/Unavailable")),
                quartz_i18n::MessageKey::new("tray.paused_unknown_time").in_language(language)
            );
        }
    }
    #[test]
    fn every_tray_text_key_resolves_in_both_catalogs() {
        for locale in [
            quartz_i18n::Language::English,
            quartz_i18n::Language::SimplifiedChinese,
        ] {
            for key in TrayTooltip::ALL.iter().map(|tooltip| tooltip.key()).chain([
                quartz_i18n::MessageKey::new("tray.open"),
                quartz_i18n::MessageKey::new("tray.exit"),
                quartz_i18n::MessageKey::new("tray.paused"),
                quartz_i18n::MessageKey::new("tray.paused_unknown_time"),
                quartz_i18n::MessageKey::new("tray.time_12"),
                quartz_i18n::MessageKey::new("tray.time_24"),
                quartz_i18n::MessageKey::new("tray.am"),
                quartz_i18n::MessageKey::new("tray.pm"),
            ]) {
                assert!(key.is_in_catalog(locale), "{locale}: {key}");
                assert_ne!(key.in_language(locale), key.as_str());
            }
        }
    }

    #[test]
    fn tooltip_counts_utf16_units_and_reserves_a_terminator() {
        let text = format!("{}a", "😀".repeat(63));
        let buffer = TrayTooltipText::try_from(text.as_str())
            .expect("127 UTF-16 units fit")
            .into_buffer();
        assert_eq!(
            String::from_utf16(&buffer[..127]).expect("complete UTF-16"),
            text
        );
        assert_eq!(buffer[127], 0);
        assert!(
            TrayTooltipText::try_from("😀".repeat(64).as_str()).is_err(),
            "128 units leave no terminator"
        );
        assert_eq!(
            TrayTooltipText::try_from("")
                .expect("empty tooltip")
                .into_buffer(),
            [0; TOOLTIP_BUFFER_UNITS]
        );
    }
    #[test]
    fn paused_tooltip_localizes_both_hour_cycles_without_exceeding_shell_limit() {
        use quartz_i18n::Language;
        use quartz_protocol::HourCycle;
        let time = jiff::civil::time(14, 30, 0, 0);
        for language in [Language::English, Language::SimplifiedChinese] {
            for cycle in [HourCycle::H12, HourCycle::H24] {
                let tooltip = TrayTooltip::Paused {
                    until_utc_ms: 0,
                    hour_cycle: Some(cycle),
                };
                let text = tooltip.paused_text(language, time, Some(cycle));
                let expected = match language {
                    Language::English => match cycle {
                        HourCycle::H12 => "Paused · resumes at 02:30 PM",
                        HourCycle::H24 => "Paused · resumes at 14:30",
                    },
                    Language::SimplifiedChinese => match cycle {
                        HourCycle::H12 => "已暂停 · 下午02:30 自动恢复",
                        HourCycle::H24 => "已暂停 · 14:30 自动恢复",
                    },
                    #[cfg(debug_assertions)]
                    Language::PseudoEnglish => unreachable!("production language cases"),
                };
                assert_eq!(text, expected);
                TrayTooltipText::try_from(text.as_str()).expect("paused tooltip fits");
            }
        }
        let tooltip = TrayTooltip::Paused {
            until_utc_ms: 0,
            hour_cycle: None,
        };
        assert_eq!(
            tooltip.paused_text(Language::English, time, None),
            "Paused · automatic resume scheduled"
        );
        for hour in [0, 12] {
            let text = tooltip.paused_text(
                Language::English,
                jiff::civil::time(hour, 0, 0, 0),
                Some(HourCycle::H12),
            );
            assert!(text.contains("12:00"));
        }
    }
    #[test]
    fn every_tooltip_and_maximum_paused_clock_fit_in_every_language() {
        use quartz_i18n::Language;
        use quartz_protocol::HourCycle;
        for &language in Language::ALL {
            for &tooltip in TrayTooltip::ALL {
                let text = tooltip.text(language);
                TrayTooltipText::try_from(text.as_str())
                    .expect("production tooltip fits shell buffer");
            }
            let tooltip = TrayTooltip::Paused {
                until_utc_ms: 0,
                hour_cycle: None,
            };
            for time in [
                jiff::civil::time(0, 59, 59, 0),
                jiff::civil::time(12, 59, 59, 0),
                jiff::civil::time(23, 59, 59, 0),
            ] {
                for cycle in [Some(HourCycle::H12), Some(HourCycle::H24), None] {
                    let text = tooltip.paused_text(language, time, cycle);
                    assert!(!text.contains("%{"), "resolved placeholders for {language}");
                    TrayTooltipText::try_from(text.as_str())
                        .expect("maximum paused clock fits shell buffer");
                }
            }
        }
    }
}
