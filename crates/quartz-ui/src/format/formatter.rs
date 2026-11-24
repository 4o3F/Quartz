//! Formats durations, percentages, dates and model values as text in one interface language.

use std::time::Duration;

use jiff::civil::{Date, Weekday};
use quartz_i18n::Language;

use super::message::{Arg, ArgValue, Msg};
#[cfg(windows)]
use crate::model::RankingKind;
use crate::model::{
    BuiltinCategory, CategoryName, ClassificationSource, Day, MINUTES_PER_HOUR, RuleCondition,
    SECONDS_PER_MINUTE, Share, StateKind,
};

/// Looks up translations and formats values for one interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Formatter {
    language: Language,
}

/// A duration rounded to whole minutes and split for rendering numbers and units in different
/// type sizes; zero parts are left out unless the whole duration is zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DurationParts {
    Minutes(u64),
    Hours(u64),
    HoursMinutes { hours: u64, minutes: u64 },
}

impl DurationParts {
    /// Splits `duration`, rounded to the nearest minute.
    #[must_use]
    pub(crate) fn from_duration(duration: Duration) -> Self {
        let seconds_per_minute = u64::from(SECONDS_PER_MINUTE);
        let minutes_per_hour = u64::from(MINUTES_PER_HOUR);
        let total = (duration.as_secs() + seconds_per_minute / 2) / seconds_per_minute;
        match (total / minutes_per_hour, total % minutes_per_hour) {
            (0, minutes) => Self::Minutes(minutes),
            (hours, 0) => Self::Hours(hours),
            (hours, minutes) => Self::HoursMinutes { hours, minutes },
        }
    }
}

/// How much a classification description says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SourceText<'a> {
    /// The rule condition alone, such as `规则：code.exe`.
    Short,
    /// The condition and the category it leads to, such as `规则：code.exe → 开发`.
    WithCategory(&'a CategoryName),
}

impl Formatter {
    /// Text for the nonempty hourly categories, retaining their measured durations.
    pub(crate) fn hourly_bar(self, bar: &crate::model::HourlyBar, day: &Day) -> Option<String> {
        let label = bar
            .visible_parts()
            .map(|part| {
                self.hourly_category(bar.hour, &day.category(part.category).name, part.total)
            })
            .collect::<Vec<_>>()
            .join("; ");
        (!label.is_empty()).then_some(label)
    }
    /// An hourly category label retains the complete measured duration.
    pub(crate) fn hourly_category(
        self,
        hour: u32,
        category: &CategoryName,
        duration: Duration,
    ) -> String {
        format!(
            "{} · {}",
            self.hour_tick(hour),
            self.text_with(
                Msg::LegendItem,
                &[
                    Arg::Name.with(&self.category_name(category)),
                    Arg::Duration.with(&self.detail_duration(duration)),
                ]
            )
        )
    }
    /// Raw intervals retain seconds and milliseconds instead of rounding short facts to minutes.
    pub(crate) fn detail_duration(self, duration: Duration) -> String {
        let seconds_per_minute = u64::from(crate::model::SECONDS_PER_MINUTE);
        let seconds = duration.as_secs();
        let millis = duration.subsec_millis();
        if seconds == 0 && millis > 0 {
            return self.text_with(
                Msg::DetailsDurationMillis,
                &[Arg::Value.with(&duration.as_millis().to_string())],
            );
        }
        if millis == 0 && seconds.is_multiple_of(seconds_per_minute) {
            return self.duration(duration);
        }
        let remainder = seconds % seconds_per_minute;
        let second_text = if millis == 0 {
            remainder.to_string()
        } else {
            format!("{remainder}.{millis:03}")
                .trim_end_matches('0')
                .to_owned()
        };
        let hours = seconds / Duration::from_hours(1).as_secs();
        let minutes = (seconds / seconds_per_minute) % u64::from(crate::model::MINUTES_PER_HOUR);
        if hours > 0 {
            self.text_with(
                Msg::DetailsDurationHoursMinutesSeconds,
                &[
                    Arg::Hours.with(&hours.to_string()),
                    Arg::Minutes.with(&minutes.to_string()),
                    Arg::Value.with(&second_text),
                ],
            )
        } else if minutes > 0 {
            self.text_with(
                Msg::DetailsDurationMinutesSeconds,
                &[
                    Arg::Minutes.with(&minutes.to_string()),
                    Arg::Value.with(&second_text),
                ],
            )
        } else {
            self.text_with(
                Msg::DetailsDurationSeconds,
                &[Arg::Value.with(&second_text)],
            )
        }
    }
    pub(crate) fn rule_label(self, label: &crate::model::live_details::RuleLabel) -> String {
        use crate::model::live_details::RuleLabel;
        match label {
            RuleLabel::Text(text) => text.clone(),
            RuleLabel::Key(key) => self
                .language
                .translate(quartz_i18n::MessageKey::new(key))
                .into_owned(),
            RuleLabel::Identity(id) => id.as_str().to_owned(),
        }
    }
    pub(crate) fn detail_source(self, source: &crate::model::live_details::DetailSource) -> String {
        use crate::model::live_details::DetailSource;
        match source {
            DetailSource::Rule(label) => self.text_with(
                Msg::DetailsRuleLine,
                &[Arg::Condition.with(&self.rule_label(label))],
            ),
            DetailSource::Manual => self.text(Msg::TooltipManual),
            DetailSource::Unclassified => self.text(Msg::DetailsUnclassified),
            DetailSource::Mixed => self.text(Msg::TooltipMixed),
        }
    }
}
impl Formatter {
    #[cfg(debug_assertions)]
    #[cfg(windows)]
    pub(crate) fn language(self) -> Language {
        self.language
    }
    /// Formats in `language`.
    #[must_use]
    pub(crate) const fn new(language: Language) -> Self {
        Self { language }
    }

