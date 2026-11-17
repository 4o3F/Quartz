//! Locale-aware text for durations, percentages, clock times and dates (10 section 6.1).

use std::time::Duration;

#[cfg(test)]
use super::model::TimeOfDay;
use gpui_kit::Global;

use super::model::{BuiltinCategory, CalendarDate, CategoryName, Share, Weekday};

/// Looks up translations and formats values for one interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Formatter {
    language: &'static str,
}

impl Global for Formatter {}

/// A duration split for rendering numbers and units in different type sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationParts {
    pub hours: Option<u64>,
    pub minutes: Option<u64>,
}

impl Formatter {
    /// Formats for `language`, one of the codes `quartz-i18n` negotiates.
    #[must_use]
    pub const fn new(language: &'static str) -> Self {
        Self { language }
    }

    /// The language code.
    #[must_use]
    pub const fn language(self) -> &'static str {
        self.language
    }

    /// Translates a key without placeholders.
    #[must_use]
    pub fn text(self, key: &str) -> String {
        quartz_i18n::translate_in_locale(key, self.language).into_owned()
    }

    /// Translates a key and fills `%{name}` placeholders.
    #[must_use]
    pub fn text_with(self, key: &str, values: &[(&str, &str)]) -> String {
        let mut text = self.text(key);
        for (name, value) in values {
            text = text.replace(&format!("%{{{name}}}"), value);
        }
        text
    }

    /// Short duration such as `3 小时 25 分` or `3h 25m`.
    #[must_use]
    pub fn duration(self, duration: Duration) -> String {
        let parts = DurationParts::from_duration(duration);
        match (parts.hours, parts.minutes) {
            (Some(hours), Some(minutes)) => self.text_with(
                "duration.hm",
                &[
                    ("hours", &hours.to_string()),
                    ("minutes", &minutes.to_string()),
                ],
            ),
            (Some(hours), None) => self.text_with("duration.h", &[("hours", &hours.to_string())]),
            (None, minutes) => self.text_with(
                "duration.m",
                &[("minutes", &minutes.unwrap_or(0).to_string())],
            ),
        }
    }

    /// Unit label for hours.
    #[must_use]
    pub fn hour_unit(self) -> String {
        self.text("duration.unit.hour")
    }

    /// Unit label for minutes.
    #[must_use]
    pub fn minute_unit(self) -> String {
        self.text("duration.unit.minute")
    }

    /// Whole percent such as `64%`.
    #[must_use]
    pub fn percent(self, share: Share) -> String {
        self.text_with("format.percent", &[("value", &share.percent().to_string())])
    }

    /// Display name of a category; user names are shown as typed.
    #[must_use]
    pub fn category_name(self, name: &CategoryName) -> String {
        let key = match name {
            CategoryName::Custom(text) => return text.clone(),
            CategoryName::Builtin(BuiltinCategory::Development) => "category.builtin.development",
            CategoryName::Builtin(BuiltinCategory::Learning) => "category.builtin.learning",
            CategoryName::Builtin(BuiltinCategory::Communication) => {
                "category.builtin.communication"
            }
            CategoryName::Builtin(BuiltinCategory::Entertainment) => {
                "category.builtin.entertainment"
            }
            CategoryName::Builtin(BuiltinCategory::System) => "category.builtin.system",
            CategoryName::Builtin(BuiltinCategory::Uncategorized) => {
                "category.builtin.uncategorized"
            }
        };
        self.text(key)
    }

    /// Weekday name.
    #[must_use]
    pub fn weekday(self, weekday: Weekday) -> String {
        let key = match weekday {
            Weekday::Mon => "date.weekday.mon",
            Weekday::Tue => "date.weekday.tue",
            Weekday::Wed => "date.weekday.wed",
            Weekday::Thu => "date.weekday.thu",
            Weekday::Fri => "date.weekday.fri",
            Weekday::Sat => "date.weekday.sat",
            Weekday::Sun => "date.weekday.sun",
        };
        self.text(key)
    }

    /// Month and day with weekday, such as `11月16日 周一` or `Mon, Nov 16`.
    #[must_use]
    pub fn date(self, date: CalendarDate) -> String {
        let month = self.text(&format!("date.month.{}", date.month));
        self.text_with(
            "date.month_day_weekday",
            &[
                ("month", &month),
                ("day", &date.day.to_string()),
                ("weekday", &self.weekday(date.weekday)),
            ],
        )
    }
}

impl DurationParts {
    /// Splits a duration into whole hours and remaining minutes, rounded to the nearest minute.
    /// Zero parts are omitted unless the whole duration is zero.
    #[must_use]
    pub fn from_duration(duration: Duration) -> Self {
        let minutes_total = (duration.as_secs() + 30) / 60;
        let (hours, minutes) = (minutes_total / 60, minutes_total % 60);
        DurationParts {
            hours: (hours > 0).then_some(hours),
            minutes: (minutes > 0 || hours == 0).then_some(minutes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZH: Formatter = Formatter::new("zh-CN");
    const EN: Formatter = Formatter::new("en");

    fn minutes(total: u64) -> Duration {
        Duration::from_secs(total * 60)
    }

    #[test]
    fn durations_follow_each_language() {
        assert_eq!(ZH.duration(minutes(205)), "3 小时 25 分");
        assert_eq!(EN.duration(minutes(205)), "3h 25m");
        assert_eq!(ZH.duration(minutes(180)), "3 小时");
        assert_eq!(EN.duration(minutes(15)), "15m");
        assert_eq!(ZH.duration(Duration::from_secs(29)), "0 分");
        assert_eq!(ZH.duration(Duration::from_secs(89)), "1 分");
    }

    #[test]
    fn percent_clock_and_date() {
        let share = Share::of(minutes(64), minutes(100));
        assert_eq!(ZH.percent(share), "64%");
        assert_eq!(TimeOfDay::hm(9, 5).to_string(), "09:05");
        let date = CalendarDate {
            month: 11,
            day: 16,
            weekday: Weekday::Mon,
        };
        assert_eq!(ZH.date(date), "11月16日 周一");
        assert_eq!(EN.date(date), "Mon, Nov 16");
    }
}
