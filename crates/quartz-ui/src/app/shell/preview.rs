//! Fixed presentation inputs for the preview; no pipe or background queries.

use std::collections::BTreeMap;

use gpui_kit::{Context, Window, point, px};
use quartz_protocol::{self as wire, LocaleSettings, StateSnapshot, StopReason};

use super::{
    AppShell, InputSyncState, OpenDetails, PanelContent,
    calendar::CalendarMonth,
    live::LoadPhase,
    live_details::{DetailPhase, LivePanel, PanelHeading},
    timeline::{SegmentContextMenu, TimelineDragMode},
};
use crate::{
    app::preview::{PreviewState, TodayPreviewState},
    format::Msg,
    link::{DayData, DetailData, EntityData, EntityKey, LoadDataError, UtcRange},
    model::{
        AnnotationEditorEffect, Day, DetailsSubject, QueryProgress, RuleEditorEffect,
        RuleImportEffect, RulesEffect, RulesTab, SegmentId, TimeOfDay, TimeSpan,
        UnclassifiedEffect, WeekEffect, live_details::LiveDetails, weekday_from_protocol,
    },
    timeline_scale::RangeSelectGesture,
};

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
/// Vertical offset from the top of the timeline plot to the first application swimlane.
const CLASSIFY_MENU_PLOT_OFFSET_Y: f32 = 32.;
/// Initial fallback position for the classify-as menu before plot bounds are measured.
const CLASSIFY_MENU_FALLBACK_X: f32 = 560.;
const CLASSIFY_MENU_FALLBACK_Y: f32 = 280.;

/// Synthetic segment id of the 13:15–14:00 unclassified browser segment in [`sample_live_day`].
const CLASSIFY_SEGMENT_ID: SegmentId = SegmentId::new(5);

const PREVIEW_RULES_VERSION: &str = "preview-rules-v1";
const PREVIEW_USER_SET_KEY: &str = "user.study-sites";
const PREVIEW_USER_SET_VERSION: u32 = 3;
const PREVIEW_PROGRESS_DONE: u32 = 120;
const PREVIEW_PROGRESS_TOTAL: u32 = 365;
const PREVIEW_WEEK_PROGRESS_DONE: u32 = 4;
const PREVIEW_WEEK_PROGRESS_TOTAL: u32 = 6;
/// Offset from week start to today in the Week preview so the 7th day is one empty future day.
const PREVIEW_WEEK_TODAY_OFFSET: i64 = 5;

const CAT_DEV_UUID: &str = "11111111-1111-4111-8111-111111111111";
const CAT_LEARN_UUID: &str = "22222222-2222-4222-8222-222222222222";
const CAT_COMM_UUID: &str = "33333333-3333-4333-8333-333333333333";
const CAT_ENTERTAIN_UUID: &str = "44444444-4444-4444-8444-444444444444";
const CAT_SYSTEM_UUID: &str = "55555555-5555-4555-8555-555555555555";
const CAT_RESEARCH_UUID: &str = "66666666-6666-4666-8666-666666666666";
const CAT_ARCHIVED_UUID: &str = "77777777-7777-4777-8777-777777777777";
const CAT_NEW_UUID: &str = "88888888-8888-4888-8888-888888888888";
const CAT_MISSING_UUID: &str = "99999999-9999-4999-8999-999999999999";

const ANNO_1_UUID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const ANNO_2_UUID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const ANNO_3_UUID: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

/// Plot-relative overlay that preview recomputes once GPUI lays out the timeline plot bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewTimelineOverlay {
    DragSelect(TimeSpan),
    ClassifyMenu { span: TimeSpan, range: UtcRange },
}

/// Which Settings section a preview screenshot anchors at the top of the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum SettingsPreviewSection {
    #[default]
    All,
    Data,
    Browser,
    About,
}

#[derive(Debug)]
pub(super) struct PreviewDay {
    pub snapshot: StateSnapshot,
    pub phase: LoadPhase,
    pub failure: Option<LoadDataError>,
    pub timeline_overlay: Option<PreviewTimelineOverlay>,
    pub settings_section: SettingsPreviewSection,
}

impl AppShell {
    pub(crate) fn configure_preview(
        &mut self,
        state: PreviewState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scenario = match state {
            PreviewState::TodayData(scenario) => Some(scenario),
            _ => None,
        };
        let first_day_of_week =
            if self.formatter.language() == quartz_i18n::Language::SimplifiedChinese {
                wire::Weekday::Monday
            } else {
                wire::Weekday::Sunday
            };
        let first_weekday = weekday_from_protocol(first_day_of_week);
        let selected = self.day.date();
        let past = matches!(
            scenario,
            Some(TodayPreviewState::EmptyPast | TodayPreviewState::DatePicker)
        );
        let today = if matches!(state, PreviewState::Week | PreviewState::WeekLoading) {
            let week_start = crate::model::week::week_start_for(selected, first_weekday)
                .expect("preview week start");
            week_start
                .checked_add(jiff::ToSpan::days(PREVIEW_WEEK_TODAY_OFFSET))
                .expect("preview week today")
        } else if past {
            selected.tomorrow().expect("sample date has a next day")
        } else {
            selected
        };
        let stop_reason = match scenario {
            Some(TodayPreviewState::Paused) => Some(StopReason::Paused),
            Some(TodayPreviewState::PersistenceFailed) => Some(StopReason::PersistenceFailed),
            _ => None,
        };
        if matches!(
            scenario,
            Some(
                TodayPreviewState::Loading
                    | TodayPreviewState::EmptyToday
                    | TodayPreviewState::EmptyPast
                    | TodayPreviewState::QueryFailed
            )
        ) {
            self.day = Day::empty(selected);
        }
        if scenario == Some(TodayPreviewState::DatePicker) {
            self.calendar = Some(CalendarMonth::new(selected));
        }

        let mut timeline_overlay = None;
        let mut settings_section = SettingsPreviewSection::All;
        match state {
            PreviewState::Rules => {
                self.configure_rules_preview();
            }
            PreviewState::RulesCategories => {
                self.configure_rules_categories_preview(window, cx);
            }
            PreviewState::RuleEditor => {
                self.configure_rule_editor_preview(false, selected);
            }
            PreviewState::RuleEditorPreview => {
                self.configure_rule_editor_preview(true, selected);
            }
            PreviewState::RuleImport => {
                self.configure_rule_import_preview();
            }
            PreviewState::Unclassified => {
                self.configure_unclassified_preview(selected);
            }
            PreviewState::Progress => {
                self.configure_progress_preview(selected);
            }
            PreviewState::TodayWebsites => {
                self.day = sample_live_day_with_websites(fixture_preview_websites());
            }
            PreviewState::TodayAnnotations => {
                self.configure_today_annotations_preview(false);
            }
            PreviewState::AnnotationEditor => {
                self.configure_today_annotations_preview(true);
            }
            PreviewState::TimelineSelect => {
                self.day = sample_live_day();
                self.drag_mode = TimelineDragMode::Select;
                timeline_overlay = Some(PreviewTimelineOverlay::DragSelect(TimeSpan::new(
                    TimeOfDay::hm(10, 15),
                    TimeOfDay::hm(11, 0),
                )));
            }
            PreviewState::ClassifyMenu => {
                timeline_overlay = Some(self.configure_classify_menu_preview());
            }
            PreviewState::Week => {
                self.configure_week_preview(today, first_weekday);
            }
            PreviewState::WeekLoading => {
                self.configure_week_loading_preview(today, first_weekday);
            }
            PreviewState::Settings => {
                self.configure_settings_preview(wire::ResidentEnabled::Enabled, first_day_of_week);
            }
            PreviewState::SettingsResidentOff => {
                self.configure_settings_preview(wire::ResidentEnabled::Disabled, first_day_of_week);
            }
            PreviewState::SettingsData => {
                self.configure_settings_preview(wire::ResidentEnabled::Enabled, first_day_of_week);
                settings_section = SettingsPreviewSection::Data;
            }
            PreviewState::SettingsDeleteConfirm => {
                self.configure_settings_preview(wire::ResidentEnabled::Enabled, first_day_of_week);
                let delete_start = jiff::civil::date(2026, 11, 10);
                let delete_end = jiff::civil::date(2026, 11, 14);
                self.settings.set_delete_start(delete_start, today);
                self.settings.set_delete_end(delete_end, today);
                self.settings.request_delete_range(today);
                settings_section = SettingsPreviewSection::Data;
            }
            PreviewState::SettingsBrowser => {
                self.configure_settings_preview(wire::ResidentEnabled::Enabled, first_day_of_week);
                settings_section = SettingsPreviewSection::Browser;
            }
            PreviewState::SettingsAbout => {
                self.configure_settings_preview(wire::ResidentEnabled::Enabled, first_day_of_week);
                settings_section = SettingsPreviewSection::About;
            }
            PreviewState::Today
            | PreviewState::TodayDetails
            | PreviewState::TodayHover
            | PreviewState::TodayFullDay
            | PreviewState::TodayData(_)
            | PreviewState::TodayAppDetails
            | PreviewState::Error(_) => {}
        }

        self.preview = Some(PreviewDay {
            snapshot: StateSnapshot {
                ui_resident: wire::ResidentEnabled::Disabled,
                logical_date: today,
                timed_policies: Default::default(),
                foreground: None,
                recording: stop_reason.is_none(),
                stop_reason,
                browser_streams: Vec::new(),
                ui_language: if self.formatter.language()
                    == quartz_i18n::Language::SimplifiedChinese
                {
                    wire::UiLanguage::SimplifiedChinese
                } else {
                    wire::UiLanguage::English
                },
                locale: Some(LocaleSettings {
                    hour_cycle: wire::HourCycle::H24,
                    first_day_of_week,
                }),
            },
            phase: if scenario == Some(TodayPreviewState::Loading) {
                LoadPhase::Loading
            } else {
                LoadPhase::Ready
            },
            failure: (scenario == Some(TodayPreviewState::QueryFailed))
                .then_some(LoadDataError::InvalidData),
            timeline_overlay,
            settings_section,
        });
    }

