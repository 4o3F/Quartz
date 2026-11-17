//! Typed ids for every translation the UI shows, with the placeholders each one takes. A test
//! checks every id against the `en` and `zh-CN` catalogs, so a typo is a test failure instead of
//! a key shown on screen.

/// Defines [`Msg`] with its catalog key and placeholders per variant.
macro_rules! messages {
    ($($(#[$doc:meta])* $variant:ident => $key:literal $([$($arg:ident),+])?,)+) => {
        /// A translation the UI shows.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub(crate) enum Msg {
            $($(#[$doc])* $variant,)+
        }

        impl Msg {
            /// Every message.
            #[cfg(test)]
            pub(crate) const ALL: &[Self] = &[$(Self::$variant,)+];

            /// The catalog key.
            #[must_use]
            pub(crate) const fn key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key,)+
                }
            }

            /// The placeholders the message takes.
            #[must_use]
            pub(crate) const fn args(self) -> &'static [Arg] {
                match self {
                    $(Self::$variant => &[$($(Arg::$arg),+)?],)+
                }
            }
        }
    };
}

messages! {
    AppName => "app.name",
    CoreErrorNewerDatabaseTitle => "ui.core_error.newer_database.title",
    CoreErrorNewerDatabaseMessage => "ui.core_error.newer_database.message",
    CoreErrorMigrationFailedTitle => "ui.core_error.migration_failed.title",
    CoreErrorMigrationFailedMessage => "ui.core_error.migration_failed.message",
    CoreErrorOtherSessionTitle => "ui.core_error.other_session.title",
    CoreErrorOtherSessionMessage => "ui.core_error.other_session.message",
    CoreErrorStartFailedTitle => "ui.core_error.start_failed.title",
    CoreErrorStartFailedMessage => "ui.core_error.start_failed.message",
    CoreErrorBackups => "ui.core_error.backups",
    CoreErrorRestoreWarning => "ui.core_error.restore_warning",
    CoreErrorRestoreManually => "ui.core_error.restore_manually" [Command],
    NavToday => "nav.today",
    NavWeek => "nav.week",
    NavRules => "nav.rules",
    NavSettings => "nav.settings",
    TodayButton => "today.today",
    StatActive => "today.stat.active",
    StatFocus => "today.stat.focus",
    StatDistraction => "today.stat.distraction",
    StatUnclassified => "today.stat.unclassified",
    StatLongestFocus => "today.stat.longest_focus",
    StatSwitches => "today.stat.switches",
    StatSwitchCount => "today.stat.switch_count" [Count],
    LegendItem => "today.legend_item" [Name, Duration],
    HourlyTitle => "today.hourly.title",
    RankingTitle => "today.ranking.title",
    RankingApps => "today.ranking.apps",
    RankingDomains => "today.ranking.domains",
    RankingDocuments => "today.ranking.documents",
    RankingMixed => "today.ranking.mixed",
    PendingTitle => "today.pending.title",
    PendingRange => "today.pending.range" [Range, Duration],
    PendingRest => "today.pending.rest",
    PendingMeeting => "today.pending.meeting",
    PendingReading => "today.pending.reading",
    PendingDismiss => "today.pending.dismiss",
    PendingOther => "today.pending.other",
    TimelineTitle => "timeline.title",
    TimelineMainTrack => "timeline.main_track",
    TimelineLanesApps => "timeline.lanes.apps",
    TimelineLanesCategory => "timeline.lanes.category",
    TimelineFullDay => "timeline.full_day",
    TimelineFitActivity => "timeline.fit_activity",
    TimelineBlockLabel => "timeline.block_label" [Name, Duration],
    TimelineStateLabel => "timeline.state_label" [State, Duration],
    StateIdle => "timeline.state.idle",
    StateLocked => "timeline.state.locked",
    StateSleep => "timeline.state.sleep",
    StateCoreStopped => "timeline.state.core_stopped",
    StateNoForeground => "timeline.state.no_foreground",
    StateExcluded => "timeline.state.excluded",
    StateGap => "timeline.state.gap",
    TooltipRule => "timeline.tooltip.rule" [Condition, Category],
    TooltipManual => "timeline.tooltip.manual",
    TooltipUnmatched => "timeline.tooltip.unmatched",
    DetailsWeek => "details.week",
    DetailsTitles => "details.titles",
    DetailsWhy => "details.why",
    DetailsToday => "details.today",
    DetailsMerged => "details.merged" [Count],
    DetailsRuleLine => "details.rule_line" [Condition],
    DetailsRulePriority => "details.rule_priority" [Rule, Priority],
    DetailsEffective => "details.effective",
    DetailsRuleSet => "details.rule_set" [Name],
    DetailsActual => "details.actual" [Value],
    DetailsCondition => "details.condition" [Field, Operator, Value],
    DetailsNoShadowed => "details.no_shadowed",
    DetailsNoRule => "details.no_rule",
    DetailsClassify => "details.action.classify",
    DetailsPrivacy => "details.action.privacy",
    DurationHoursMinutes => "duration.hm" [Hours, Minutes],
    DurationHours => "duration.h" [Hours],
    DurationMinutes => "duration.m" [Minutes],
    UnitHour => "duration.unit.hour",
    UnitMinute => "duration.unit.minute",
    Percent => "format.percent" [Value],
    MonthJanuary => "date.month.1",
    MonthFebruary => "date.month.2",
    MonthMarch => "date.month.3",
    MonthApril => "date.month.4",
    MonthMay => "date.month.5",
    MonthJune => "date.month.6",
    MonthJuly => "date.month.7",
    MonthAugust => "date.month.8",
    MonthSeptember => "date.month.9",
    MonthOctober => "date.month.10",
    MonthNovember => "date.month.11",
    MonthDecember => "date.month.12",
    WeekdayMonday => "date.weekday.mon",
    WeekdayTuesday => "date.weekday.tue",
    WeekdayWednesday => "date.weekday.wed",
    WeekdayThursday => "date.weekday.thu",
    WeekdayFriday => "date.weekday.fri",
    WeekdaySaturday => "date.weekday.sat",
    WeekdaySunday => "date.weekday.sun",
    MonthDayWeekday => "date.month_day_weekday" [Month, Day, Weekday],
    CategoryDevelopment => "category.builtin.development",
    CategoryLearning => "category.builtin.learning",
    CategoryCommunication => "category.builtin.communication",
    CategoryEntertainment => "category.builtin.entertainment",
    CategorySystem => "category.builtin.system",
    CategoryUncategorized => "category.builtin.uncategorized",
}

