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
    RankingHiddenDocument => "today.ranking.hidden_document",
    RankingTruncated => "today.ranking.truncated" [Count],
    RankingDocumentsEmptyTitle => "today.ranking.documents_empty.title",
    RankingDocumentsEmptyMessage => "today.ranking.documents_empty.message",
    RankingDocumentsEmptyAction => "today.ranking.documents_empty.action",
    PendingTitle => "today.pending.title",
    PendingRange => "today.pending.range" [Range, Duration],
    PendingRest => "today.pending.rest",
    PendingMeeting => "today.pending.meeting",
    PendingReading => "today.pending.reading",
    PendingDismiss => "today.pending.dismiss",
    PendingOther => "today.pending.other",
    TimelineTitle => "timeline.title",
    TimelineMainTrack => "timeline.main_track",
    TimelineModePan => "timeline.mode.pan",
    TimelineModeSelect => "timeline.mode.select",
    TimelineClassifyMenuTitle => "timeline.classify.title",
    TimelineClassifyMenuOther => "timeline.classify.other",
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
    RulesEntryCategoryMissing => "rules.entry.category_missing",
    RulesTabRuleSets => "rules.tab.rule_sets",
    RulesTabCategories => "rules.tab.categories",
    RulesCategoriesHeading => "rules.categories.heading",
    RulesCategoriesEmpty => "rules.categories.empty",
    RulesCategoriesTruncated => "rules.categories.truncated" [Count],
    RulesCategoriesColName => "rules.categories.col.name",
    RulesCategoriesColTendency => "rules.categories.col.tendency",
    RulesCategoriesColActions => "rules.categories.col.actions",
    RulesCategoriesTendencyFocus => "rules.categories.tendency.focus",
    RulesCategoriesTendencyNeutral => "rules.categories.tendency.neutral",
    RulesCategoriesTendencyDistraction => "rules.categories.tendency.distraction",
    RulesCategoriesBadgeRenamed => "rules.categories.badge.renamed",
    RulesCategoriesBadgeArchived => "rules.categories.badge.archived",
    RulesCategoriesActionNew => "rules.categories.action.new",
    RulesCategoriesActionEdit => "rules.categories.action.edit",
    RulesCategoriesActionArchive => "rules.categories.action.archive",
    RulesCategoriesActionRestore => "rules.categories.action.restore",
    RulesCategoriesActionMoveUp => "rules.categories.action.move_up",
    RulesCategoriesActionMoveDown => "rules.categories.action.move_down",
    RulesCategoriesActionShowArchived => "rules.categories.action.show_archived" [Count],
    RulesCategoriesActionHideArchived => "rules.categories.action.hide_archived" [Count],
    RulesCategoriesArchivedInfo => "rules.categories.archived.info",
    RulesCategoriesArchivedEmpty => "rules.categories.archived.empty",
    RulesCategoriesArchiveConfirmPrompt => "rules.categories.archive_confirm.prompt" [Name],
    RulesCategoriesArchiveConfirm => "rules.categories.archive_confirm.confirm",
    RulesCategoriesEditorCreateTitle => "rules.categories.editor.create_title",
    RulesCategoriesEditorEditTitle => "rules.categories.editor.edit_title",
    RulesCategoriesEditorNameLabel => "rules.categories.editor.name_label",
    RulesCategoriesEditorNamePlaceholder => "rules.categories.editor.name_placeholder",
    RulesCategoriesEditorResetDefault => "rules.categories.editor.reset_default",
    RulesCategoriesEditorBuiltinHint => "rules.categories.editor.builtin_hint",
    RulesCategoriesEditorColorLabel => "rules.categories.editor.color_label",
    RulesCategoriesEditorTendencyLabel => "rules.categories.editor.tendency_label",
    RulesCategoriesEditorCreateSubmit => "rules.categories.editor.create_submit",
    RulesCategoriesEditorSaveSubmit => "rules.categories.editor.save_submit",
    RulesCategoriesValidationBlank => "rules.categories.validation.blank",
    RulesCategoriesValidationTooLong => "rules.categories.validation.too_long",
    RulesCategoriesValidationReserved => "rules.categories.validation.reserved",
    RulesCategoriesLoadFailedTitle => "rules.categories.load_failed.title",
    RulesCategoriesLoadFailedMessage => "rules.categories.load_failed.message",
    RulesCategoriesStaleTitle => "rules.categories.stale.title",
    RulesCategoriesStaleMessage => "rules.categories.stale.message",
    RulesCategoriesMutationFailedInvalid => "rules.categories.mutation_failed.invalid",
    RulesCategoriesMutationFailedReadonly => "rules.categories.mutation_failed.readonly",
    RulesCategoriesMutationFailedBusy => "rules.categories.mutation_failed.busy",
    RulesCategoriesMutationFailedGeneric => "rules.categories.mutation_failed.generic",
    RulesActionEdit => "rules.action.edit",
    RulesActionNewSet => "rules.action.new_set",
    RulesActionOverrideInChild => "rules.action.override_in_child",
    RulesEditorTitleEdit => "rules.editor.title.edit" [Name],
    RulesEditorTitleNew => "rules.editor.title.new",
    RulesEditorBadgeDirty => "rules.editor.badge.dirty",
    RulesEditorLoading => "rules.editor.loading",
    RulesEditorLoadFailed => "rules.editor.load_failed",
    RulesEditorKeyLabel => "rules.editor.key.label",
    RulesEditorKeyPlaceholder => "rules.editor.key.placeholder",
    RulesEditorKindLabel => "rules.editor.kind.label",
    RulesEditorParentLabel => "rules.editor.parent.label",
    RulesEditorParentNone => "rules.editor.parent.none",
    RulesEditorTextLabel => "rules.editor.text.label",
    RulesEditorTextPlaceholder => "rules.editor.text.placeholder",
    RulesEditorKeyErrorEmpty => "rules.editor.key_error.empty",
    RulesEditorKeyErrorPrefix => "rules.editor.key_error.prefix",
    RulesEditorKeyErrorMissingSlug => "rules.editor.key_error.missing_slug",
    RulesEditorKeyErrorInvalidChars => "rules.editor.key_error.invalid_chars",
    RulesEditorKeyErrorExists => "rules.editor.key_error.exists",
    RulesEditorValidationPending => "rules.editor.validation.pending",
    RulesEditorValidationChecking => "rules.editor.validation.checking",
    RulesEditorValidationValid => "rules.editor.validation.valid",
    RulesEditorValidationInvalidCount => "rules.editor.validation.invalid_count" [Count],
    RulesEditorValidationFailed => "rules.editor.validation.failed",
    RulesEditorDiagnosticsTruncated => "rules.editor.diagnostics.truncated" [Count],
    RulesEditorDiagnosticLocation => "rules.editor.diagnostics.location" [Line, Column],
    RulesDiagFormat => "rules.diag.format",
    RulesDiagKey => "rules.diag.key",
    RulesDiagSet => "rules.diag.set",
    RulesDiagParent => "rules.diag.parent",
    RulesDiagTarget => "rules.diag.target",
    RulesDiagConflict => "rules.diag.conflict",
    RulesDiagField => "rules.diag.field",
    RulesDiagOp => "rules.diag.op",
    RulesDiagValue => "rules.diag.value",
    RulesDiagRegex => "rules.diag.regex",
    RulesDiagRef => "rules.diag.ref",
    RulesDiagDup => "rules.diag.dup",
    RulesDiagUnknown => "rules.diag.unknown" [Code, Value],
    RulesEditorActionPreview => "rules.editor.action.preview",
    RulesEditorActionSave => "rules.editor.action.save",
    RulesEditorActionSaving => "rules.editor.action.saving",
    RulesEditorActionClose => "rules.editor.action.close",
    RulesEditorStaleTitle => "rules.editor.stale.title",
    RulesEditorStaleMessage => "rules.editor.stale.message" [Value],
    RulesEditorUnsavedPrompt => "rules.editor.unsaved.prompt",
    RulesEditorUnsavedDiscard => "rules.editor.unsaved.discard",
    RulesEditorUnsavedStay => "rules.editor.unsaved.stay",
    RulesEditorSaveFailedIdentityKind => "rules.editor.save_failed.identity_kind",
    RulesEditorSaveFailedIdentityParent => "rules.editor.save_failed.identity_parent",
    RulesEditorSaveFailedGeneric => "rules.editor.save_failed.generic",
    RulesPreviewHeading => "rules.preview.heading" [Range],
    RulesPreviewLoading => "rules.preview.loading",
    RulesPreviewFailed => "rules.preview.failed",
    RulesPreviewSummaryBefore => "rules.preview.summary.before" [Duration],
    RulesPreviewSummaryAfter => "rules.preview.summary.after" [Duration],
    RulesPreviewSummaryDocumentChanges => "rules.preview.summary.document_changes" [Count],
    RulesPreviewSummaryFurtherHidden => "rules.preview.summary.further_hidden" [Count],
    RulesPreviewCategoriesHeading => "rules.preview.categories.heading",
    RulesPreviewCategoriesBeforeAfter => "rules.preview.categories.before_after" [Name, Value],
    RulesPreviewRulesHeading => "rules.preview.rules.heading" [Count],
    RulesPreviewRulesEmpty => "rules.preview.rules.empty",
    RulesPreviewRuleStats => "rules.preview.rule_stats" [Count, Duration, Value],
    RulesPreviewSamplesHeading => "rules.preview.samples.heading" [Count],
    RulesPreviewSamplesEmpty => "rules.preview.samples.empty",
    RulesPreviewSamplesTruncated => "rules.preview.samples.truncated" [Count],
    RulesPreviewSampleHiddenOrEmpty => "rules.preview.sample.hidden_or_empty",
    RulesPreviewSampleChange => "rules.preview.sample.change" [Value],
    RulesPreviewHintValidateFirst => "rules.preview.hint.validate_first",
    RulesActionPasteImport => "rules.action.paste_import",
    RulesImportTitle => "rules.import.title",
    RulesImportTextLabel => "rules.import.text.label",
    RulesImportTextPlaceholder => "rules.import.text.placeholder",
    RulesImportActionImport => "rules.import.action.import",
    RulesImportActionImporting => "rules.import.action.importing",
    RulesImportPlanCreate => "rules.import.plan.create" [Name],
    RulesImportPlanReplace => "rules.import.plan.replace" [Name, Value],
    RulesImportKeyErrorMissing => "rules.import.key_error.missing",
    RulesImportKeyErrorInvalid => "rules.import.key_error.invalid",
    RulesImportKeyErrorBuiltin => "rules.import.key_error.builtin",
    RulesImportStaleMessage => "rules.import.stale.message" [Value],
    RulesImportFailedGeneric => "rules.import.failed.generic",
    RulesTabUnclassified => "rules.tab.unclassified",
    RulesUnclassifiedHeading => "rules.unclassified.heading",
    RulesUnclassifiedRange => "rules.unclassified.range" [Range],
    RulesUnclassifiedGroupApp => "rules.unclassified.group.app",
    RulesUnclassifiedGroupDocument => "rules.unclassified.group.document",
    RulesUnclassifiedSearchPlaceholder => "rules.unclassified.search.placeholder",
    RulesUnclassifiedSearchTooLong => "rules.unclassified.search.too_long",
    RulesUnclassifiedEmpty => "rules.unclassified.empty",
    RulesUnclassifiedSearchEmpty => "rules.unclassified.search_empty",
    RulesUnclassifiedTruncated => "rules.unclassified.truncated" [Count],
    RulesUnclassifiedHiddenApp => "rules.unclassified.hidden_app",
    RulesUnclassifiedHiddenDocument => "rules.unclassified.hidden_document",
    RulesUnclassifiedActionCreateRule => "rules.unclassified.action.create_rule",
    RulesUnclassifiedLoadFailedTitle => "rules.unclassified.load_failed.title",
    RulesUnclassifiedLoadFailedMessage => "rules.unclassified.load_failed.message",
    TodayAnnotationsButton => "today.annotations.button",
    TodayAnnotationsTabDetails => "today.annotations.tab.details",
    TodayAnnotationsTabAnnotations => "today.annotations.tab.annotations",
    TodayAnnotationsTitle => "today.annotations.title",
    TodayAnnotationsActionNew => "today.annotations.action.new",
    TodayAnnotationsEmpty => "today.annotations.empty",
    TodayAnnotationsTruncated => "today.annotations.truncated" [Count],
    TodayAnnotationsLoadFailedTitle => "today.annotations.load_failed.title",
    TodayAnnotationsLoadFailedMessage => "today.annotations.load_failed.message",
    TodayAnnotationsStaleMessage => "today.annotations.stale.message",
    TodayAnnotationsCategoryMissing => "today.annotations.category_missing",
    TodayAnnotationsKindCategoryOverride => "today.annotations.kind.category_override",
    TodayAnnotationsKindOfflineActivity => "today.annotations.kind.offline_activity",
    TodayAnnotationsKindActiveOverride => "today.annotations.kind.active_override",
    TodayAnnotationsKindDismissedAway => "today.annotations.kind.dismissed_away",
    TodayAnnotationsKindNote => "today.annotations.kind.note",
    TodayAnnotationsEditorCreateTitle => "today.annotations.editor.create_title",
    TodayAnnotationsEditorEditTitle => "today.annotations.editor.edit_title",
    TodayAnnotationsEditorKindLabel => "today.annotations.editor.kind_label",
    TodayAnnotationsEditorKindFixed => "today.annotations.editor.kind_fixed",
    TodayAnnotationsEditorTimeRange => "today.annotations.editor.time_range",
    TodayAnnotationsEditorStartLabel => "today.annotations.editor.start_label",
    TodayAnnotationsEditorEndLabel => "today.annotations.editor.end_label",
    TodayAnnotationsEditorTimePlaceholder => "today.annotations.editor.time_placeholder",
    TodayAnnotationsEditorCategoryLabel => "today.annotations.editor.category_label",
    TodayAnnotationsEditorCategoryNone => "today.annotations.editor.category_none",
    TodayAnnotationsEditorCategoryLoading => "today.annotations.editor.category_loading",
    TodayAnnotationsEditorCategoryArchivedNote => "today.annotations.editor.category_archived_note" [Count],
    TodayAnnotationsEditorLabelField => "today.annotations.editor.label_field",
    TodayAnnotationsEditorLabelPlaceholder => "today.annotations.editor.label_placeholder",
    TodayAnnotationsEditorNoteField => "today.annotations.editor.note_field",
    TodayAnnotationsEditorNotePlaceholder => "today.annotations.editor.note_placeholder",
    TodayAnnotationsEditorScalarCount => "today.annotations.editor.scalar_count" [Count, Value],
    TodayAnnotationsEditorActionSave => "today.annotations.editor.action.save",
    TodayAnnotationsEditorActionSaving => "today.annotations.editor.action.saving",
    TodayAnnotationsEditorActionRetrySave => "today.annotations.editor.action.retry_save",
    TodayAnnotationsEditorActionCancel => "today.annotations.editor.action.cancel",
    TodayAnnotationsEditorActionDelete => "today.annotations.editor.action.delete",
    TodayAnnotationsEditorDeleteConfirmPrompt => "today.annotations.editor.delete_confirm.prompt",
    TodayAnnotationsEditorDeleteConfirmConfirm => "today.annotations.editor.delete_confirm.confirm",
    TodayAnnotationsValidationInvalidTime => "today.annotations.validation.invalid_time",
    TodayAnnotationsValidationDstGap => "today.annotations.validation.dst_gap",
    TodayAnnotationsValidationOutsideDay => "today.annotations.validation.outside_day",
    TodayAnnotationsValidationEndBeforeStart => "today.annotations.validation.end_before_start",
    TodayAnnotationsValidationLabelTooLong => "today.annotations.validation.label_too_long" [Count],
    TodayAnnotationsValidationNoteTooLong => "today.annotations.validation.note_too_long" [Count],
    TodayAnnotationsValidationCategoryRequired => "today.annotations.validation.category_required",
    TodayAnnotationsValidationNoteContentRequired => "today.annotations.validation.note_content_required",
    TodayAnnotationsValidationNoDay => "today.annotations.validation.no_day",
    TodayAnnotationsErrorBadRequest => "today.annotations.error.bad_request",
    TodayAnnotationsErrorInvalidRange => "today.annotations.error.invalid_range",
    TodayAnnotationsErrorStale => "today.annotations.error.stale",
    TodayAnnotationsErrorBusy => "today.annotations.error.busy",
    TodayAnnotationsErrorGeneric => "today.annotations.error.generic",
    TodayAnnotationsErrorCategoryUnavailable => "today.annotations.error.category_unavailable",
    QueryProgressDays => "query.progress.days" [Done, Total],
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
    Code,
    Column,
    Command,
    Condition,
    Count,
    Day,
    Done,
    Duration,
    Field,
    Hours,
    Line,
    Minutes,
    Month,
    Name,
    Operator,
    Priority,
    Range,
    Rule,
    State,
    Total,
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
            Self::Code => "code",
            Self::Column => "column",
            Self::Command => "command",
            Self::Condition => "condition",
            Self::Count => "count",
            Self::Day => "day",
            Self::Done => "done",
            Self::Duration => "duration",
            Self::Field => "field",
            Self::Hours => "hours",
            Self::Line => "line",
            Self::Minutes => "minutes",
            Self::Month => "month",
            Self::Name => "name",
            Self::Operator => "operator",
            Self::Priority => "priority",
            Self::Range => "range",
            Self::Rule => "rule",
            Self::State => "state",
            Self::Total => "total",
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