    /// A message without placeholders.
    #[must_use]
    pub(crate) fn text(self, msg: Msg) -> String {
        self.language.translate(msg.key()).into_owned()
    }

    /// A message with its placeholders filled in one pass, so placeholder-like text inside a
    /// value is never substituted again.
    #[must_use]
    pub(crate) fn text_with(self, msg: Msg, values: &[ArgValue<'_>]) -> String {
        debug_assert!(
            values.len() == msg.args().len()
                && values.iter().all(|entry| msg.args().contains(&entry.arg)),
            "{msg:?} takes {:?}",
            msg.args()
        );
        fill_placeholders(&self.text(msg), values)
    }

    /// Short duration such as `3 小时 25 分` or `3h 25m`.
    #[must_use]
    pub(crate) fn duration(self, duration: Duration) -> String {
        match DurationParts::from_duration(duration) {
            DurationParts::HoursMinutes { hours, minutes } => self.text_with(
                Msg::DurationHoursMinutes,
                &[
                    Arg::Hours.with(&hours.to_string()),
                    Arg::Minutes.with(&minutes.to_string()),
                ],
            ),
            DurationParts::Hours(hours) => {
                self.text_with(Msg::DurationHours, &[Arg::Hours.with(&hours.to_string())])
            }
            DurationParts::Minutes(minutes) => self.whole_minutes(minutes),
        }
    }

    /// A count of minutes such as `60 分`, without converting to hours.
    #[must_use]
    pub(crate) fn whole_minutes(self, minutes: u64) -> String {
        self.text_with(
            Msg::DurationMinutes,
            &[Arg::Minutes.with(&minutes.to_string())],
        )
    }

    /// Whole percent such as `64%`.
    #[must_use]
    pub(crate) fn percent(self, share: Share) -> String {
        self.text_with(
            Msg::Percent,
            &[Arg::Value.with(&share.percent().to_string())],
        )
    }

    /// The application switches per hour of the status card.
    #[must_use]
    #[cfg(windows)]
    pub(crate) fn switches(self, per_hour: u64) -> String {
        self.text_with(
            Msg::StatSwitchCount,
            &[Arg::Count.with(&per_hour.to_string())],
        )
    }

    /// Display name of a category; user names are shown as typed.
    #[must_use]
    pub(crate) fn category_name(self, name: &CategoryName) -> String {
        let msg = match name {
            CategoryName::Custom(text) => return text.clone(),
            CategoryName::Builtin(BuiltinCategory::Development) => Msg::CategoryDevelopment,
            CategoryName::Builtin(BuiltinCategory::Learning) => Msg::CategoryLearning,
            CategoryName::Builtin(BuiltinCategory::Communication) => Msg::CategoryCommunication,
            CategoryName::Builtin(BuiltinCategory::Entertainment) => Msg::CategoryEntertainment,
            CategoryName::Builtin(BuiltinCategory::System) => Msg::CategorySystem,
            CategoryName::Builtin(BuiltinCategory::Uncategorized) => Msg::CategoryUncategorized,
        };
        self.text(msg)
    }

    /// Name of a non-foreground state.
    #[must_use]
    pub(crate) fn state_name(self, kind: StateKind) -> String {
        self.text(match kind {
            StateKind::Idle { .. } => Msg::StateIdle,
            StateKind::Locked => Msg::StateLocked,
            StateKind::Sleep => Msg::StateSleep,
            StateKind::CoreStopped => Msg::StateCoreStopped,
            StateKind::NoForeground => Msg::StateNoForeground,
            StateKind::Excluded => Msg::StateExcluded,
            StateKind::CollectionGap => Msg::StateGap,
        })
    }

    /// Label of a ranking tab.
    #[must_use]
    #[cfg(windows)]
    pub(crate) fn ranking_tab(self, kind: RankingKind) -> String {
        self.text(match kind {
            RankingKind::Apps => Msg::RankingApps,
            RankingKind::Domains => Msg::RankingDomains,
            RankingKind::Documents => Msg::RankingDocuments,
            RankingKind::Mixed => Msg::RankingMixed,
        })
    }

    /// Why a segment of `day` has its category.
    #[must_use]
    pub(crate) fn classification(
        self,
        day: &Day,
        source: &ClassificationSource,
        form: SourceText<'_>,
    ) -> String {
        match source {
            ClassificationSource::ProtocolRule(rule) => match form {
                SourceText::Short => {
                    self.text_with(Msg::DetailsRuleLine, &[Arg::Condition.with(rule.as_str())])
                }
                SourceText::WithCategory(category) => self.text_with(
                    Msg::TooltipRule,
                    &[
                        Arg::Condition.with(rule.as_str()),
                        Arg::Category.with(&self.category_name(category)),
                    ],
                ),
            },
            ClassificationSource::Mixed => self.text(Msg::TooltipMixed),
            ClassificationSource::Rule { rule, .. } => {
                let condition = day.rule(*rule).condition.value.as_str();
                match form {
                    SourceText::Short => {
                        self.text_with(Msg::DetailsRuleLine, &[Arg::Condition.with(condition)])
                    }
                    SourceText::WithCategory(category) => self.text_with(
                        Msg::TooltipRule,
                        &[
                            Arg::Condition.with(condition),
                            Arg::Category.with(&self.category_name(category)),
                        ],
                    ),
                }
            }
            ClassificationSource::Manual => self.text(Msg::TooltipManual),
            ClassificationSource::Unmatched => self.text(Msg::TooltipUnmatched),
        }
    }

    /// A rule condition as rule text, such as `app.exe_name equals "code.exe"`.
    #[must_use]
    pub(crate) fn rule_condition(self, condition: &RuleCondition) -> String {
        self.text_with(
            Msg::DetailsCondition,
            &[
                Arg::Field.with(condition.field.as_str()),
                Arg::Operator.with(condition.operator.as_str()),
                Arg::Value.with(&format!("{:?}", condition.value)),
            ],
        )
    }

    /// The hour number on the compact chart axis; the axis omits minutes and the day period.
    pub(crate) fn hour_tick(self, hour: u32) -> String {
        hour.to_string()
    }

    /// Weekday name.
    #[must_use]
    pub(crate) fn weekday(self, weekday: Weekday) -> String {
        self.text(match weekday {
            Weekday::Monday => Msg::WeekdayMonday,
            Weekday::Tuesday => Msg::WeekdayTuesday,
            Weekday::Wednesday => Msg::WeekdayWednesday,
            Weekday::Thursday => Msg::WeekdayThursday,
            Weekday::Friday => Msg::WeekdayFriday,
            Weekday::Saturday => Msg::WeekdaySaturday,
            Weekday::Sunday => Msg::WeekdaySunday,
        })
    }

    /// Month and day with weekday, such as `11月16日 周一` or `Mon, Nov 16`.
    #[must_use]
    pub(crate) fn date(self, date: Date) -> String {
        let month = Msg::month(date.month()).expect("a civil Date always has a month in 1..=12");
        self.text_with(
            Msg::MonthDayWeekday,
            &[
                Arg::Month.with(&self.text(month)),
                Arg::Day.with(&date.day().to_string()),
                Arg::Weekday.with(&self.weekday(date.weekday())),
            ],
        )
    }
}

/// Replaces every `%{name}` whose name appears in `values`; other text, including placeholders
/// without a value, is copied unchanged.
fn fill_placeholders(template: &str, values: &[ArgValue<'_>]) -> String {
    let mut text = String::with_capacity(template.len());
    let mut rest = template;
    while let Some((before, inner)) = rest.split_once("%{") {
        text.push_str(before);
        let filled = inner.split_once('}').and_then(|(name, after)| {
            values
                .iter()
                .find(|entry| entry.arg.name() == name)
                .map(|entry| (entry.value, after))
        });
        match filled {
            Some((value, after)) => {
                text.push_str(value);
                rest = after;
            }
            None => {
                text.push_str("%{");
                rest = inner;
            }
        }
    }
    text.push_str(rest);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AwayLabel, RuleField, RuleOperator, TimeOfDay};

    const ZH: Formatter = Formatter::new(Language::SimplifiedChinese);
    const EN: Formatter = Formatter::new(Language::English);

    fn minutes(total: u64) -> Duration {
        Duration::from_secs(total * 60)
    }

    #[test]
    fn detail_durations_preserve_subminute_elapsed_time() {
        assert_eq!(ZH.detail_duration(Duration::from_millis(75)), "75 毫秒");
        assert_eq!(EN.detail_duration(Duration::from_millis(75)), "75 ms");
        assert_eq!(ZH.detail_duration(Duration::from_millis(1500)), "1.5 秒");
        assert_eq!(EN.detail_duration(Duration::from_secs(65)), "1 min 5 s");
        assert_eq!(
            ZH.detail_duration(Duration::from_secs(3605)),
            "1 小时 0 分 5 秒"
        );
        assert_eq!(ZH.detail_duration(Duration::from_secs(60)), "1 分");
    }
    #[test]
    fn durations_follow_each_language() {
        assert_eq!(ZH.duration(minutes(205)), "3 小时 25 分");
        assert_eq!(EN.duration(minutes(205)), "3h 25m");
        assert_eq!(ZH.duration(minutes(180)), "3 小时");
        assert_eq!(EN.duration(minutes(15)), "15m");
        assert_eq!(ZH.duration(Duration::from_secs(29)), "0 分");
        assert_eq!(ZH.duration(Duration::from_secs(89)), "1 分");
        assert_eq!(ZH.whole_minutes(60), "60 分");
    }

    #[test]
    fn duration_parts_leave_out_zero_parts() {
        assert_eq!(
            DurationParts::from_duration(Duration::ZERO),
            DurationParts::Minutes(0)
        );
        assert_eq!(
            DurationParts::from_duration(minutes(120)),
            DurationParts::Hours(2)
        );
        assert_eq!(
            DurationParts::from_duration(minutes(61)),
            DurationParts::HoursMinutes {
                hours: 1,
                minutes: 1
            }
        );
    }

    #[test]
    fn percent_clock_and_date() {
        let share = Share::of(minutes(64), minutes(100));
        assert_eq!(ZH.percent(share), "64%");
        assert_eq!(TimeOfDay::hm(9, 5).to_string(), "09:05");
        let date = jiff::civil::date(2026, 11, 16);
        assert_eq!(ZH.date(date), "11月16日 周一");
        assert_eq!(EN.date(date), "Mon, Nov 16");
    }

    #[test]
    fn placeholder_text_inside_a_value_is_not_substituted_again() {
        let label = EN.text_with(
            Msg::TimelineBlockLabel,
            &[
                Arg::Name.with("%{duration} %{name}"),
                Arg::Duration.with("5m"),
            ],
        );
        assert_eq!(label, "%{duration} %{name} 5m");
    }

    #[test]
    fn unknown_and_unclosed_placeholders_are_copied() {
        assert_eq!(
            fill_placeholders("a %{x} %{y", &[Arg::Name.with("1")]),
            "a %{x} %{y"
        );
        assert_eq!(
            fill_placeholders("%{name}%{name}", &[Arg::Name.with("1")]),
            "11"
        );
    }

    #[test]
    fn rule_conditions_quote_and_escape_the_value() {
        let condition = RuleCondition {
            field: RuleField::AppExeName,
            operator: RuleOperator::Equals,
            value: r#"a "b".exe"#.to_owned(),
        };
        assert_eq!(
            EN.rule_condition(&condition),
            r#"app.exe_name equals "a \"b\".exe""#
        );
    }

    #[test]
    fn protocol_rule_tooltip_includes_the_category_in_each_language() {
        let day = crate::model::fixture::day(vec![], vec![]);
        let rule = ClassificationSource::ProtocolRule("builtin/editor@1".parse().expect("rule"));
        let work = CategoryName::Builtin(BuiltinCategory::Development);
        for formatter in [ZH, EN, Formatter::new(Language::PseudoEnglish)] {
            let tooltip = formatter.classification(&day, &rule, SourceText::WithCategory(&work));
            assert_eq!(
                tooltip,
                formatter.text_with(
                    Msg::TooltipRule,
                    &[
                        Arg::Condition.with("builtin/editor@1"),
                        Arg::Category.with(&formatter.category_name(&work)),
                    ]
                )
            );
        }
    }

    #[test]
    fn classification_text_names_the_rule_value_or_the_source() {
        use crate::model::{
            CategoryName,
            fixture::{EDITOR_RULE, day},
        };
        let day = day(vec![], vec![]);
        let rule = ClassificationSource::Rule {
            rule: EDITOR_RULE,
            actual: "editor.exe".to_owned(),
        };
        let work = CategoryName::Builtin(BuiltinCategory::Development);
        assert_eq!(
            EN.classification(&day, &rule, SourceText::Short),
            "Rule: editor.exe"
        );
        assert_eq!(
            EN.classification(&day, &rule, SourceText::WithCategory(&work)),
            "Rule: editor.exe → Development"
        );
        assert_eq!(
            EN.classification(&day, &ClassificationSource::Manual, SourceText::Short),
            "Manual label"
        );
        assert_eq!(
            EN.classification(&day, &ClassificationSource::Unmatched, SourceText::Short),
            "No rule matched"
        );
    }

    #[test]
    fn every_state_has_a_name() {
        let kinds = [
            StateKind::Idle {
                preceding: None,
                away: AwayLabel::Settled,
            },
            StateKind::Locked,
            StateKind::Sleep,
            StateKind::CoreStopped,
            StateKind::NoForeground,
            StateKind::Excluded,
            StateKind::CollectionGap,
        ];
        let names: std::collections::BTreeSet<String> =
            kinds.into_iter().map(|kind| EN.state_name(kind)).collect();
        assert_eq!(names.len(), kinds.len());
    }
}