/// A named placeholder, written `%{name}` in the catalogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Arg {
    Category,
    Command,
    Condition,
    Count,
    Day,
    Duration,
    Field,
    Hours,
    Minutes,
    Month,
    Name,
    Operator,
    Priority,
    Range,
    Rule,
    State,
    Value,
    Weekday,
}

impl Arg {
    /// The placeholder name inside `%{…}`.
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Category => "category",
            Self::Command => "command",
            Self::Condition => "condition",
            Self::Count => "count",
            Self::Day => "day",
            Self::Duration => "duration",
            Self::Field => "field",
            Self::Hours => "hours",
            Self::Minutes => "minutes",
            Self::Month => "month",
            Self::Name => "name",
            Self::Operator => "operator",
            Self::Priority => "priority",
            Self::Range => "range",
            Self::Rule => "rule",
            Self::State => "state",
            Self::Value => "value",
            Self::Weekday => "weekday",
        }
    }
}

impl Msg {
    /// Month names in calendar order.
    const MONTHS: [Self; 12] = [
        Self::MonthJanuary,
        Self::MonthFebruary,
        Self::MonthMarch,
        Self::MonthApril,
        Self::MonthMay,
        Self::MonthJune,
        Self::MonthJuly,
        Self::MonthAugust,
        Self::MonthSeptember,
        Self::MonthOctober,
        Self::MonthNovember,
        Self::MonthDecember,
    ];

    /// The name of `month`, numbered 1 to 12.
    ///
    /// # Panics
    /// Panics for numbers outside 1 to 12; month numbers come from validated dates.
    #[must_use]
    pub(crate) fn month(month: i8) -> Self {
        usize::try_from(month - 1)
            .ok()
            .and_then(|index| Self::MONTHS.get(index))
            .copied()
            .unwrap_or_else(|| panic!("month {month} is outside 1 to 12"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use quartz_i18n::Language;

    use super::*;

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split("%{")
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
            .collect()
    }

    #[test]
    fn every_message_exists_in_english_and_chinese_with_its_placeholders() {
        for language in [Language::English, Language::SimplifiedChinese] {
            for msg in Msg::ALL {
                assert!(
                    language.has_message(msg.key()),
                    "{language}: {} is missing",
                    msg.key()
                );
                let template = language.translate(msg.key());
                let expected: BTreeSet<&str> = msg.args().iter().map(|arg| arg.name()).collect();
                assert_eq!(
                    placeholders(&template),
                    expected,
                    "{language}: placeholders of {}",
                    msg.key()
                );
            }
        }
    }

    #[test]
    fn message_keys_are_unique() {
        let keys: BTreeSet<&str> = Msg::ALL.iter().map(|msg| msg.key()).collect();
        assert_eq!(keys.len(), Msg::ALL.len());
    }

    #[test]
    fn months_are_numbered_from_one() {
        assert_eq!(Msg::month(1), Msg::MonthJanuary);
        assert_eq!(Msg::month(12), Msg::MonthDecember);
    }
}