    /// Computes plot-aligned coordinates for timeline preview overlays once GPUI reports bounds.
    pub(super) fn sync_preview_plot_overlays(
        &mut self,
        origin_x: f32,
        origin_y: f32,
        width: f32,
        cx: &mut Context<Self>,
    ) {
        if !width.is_finite() || width <= 0. {
            return;
        }
        let Some(overlay) = self
            .preview
            .as_ref()
            .and_then(|preview| preview.timeline_overlay)
        else {
            return;
        };
        let view = self.view();
        match overlay {
            PreviewTimelineOverlay::DragSelect(span) => {
                let Some(placement) = view.place(span) else {
                    return;
                };
                let anchor_x = origin_x + placement.left * width;
                let current_x = origin_x + (placement.left + placement.width) * width;
                let mut gesture = RangeSelectGesture::new(anchor_x, view);
                let _ = gesture.move_to(current_x);
                if self.range_select != Some(gesture) {
                    self.range_select = Some(gesture);
                    cx.notify();
                }
            }
            PreviewTimelineOverlay::ClassifyMenu { span, range } => {
                let Some(placement) = view.place(span) else {
                    return;
                };
                let menu_x = origin_x + (placement.left + placement.width * 0.5) * width;
                let menu_y = origin_y + CLASSIFY_MENU_PLOT_OFFSET_Y;
                let menu = SegmentContextMenu {
                    range,
                    position: point(px(menu_x), px(menu_y)),
                };
                if self.segment_menu != Some(menu) {
                    self.segment_menu = Some(menu);
                    cx.notify();
                }
            }
        }
    }

    fn load_fixture_rules_and_categories(&mut self) {
        let RulesEffect::LoadPage {
            tree_generation,
            categories_generation,
        } = self.rules.open()
        else {
            unreachable!("opening RulesPage emits LoadPage");
        };
        self.rules
            .apply_categories_loaded(categories_generation, Ok(fixture_categories_response()));
        let _ = self
            .rules
            .apply_tree_loaded(tree_generation, Ok(fixture_rule_tree_responses()));
        let RulesEffect::LoadDetail { generation, .. } =
            self.rules.select(PREVIEW_USER_SET_KEY.to_owned())
        else {
            unreachable!("selecting user rule set emits LoadDetail");
        };
        self.rules
            .apply_detail_loaded(generation, Ok(fixture_user_set_detail_response()));
    }

    fn configure_rules_preview(&mut self) {
        self.load_fixture_rules_and_categories();
        self.rules.set_active_tab(RulesTab::RuleSets);
    }

    fn configure_rules_categories_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load_fixture_rules_and_categories();
        self.rules.set_active_tab(RulesTab::Categories);
        self.rules.toggle_archived_visibility();
        self.rules.begin_create_category(parse_uuid(CAT_NEW_UUID));
        let placeholder = self
            .formatter
            .text(Msg::RulesCategoriesEditorNamePlaceholder);
        self.sync_category_name_input("", placeholder, window, cx);
    }

    fn configure_rule_editor_preview(
        &mut self,
        with_preview_results: bool,
        today: wire::LocalDate,
    ) {
        self.load_fixture_rules_and_categories();
        self.rules.set_active_tab(RulesTab::RuleSets);
        let RuleEditorEffect::ExportText {
            generation: load_gen,
            ..
        } = self.rule_editor.open_edit(
            PREVIEW_USER_SET_KEY.to_owned(),
            wire::RuleKind::Classification,
            PREVIEW_USER_SET_VERSION,
        )
        else {
            unreachable!("open_edit emits ExportText");
        };
        let text = if with_preview_results {
            fixture_rule_editor_valid_text()
        } else {
            fixture_rule_editor_invalid_text()
        };
        let RuleEditorEffect::Validate {
            generation: val_gen,
            ..
        } = self.rule_editor.apply_export_loaded(
            load_gen,
            Ok(wire::RulesTextExportResponse {
                text,
                versions: wire::QueryVersions(PREVIEW_RULES_VERSION.to_owned()),
            }),
        )
        else {
            unreachable!("apply_export_loaded emits Validate");
        };
        if with_preview_results {
            self.rule_editor.apply_validation_finished(
                val_gen,
                Ok(wire::RulesTextValidateResponse {
                    valid: true,
                    diagnostics: Vec::new(),
                    truncated: false,
                }),
            );
            let RuleEditorEffect::Preview {
                buffer_generation,
                preview_generation,
                ..
            } = self.rule_editor.request_preview(today)
            else {
                unreachable!("validated rule editor emits Preview");
            };
            self.rule_editor.apply_preview_finished(
                buffer_generation,
                preview_generation,
                Ok(fixture_rule_preview_response(sample_day_start_utc_ms())),
            );
        } else {
            self.rule_editor.apply_validation_finished(
                val_gen,
                Ok(wire::RulesTextValidateResponse {
                    valid: false,
                    diagnostics: vec![wire::RuleDiagnostic {
                        code: "E-RULE-FIELD".to_owned(),
                        path: "rule[0].when[0].field".to_owned(),
                        location: Some(wire::RuleDiagnosticLocation {
                            line: 16,
                            column: 13,
                        }),
                        params: BTreeMap::from([(
                            "field".to_owned(),
                            "app.unknown_field".to_owned(),
                        )]),
                    }],
                    truncated: false,
                }),
            );
        }
        self.rule_editor_text_sync = InputSyncState::NeedsSync;
    }

    fn configure_rule_import_preview(&mut self) {
        self.load_fixture_rules_and_categories();
        self.rules.set_active_tab(RulesTab::RuleSets);
        self.rule_import.open();
        let RuleImportEffect::ScheduleValidate { generation } = self
            .rule_import
            .set_text(fixture_rule_editor_valid_text(), self.rules.tree())
        else {
            unreachable!("pasted rule set with valid key schedules validation");
        };
        let RuleImportEffect::Validate { generation, .. } =
            self.rule_import.trigger_validate(generation)
        else {
            unreachable!("trigger_validate emits Validate");
        };
        self.rule_import.apply_validation_finished(
            generation,
            Ok(wire::RulesTextValidateResponse {
                valid: true,
                diagnostics: Vec::new(),
                truncated: false,
            }),
        );
        self.rule_import_text_sync = InputSyncState::NeedsSync;
    }

    fn configure_unclassified_preview(&mut self, today: wire::LocalDate) {
        self.load_fixture_rules_and_categories();
        self.rules.set_active_tab(RulesTab::Unclassified);
        let UnclassifiedEffect::Query {
            range, generation, ..
        } = self.unclassified.open(today)
        else {
            unreachable!("opening UnclassifiedQueue emits Query");
        };
        self.unclassified
            .apply_loaded(generation, range, Ok(fixture_unclassified_response()));
        self.unclassified_search_sync = InputSyncState::NeedsSync;
    }

    fn configure_progress_preview(&mut self, today: wire::LocalDate) {
        self.load_fixture_rules_and_categories();
        self.rules.set_active_tab(RulesTab::Unclassified);
        let UnclassifiedEffect::Query { generation, .. } = self.unclassified.open(today) else {
            unreachable!("opening UnclassifiedQueue emits Query");
        };
        let _ = self.unclassified.apply_progress(
            generation,
            QueryProgress {
                done: PREVIEW_PROGRESS_DONE,
                total: PREVIEW_PROGRESS_TOTAL,
            },
        );
        self.unclassified_search_sync = InputSyncState::NeedsSync;
    }

    fn configure_week_preview(
        &mut self,
        today: wire::LocalDate,
        first_weekday: jiff::civil::Weekday,
    ) {
        self.load_fixture_rules_and_categories();
        let WeekEffect::Query {
            week_start,
            generation,
            ..
        } = self.week.open(today, first_weekday)
        else {
            unreachable!("opening WeekPage emits Query");
        };
        let follow_up = self.week.apply_loaded(
            generation,
            week_start,
            today,
            Ok(fixture_week_range_response(week_start)),
            self.rules.categories(),
        );
        debug_assert_eq!(follow_up, WeekEffect::None);
    }

    fn configure_week_loading_preview(
        &mut self,
        today: wire::LocalDate,
        first_weekday: jiff::civil::Weekday,
    ) {
        let WeekEffect::Query { generation, .. } = self.week.open(today, first_weekday) else {
            unreachable!("opening WeekPage emits Query");
        };
        let _ = self.week.apply_progress(
            generation,
            QueryProgress {
                done: PREVIEW_WEEK_PROGRESS_DONE,
                total: PREVIEW_WEEK_PROGRESS_TOTAL,
            },
        );
    }

    fn configure_settings_preview(
        &mut self,
        resident: wire::ResidentEnabled,
        first_day_of_week: wire::Weekday,
    ) {
        let crate::model::SettingsEffect::LoadSettings { generation } = self.settings.open() else {
            unreachable!("opening SettingsPage emits LoadSettings");
        };
        let ui_language = if self.formatter.language() == quartz_i18n::Language::SimplifiedChinese {
            wire::UiLanguage::SimplifiedChinese
        } else {
            wire::UiLanguage::English
        };
        let ui = wire::UiSettings {
            resident: wire::ResidentSettings { enabled: resident },
            prewarm: wire::PrewarmSettings {
                enabled: wire::PrewarmEnabled::Enabled,
                on_battery: wire::PrewarmOnBattery::Blocked,
            },
            language: wire::UiLanguageSetting::Auto,
            hour_cycle: wire::HourCycleSetting::Auto,
            first_day_of_week: wire::FirstDayOfWeekSetting::Auto,
        };
        let system = wire::EffectiveUiSettings {
            resident: wire::ResidentSettings::default(),
            prewarm: wire::PrewarmSettings::default(),
            ui_language,
            locale: Some(LocaleSettings {
                hour_cycle: wire::HourCycle::H24,
                first_day_of_week,
            }),
        };
        let autostart = match resident {
            wire::ResidentEnabled::Enabled => wire::AutostartEnabled::Enabled,
            wire::ResidentEnabled::Disabled => wire::AutostartEnabled::Disabled,
        };
        self.settings.apply_loaded(
            generation,
            Ok(wire::SettingsResponse {
                ui,
                effective: ui.resolve(system),
                autostart,
                title_retention: wire::TitleRetention::Forever,
            }),
        );
        let crate::model::SettingsEffect::LoadDataInfo {
            generation: data_gen,
        } = self.settings.load_data_info()
        else {
            unreachable!("load_data_info emits LoadDataInfo");
        };
        self.settings
            .apply_data_info_loaded(data_gen, Ok(fixture_data_info_response()));
        let crate::model::SettingsEffect::LoadCoreStatus {
            generation: status_gen,
        } = self.settings.load_core_status()
        else {
            unreachable!("load_core_status emits LoadCoreStatus");
        };
        self.settings
            .apply_core_status_loaded(status_gen, Ok(fixture_core_status_response()));
        let crate::model::SettingsEffect::LoadBrowserHosts {
            generation: browser_gen,
        } = self.settings.load_browser_hosts()
        else {
            unreachable!("load_browser_hosts emits LoadBrowserHosts");
        };
        self.settings
            .apply_browser_hosts_loaded(browser_gen, Ok(fixture_browser_hosts_response()));
        self.settings_selects_sync = InputSyncState::NeedsSync;
    }

    fn configure_today_annotations_preview(&mut self, open_create_error: bool) {
        self.day = sample_live_day();
        let RulesEffect::LoadCategories { generation } = self.rules.reload_categories() else {
            unreachable!("reload_categories emits LoadCategories");
        };
        self.rules
            .apply_categories_loaded(generation, Ok(fixture_categories_response()));
        self.details = Some(fixture_selected_segment_details(&self.day));

        let ctx = self
            .day
            .live_time_context()
            .expect("sample live day provides time context");
        let day_start = ctx.bounds.start;
        let _ = self.annotation_editor.set_day_context(Some(ctx));
        let AnnotationEditorEffect::Query {
            date, generation, ..
        } = self.annotation_editor.open_panel()
        else {
            unreachable!("opening annotations panel emits Query");
        };
        self.annotation_editor.apply_loaded(
            generation,
            date,
            Ok(fixture_annotations_response(day_start)),
        );

        if open_create_error {
            self.annotation_editor.open_create(
                UtcRange {
                    start: day_start + 14 * HOUR_MS,
                    end: day_start + 14 * HOUR_MS + 45 * MINUTE_MS,
                },
                Some(parse_uuid(CAT_DEV_UUID)),
            );
            self.annotation_editor
                .set_label_input("Focused session".to_owned());
            self.annotation_editor.set_end_input("13:30".to_owned());
            let _ = self.annotation_editor.submit(|| None);
            self.annotation_inputs_sync = InputSyncState::NeedsSync;
        }
    }

    fn configure_classify_menu_preview(&mut self) -> PreviewTimelineOverlay {
        self.day = sample_live_day();
        let RulesEffect::LoadCategories { generation } = self.rules.reload_categories() else {
            unreachable!("reload_categories emits LoadCategories");
        };
        self.rules
            .apply_categories_loaded(generation, Ok(fixture_categories_response()));
        let _ = self
            .annotation_editor
            .set_day_context(self.day.live_time_context());
        self.details = Some(fixture_selected_segment_details(&self.day));

        let segment = self
            .day
            .segment(CLASSIFY_SEGMENT_ID)
            .expect("classify sample segment exists");
        let span = segment.span;
        let range = self
            .day
            .utc_range(span)
            .expect("live day converts segment span to UTC range");
        self.segment_menu = Some(SegmentContextMenu {
            range,
            position: point(px(CLASSIFY_MENU_FALLBACK_X), px(CLASSIFY_MENU_FALLBACK_Y)),
        });
        PreviewTimelineOverlay::ClassifyMenu { span, range }
    }
}

