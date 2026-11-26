//! Typed ids for every translation the UI shows, with the placeholders each one takes. A test
//! checks every id against the `en` and `zh-CN` catalogs, so a typo is a test failure instead of
//! a key shown on screen.

use quartz_i18n::MessageKey;

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
            pub(crate) const fn key(self) -> MessageKey<'static> {
                MessageKey::new(match self {
                    $(Self::$variant => $key,)+
                })
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
    AnnotationPresetRest => "annotation.preset.rest",
    ClockTwelve => "clock.twelve" [Hours, Minutes, Name],
    ClockMorning => "clock.morning",
    ClockAfternoon => "clock.afternoon",
    LocaleUnavailable => "today.locale_unavailable",
    StopTitle => "ui.link.stopped",
    CalendarMonth => "today.calendar.month" [Month, Value],
    TooltipMixed => "tooltip.mixed",
    DayUpdating => "today.updating",
    DayFailedTitle => "today.query_failed.title",
    DayFailedMessage => "today.query_failed.message",
    DayBusy => "today.query_failed.busy",
    RetryDay => "today.retry",
    GoToday => "today.go_today",
    EmptyTodayTitle => "today.empty_today.title",
    EmptyTodayMessage => "today.empty_today.message",
    EmptyPastTitle => "today.empty_past.title",
    EmptyPastMessage => "today.empty_past.message",
    Resume => "today.resume",
    ResumeFailed => "today.resume_failed",
    AwayFailed => "today.pending.failed" [Name],
    StopStarting => "today.stop.starting",
    StopPersistence => "today.stop.persistence",
    StopSession => "today.stop.session",
    StopGap => "today.stop.gap",
    StopLocked => "today.stop.locked",
    StopDisconnected => "today.stop.disconnected",
    StopSuspended => "today.stop.suspended",
    StopPaused => "today.stop.paused",
    StopForeground => "today.stop.foreground",
    StopEnded => "today.stop.ended",
    FailureDetails => "ui.failure.details",
    FailureRecoverable => "ui.failure.recoverable",
    FailureUnrecoverable => "ui.failure.unrecoverable",
    FailureStarting => "ui.failure.starting",
    FailureCopyCommand => "ui.failure.copy_command",
    FailureRestoreCommand => "ui.failure.restore_command",
    FailureCoreVersion => "ui.failure.core_version",
    FailureUiVersion => "ui.failure.ui_version",
    FailureProtocol => "ui.failure.protocol",

    FailureRetry => "ui.failure.retry",
    FailureCopyDetails => "ui.failure.copy_details",
    FailureExit => "ui.failure.exit",
    FailureCode => "ui.failure.code",
    FailureOfflineMessage => "ui.failure.offline_message",
    FailureBusyMessage => "ui.failure.busy_message",
    FailureTimeoutMessage => "ui.failure.timeout_message",
    FailureIoMessage => "ui.failure.io_message",
    FailureMismatchMessage => "ui.failure.mismatch_message",
    FailureIdentityTitle => "ui.failure.identity_title",
    FailureIdentityMessage => "ui.failure.identity_message",
    FailureRejectedMessage => "ui.failure.rejected_message",
    FailureInvalidTitle => "ui.failure.invalid_title",
    FailureInvalidMessage => "ui.failure.invalid_message",
    FailureInterfaceTitle => "ui.failure.interface_title",
    FailureInterfaceMessage => "ui.failure.interface_message",
    LinkConnecting => "ui.link.connecting",
    LinkOffline => "ui.link.offline",
    LinkMismatch => "ui.link.mismatch",
    LinkRejected => "ui.link.rejected",
    LinkFailed => "ui.link.failed",
    LinkStartCore => "ui.link.start_core",
    LinkRecording => "ui.link.recording",
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
    DetailsDurationMillis => "details.duration.milliseconds" [Value],
    DetailsDurationSeconds => "details.duration.seconds" [Value],
    DetailsDurationMinutesSeconds => "details.duration.minutes_seconds" [Minutes, Value],
    DetailsDurationHoursMinutesSeconds => "details.duration.hours_minutes_seconds" [Hours, Minutes, Value],
    DetailsClockTwelve => "details.clock.twelve" [Hours, Minutes, Value, Name],
    DetailsIntervals => "details.intervals",
    DetailsFailed => "details.failed",
    DetailsFailedMessage => "details.failed_message",
    DetailsEmpty => "details.empty",
    DetailsNoTitles => "details.no_titles",
    DetailsRules => "details.rules",
    DetailsUnclassified => "details.unclassified",
    DetailsDelete => "details.action.delete",
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
    RulesTreeTitle => "rules.tree.title",
    RulesTreeEmpty => "rules.tree.empty",
    RulesTreeTruncated => "rules.tree.truncated" [Count],
    RulesKindClassification => "rules.kind.classification",
    RulesKindExtraction => "rules.kind.extraction",
    RulesKindPrivacy => "rules.kind.privacy",
    RulesSourceBuiltin => "rules.source.builtin",
    RulesSourceUser => "rules.source.user",
    RulesBadgeDisabled => "rules.badge.disabled",
    RulesBadgeOverridden => "rules.badge.overridden",
    RulesCountsAdded => "rules.counts.added" [Count],
    RulesCountsOverridden => "rules.counts.overridden" [Count],
    RulesCountsDisabled => "rules.counts.disabled" [Count],
    RulesWarningTargetDeleted => "rules.warning.target_deleted",
    RulesWarningConflict => "rules.warning.conflict",
    RulesVersion => "rules.version" [Value],
    RulesDetailSelectPrompt => "rules.detail.select_prompt",
    RulesDetailLoading => "rules.detail.loading",
    RulesDetailEmpty => "rules.detail.empty",
    RulesDetailTruncated => "rules.detail.truncated" [Count],
    RulesDetailParent => "rules.detail.parent" [Name],
    RulesDetailParentVersion => "rules.detail.parent_version" [Name, Value],
    RulesDetailEntriesHeading => "rules.detail.entries_heading" [Count],
    RulesDetailBuiltinReadonly => "rules.detail.builtin_readonly",
    RulesDetailDisabledByParent => "rules.detail.disabled_by_parent",
    RulesActionEnable => "rules.action.enable",
    RulesActionDisable => "rules.action.disable",
    RulesActionDelete => "rules.action.delete",
    RulesActionDeleteConfirmPrompt => "rules.action.delete_confirm_prompt" [Name],
    RulesActionDeleteConfirm => "rules.action.delete_confirm",
    RulesActionCancel => "rules.action.cancel",
    RulesActionDeleteHasChildren => "rules.action.delete_has_children",
    RulesActionReload => "rules.action.reload",
    RulesLoadFailedTitle => "rules.load_failed.title",
    RulesLoadFailedMessage => "rules.load_failed.message",
    RulesDetailFailedTitle => "rules.detail_failed.title",
    RulesDetailFailedMessage => "rules.detail_failed.message",
    RulesStaleTitle => "rules.stale.title",
    RulesStaleMessage => "rules.stale.message",
    RulesMutationFailedReadonly => "rules.mutation_failed.readonly",
    RulesMutationFailedHasChildren => "rules.mutation_failed.has_children",
    RulesMutationFailedNotFound => "rules.mutation_failed.not_found",
    RulesMutationFailedGeneric => "rules.mutation_failed.generic",
    RulesEntryKindRule => "rules.entry.kind.rule",
    RulesEntryKindOverride => "rules.entry.kind.override",
    RulesEntryKindDisable => "rules.entry.kind.disable",
    RulesEntryStateEffective => "rules.entry.state.effective",
    RulesEntryStateDisabled => "rules.entry.state.disabled",
    RulesEntryStateOverridden => "rules.entry.state.overridden",
    RulesEntryStateTargetMissing => "rules.entry.state.target_missing",
    RulesEntryUnnamed => "rules.entry.unnamed",
    RulesEntryPriority => "rules.entry.priority" [Priority],
    RulesEntryProvenance => "rules.entry.provenance" [Value],
    RulesEntryDisables => "rules.entry.disables" [Value],
    RulesEntryMatchAll => "rules.entry.match_all" [Condition],
    RulesEntryMatchAny => "rules.entry.match_any" [Condition],
    RulesEntryExtractionPattern => "rules.entry.extraction_pattern" [Value],
    RulesEntryPrivacyEffect => "rules.entry.privacy_effect" [Value],
    RulesEntryNotCondition => "rules.entry.not_condition" [Condition],
    RulesEntryNoConditions => "rules.entry.no_conditions",
    RulesEntryTarget => "rules.entry.target" [Value],
}