fn parse_uuid(text: &str) -> wire::Uuid {
    text.parse().expect("preview fixture UUID is valid")
}

fn sample_day_date() -> wire::LocalDate {
    jiff::civil::date(2026, 11, 16)
}

fn sample_day_start_utc_ms() -> i64 {
    sample_day_date()
        .at(0, 0, 0, 0)
        .to_zoned_datetime(jiff::tz::TimeZone::UTC)
        .expect("sample date at UTC midnight")
        .timestamp()
        .as_millisecond()
}

fn fixture_data_info_response() -> wire::DataInfoResponse {
    let base_ms = sample_day_start_utc_ms();
    wire::DataInfoResponse {
        database_path: r"C:\Users\alice\AppData\Local\Quartz\quartz.db".to_owned(),
        database_bytes: 148_500_000,
        logs_path: r"C:\Users\alice\AppData\Local\Quartz\logs".to_owned(),
        backups_path: r"C:\Users\alice\AppData\Local\Quartz\backups".to_owned(),
        backups: vec![
            wire::BackupEntry {
                file_name: "manual-20261115T183000Z.db".to_owned(),
                bytes: 145_200_000,
                created_utc_ms: base_ms - 5 * HOUR_MS - 30 * MINUTE_MS,
                kind: wire::BackupKind::Manual,
            },
            wire::BackupEntry {
                file_name: "pre-migrate-20261110-20261112T091500Z.db".to_owned(),
                bytes: 132_800_000,
                created_utc_ms: base_ms - 4 * 24 * HOUR_MS + 9 * HOUR_MS + 15 * MINUTE_MS,
                kind: wire::BackupKind::Migration,
            },
            wire::BackupEntry {
                file_name: "manual-20261108T210000Z.db".to_owned(),
                bytes: 118_400_000,
                created_utc_ms: base_ms - 8 * 24 * HOUR_MS + 21 * HOUR_MS,
                kind: wire::BackupKind::Manual,
            },
        ],
    }
}

fn fixture_core_status_response() -> wire::CoreStatusResponse {
    wire::CoreStatusResponse {
        recording: true,
        stop_reason: None,
        database: wire::DatabaseState::Ok,
        browser_streams: Vec::new(),
        instance_id: wire::InstanceId("preview-core-instance".to_owned()),
        core_version: wire::CoreVersion(env!("CARGO_PKG_VERSION").to_owned()),
    }
}

fn fixture_browser_hosts_response() -> wire::BrowserHostsStatusResponse {
    wire::BrowserHostsStatusResponse {
        hosts: vec![
            wire::BrowserHostEntry {
                browser: wire::BrowserKind::Chrome,
                registration: wire::BrowserHostRegistration::Registered,
                connected: wire::BrowserHostConnection::Connected,
            },
            wire::BrowserHostEntry {
                browser: wire::BrowserKind::Edge,
                registration: wire::BrowserHostRegistration::NeedsReenable,
                connected: wire::BrowserHostConnection::Disconnected,
            },
            wire::BrowserHostEntry {
                browser: wire::BrowserKind::Firefox,
                registration: wire::BrowserHostRegistration::NotRegistered,
                connected: wire::BrowserHostConnection::Disconnected,
            },
        ],
    }
}

fn category_ref_dev() -> wire::CategoryRef {
    wire::CategoryRef::Classified {
        category: wire::Category {
            id: wire::CategoryId(1),
            uuid: wire::CategoryUuid(CAT_DEV_UUID.to_owned()),
            builtin_key: Some(wire::BuiltinCategory::Development),
            name: None,
            color: "cat.8".to_owned(),
            tendency: wire::CategoryTendency::Focus,
        }
        .into(),
    }
}

fn category_ref_learning() -> wire::CategoryRef {
    wire::CategoryRef::Classified {
        category: wire::Category {
            id: wire::CategoryId(2),
            uuid: wire::CategoryUuid(CAT_LEARN_UUID.to_owned()),
            builtin_key: Some(wire::BuiltinCategory::Learning),
            name: None,
            color: "cat.1".to_owned(),
            tendency: wire::CategoryTendency::Focus,
        }
        .into(),
    }
}

fn category_ref_comm() -> wire::CategoryRef {
    wire::CategoryRef::Classified {
        category: wire::Category {
            id: wire::CategoryId(3),
            uuid: wire::CategoryUuid(CAT_COMM_UUID.to_owned()),
            builtin_key: Some(wire::BuiltinCategory::Communication),
            name: None,
            color: "cat.5".to_owned(),
            tendency: wire::CategoryTendency::Neutral,
        }
        .into(),
    }
}

fn category_ref_entertainment() -> wire::CategoryRef {
    wire::CategoryRef::Classified {
        category: wire::Category {
            id: wire::CategoryId(4),
            uuid: wire::CategoryUuid(CAT_ENTERTAIN_UUID.to_owned()),
            builtin_key: Some(wire::BuiltinCategory::Entertainment),
            name: None,
            color: "cat.11".to_owned(),
            tendency: wire::CategoryTendency::Distraction,
        }
        .into(),
    }
}

fn fixture_categories_response() -> wire::CategoriesListResponse {
    wire::CategoriesListResponse {
        entities: vec![
            wire::EditableCategory {
                uuid: parse_uuid(CAT_DEV_UUID),
                builtin_key: Some(wire::BuiltinCategory::Development),
                parent_uuid: None,
                name: None,
                color: "cat.8".to_owned(),
                tendency: wire::CategoryTendency::Focus,
                sort_order: 0,
                archived: false,
            },
            wire::EditableCategory {
                uuid: parse_uuid(CAT_LEARN_UUID),
                builtin_key: Some(wire::BuiltinCategory::Learning),
                parent_uuid: None,
                name: None,
                color: "cat.1".to_owned(),
                tendency: wire::CategoryTendency::Focus,
                sort_order: 1,
                archived: false,
            },
            wire::EditableCategory {
                uuid: parse_uuid(CAT_COMM_UUID),
                builtin_key: Some(wire::BuiltinCategory::Communication),
                parent_uuid: None,
                name: None,
                color: "cat.5".to_owned(),
                tendency: wire::CategoryTendency::Neutral,
                sort_order: 2,
                archived: false,
            },
            wire::EditableCategory {
                uuid: parse_uuid(CAT_ENTERTAIN_UUID),
                builtin_key: Some(wire::BuiltinCategory::Entertainment),
                parent_uuid: None,
                name: None,
                color: "cat.11".to_owned(),
                tendency: wire::CategoryTendency::Distraction,
                sort_order: 3,
                archived: false,
            },
            wire::EditableCategory {
                uuid: parse_uuid(CAT_SYSTEM_UUID),
                builtin_key: Some(wire::BuiltinCategory::System),
                parent_uuid: None,
                name: None,
                color: "cat.14".to_owned(),
                tendency: wire::CategoryTendency::Neutral,
                sort_order: 4,
                archived: false,
            },
            wire::EditableCategory {
                uuid: parse_uuid(CAT_RESEARCH_UUID),
                builtin_key: None,
                parent_uuid: None,
                name: Some("Research".to_owned()),
                color: "cat.2".to_owned(),
                tendency: wire::CategoryTendency::Focus,
                sort_order: 5,
                archived: false,
            },
            wire::EditableCategory {
                uuid: parse_uuid(CAT_ARCHIVED_UUID),
                builtin_key: None,
                parent_uuid: None,
                name: Some("Legacy Archive".to_owned()),
                color: "cat.9".to_owned(),
                tendency: wire::CategoryTendency::Neutral,
                sort_order: 6,
                archived: true,
            },
        ],
        versions: wire::QueryVersions(PREVIEW_RULES_VERSION.to_owned()),
        truncated: false,
    }
}

fn fixture_rule_tree_responses() -> Vec<wire::RuleSetsTreeResponse> {
    let versions = wire::QueryVersions(PREVIEW_RULES_VERSION.to_owned());
    vec![
        wire::RuleSetsTreeResponse {
            kind: wire::RuleKind::Classification,
            effective_version: "cls-eff-1".to_owned(),
            nodes: vec![
                wire::RuleSetNode {
                    key: "builtin.classification".to_owned(),
                    name: "Built-in classification".to_owned(),
                    parent: None,
                    version: 1,
                    enabled: true,
                    sibling_order: 0,
                    source: wire::RuleSetSource::Builtin,
                    counts: wire::RuleSetCounts {
                        added: 12,
                        overridden: 0,
                        disabled: 0,
                    },
                    warnings: Vec::new(),
                },
                wire::RuleSetNode {
                    key: PREVIEW_USER_SET_KEY.to_owned(),
                    name: "Study sites".to_owned(),
                    parent: Some("builtin.classification".to_owned()),
                    version: PREVIEW_USER_SET_VERSION,
                    enabled: true,
                    sibling_order: 0,
                    source: wire::RuleSetSource::User,
                    counts: wire::RuleSetCounts {
                        added: 2,
                        overridden: 1,
                        disabled: 0,
                    },
                    warnings: Vec::new(),
                },
                wire::RuleSetNode {
                    key: "user.weekend-mode".to_owned(),
                    name: "Weekend mode".to_owned(),
                    parent: Some("builtin.classification".to_owned()),
                    version: 1,
                    enabled: false,
                    sibling_order: 1,
                    source: wire::RuleSetSource::User,
                    counts: wire::RuleSetCounts {
                        added: 1,
                        overridden: 0,
                        disabled: 1,
                    },
                    warnings: Vec::new(),
                },
            ],
            versions: versions.clone(),
            truncated: false,
        },
        wire::RuleSetsTreeResponse {
            kind: wire::RuleKind::Extraction,
            effective_version: "ext-eff-1".to_owned(),
            nodes: vec![wire::RuleSetNode {
                key: "builtin.extraction".to_owned(),
                name: "Built-in extraction".to_owned(),
                parent: None,
                version: 1,
                enabled: true,
                sibling_order: 0,
                source: wire::RuleSetSource::Builtin,
                counts: wire::RuleSetCounts {
                    added: 6,
                    overridden: 0,
                    disabled: 0,
                },
                warnings: Vec::new(),
            }],
            versions: versions.clone(),
            truncated: false,
        },
        wire::RuleSetsTreeResponse {
            kind: wire::RuleKind::Privacy,
            effective_version: "prv-eff-1".to_owned(),
            nodes: vec![wire::RuleSetNode {
                key: "builtin.privacy".to_owned(),
                name: "Built-in privacy".to_owned(),
                parent: None,
                version: 1,
                enabled: true,
                sibling_order: 0,
                source: wire::RuleSetSource::Builtin,
                counts: wire::RuleSetCounts {
                    added: 4,
                    overridden: 0,
                    disabled: 0,
                },
                warnings: Vec::new(),
            }],
            versions,
            truncated: false,
        },
    ]
}

fn fixture_user_set_detail_response() -> wire::RuleSetsGetResponse {
    wire::RuleSetsGetResponse {
        key: PREVIEW_USER_SET_KEY.to_owned(),
        name: "Study sites".to_owned(),
        version: PREVIEW_USER_SET_VERSION,
        parent_version: Some(1),
        entries: vec![
            wire::RuleSetEntry {
                entry_kind: wire::RuleEntryKind::Rule,
                content: wire::RuleEntryContent {
                    id: Some("docs-site".to_owned()),
                    target: None,
                    reason: None,
                    label: Some("Reference docs".to_owned()),
                    priority: Some(60),
                    detector: None,
                    languages: None,
                    verified: None,
                    label_key: None,
                    category: Some(parse_uuid(CAT_LEARN_UUID)),
                    r#match: Some(wire::RuleMatch::All),
                    when: Some(vec![wire::RuleCondition {
                        field: wire::RuleField::Domain,
                        op: wire::RuleOperator::DomainSuffix,
                        value: "docs.example".to_owned(),
                        case_sensitive: false,
                        negate: false,
                    }]),
                    app: None,
                    pattern: None,
                    window_title: None,
                    web: None,
                },
                provenance: vec![wire::RuleProvenance {
                    key: PREVIEW_USER_SET_KEY.to_owned(),
                    version: PREVIEW_USER_SET_VERSION,
                    id: "docs-site".to_owned(),
                }],
                state: wire::RuleEntryState::Effective,
            },
            wire::RuleSetEntry {
                entry_kind: wire::RuleEntryKind::Rule,
                content: wire::RuleEntryContent {
                    id: Some("notes-app".to_owned()),
                    target: None,
                    reason: None,
                    label: Some("Notes workspace".to_owned()),
                    priority: Some(55),
                    detector: None,
                    languages: None,
                    verified: None,
                    label_key: None,
                    category: Some(parse_uuid(CAT_DEV_UUID)),
                    r#match: Some(wire::RuleMatch::All),
                    when: Some(vec![wire::RuleCondition {
                        field: wire::RuleField::ExeName,
                        op: wire::RuleOperator::Equals,
                        value: "notes-app.exe".to_owned(),
                        case_sensitive: false,
                        negate: false,
                    }]),
                    app: None,
                    pattern: None,
                    window_title: None,
                    web: None,
                },
                provenance: vec![wire::RuleProvenance {
                    key: PREVIEW_USER_SET_KEY.to_owned(),
                    version: PREVIEW_USER_SET_VERSION,
                    id: "notes-app".to_owned(),
                }],
                state: wire::RuleEntryState::Effective,
            },
            wire::RuleSetEntry {
                entry_kind: wire::RuleEntryKind::Override,
                content: wire::RuleEntryContent {
                    id: None,
                    target: Some("builtin.classification/browser-video".to_owned()),
                    reason: None,
                    label: Some("Study video portals".to_owned()),
                    priority: Some(70),
                    detector: None,
                    languages: None,
                    verified: None,
                    label_key: None,
                    category: Some(parse_uuid(CAT_LEARN_UUID)),
                    r#match: Some(wire::RuleMatch::All),
                    when: Some(vec![wire::RuleCondition {
                        field: wire::RuleField::Domain,
                        op: wire::RuleOperator::Equals,
                        value: "lectures.example".to_owned(),
                        case_sensitive: false,
                        negate: false,
                    }]),
                    app: None,
                    pattern: None,
                    window_title: None,
                    web: None,
                },
                provenance: vec![
                    wire::RuleProvenance {
                        key: "builtin.classification".to_owned(),
                        version: 1,
                        id: "browser-video".to_owned(),
                    },
                    wire::RuleProvenance {
                        key: PREVIEW_USER_SET_KEY.to_owned(),
                        version: PREVIEW_USER_SET_VERSION,
                        id: "browser-video".to_owned(),
                    },
                ],
                state: wire::RuleEntryState::Effective,
            },
        ],
        versions: wire::QueryVersions(PREVIEW_RULES_VERSION.to_owned()),
        truncated: false,
    }
}

fn fixture_rule_editor_invalid_text() -> String {
    r#"format = "quartz-rules/1"

[set]
key = "user.study-sites"
kind = "classification"
name = "Study sites"
parent = "builtin.classification"

[[rule]]
id = "docs-site"
label = "Reference docs"
category = "builtin:learning"
priority = 60
match = "all"
when = [
  { field = "app.unknown_field", op = "equals", value = "docs.example" },
]
"#
    .to_owned()
}

fn fixture_rule_editor_valid_text() -> String {
    r#"format = "quartz-rules/1"

[set]
key = "user.study-sites"
kind = "classification"
name = "Study sites"
parent = "builtin.classification"

[[rule]]
id = "docs-site"
label = "Reference docs"
category = "builtin:learning"
priority = 60
match = "all"
when = [
  { field = "web.domain", op = "domain_suffix", value = "docs.example" },
]

[[rule]]
id = "notes-app"
label = "Notes workspace"
category = "builtin:development"
priority = 55
match = "all"
when = [
  { field = "app.exe_name", op = "equals", value = "notes-app.exe" },
]
"#
    .to_owned()
}