impl From<quartz_protocol::AnnotationLabelPreset> for Msg {
    fn from(preset: quartz_protocol::AnnotationLabelPreset) -> Self {
        match preset {
            quartz_protocol::AnnotationLabelPreset::Rest => Self::AnnotationPresetRest,
        }
    }
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

/// One placeholder argument paired with its replacement text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArgValue<'a> {
    pub(crate) arg: Arg,
    pub(crate) value: &'a str,
}

impl Arg {
    /// Pairs this placeholder with its replacement text.
    #[must_use]
    pub(crate) const fn with(self, value: &str) -> ArgValue<'_> {
        ArgValue { arg: self, value }
    }

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

    /// The name of `month`, numbered 1 to 12; `None` outside that range.
    #[must_use]
    pub(crate) fn month(month: i8) -> Option<Self> {
        let index = usize::try_from(month.checked_sub(1)?).ok()?;
        Self::MONTHS.get(index).copied()
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
    fn annotation_label_preset_uses_the_current_language() {
        let msg = Msg::from(quartz_protocol::AnnotationLabelPreset::Rest);
        assert_eq!(msg.key(), MessageKey::new("annotation.preset.rest"));
        let zh = crate::format::Formatter::new(Language::SimplifiedChinese);
        let en = crate::format::Formatter::new(Language::English);
        assert_eq!(zh.text(msg), "\u{4f11}\u{606f}");
        assert_eq!(en.text(msg), "Rest");
    }

    #[test]
    fn every_message_exists_in_english_and_chinese_with_its_placeholders() {
        for &language in Language::ALL {
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
        let keys: BTreeSet<MessageKey<'static>> = Msg::ALL.iter().map(|msg| msg.key()).collect();
        assert_eq!(keys.len(), Msg::ALL.len());
    }

    #[test]
    fn months_are_numbered_from_one() {
        assert_eq!(Msg::month(1), Some(Msg::MonthJanuary));
        assert_eq!(Msg::month(12), Some(Msg::MonthDecember));
        assert_eq!(Msg::month(0), None);
        assert_eq!(Msg::month(13), None);
        assert_eq!(Msg::month(i8::MIN), None);
    }
}