fn fixture_rule_preview_response(day_start: i64) -> wire::RulesPreviewResponse {
    wire::RulesPreviewResponse {
        rules: vec![
            wire::RulePreviewStatistics {
                rule: "user.study-sites/docs-site".to_owned(),
                matched_count: 14,
                matched_ms: 30 * MINUTE_MS,
                effective_ms: 30 * MINUTE_MS,
            },
            wire::RulePreviewStatistics {
                rule: "user.study-sites/notes-app".to_owned(),
                matched_count: 8,
                matched_ms: 20 * MINUTE_MS,
                effective_ms: 20 * MINUTE_MS,
            },
        ],
        totals: wire::RulePreviewTotals {
            before: wire::RulePreviewSideTotals {
                active_ms: 4 * HOUR_MS,
                categories: vec![
                    wire::PreviewCategoryRank {
                        category: category_ref_dev(),
                        active_ms: 2 * HOUR_MS,
                    },
                    wire::PreviewCategoryRank {
                        category: category_ref_learning(),
                        active_ms: HOUR_MS,
                    },
                    wire::PreviewCategoryRank {
                        category: wire::CategoryRef::Unclassified,
                        active_ms: HOUR_MS,
                    },
                ],
            },
            after: wire::RulePreviewSideTotals {
                active_ms: 4 * HOUR_MS,
                categories: vec![
                    wire::PreviewCategoryRank {
                        category: category_ref_dev(),
                        active_ms: 2 * HOUR_MS + 20 * MINUTE_MS,
                    },
                    wire::PreviewCategoryRank {
                        category: category_ref_learning(),
                        active_ms: HOUR_MS + 30 * MINUTE_MS,
                    },
                    wire::PreviewCategoryRank {
                        category: wire::CategoryRef::Unclassified,
                        active_ms: 10 * MINUTE_MS,
                    },
                ],
            },
        },
        document_change_count: 2,
        further_hidden_count: 1,
        samples: vec![
            wire::RulePreviewSample {
                start_utc_ms: day_start + 9 * HOUR_MS + 15 * MINUTE_MS,
                end_utc_ms: day_start + 9 * HOUR_MS + 45 * MINUTE_MS,
                clock_segment_id: wire::ClockSegmentId(101),
                rule: "user.study-sites/docs-site".to_owned(),
                before: wire::RulePreviewOutput::Classification {
                    category: wire::CategoryRef::Unclassified,
                },
                after: wire::RulePreviewOutput::Classification {
                    category: category_ref_learning(),
                },
            },
            wire::RulePreviewSample {
                start_utc_ms: day_start + 10 * HOUR_MS + 30 * MINUTE_MS,
                end_utc_ms: day_start + 10 * HOUR_MS + 50 * MINUTE_MS,
                clock_segment_id: wire::ClockSegmentId(102),
                rule: "user.study-sites/notes-app".to_owned(),
                before: wire::RulePreviewOutput::Classification {
                    category: wire::CategoryRef::Unclassified,
                },
                after: wire::RulePreviewOutput::Classification {
                    category: category_ref_dev(),
                },
            },
            wire::RulePreviewSample {
                start_utc_ms: day_start + 14 * HOUR_MS,
                end_utc_ms: day_start + 14 * HOUR_MS + 15 * MINUTE_MS,
                clock_segment_id: wire::ClockSegmentId(103),
                rule: "user.study-sites/private-docs".to_owned(),
                before: wire::RulePreviewOutput::Extraction {
                    extraction: wire::TitleExtraction::default(),
                },
                after: wire::RulePreviewOutput::Extraction {
                    extraction: wire::TitleExtraction::default(),
                },
            },
        ],
        samples_truncated: false,
    }
}

fn fixture_unclassified_response() -> wire::QueryUnclassifiedResponse {
    wire::QueryUnclassifiedResponse {
        time_zone: "UTC".to_owned(),
        day_start_offset_min: 0,
        reference_range: None,
        items: vec![
            wire::UnclassifiedItem {
                entity: wire::WorkEntity::App {
                    id: wire::ApplicationId(21),
                    exe_name: Some("draftstudio.exe".to_owned()),
                    display_name: "Draft Studio".to_owned(),
                },
                active_ms: 75 * MINUTE_MS,
                sample_ranges: Vec::new(),
            },
            wire::UnclassifiedItem {
                entity: wire::WorkEntity::App {
                    id: wire::ApplicationId(22),
                    exe_name: Some("packetview.exe".to_owned()),
                    display_name: "Packet Viewer".to_owned(),
                },
                active_ms: 45 * MINUTE_MS,
                sample_ranges: Vec::new(),
            },
            wire::UnclassifiedItem {
                entity: wire::WorkEntity::App {
                    id: wire::ApplicationId(23),
                    exe_name: Some("localwiki.exe".to_owned()),
                    display_name: "Local Wiki".to_owned(),
                },
                active_ms: 30 * MINUTE_MS,
                sample_ranges: Vec::new(),
            },
            wire::UnclassifiedItem {
                entity: wire::WorkEntity::App {
                    id: wire::ApplicationId(24),
                    exe_name: None,
                    display_name: String::new(),
                },
                active_ms: 18 * MINUTE_MS,
                sample_ranges: Vec::new(),
            },
            wire::UnclassifiedItem {
                entity: wire::WorkEntity::App {
                    id: wire::ApplicationId(25),
                    exe_name: Some("calcpad.exe".to_owned()),
                    display_name: "Calc Pad".to_owned(),
                },
                active_ms: 9 * MINUTE_MS,
                sample_ranges: Vec::new(),
            },
        ],
        versions: wire::QueryVersions(PREVIEW_RULES_VERSION.to_owned()),
        truncated: false,
    }
}

fn fixture_annotations_response(day_start: i64) -> wire::QueryAnnotationsResponse {
    wire::QueryAnnotationsResponse {
        entities: vec![
            wire::SavedAnnotation {
                uuid: wire::AnnotationId(parse_uuid(ANNO_1_UUID)),
                kind: wire::AnnotationKind::CategoryOverride,
                start_utc_ms: day_start + 9 * HOUR_MS + 15 * MINUTE_MS,
                end_utc_ms: day_start + 10 * HOUR_MS,
                category: Some(wire::AnnotationCategory::Uuid {
                    uuid: parse_uuid(CAT_LEARN_UUID),
                }),
                label_preset: None,
                label: Some("Architecture reading".to_owned()),
                note: Some("Reviewed workspace layout notes and protocol specs.".to_owned()),
                created_utc_ms: day_start + 10 * HOUR_MS,
                updated_utc_ms: day_start + 10 * HOUR_MS,
            },
            wire::SavedAnnotation {
                uuid: wire::AnnotationId(parse_uuid(ANNO_2_UUID)),
                kind: wire::AnnotationKind::OfflineActivity,
                start_utc_ms: day_start + 12 * HOUR_MS,
                end_utc_ms: day_start + 12 * HOUR_MS + 45 * MINUTE_MS,
                category: Some(wire::AnnotationCategory::Uuid {
                    uuid: parse_uuid(CAT_COMM_UUID),
                }),
                label_preset: None,
                label: Some("Design sync".to_owned()),
                note: Some("Whiteboard walkthrough for scheduling constraints.".to_owned()),
                created_utc_ms: day_start + 13 * HOUR_MS,
                updated_utc_ms: day_start + 13 * HOUR_MS,
            },
            wire::SavedAnnotation {
                uuid: wire::AnnotationId(parse_uuid(ANNO_3_UUID)),
                kind: wire::AnnotationKind::CategoryOverride,
                start_utc_ms: day_start + 15 * HOUR_MS + 10 * MINUTE_MS,
                end_utc_ms: day_start + 15 * HOUR_MS + 35 * MINUTE_MS,
                category: Some(wire::AnnotationCategory::Uuid {
                    uuid: parse_uuid(CAT_MISSING_UUID),
                }),
                label_preset: None,
                label: Some("Legacy tooling review".to_owned()),
                note: None,
                created_utc_ms: day_start + 15 * HOUR_MS + 40 * MINUTE_MS,
                updated_utc_ms: day_start + 15 * HOUR_MS + 40 * MINUTE_MS,
            },
        ],
        versions: wire::QueryVersions("preview-annotations-v1".to_owned()),
        truncated: false,
    }
}

fn sample_live_apps() -> [wire::ApplicationRef; 4] {
    [
        wire::ApplicationRef {
            id: wire::ApplicationId(1),
            exe_name: Some("codestudio.exe".to_owned()),
            display_name: "Code Studio".to_owned(),
            aumid: None,
        },
        wire::ApplicationRef {
            id: wire::ApplicationId(2),
            exe_name: Some("browser.exe".to_owned()),
            display_name: "Web Browser".to_owned(),
            aumid: None,
        },
        wire::ApplicationRef {
            id: wire::ApplicationId(3),
            exe_name: Some("teamchat.exe".to_owned()),
            display_name: "Team Chat".to_owned(),
            aumid: None,
        },
        wire::ApplicationRef {
            id: wire::ApplicationId(4),
            exe_name: Some("terminal.exe".to_owned()),
            display_name: "Shell Terminal".to_owned(),
            aumid: None,
        },
    ]
}

fn sample_live_day() -> Day {
    sample_live_day_with_websites(Vec::new())
}

fn sample_live_day_with_websites(websites: Vec<wire::WebsiteRank>) -> Day {
    let date = sample_day_date();
    let day_start = sample_day_start_utc_ms();
    let day_end = day_start + 24 * HOUR_MS;
    let at_hm = |hours: i64, minutes: i64| day_start + hours * HOUR_MS + minutes * MINUTE_MS;

    let [code_studio, browser, team_chat, terminal] = sample_live_apps();
    let dev = category_ref_dev();
    let learning = category_ref_learning();
    let comm = category_ref_comm();
    let entertainment = category_ref_entertainment();

    let rule_source = |spec: &str| wire::CategorySource::Rule {
        rule: spec.parse().expect("valid sample rule id"),
    };

    let active_segment = |start: i64,
                          end: i64,
                          clock_id: i64,
                          app: &wire::ApplicationRef,
                          category: &wire::CategoryRef,
                          source: wire::CategorySource,
                          title: &str| wire::TimelineSegment {
        extraction: None,
        document_ref: None,
        start_utc_ms: start,
        end_utc_ms: end,
        duration_ms: end - start,
        clock_segment_id: Some(wire::ClockSegmentId(clock_id)),
        activity: wire::Activity::Active,
        category: category.clone(),
        category_source: source,
        app: Some(app.clone()),
        window_instance: None,
        title: Some(title.to_owned()),
    };

    let state_segment =
        |start: i64, end: i64, clock_id: i64, activity: wire::Activity| wire::TimelineSegment {
            extraction: None,
            document_ref: None,
            start_utc_ms: start,
            end_utc_ms: end,
            duration_ms: end - start,
            clock_segment_id: Some(wire::ClockSegmentId(clock_id)),
            activity,
            category: wire::CategoryRef::Unclassified,
            category_source: wire::CategorySource::Unclassified,
            app: None,
            window_instance: None,
            title: None,
        };

    let timeline = vec![
        active_segment(
            at_hm(9, 0),
            at_hm(9, 45),
            1,
            &code_studio,
            &dev,
            rule_source("builtin.classification/codestudio@1"),
            "workspace/main.rs",
        ),
        active_segment(
            at_hm(9, 45),
            at_hm(10, 15),
            2,
            &browser,
            &learning,
            rule_source("user.study-sites/docs-site@3"),
            "Reference docs",
        ),
        active_segment(
            at_hm(10, 15),
            at_hm(11, 10),
            3,
            &code_studio,
            &dev,
            rule_source("builtin.classification/codestudio@1"),
            "workspace/timeline.rs",
        ),
        active_segment(
            at_hm(11, 10),
            at_hm(11, 30),
            4,
            &terminal,
            &dev,
            rule_source("builtin.classification/terminal@1"),
            "build check",
        ),
        active_segment(
            at_hm(11, 30),
            at_hm(12, 0),
            5,
            &team_chat,
            &comm,
            rule_source("builtin.classification/teamchat@1"),
            "Daily standup",
        ),
        state_segment(
            at_hm(12, 0),
            at_hm(13, 15),
            6,
            wire::Activity::Stopped {
                reasons: vec![wire::QueryStopReason::Locked],
            },
        ),
        // Index 5 among active raw segments -> CLASSIFY_SEGMENT_ID.
        active_segment(
            at_hm(13, 15),
            at_hm(14, 0),
            7,
            &browser,
            &wire::CategoryRef::Unclassified,
            wire::CategorySource::Unclassified,
            "internal-portal.example",
        ),
        active_segment(
            at_hm(14, 0),
            at_hm(15, 15),
            8,
            &code_studio,
            &dev,
            rule_source("builtin.classification/codestudio@1"),
            "workspace/rules.rs",
        ),
        state_segment(at_hm(15, 15), at_hm(15, 35), 9, wire::Activity::Idle),
        active_segment(
            at_hm(15, 35),
            at_hm(16, 15),
            10,
            &browser,
            &entertainment,
            rule_source("builtin.classification/media@1"),
            "Video stream",
        ),
        active_segment(
            at_hm(16, 15),
            at_hm(17, 0),
            11,
            &team_chat,
            &comm,
            rule_source("builtin.classification/teamchat@1"),
            "Design review",
        ),
    ];

    let hour_stack = |hour: i64, categories: Vec<wire::CategoryDuration>| wire::HourStack {
        start_utc_ms: at_hm(hour, 0),
        end_utc_ms: at_hm(hour + 1, 0),
        local_start: format!("2026-11-16T{hour:02}:00:00+00:00[UTC]"),
        categories,
    };

    let hours = vec![
        hour_stack(
            9,
            vec![
                wire::CategoryDuration {
                    category: dev.clone(),
                    duration_ms: 45 * MINUTE_MS,
                },
                wire::CategoryDuration {
                    category: learning.clone(),
                    duration_ms: 15 * MINUTE_MS,
                },
            ],
        ),
        hour_stack(
            10,
            vec![
                wire::CategoryDuration {
                    category: learning,
                    duration_ms: 15 * MINUTE_MS,
                },
                wire::CategoryDuration {
                    category: dev.clone(),
                    duration_ms: 45 * MINUTE_MS,
                },
            ],
        ),
        hour_stack(
            11,
            vec![
                wire::CategoryDuration {
                    category: dev.clone(),
                    duration_ms: 30 * MINUTE_MS,
                },
                wire::CategoryDuration {
                    category: comm.clone(),
                    duration_ms: 30 * MINUTE_MS,
                },
            ],
        ),
        hour_stack(12, Vec::new()),
        hour_stack(
            13,
            vec![wire::CategoryDuration {
                category: wire::CategoryRef::Unclassified,
                duration_ms: 45 * MINUTE_MS,
            }],
        ),
        hour_stack(
            14,
            vec![wire::CategoryDuration {
                category: dev.clone(),
                duration_ms: 60 * MINUTE_MS,
            }],
        ),
        hour_stack(
            15,
            vec![
                wire::CategoryDuration {
                    category: dev,
                    duration_ms: 15 * MINUTE_MS,
                },
                wire::CategoryDuration {
                    category: entertainment.clone(),
                    duration_ms: 25 * MINUTE_MS,
                },
            ],
        ),
        hour_stack(
            16,
            vec![
                wire::CategoryDuration {
                    category: entertainment,
                    duration_ms: 15 * MINUTE_MS,
                },
                wire::CategoryDuration {
                    category: comm,
                    duration_ms: 45 * MINUTE_MS,
                },
            ],
        ),
    ];

    let data = DayData {
        date,
        bounds: UtcRange {
            start: day_start,
            end: day_end,
        },
        visible: UtcRange {
            start: day_start,
            end: day_end,
        },
        reference_range: Some(wire::ReferenceRange {
            start_utc_ms: day_start,
            end_utc_ms: day_end,
        }),
        metrics: wire::DayMetrics {
            active_ms: 385 * MINUTE_MS,
            focus_ratio: 0.60,
            distract_ratio: 0.10,
            unclassified_ms: 45 * MINUTE_MS,
            longest_focus_ms: 75 * MINUTE_MS,
            switches_per_hour: 5.0,
        },
        hours,
        applications: vec![
            wire::ApplicationRank {
                app: code_studio,
                active_ms: 175 * MINUTE_MS,
            },
            wire::ApplicationRank {
                app: browser,
                active_ms: 115 * MINUTE_MS,
            },
            wire::ApplicationRank {
                app: team_chat,
                active_ms: 75 * MINUTE_MS,
            },
            wire::ApplicationRank {
                app: terminal,
                active_ms: 20 * MINUTE_MS,
            },
        ],
        documents: vec![
            wire::DocumentRank {
                document_ref: wire::DocumentRef::from_encoded("preview:doc:main".to_owned()),
                app: wire::ApplicationId(1),
                document: "main.rs".to_owned(),
                project: Some("workspace".to_owned()),
                profile: None,
                active_ms: 100 * MINUTE_MS,
            },
            wire::DocumentRank {
                document_ref: wire::DocumentRef::from_encoded("preview:doc:rules".to_owned()),
                app: wire::ApplicationId(1),
                document: "rules.rs".to_owned(),
                project: Some("workspace".to_owned()),
                profile: None,
                active_ms: 75 * MINUTE_MS,
            },
        ],
        websites,
        truncated: false,
        categories: timeline.clone(),
        raw: timeline,
        pending: Vec::new(),
    };
    Day::from_protocol(data).expect("sample live day is valid")
}

fn fixture_preview_websites() -> Vec<wire::WebsiteRank> {
    vec![
        wire::WebsiteRank {
            domain: "github.com".to_owned(),
            active_ms: 48 * MINUTE_MS,
        },
        wire::WebsiteRank {
            domain: "docs.rs".to_owned(),
            active_ms: 32 * MINUTE_MS,
        },
        wire::WebsiteRank {
            domain: "developer.mozilla.org".to_owned(),
            active_ms: 18 * MINUTE_MS,
        },
        wire::WebsiteRank {
            domain: "xn--mnchen-3ya.de".to_owned(),
            active_ms: 11 * MINUTE_MS,
        },
        wire::WebsiteRank {
            domain: wire::INTERNAL_BROWSER_DOMAIN.to_owned(),
            active_ms: 4 * MINUTE_MS,
        },
        wire::WebsiteRank {
            domain: wire::LOCAL_FILE_BROWSER_DOMAIN.to_owned(),
            active_ms: 2 * MINUTE_MS,
        },
    ]
}

fn fixture_selected_segment_details(day: &Day) -> OpenDetails {
    let subject = DetailsSubject::Segment(CLASSIFY_SEGMENT_ID);
    let request = day
        .detail_request(subject)
        .expect("live day builds segment detail request");
    let segment = day
        .segment(CLASSIFY_SEGMENT_ID)
        .expect("classify sample segment exists");
    let range = day
        .utc_range(segment.span)
        .expect("live day converts span to UTC range");
    let [_, browser, _, _] = sample_live_apps();
    let app_id = browser.id;
    let date = day.date();
    let daily_minutes = [65_i64, 80, 90, 50, 110, 40, 115];
    let daily = daily_minutes
        .into_iter()
        .enumerate()
        .map(|(index, minutes)| {
            let days_ago = i64::try_from(wire::ENTITY_DETAIL_DAYS - 1 - index)
                .expect("entity detail day offset fits");
            wire::EntityDailyDuration {
                local_date: date
                    .checked_sub(jiff::Span::new().days(days_ago))
                    .expect("sample date minus days fits"),
                active_ms: minutes * MINUTE_MS,
            }
        })
        .collect();
    let detail_data = DetailData {
        request: request.clone(),
        intervals: vec![wire::TimelineSegment {
            extraction: None,
            document_ref: None,
            start_utc_ms: range.start,
            end_utc_ms: range.end,
            duration_ms: range.end - range.start,
            clock_segment_id: Some(wire::ClockSegmentId(7)),
            activity: wire::Activity::Active,
            category: wire::CategoryRef::Unclassified,
            category_source: wire::CategorySource::Unclassified,
            app: Some(browser.clone()),
            window_instance: None,
            title: Some("internal-portal.example".to_owned()),
        }],
        entities: vec![EntityData {
            entity: EntityKey::App(app_id),
            daily,
            titles: vec![
                wire::EntityTitleDuration {
                    title: "internal-portal.example".to_owned(),
                    duration_ms: 45 * MINUTE_MS,
                },
                wire::EntityTitleDuration {
                    title: "Reference docs".to_owned(),
                    duration_ms: 30 * MINUTE_MS,
                },
            ],
            rules: Vec::new(),
        }],
    };
    let loaded = LiveDetails::from_protocol(detail_data).expect("preview segment details map");
    let color = Some(day.category(segment.category).color);
    OpenDetails {
        subject,
        content: PanelContent::Live(LivePanel {
            request,
            heading: PanelHeading::Text(browser.display_name),
            subtitle: browser.exe_name,
            color,
            phase: DetailPhase::Ready(loaded),
        }),
    }
}

fn fixture_week_range_response(week_start: wire::LocalDate) -> wire::QueryRangeResponse {
    let cat = |id: i64,
               uuid: &str,
               builtin_key: Option<wire::BuiltinCategory>,
               name: Option<&str>,
               color: &str,
               tendency: wire::CategoryTendency| wire::CategoryRef::Classified {
        category: wire::Category {
            id: wire::CategoryId(id),
            uuid: wire::CategoryUuid(uuid.to_owned()),
            builtin_key,
            name: name.map(str::to_owned),
            color: color.to_owned(),
            tendency,
        }
        .into(),
    };
    let dev = cat(
        1,
        CAT_DEV_UUID,
        Some(wire::BuiltinCategory::Development),
        None,
        "cat.8",
        wire::CategoryTendency::Focus,
    );
    let learn = cat(
        2,
        CAT_LEARN_UUID,
        Some(wire::BuiltinCategory::Learning),
        None,
        "cat.1",
        wire::CategoryTendency::Focus,
    );
    let comm = cat(
        3,
        CAT_COMM_UUID,
        Some(wire::BuiltinCategory::Communication),
        None,
        "cat.3",
        wire::CategoryTendency::Neutral,
    );
    let entertain = cat(
        4,
        CAT_ENTERTAIN_UUID,
        Some(wire::BuiltinCategory::Entertainment),
        None,
        "cat.13",
        wire::CategoryTendency::Distraction,
    );
    let system = cat(
        5,
        CAT_SYSTEM_UUID,
        Some(wire::BuiltinCategory::System),
        None,
        "cat.10",
        wire::CategoryTendency::Neutral,
    );

    let bucket = |offset_days: i64, entries: Vec<(wire::CategoryRef, i64)>| {
        let start_date = week_start
            .checked_add(jiff::ToSpan::days(offset_days))
            .expect("fixture week day");
        let end_date = start_date.tomorrow().expect("fixture week next day");
        let active_ms: i64 = entries.iter().map(|(_, ms)| *ms).sum();
        let groups = entries
            .into_iter()
            .map(|(category, ms)| wire::RangeGroup {
                dimension: wire::RangeDimension::Category,
                entity: wire::RangeEntity::Category(wire::CategoryEntity::Category { category }),
                active_ms: ms,
            })
            .collect();
        wire::RangeSummaryBucket {
            start_date,
            end_date,
            active_ms,
            groups,
        }
    };

    // 6 observed days (day 2 has 0 activity) + day 6 omitted as an empty future day.
    let buckets = vec![
        bucket(
            0,
            vec![
                (dev.clone(), 4 * HOUR_MS + 30 * MINUTE_MS),
                (learn.clone(), HOUR_MS + 15 * MINUTE_MS),
                (comm.clone(), HOUR_MS),
                (entertain.clone(), 30 * MINUTE_MS),
            ],
        ),
        bucket(
            1,
            vec![
                (dev.clone(), 3 * HOUR_MS + 50 * MINUTE_MS),
                (learn.clone(), HOUR_MS),
                (comm.clone(), 50 * MINUTE_MS),
                (entertain.clone(), 45 * MINUTE_MS),
                (wire::CategoryRef::Unclassified, 15 * MINUTE_MS),
            ],
        ),
        // Zero-activity day: empty column and no point on the share lines.
        bucket(2, Vec::new()),
        bucket(
            3,
            vec![
                (dev.clone(), 5 * HOUR_MS + 30 * MINUTE_MS),
                (learn.clone(), HOUR_MS + 20 * MINUTE_MS),
                (comm.clone(), 40 * MINUTE_MS),
                (system.clone(), 25 * MINUTE_MS),
                (entertain.clone(), 15 * MINUTE_MS),
            ],
        ),
        bucket(
            4,
            vec![
                (dev.clone(), 2 * HOUR_MS + 10 * MINUTE_MS),
                (comm, HOUR_MS + 15 * MINUTE_MS),
                (entertain.clone(), HOUR_MS + 35 * MINUTE_MS),
                (wire::CategoryRef::Unclassified, 20 * MINUTE_MS),
            ],
        ),
        bucket(
            5,
            vec![
                (dev, 2 * HOUR_MS),
                (learn, 50 * MINUTE_MS),
                (entertain, 35 * MINUTE_MS),
                (system, 20 * MINUTE_MS),
            ],
        ),
    ];

    wire::QueryRangeResponse {
        time_zone: "Asia/Shanghai".to_owned(),
        day_start_offset_min: 240,
        reference_range: None,
        buckets,
        versions: wire::QueryVersions("preview-week-v1".to_owned()),
        truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AnnotationCategoryDisplay, AnnotationEditor, CategoryCatalog, ImportAction, ImportTarget,
        LoadProgress, LoadState, NodeEnablement, PreviewState as RulePreviewState, ProgressUpdate,
        RuleEditor, RuleImport, RulesPage, UnclassifiedEntityLabel, UnclassifiedQueue,
        ValidateAnnotationDraftError, ValidateAnnotationTimeError, ValidationState,
        resolve_annotation_category,
    };

    #[test]
    fn m4_rules_and_categories_fixtures_satisfy_scenario_invariants() {
        let mut page = RulesPage::new();
        let RulesEffect::LoadPage {
            tree_generation,
            categories_generation,
        } = page.open()
        else {
            panic!("open emits LoadPage");
        };
        page.apply_categories_loaded(categories_generation, Ok(fixture_categories_response()));
        let _ = page.apply_tree_loaded(tree_generation, Ok(fixture_rule_tree_responses()));
        let RulesEffect::LoadDetail { generation, .. } =
            page.select(PREVIEW_USER_SET_KEY.to_owned())
        else {
            panic!("select emits LoadDetail");
        };
        page.apply_detail_loaded(generation, Ok(fixture_user_set_detail_response()));

        let tree = page.tree().expect("tree loaded");
        let builtin = tree
            .find("builtin.classification")
            .expect("builtin root present");
        assert!(
            builtin.override_marker.is_overridden(),
            "builtin classification set displays override marker"
        );
        let disabled = tree
            .find("user.weekend-mode")
            .expect("disabled user set present");
        assert_eq!(disabled.enablement, NodeEnablement::Disabled);
        assert_eq!(page.selected_key(), Some(PREVIEW_USER_SET_KEY));
        let LoadState::Ready(detail) = page.detail_state() else {
            panic!("selected user set detail loaded");
        };
        assert!(!detail.entries.is_empty());

        let catalog = page.categories().expect("categories loaded");
        assert_eq!(catalog.archived_items().len(), 1);
        page.toggle_archived_visibility();
        assert_eq!(page.visible_archived_categories().len(), 1);
        page.begin_create_category(parse_uuid(CAT_NEW_UUID));
        assert!(page.category_editor().is_some());
    }

    #[test]
    fn m4_rule_editor_import_and_unclassified_fixtures_satisfy_invariants() {
        let mut page = RulesPage::new();
        let RulesEffect::LoadPage {
            tree_generation, ..
        } = page.open()
        else {
            panic!("open");
        };
        let _ = page.apply_tree_loaded(tree_generation, Ok(fixture_rule_tree_responses()));

        // Rule editor with 1 diagnostic.
        let mut invalid_editor = RuleEditor::new();
        let RuleEditorEffect::ExportText { generation, .. } = invalid_editor.open_edit(
            PREVIEW_USER_SET_KEY.to_owned(),
            wire::RuleKind::Classification,
            PREVIEW_USER_SET_VERSION,
        ) else {
            panic!("open_edit");
        };
        let RuleEditorEffect::Validate {
            generation: val_gen,
            ..
        } = invalid_editor.apply_export_loaded(
            generation,
            Ok(wire::RulesTextExportResponse {
                text: fixture_rule_editor_invalid_text(),
                versions: wire::QueryVersions(PREVIEW_RULES_VERSION.to_owned()),
            }),
        )
        else {
            panic!("export");
        };
        invalid_editor.apply_validation_finished(
            val_gen,
            Ok(wire::RulesTextValidateResponse {
                valid: false,
                diagnostics: vec![wire::RuleDiagnostic {
                    code: "E-RULE-FIELD".to_owned(),
                    path: "rule[0].when[0].field".to_owned(),
                    location: Some(wire::RuleDiagnosticLocation {
                        line: 14,
                        column: 13,
                    }),
                    params: BTreeMap::new(),
                }],
                truncated: false,
            }),
        );
        assert!(matches!(
            invalid_editor.session().expect("session").validation,
            ValidationState::Invalid {
                ref diagnostics, ..
            } if diagnostics.len() == 1
        ));

        // Rule editor with clean validate + preview results incl. hidden-title sample.
        let mut preview_editor = RuleEditor::new();
        let RuleEditorEffect::ExportText { generation, .. } = preview_editor.open_edit(
            PREVIEW_USER_SET_KEY.to_owned(),
            wire::RuleKind::Classification,
            PREVIEW_USER_SET_VERSION,
        ) else {
            panic!("open_edit");
        };
        let RuleEditorEffect::Validate {
            generation: val_gen,
            ..
        } = preview_editor.apply_export_loaded(
            generation,
            Ok(wire::RulesTextExportResponse {
                text: fixture_rule_editor_valid_text(),
                versions: wire::QueryVersions(PREVIEW_RULES_VERSION.to_owned()),
            }),
        )
        else {
            panic!("export");
        };
        preview_editor.apply_validation_finished(
            val_gen,
            Ok(wire::RulesTextValidateResponse {
                valid: true,
                diagnostics: Vec::new(),
                truncated: false,
            }),
        );
        let RuleEditorEffect::Preview {
            buffer_generation,
            preview_generation,
            ..
        } = preview_editor.request_preview(sample_day_date())
        else {
            panic!("request_preview");
        };
        preview_editor.apply_preview_finished(
            buffer_generation,
            preview_generation,
            Ok(fixture_rule_preview_response(sample_day_start_utc_ms())),
        );
        let RulePreviewState::Ready(preview) = &preview_editor.session().expect("session").preview
        else {
            panic!("preview ready");
        };
        assert!(!preview.rules.is_empty());
        assert!(!preview.totals.before.categories.is_empty());
        assert!(!preview.totals.after.categories.is_empty());
        assert!(preview.samples.iter().any(|sample| matches!(
            &sample.after,
            wire::RulePreviewOutput::Extraction { extraction }
                if extraction == &wire::TitleExtraction::default()
        )));

        // Paste import targeting replace existing.
        let mut import = RuleImport::new();
        import.open();
        let _ = import.set_text(fixture_rule_editor_valid_text(), page.tree());
        assert!(matches!(
            import.session().expect("import session").target,
            ImportTarget::Resolved(ref plan)
                if plan.key == PREVIEW_USER_SET_KEY
                    && plan.action
                        == ImportAction::Replace {
                            base_version: PREVIEW_USER_SET_VERSION
                        }
        ));

        // Unclassified queue: app grouping, 5 rows incl. 1 hidden, truncated false.
        let mut queue = UnclassifiedQueue::new();
        let UnclassifiedEffect::Query {
            range, generation, ..
        } = queue.open(sample_day_date())
        else {
            panic!("open unclassified");
        };
        queue.apply_loaded(generation, range, Ok(fixture_unclassified_response()));
        assert_eq!(queue.group_by(), wire::UnclassifiedGroupBy::App);
        let LoadState::Ready(list) = queue.state() else {
            panic!("unclassified ready");
        };
        assert_eq!(list.items.len(), 5);
        assert!(!list.truncated);
        assert!(
            list.items
                .iter()
                .any(|row| row.label == UnclassifiedEntityLabel::HiddenApp)
        );

        // Progress preview: unclassified tab loading with 120/365 progress.
        let mut loading_queue = UnclassifiedQueue::new();
        let UnclassifiedEffect::Query { generation, .. } = loading_queue.open(sample_day_date())
        else {
            panic!("open unclassified for progress preview");
        };
        assert_eq!(
            loading_queue.apply_progress(
                generation,
                QueryProgress {
                    done: PREVIEW_PROGRESS_DONE,
                    total: PREVIEW_PROGRESS_TOTAL,
                },
            ),
            ProgressUpdate::Applied
        );
        assert!(matches!(loading_queue.state(), LoadState::Loading));
        assert_eq!(
            loading_queue.progress(),
            LoadProgress::Counting {
                done: PREVIEW_PROGRESS_DONE,
                total: PREVIEW_PROGRESS_TOTAL,
            }
        );
    }

    #[test]
    fn m4_today_annotations_editor_and_timeline_fixtures_satisfy_invariants() {
        let day = sample_live_day();
        let ctx = day.live_time_context().expect("live day time context");
        let segment = day
            .segment(CLASSIFY_SEGMENT_ID)
            .expect("classify segment present");
        assert_eq!(
            segment.span,
            TimeSpan::new(TimeOfDay::hm(13, 15), TimeOfDay::hm(14, 0))
        );
        let _ = fixture_selected_segment_details(&day);

        let catalog =
            CategoryCatalog::from_protocol(fixture_categories_response()).expect("catalog");
        let mut annotations = AnnotationEditor::new();
        let _ = annotations.set_day_context(Some(ctx.clone()));
        let AnnotationEditorEffect::Query {
            date, generation, ..
        } = annotations.open_panel()
        else {
            panic!("open annotations");
        };
        annotations.apply_loaded(
            generation,
            date,
            Ok(fixture_annotations_response(ctx.bounds.start)),
        );
        let list = annotations.list().expect("annotations list ready");
        assert_eq!(list.items.len(), 3);
        assert!(list.items.iter().any(|row| {
            row.category.as_ref().is_some_and(|cat| {
                resolve_annotation_category(cat, Some(&catalog))
                    == AnnotationCategoryDisplay::Missing
            })
        }));

        annotations.open_create(
            UtcRange {
                start: ctx.bounds.start + 14 * HOUR_MS,
                end: ctx.bounds.start + 14 * HOUR_MS + 45 * MINUTE_MS,
            },
            Some(parse_uuid(CAT_DEV_UUID)),
        );
        annotations.set_end_input("13:30".to_owned());
        assert_eq!(annotations.submit(|| None), AnnotationEditorEffect::None);
        assert_eq!(
            annotations.draft().and_then(|draft| draft.validation_error),
            Some(ValidateAnnotationDraftError::Time {
                source: ValidateAnnotationTimeError::EndNotAfterStart,
            })
        );
    }

    #[test]
    fn week_preview_fixtures_include_zero_activity_day_future_day_and_loading_progress() {
        use crate::model::{WEEK_DAY_COUNT, WeekDayKind, WeekLoadState, WeekPage};

        let catalog =
            CategoryCatalog::from_protocol(fixture_categories_response()).expect("catalog");
        let week_start =
            crate::model::week::week_start_for(sample_day_date(), jiff::civil::Weekday::Monday)
                .expect("week start");
        let today = week_start
            .checked_add(jiff::ToSpan::days(PREVIEW_WEEK_TODAY_OFFSET))
            .expect("preview today");

        let mut week = WeekPage::new();
        let WeekEffect::Query {
            week_start: query_start,
            generation,
            ..
        } = week.open(today, jiff::civil::Weekday::Monday)
        else {
            panic!("opening week emits query");
        };
        assert_eq!(
            week.apply_loaded(
                generation,
                query_start,
                today,
                Ok(fixture_week_range_response(query_start)),
                Some(&catalog),
            ),
            WeekEffect::None
        );
        let WeekLoadState::Ready(data) = week.state() else {
            panic!("week fixture ready");
        };
        assert_eq!(data.days.len(), WEEK_DAY_COUNT);
        let zero_day = data.days.get(2).expect("zero-activity day");
        assert_eq!(zero_day.kind, WeekDayKind::Observed);
        assert!(zero_day.active.is_zero());
        assert_eq!(zero_day.focus_share, None);
        assert_eq!(zero_day.distraction_share, None);

        let future_day = data.days.get(6).expect("future day");
        assert_eq!(future_day.kind, WeekDayKind::Future);
        assert!(future_day.active.is_zero());
        assert_eq!(future_day.focus_share, None);

        let mut loading = WeekPage::new();
        let WeekEffect::Query { generation, .. } =
            loading.open(today, jiff::civil::Weekday::Monday)
        else {
            panic!("opening loading week emits query");
        };
        assert_eq!(
            loading.apply_progress(
                generation,
                QueryProgress {
                    done: PREVIEW_WEEK_PROGRESS_DONE,
                    total: PREVIEW_WEEK_PROGRESS_TOTAL,
                },
            ),
            ProgressUpdate::Applied
        );
        assert!(matches!(loading.state(), WeekLoadState::Loading));
        assert_eq!(
            loading.progress(),
            LoadProgress::Counting {
                done: PREVIEW_WEEK_PROGRESS_DONE,
                total: PREVIEW_WEEK_PROGRESS_TOTAL,
            }
        );
    }

    #[test]
    fn settings_data_delete_confirm_and_about_fixtures_satisfy_invariants() {
        use crate::model::{
            CoreStatusLoadState, DataInfoLoadState, DeleteRangeConfirm, SettingsEffect,
            SettingsPage,
        };

        let mut page = SettingsPage::new();
        let SettingsEffect::LoadDataInfo { generation } = page.load_data_info() else {
            panic!("load_data_info emits LoadDataInfo");
        };
        page.apply_data_info_loaded(generation, Ok(fixture_data_info_response()));
        let DataInfoLoadState::Ready(info) = page.data_info() else {
            panic!("fixture data info ready");
        };
        assert_eq!(info.backups.len(), 3);
        assert!(!info.logs_path.is_empty());

        let today = sample_day_date();
        page.set_delete_start(jiff::civil::date(2026, 11, 10), today);
        page.set_delete_end(jiff::civil::date(2026, 11, 14), today);
        page.request_delete_range(today);
        assert!(matches!(
            page.delete_confirm(),
            DeleteRangeConfirm::Pending(range) if range.days == 5
        ));

        let SettingsEffect::LoadCoreStatus { generation } = page.load_core_status() else {
            panic!("load_core_status emits LoadCoreStatus");
        };
        page.apply_core_status_loaded(generation, Ok(fixture_core_status_response()));
        let CoreStatusLoadState::Ready(status) = page.core_status() else {
            panic!("fixture core status ready");
        };
        assert!(status.recording);
        assert_eq!(status.core_version.0, env!("CARGO_PKG_VERSION"));

        let SettingsEffect::LoadBrowserHosts { generation } = page.load_browser_hosts() else {
            panic!("load_browser_hosts emits LoadBrowserHosts");
        };
        page.apply_browser_hosts_loaded(generation, Ok(fixture_browser_hosts_response()));
        let crate::model::BrowserHostsLoadState::Ready(hosts) = page.browser_hosts() else {
            panic!("fixture browser hosts ready");
        };
        let browsers: Vec<_> = hosts.iter().map(|host| host.browser).collect();
        assert_eq!(
            browsers,
            [
                wire::BrowserKind::Chrome,
                wire::BrowserKind::Edge,
                wire::BrowserKind::Firefox
            ]
        );
    }

    #[test]
    fn today_websites_preview_fixture_satisfies_invariants() {
        use crate::model::RankLabel;
        assert!(sample_live_day().websites().is_empty());
        let day = sample_live_day_with_websites(fixture_preview_websites());
        let rows = day.websites();
        assert_eq!(rows.len(), 6);
        assert!(
            rows.iter()
                .any(|row| row.label == RankLabel::Visible("xn--mnchen-3ya.de".to_owned()))
        );
        assert!(
            rows.iter()
                .any(|row| row.label == RankLabel::BrowserInternal)
        );
        assert!(rows.iter().any(|row| row.label == RankLabel::LocalFile));
    }
}
