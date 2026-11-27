//! Settings page: left-aligned single column of group cards in order General, Recording, Data,
//! and About (07 section 7, 03 section 6).

use std::path::Path;

use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Div, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window,
    assets::IconName,
    component::{
        Disableable as _, Icon, IndexPath, Sizable as _,
        select::{Select, SelectEvent, SelectItem, SelectState},
        spinner::Spinner,
        switch::Switch,
    },
    div,
    prelude::FluentBuilder as _,
    px,
};
use quartz_protocol as wire;

use super::{
    AppShell, Frame, InputSyncState, Page,
    calendar::{CalendarMonth, CalendarPopover, CalendarTarget, MonthDirection},
    live::stop_reason_msg,
};
use crate::{
    app::{
        icons::AppAssets,
        metrics::{Density, Ramp},
        open_folder::{open_folder, parent_directory},
        tokens::ContrastMode,
        widgets::{ButtonContent, InfoBarStyle, RampExt as _, SkeletonTone, Widgets},
    },
    format::{Arg, Formatter, Msg},
    layout::WindowClass,
    link::{LoadDataError, LoadDataErrorCode, RequestCoreError},
    model::{
        CompactNotice, CoreStatusLoadState, DataInfoLoadState, DataInfoView, DataOperation,
        DeleteNotice, DeleteRangeConfirm, GeneralSettingChange, MAX_DELETE_RANGE_DAYS,
        RetentionConfirm, RowVisibility, SaveActivity, SettingsEffect, SettingsLoadState,
        SettingsValues, ValidateDeleteRangeError, max_deletable_date,
    },
};

/// Maximum width of the left-aligned settings column in logical pixels.
const SETTINGS_COLUMN_MAX_WIDTH: f32 = 720.;
/// Width of dropdown select controls on settings rows in logical pixels.
const SETTINGS_SELECT_WIDTH: f32 = 168.;
/// Minimum width of each date-range picker button in logical pixels.
const SETTINGS_DATE_FIELD_MIN_WIDTH: f32 = 152.;
/// Horizontal offset of the Settings date-range calendar popover relative to its field container.
const SETTINGS_CALENDAR_LEFT: f32 = 0.;
/// Diameter of the inline indeterminate busy spinner inside action buttons.
const BUSY_SPINNER_SIZE: f32 = 14.;
/// Diameter of the recording status dot in the About group.
const STATUS_DOT_SIZE: f32 = 6.;
/// Number of placeholder rows shown while initial `settings.get` is in flight.
const SKELETON_ROW_COUNT: usize = 5;
/// Number of placeholder rows shown while initial `data.info` is in flight.
const DATA_SKELETON_ROW_COUNT: usize = 4;
/// Static loading skeleton dimensions in logical pixels.
const SKELETON_LABEL_WIDTH: f32 = 140.;
const SKELETON_DESC_WIDTH: f32 = 240.;
const SKELETON_CONTROL_WIDTH: f32 = 112.;
const SKELETON_LABEL_HEIGHT: f32 = 14.;
const SKELETON_DESC_HEIGHT: f32 = 12.;
const SKELETON_CONTROL_HEIGHT: f32 = 24.;

/// Typed option item for gpui-component [`Select`] controls on the Settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SelectOption<V> {
    pub(super) value: V,
    pub(super) label: SharedString,
}

impl<V: Clone + PartialEq> SelectItem for SelectOption<V> {
    type Value = V;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

/// Which endpoint of the Data group's date-range deletion picker currently owns the calendar flyout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeleteDateField {
    Start,
    End,
}

impl DeleteDateField {
    const fn calendar_target(self) -> CalendarTarget {
        match self {
            Self::Start => CalendarTarget::SettingsDeleteStart,
            Self::End => CalendarTarget::SettingsDeleteEnd,
        }
    }
}

/// Open calendar popover state for the Settings date-range deletion pickers.
#[derive(Debug, Clone, Copy)]
pub(super) struct SettingsCalendar {
    pub(super) field: DeleteDateField,
    pub(super) month: CalendarMonth,
}

#[derive(Debug)]
struct DeleteDateFieldSpec {
    field: DeleteDateField,
    label: String,
    selected: Option<wire::LocalDate>,
    enabled: bool,
    today: wire::LocalDate,
    yesterday: Option<wire::LocalDate>,
    first_weekday: Option<jiff::civil::Weekday>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowIndent {
    TopLevel,
    Nested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowDivider {
    None,
    Top,
}

fn language_options(formatter: Formatter) -> Vec<SelectOption<wire::UiLanguageSetting>> {
    vec![
        SelectOption {
            value: wire::UiLanguageSetting::Auto,
            label: formatter.text(Msg::SettingsLanguageAuto).into(),
        },
        SelectOption {
            value: wire::UiLanguageSetting::ZhCn,
            label: formatter.text(Msg::SettingsLanguageZhCn).into(),
        },
        SelectOption {
            value: wire::UiLanguageSetting::En,
            label: formatter.text(Msg::SettingsLanguageEn).into(),
        },
    ]
}

fn hour_cycle_options(formatter: Formatter) -> Vec<SelectOption<wire::HourCycleSetting>> {
    vec![
        SelectOption {
            value: wire::HourCycleSetting::Auto,
            label: formatter.text(Msg::SettingsHourCycleAuto).into(),
        },
        SelectOption {
            value: wire::HourCycleSetting::H12,
            label: formatter.text(Msg::SettingsHourCycleH12).into(),
        },
        SelectOption {
            value: wire::HourCycleSetting::H24,
            label: formatter.text(Msg::SettingsHourCycleH24).into(),
        },
    ]
}

fn first_weekday_options(formatter: Formatter) -> Vec<SelectOption<wire::FirstDayOfWeekSetting>> {
    vec![
        SelectOption {
            value: wire::FirstDayOfWeekSetting::Auto,
            label: formatter.text(Msg::SettingsFirstWeekdayAuto).into(),
        },
        SelectOption {
            value: wire::FirstDayOfWeekSetting::Monday,
            label: formatter.text(Msg::SettingsFirstWeekdayMonday).into(),
        },
        SelectOption {
            value: wire::FirstDayOfWeekSetting::Tuesday,
            label: formatter.text(Msg::SettingsFirstWeekdayTuesday).into(),
        },
        SelectOption {
            value: wire::FirstDayOfWeekSetting::Wednesday,
            label: formatter.text(Msg::SettingsFirstWeekdayWednesday).into(),
        },
        SelectOption {
            value: wire::FirstDayOfWeekSetting::Thursday,
            label: formatter.text(Msg::SettingsFirstWeekdayThursday).into(),
        },
        SelectOption {
            value: wire::FirstDayOfWeekSetting::Friday,
            label: formatter.text(Msg::SettingsFirstWeekdayFriday).into(),
        },
        SelectOption {
            value: wire::FirstDayOfWeekSetting::Saturday,
            label: formatter.text(Msg::SettingsFirstWeekdaySaturday).into(),
        },
        SelectOption {
            value: wire::FirstDayOfWeekSetting::Sunday,
            label: formatter.text(Msg::SettingsFirstWeekdaySunday).into(),
        },
    ]
}

fn title_retention_label(formatter: Formatter, retention: wire::TitleRetention) -> String {
    formatter.text(match retention {
        wire::TitleRetention::Forever => Msg::SettingsTitleRetentionForever,
        wire::TitleRetention::Days30 => Msg::SettingsTitleRetention30d,
        wire::TitleRetention::Days90 => Msg::SettingsTitleRetention90d,
        wire::TitleRetention::Days365 => Msg::SettingsTitleRetention365d,
    })
}

fn title_retention_options(formatter: Formatter) -> Vec<SelectOption<wire::TitleRetention>> {
    [
        wire::TitleRetention::Forever,
        wire::TitleRetention::Days30,
        wire::TitleRetention::Days90,
        wire::TitleRetention::Days365,
    ]
    .into_iter()
    .map(|value| SelectOption {
        value,
        label: title_retention_label(formatter, value).into(),
    })
    .collect()
}

fn option_index<V: PartialEq>(options: &[SelectOption<V>], value: &V) -> Option<IndexPath> {
    options
        .iter()
        .position(|option| option.value == *value)
        .map(|row| IndexPath::default().row(row))
}

impl AppShell {
    /// Loads settings, data info, and core status whenever the Settings page opens.
    pub(super) fn open_settings_page(&mut self, cx: &mut Context<Self>) {
        self.settings_calendar = None;
        let settings_effect = self.settings.open();
        let data_effect = self.settings.load_data_info();
        let status_effect = self.settings.load_core_status();
        self.run_settings_effect(settings_effect, cx);
        self.run_settings_effect(data_effect, cx);
        self.run_settings_effect(status_effect, cx);
    }

    fn retry_settings(&mut self, cx: &mut Context<Self>) {
        let effect = self.settings.open();
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn retry_data_info(&mut self, cx: &mut Context<Self>) {
        let effect = self.settings.load_data_info();
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn retry_core_status(&mut self, cx: &mut Context<Self>) {
        let effect = self.settings.load_core_status();
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn change_general_setting(&mut self, change: GeneralSettingChange, cx: &mut Context<Self>) {
        let effect = self.settings.change_general(change);
        self.settings_selects_sync = InputSyncState::NeedsSync;
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn select_title_retention(&mut self, target: wire::TitleRetention, cx: &mut Context<Self>) {
        let effect = self.settings.select_title_retention(target);
        self.settings_selects_sync = InputSyncState::NeedsSync;
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn confirm_title_retention(&mut self, cx: &mut Context<Self>) {
        let effect = self.settings.confirm_title_retention();
        self.settings_selects_sync = InputSyncState::NeedsSync;
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn cancel_title_retention(&mut self, cx: &mut Context<Self>) {
        self.settings.cancel_title_retention();
        self.settings_selects_sync = InputSyncState::NeedsSync;
        cx.notify();
    }

    fn start_manual_backup(&mut self, cx: &mut Context<Self>) {
        let effect = self.settings.start_backup();
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn start_database_compact(&mut self, cx: &mut Context<Self>) {
        let effect = self.settings.start_compact();
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn toggle_settings_calendar(&mut self, field: DeleteDateField, cx: &mut Context<Self>) {
        if self.effective_first_weekday().is_none() || self.settings.data_operation().is_busy() {
            return;
        }
        let Some(yesterday) = max_deletable_date(self.current_logical_today()) else {
            return;
        };
        if self
            .settings_calendar
            .is_some_and(|open| open.field == field)
        {
            self.settings_calendar = None;
            cx.notify();
            return;
        }
        let anchor = match field {
            DeleteDateField::Start => self
                .settings
                .delete_start()
                .or_else(|| self.settings.delete_end())
                .unwrap_or(yesterday),
            DeleteDateField::End => self
                .settings
                .delete_end()
                .or_else(|| self.settings.delete_start())
                .unwrap_or(yesterday),
        }
        .min(yesterday);
        self.settings_calendar = Some(SettingsCalendar {
            field,
            month: CalendarMonth::new(anchor),
        });
        cx.notify();
    }

    pub(super) fn move_settings_calendar(
        &mut self,
        direction: MonthDirection,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.settings_calendar else {
            return;
        };
        let Some(yesterday) = max_deletable_date(self.current_logical_today()) else {
            return;
        };
        if let Some(next_month) = open.month.step(direction, yesterday) {
            self.settings_calendar = Some(SettingsCalendar {
                field: open.field,
                month: next_month,
            });
            cx.notify();
        }
    }

    pub(super) fn select_settings_delete_date(
        &mut self,
        field: DeleteDateField,
        date: jiff::civil::Date,
        cx: &mut Context<Self>,
    ) {
        let today = self.current_logical_today();
        match field {
            DeleteDateField::Start => self.settings.set_delete_start(date, today),
            DeleteDateField::End => self.settings.set_delete_end(date, today),
        }
        self.settings_calendar = None;
        cx.notify();
    }

    fn request_delete_range(&mut self, cx: &mut Context<Self>) {
        self.settings_calendar = None;
        let today = self.current_logical_today();
        self.settings.request_delete_range(today);
        cx.notify();
    }

    fn cancel_delete_range(&mut self, cx: &mut Context<Self>) {
        self.settings.cancel_delete_range();
        cx.notify();
    }

    fn confirm_delete_range(&mut self, cx: &mut Context<Self>) {
        let today = self.current_logical_today();
        let effect = self.settings.confirm_delete_range(today);
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn run_settings_effect(&mut self, effect: SettingsEffect, cx: &mut Context<Self>) {
        match effect {
            SettingsEffect::None => {}
            SettingsEffect::LoadSettings { generation } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.settings.apply_loaded(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    self.settings_selects_sync = InputSyncState::NeedsSync;
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.get_settings().await;
                    let _ = this.update(cx, |this, cx| {
                        this.settings.apply_loaded(generation, result);
                        this.settings_selects_sync = InputSyncState::NeedsSync;
                        cx.notify();
                    });
                })
                .detach();
            }
            SettingsEffect::SaveSettings {
                request,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.settings.apply_saved(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    self.settings_selects_sync = InputSyncState::NeedsSync;
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.set_settings(request).await;
                    let _ = this.update(cx, |this, cx| {
                        this.settings.apply_saved(generation, result);
                        this.settings_selects_sync = InputSyncState::NeedsSync;
                        cx.notify();
                    });
                })
                .detach();
            }
            SettingsEffect::LoadDataInfo { generation } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.settings.apply_data_info_loaded(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.get_data_info().await;
                    let _ = this.update(cx, |this, cx| {
                        this.settings.apply_data_info_loaded(generation, result);
                        cx.notify();
                    });
                })
                .detach();
            }
            SettingsEffect::LoadCoreStatus { generation } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.settings.apply_core_status_loaded(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.get_core_status().await;
                    let _ = this.update(cx, |this, cx| {
                        this.settings.apply_core_status_loaded(generation, result);
                        cx.notify();
                    });
                })
                .detach();
            }
            SettingsEffect::Backup { generation } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let follow_up = self.settings.apply_backup_finished(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    self.run_settings_effect(follow_up, cx);
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.create_backup().await;
                    let _ = this.update(cx, |this, cx| {
                        let follow_up = this.settings.apply_backup_finished(generation, result);
                        this.run_settings_effect(follow_up, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            SettingsEffect::Compact { generation } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let follow_up = self.settings.apply_compact_finished(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    self.run_settings_effect(follow_up, cx);
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.compact_database().await;
                    let _ = this.update(cx, |this, cx| {
                        let follow_up = this.settings.apply_compact_finished(generation, result);
                        this.run_settings_effect(follow_up, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            SettingsEffect::DeleteRange {
                request,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let follow_up = self.settings.apply_delete_finished(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    self.run_settings_effect(follow_up, cx);
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.delete_data_range(request).await;
                    let _ = this.update(cx, |this, cx| {
                        let follow_up = this.settings.apply_delete_finished(generation, result);
                        this.run_settings_effect(follow_up, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// Lazily initializes and synchronizes the Settings dropdown `SelectState` entities from model
    /// state and current UI language.
    pub(super) fn sync_settings_selects_if_needed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Settings {
            return;
        }
        let Some(values) = self.settings.values().copied() else {
            return;
        };
        let needs_sync = self.settings_selects_sync == InputSyncState::NeedsSync;

        let language_items = language_options(self.formatter);
        match &self.settings_language_select {
            None => {
                let selected = option_index(&language_items, &values.ui.language);
                let state = cx.new(|cx| SelectState::new(language_items, selected, window, cx));
                cx.subscribe(
                    &state,
                    |this,
                     _,
                     event: &SelectEvent<Vec<SelectOption<wire::UiLanguageSetting>>>,
                     cx| {
                        if let SelectEvent::Confirm(Some(value)) = event {
                            this.change_general_setting(GeneralSettingChange::Language(*value), cx);
                        }
                    },
                )
                .detach();
                self.settings_language_select = Some(state);
            }
            Some(state) if needs_sync => {
                let selected = values.ui.language;
                state.update(cx, |state, cx| {
                    state.set_items(language_items, window, cx);
                    state.set_selected_value(&selected, window, cx);
                });
            }
            Some(_) => {}
        }

        let hour_cycle_items = hour_cycle_options(self.formatter);
        match &self.settings_hour_cycle_select {
            None => {
                let selected = option_index(&hour_cycle_items, &values.ui.hour_cycle);
                let state = cx.new(|cx| SelectState::new(hour_cycle_items, selected, window, cx));
                cx.subscribe(
                    &state,
                    |this,
                     _,
                     event: &SelectEvent<Vec<SelectOption<wire::HourCycleSetting>>>,
                     cx| {
                        if let SelectEvent::Confirm(Some(value)) = event {
                            this.change_general_setting(
                                GeneralSettingChange::HourCycle(*value),
                                cx,
                            );
                        }
                    },
                )
                .detach();
                self.settings_hour_cycle_select = Some(state);
            }
            Some(state) if needs_sync => {
                let selected = values.ui.hour_cycle;
                state.update(cx, |state, cx| {
                    state.set_items(hour_cycle_items, window, cx);
                    state.set_selected_value(&selected, window, cx);
                });
            }
            Some(_) => {}
        }

        let first_weekday_items = first_weekday_options(self.formatter);
        match &self.settings_first_weekday_select {
            None => {
                let selected = option_index(&first_weekday_items, &values.ui.first_day_of_week);
                let state =
                    cx.new(|cx| SelectState::new(first_weekday_items, selected, window, cx));
                cx.subscribe(
                    &state,
                    |this,
                     _,
                     event: &SelectEvent<Vec<SelectOption<wire::FirstDayOfWeekSetting>>>,
                     cx| {
                        if let SelectEvent::Confirm(Some(value)) = event {
                            this.change_general_setting(
                                GeneralSettingChange::FirstDayOfWeek(*value),
                                cx,
                            );
                        }
                    },
                )
                .detach();
                self.settings_first_weekday_select = Some(state);
            }
            Some(state) if needs_sync => {
                let selected = values.ui.first_day_of_week;
                state.update(cx, |state, cx| {
                    state.set_items(first_weekday_items, window, cx);
                    state.set_selected_value(&selected, window, cx);
                });
            }
            Some(_) => {}
        }

        let displayed_retention = self
            .settings
            .displayed_title_retention()
            .unwrap_or(values.title_retention);
        let retention_items = title_retention_options(self.formatter);
        match &self.settings_retention_select {
            None => {
                let selected = option_index(&retention_items, &displayed_retention);
                let state = cx.new(|cx| SelectState::new(retention_items, selected, window, cx));
                cx.subscribe(
                    &state,
                    |this, _, event: &SelectEvent<Vec<SelectOption<wire::TitleRetention>>>, cx| {
                        if let SelectEvent::Confirm(Some(value)) = event {
                            this.select_title_retention(*value, cx);
                        }
                    },
                )
                .detach();
                self.settings_retention_select = Some(state);
            }
            Some(state) if needs_sync => {
                state.update(cx, |state, cx| {
                    state.set_items(retention_items, window, cx);
                    state.set_selected_value(&displayed_retention, window, cx);
                });
            }
            Some(_) => {}
        }

        self.settings_selects_sync = InputSyncState::Synced;
    }

    fn backup_time_zone(&self) -> Option<jiff::tz::TimeZone> {
        #[cfg(feature = "preview")]
        if self.preview.is_some() {
            return Some(jiff::tz::TimeZone::UTC);
        }
        jiff::tz::TimeZone::try_system().ok()
    }

    /// Renders the Settings page as a left-aligned single column of group cards in order:
    /// General, Recording, Data, About.
    pub(super) fn render_settings(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let compact = frame.layout.class == WindowClass::Compact;

        let general_card: AnyElement = match self.settings.state() {
            SettingsLoadState::Loading => {
                self.render_settings_loading_card(ui, Msg::SettingsGroupGeneral, SKELETON_ROW_COUNT)
            }
            SettingsLoadState::Failed(error) => self.render_settings_failed_card(ui, error, cx),
            SettingsLoadState::Ready(values) => self.render_general_card(ui, values, cx),
        };

        let recording_card: Option<AnyElement> = match self.settings.state() {
            SettingsLoadState::Loading => {
                Some(self.render_settings_loading_card(ui, Msg::SettingsGroupRecording, 1))
            }
            SettingsLoadState::Failed(_) => None,
            SettingsLoadState::Ready(_) => Some(self.render_recording_card(ui, cx)),
        };

        let data_card = self.render_data_card(ui, cx);
        let about_card = self.render_about_card(ui, cx);

        #[cfg(feature = "preview")]
        let cards: Vec<AnyElement> = match self.preview.as_ref().map(|p| p.settings_section) {
            Some(super::preview::SettingsPreviewSection::Data) => vec![data_card, about_card],
            Some(super::preview::SettingsPreviewSection::About) => vec![about_card],
            Some(super::preview::SettingsPreviewSection::All) | None => {
                let mut all = vec![general_card];
                if let Some(recording) = recording_card {
                    all.push(recording);
                }
                all.push(data_card);
                all.push(about_card);
                all
            }
        };

        #[cfg(not(feature = "preview"))]
        let cards: Vec<AnyElement> = {
            let mut all = vec![general_card];
            if let Some(recording) = recording_card {
                all.push(recording);
            }
            all.push(data_card);
            all.push(about_card);
            all
        };

        let content = div()
            .flex()
            .flex_col()
            .gap(space.l)
            .w_full()
            .max_w(px(SETTINGS_COLUMN_MAX_WIDTH))
            .px(if compact { space.l } else { space.xxl })
            .pt(if compact { space.l } else { space.xxxl })
            .pb(space.xxl)
            .child(
                div()
                    .ramp(ui, Ramp::Title)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(Msg::NavSettings)),
            )
            .children(cards);

        div()
            .id("settings-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(content)
    }

    fn render_general_card(
        &self,
        ui: &Widgets<'_>,
        values: &SettingsValues,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let save_activity = self.settings.save_activity();
        let saving = save_activity == SaveActivity::Saving;

        let autostart_label = self.formatter.text(Msg::SettingsAutostartLabel);
        let autostart_switch = Switch::new("settings-autostart")
            .checked(values.autostart == wire::AutostartEnabled::Enabled)
            .disabled(saving)
            .accessibility_label(autostart_label.clone())
            .on_change(cx.listener(|this, &checked, _, cx| {
                this.change_general_setting(
                    GeneralSettingChange::Autostart(wire::AutostartEnabled::from(checked)),
                    cx,
                );
            }));

        let resident_label = self.formatter.text(Msg::SettingsResidentLabel);
        let resident_switch = Switch::new("settings-resident")
            .checked(values.ui.resident.enabled == wire::ResidentEnabled::Enabled)
            .disabled(saving)
            .accessibility_label(resident_label.clone())
            .on_change(cx.listener(|this, &checked, _, cx| {
                this.change_general_setting(
                    GeneralSettingChange::Resident(wire::ResidentEnabled::from(checked)),
                    cx,
                );
            }));

        let prewarm_row = (values.prewarm_visibility() == RowVisibility::Visible).then(|| {
            let label = self.formatter.text(Msg::SettingsPrewarmLabel);
            let control = Switch::new("settings-prewarm")
                .checked(values.ui.prewarm.enabled == wire::PrewarmEnabled::Enabled)
                .disabled(saving)
                .accessibility_label(label.clone())
                .on_change(cx.listener(|this, &checked, _, cx| {
                    this.change_general_setting(
                        GeneralSettingChange::PrewarmEnabled(wire::PrewarmEnabled::from(checked)),
                        cx,
                    );
                }));
            self.render_setting_row(
                ui,
                label,
                self.formatter.text(Msg::SettingsPrewarmDescription),
                control.into_any_element(),
                RowIndent::TopLevel,
                RowDivider::Top,
            )
        });

        let prewarm_battery_row =
            (values.prewarm_on_battery_visibility() == RowVisibility::Visible).then(|| {
                let label = self.formatter.text(Msg::SettingsPrewarmBatteryLabel);
                let control = Switch::new("settings-prewarm-battery")
                    .checked(values.ui.prewarm.on_battery == wire::PrewarmOnBattery::Allowed)
                    .disabled(saving)
                    .accessibility_label(label.clone())
                    .on_change(cx.listener(|this, &checked, _, cx| {
                        this.change_general_setting(
                            GeneralSettingChange::PrewarmOnBattery(wire::PrewarmOnBattery::from(
                                checked,
                            )),
                            cx,
                        );
                    }));
                self.render_setting_row(
                    ui,
                    label,
                    self.formatter.text(Msg::SettingsPrewarmBatteryDescription),
                    control.into_any_element(),
                    RowIndent::Nested,
                    RowDivider::Top,
                )
            });

        let language_label = self.formatter.text(Msg::SettingsLanguageLabel);
        let language_control = self.render_select_control(
            ui,
            "settings-language-select",
            language_label.clone(),
            self.settings_language_select.as_ref(),
            save_activity,
        );

        let hour_cycle_label = self.formatter.text(Msg::SettingsHourCycleLabel);
        let hour_cycle_control = self.render_select_control(
            ui,
            "settings-hour-cycle-select",
            hour_cycle_label.clone(),
            self.settings_hour_cycle_select.as_ref(),
            save_activity,
        );

        let first_weekday_label = self.formatter.text(Msg::SettingsFirstWeekdayLabel);
        let first_weekday_control = self.render_select_control(
            ui,
            "settings-first-weekday-select",
            first_weekday_label.clone(),
            self.settings_first_weekday_select.as_ref(),
            save_activity,
        );

        let inline_error = self
            .settings
            .general_error()
            .map(|error| self.render_inline_error(ui, Msg::SettingsSaveFailed, error));

        ui.card()
            .flex()
            .flex_col()
            .w_full()
            .p(space.l)
            .gap(space.s)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(Msg::SettingsGroupGeneral)),
            )
            .children(inline_error)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .child(self.render_setting_row(
                        ui,
                        autostart_label,
                        self.formatter.text(Msg::SettingsAutostartDescription),
                        autostart_switch.into_any_element(),
                        RowIndent::TopLevel,
                        RowDivider::None,
                    ))
                    .child(self.render_setting_row(
                        ui,
                        resident_label,
                        self.formatter.text(Msg::SettingsResidentDescription),
                        resident_switch.into_any_element(),
                        RowIndent::TopLevel,
                        RowDivider::Top,
                    ))
                    .children(prewarm_row)
                    .children(prewarm_battery_row)
                    .child(self.render_setting_row(
                        ui,
                        language_label,
                        self.formatter.text(Msg::SettingsLanguageDescription),
                        language_control,
                        RowIndent::TopLevel,
                        RowDivider::Top,
                    ))
                    .child(self.render_setting_row(
                        ui,
                        hour_cycle_label,
                        self.formatter.text(Msg::SettingsHourCycleDescription),
                        hour_cycle_control,
                        RowIndent::TopLevel,
                        RowDivider::Top,
                    ))
                    .child(self.render_setting_row(
                        ui,
                        first_weekday_label,
                        self.formatter.text(Msg::SettingsFirstWeekdayDescription),
                        first_weekday_control,
                        RowIndent::TopLevel,
                        RowDivider::Top,
                    )),
            )
            .into_any_element()
    }

    fn render_recording_card(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let save_activity = self.settings.save_activity();
        let saving = save_activity == SaveActivity::Saving;

        let retention_label = self.formatter.text(Msg::SettingsTitleRetentionLabel);
        let retention_control = self.render_select_control(
            ui,
            "settings-title-retention-select",
            retention_label.clone(),
            self.settings_retention_select.as_ref(),
            save_activity,
        );

        let inline_error = self
            .settings
            .recording_error()
            .map(|error| self.render_inline_error(ui, Msg::SettingsSaveFailed, error));

        let confirm_row = match self.settings.retention_confirm() {
            RetentionConfirm::None => None,
            RetentionConfirm::Pending { target } => {
                let colors = ui.info_bar_colors(InfoBarStyle::Critical);
                let target_text = title_retention_label(self.formatter, target);
                let confirm_btn = if saving {
                    ui.disabled_button(
                        "settings-retention-confirm",
                        Density::Compact,
                        ButtonContent::Label(
                            self.formatter
                                .text(Msg::SettingsTitleRetentionConfirmAction),
                        ),
                    )
                } else {
                    self.destructive_button(
                        ui,
                        "settings-retention-confirm",
                        Density::Compact,
                        self.formatter
                            .text(Msg::SettingsTitleRetentionConfirmAction),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_title_retention(cx)))
                };
                let cancel_btn = if saving {
                    ui.disabled_button(
                        "settings-retention-cancel",
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::SettingsCancel)),
                    )
                } else {
                    ui.button(
                        "settings-retention-cancel",
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::SettingsCancel)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_title_retention(cx)))
                };
                Some(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap(space.m)
                        .px(space.m)
                        .py(space.s)
                        .rounded(ui.metrics.radius.control)
                        .bg(colors.background)
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_start()
                                .gap(space.s)
                                .flex_1()
                                .min_w(px(0.))
                                .child(ui.icon(
                                    IconName::TriangleAlert,
                                    Density::Compact,
                                    colors.icon,
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .whitespace_normal()
                                        .ramp(ui, Ramp::Body)
                                        .text_color(colors.foreground)
                                        .child(self.formatter.text_with(
                                            Msg::SettingsTitleRetentionConfirmPrompt,
                                            &[Arg::Value.with(&target_text)],
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(space.s)
                                .flex_shrink_0()
                                .child(confirm_btn)
                                .child(cancel_btn),
                        ),
                )
            }
        };

        ui.card()
            .flex()
            .flex_col()
            .w_full()
            .p(space.l)
            .gap(space.s)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(Msg::SettingsGroupRecording)),
            )
            .children(inline_error)
            .child(self.render_setting_row(
                ui,
                retention_label,
                self.formatter.text(Msg::SettingsTitleRetentionDescription),
                retention_control,
                RowIndent::TopLevel,
                RowDivider::None,
            ))
            .children(confirm_row)
            .into_any_element()
    }

    fn render_data_card(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> AnyElement {
        match self.settings.data_info() {
            DataInfoLoadState::Loading => self.render_settings_loading_card(
                ui,
                Msg::SettingsGroupData,
                DATA_SKELETON_ROW_COUNT,
            ),
            DataInfoLoadState::Failed(error) => self.render_data_failed_card(ui, error, cx),
            DataInfoLoadState::Ready(info) => self.render_data_ready_card(ui, info, cx),
        }
    }

    fn render_data_failed_card(
        &self,
        ui: &Widgets<'_>,
        error: &LoadDataError,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        ui.card()
            .flex()
            .flex_col()
            .w_full()
            .p(space.l)
            .gap(space.m)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(Msg::SettingsGroupData)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.m)
                    .child(ui.icon(
                        IconName::TriangleAlert,
                        Density::Standard,
                        palette.status_caution,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(space.xxs)
                            .child(
                                div()
                                    .ramp(ui, Ramp::BodyStrong)
                                    .text_color(palette.text_primary)
                                    .child(self.formatter.text(Msg::SettingsDataLoadFailedTitle)),
                            )
                            .child(
                                div()
                                    .ramp(ui, Ramp::Body)
                                    .text_color(palette.text_secondary)
                                    .child(self.formatter.text(Msg::SettingsDataLoadFailedMessage)),
                            )
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .font(ui.mono_font())
                                    .text_color(palette.text_tertiary)
                                    .child(error.code().to_string()),
                            ),
                    )
                    .child(
                        ui.button(
                            "retry-data-info",
                            Density::Standard,
                            ButtonContent::Label(self.formatter.text(Msg::SettingsRetry)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.retry_data_info(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_data_ready_card(
        &self,
        ui: &Widgets<'_>,
        info: &DataInfoView,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        ui.card()
            .flex()
            .flex_col()
            .w_full()
            .p(space.l)
            .gap(space.s)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(Msg::SettingsGroupData)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .child(self.render_db_location_row(ui, &info.database_path, cx))
                    .child(self.render_db_size_row(ui, info.database_bytes))
                    .child(self.render_backups_section(ui, &info.backups, cx))
                    .child(self.render_compact_section(ui, cx))
                    .child(self.render_delete_range_section(ui, cx)),
            )
            .into_any_element()
    }

    fn render_db_location_row(
        &self,
        ui: &Widgets<'_>,
        database_path: &str,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let path_for_copy = database_path.to_owned();
        let path_for_open = database_path.to_owned();

        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x(space.l)
            .gap_y(space.xs)
            .py(space.m)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(space.xxs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Body)
                            .text_color(palette.text_primary)
                            .child(self.formatter.text(Msg::SettingsDataDbLocationLabel)),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .font(ui.mono_font())
                            .whitespace_normal()
                            .text_color(palette.text_secondary)
                            .child(database_path.to_owned()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.s)
                    .flex_shrink_0()
                    .child(
                        ui.button(
                            "settings-copy-db-path",
                            Density::Compact,
                            ButtonContent::IconLabel(
                                IconName::Copy,
                                self.formatter.text(Msg::SettingsDataCopyPath),
                            ),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(path_for_copy.clone()));
                        })),
                    )
                    .child(
                        ui.button(
                            "settings-open-db-folder",
                            Density::Standard,
                            ButtonContent::Label(self.formatter.text(Msg::SettingsDataOpenFolder)),
                        )
                        .on_click(cx.listener(move |_, _, _, _| {
                            open_folder(parent_directory(&path_for_open));
                        })),
                    ),
            )
    }

    fn render_db_size_row(&self, ui: &Widgets<'_>, database_bytes: u64) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x(space.l)
            .gap_y(space.xs)
            .py(space.m)
            .border_t_1()
            .border_color(palette.stroke_divider)
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(Msg::SettingsDataDbSizeLabel)),
            )
            .child(
                div()
                    .ramp(ui, Ramp::BodyStrong)
                    .tabular_figures()
                    .text_color(palette.text_primary)
                    .child(self.formatter.bytes(database_bytes)),
            )
    }

    fn render_backups_section(
        &self,
        ui: &Widgets<'_>,
        backups: &[wire::BackupEntry],
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let data_op = self.settings.data_operation();

        let backup_button: AnyElement = match data_op {
            DataOperation::BackingUp => self
                .busy_button(
                    ui,
                    "settings-backup-now",
                    Density::Standard,
                    self.formatter.text(Msg::SettingsDataBackupRunning),
                )
                .into_any_element(),
            DataOperation::Compacting | DataOperation::Deleting => ui
                .disabled_button(
                    "settings-backup-now",
                    Density::Standard,
                    ButtonContent::Label(self.formatter.text(Msg::SettingsDataBackupNow)),
                )
                .into_any_element(),
            DataOperation::Idle => ui
                .button(
                    "settings-backup-now",
                    Density::Standard,
                    ButtonContent::Label(self.formatter.text(Msg::SettingsDataBackupNow)),
                )
                .on_click(cx.listener(|this, _, _, cx| this.start_manual_backup(cx)))
                .into_any_element(),
        };

        let backup_error = self.settings.backup_error().map(|error| {
            let msg = if error.code() == LoadDataErrorCode::Protocol(wire::ErrorKind::BackupExists)
            {
                Msg::SettingsDataBackupExistsError
            } else {
                Msg::SettingsDataBackupFailed
            };
            self.render_inline_error(ui, msg, error)
        });

        let zone = self.backup_time_zone();
        let hour_cycle = self
            .day_snapshot()
            .and_then(|snapshot| snapshot.locale)
            .map(|locale| locale.hour_cycle);

        let list: AnyElement = if backups.is_empty() {
            div()
                .ramp(ui, Ramp::Caption)
                .text_color(palette.text_tertiary)
                .child(self.formatter.text(Msg::SettingsDataBackupEmpty))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .w_full()
                .rounded(ui.metrics.radius.control)
                .bg(palette.fill_card_secondary)
                .border_1()
                .border_color(palette.stroke_card)
                .children(backups.iter().enumerate().map(|(index, entry)| {
                    let kind_text = self.formatter.text(match entry.kind {
                        wire::BackupKind::Manual => Msg::SettingsDataBackupKindManual,
                        wire::BackupKind::Migration => Msg::SettingsDataBackupKindMigration,
                    });
                    let timestamp = zone.as_ref().and_then(|zone| {
                        self.formatter
                            .backup_timestamp(entry.created_utc_ms, zone, hour_cycle)
                    });
                    let mut row = div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_x(space.m)
                        .gap_y(space.xxs)
                        .px(space.m)
                        .py(space.s);
                    if index > 0 {
                        row = row.border_t_1().border_color(palette.stroke_divider);
                    }
                    row.child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .ramp(ui, Ramp::Caption)
                            .font(ui.mono_font())
                            .text_color(palette.text_primary)
                            .child(entry.file_name.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(space.m)
                            .flex_shrink_0()
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .text_color(palette.text_secondary)
                                    .child(kind_text),
                            )
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .tabular_figures()
                                    .text_color(palette.text_secondary)
                                    .child(self.formatter.bytes(entry.bytes)),
                            )
                            .children(timestamp.map(|text| {
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .tabular_figures()
                                    .text_color(palette.text_tertiary)
                                    .child(text)
                            })),
                    )
                }))
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .py(space.m)
            .border_t_1()
            .border_color(palette.stroke_divider)
            .child(self.render_setting_row(
                ui,
                self.formatter.text(Msg::SettingsDataBackupTitle),
                self.formatter.text(Msg::SettingsDataBackupNote),
                backup_button,
                RowIndent::TopLevel,
                RowDivider::None,
            ))
            .children(backup_error)
            .child(list)
    }

    fn render_compact_section(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let data_op = self.settings.data_operation();

        let compact_button: AnyElement = match data_op {
            DataOperation::Compacting => self
                .busy_button(
                    ui,
                    "settings-compact-db",
                    Density::Standard,
                    self.formatter.text(Msg::SettingsDataCompactRunning),
                )
                .into_any_element(),
            DataOperation::BackingUp | DataOperation::Deleting => ui
                .disabled_button(
                    "settings-compact-db",
                    Density::Standard,
                    ButtonContent::Label(self.formatter.text(Msg::SettingsDataCompactAction)),
                )
                .into_any_element(),
            DataOperation::Idle => ui
                .button(
                    "settings-compact-db",
                    Density::Standard,
                    ButtonContent::Label(self.formatter.text(Msg::SettingsDataCompactAction)),
                )
                .on_click(cx.listener(|this, _, _, cx| this.start_database_compact(cx)))
                .into_any_element(),
        };

        let notice: Option<AnyElement> = match self.settings.compact_notice() {
            CompactNotice::None => None,
            CompactNotice::Succeeded(response) => {
                let before = self.formatter.bytes(response.before_bytes);
                let after = self.formatter.bytes(response.after_bytes);
                Some(
                    div()
                        .ramp(ui, Ramp::Body)
                        .tabular_figures()
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text_with(
                            Msg::SettingsDataCompactSuccess,
                            &[Arg::Start.with(&before), Arg::End.with(&after)],
                        ))
                        .into_any_element(),
                )
            }
            CompactNotice::Failed(error) => Some(
                self.render_inline_error(ui, Msg::SettingsDataCompactFailed, error)
                    .into_any_element(),
            ),
        };

        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .py(space.m)
            .border_t_1()
            .border_color(palette.stroke_divider)
            .child(self.render_setting_row(
                ui,
                self.formatter.text(Msg::SettingsDataCompactTitle),
                self.formatter.text(Msg::SettingsDataCompactDescription),
                compact_button,
                RowIndent::TopLevel,
                RowDivider::None,
            ))
            .children(notice)
    }

    fn render_delete_range_section(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let today = self.current_logical_today();
        let yesterday = max_deletable_date(today);
        let first_weekday = self.effective_first_weekday();
        let data_op = self.settings.data_operation();
        let busy = data_op.is_busy();
        let can_pick = first_weekday.is_some() && yesterday.is_some() && !busy;

        let validation = self.settings.delete_range_validation(today);
        let confirm_open = !matches!(self.settings.delete_confirm(), DeleteRangeConfirm::None);

        let start_picker = self.render_delete_date_field(
            ui,
            DeleteDateFieldSpec {
                field: DeleteDateField::Start,
                label: self.formatter.text(Msg::SettingsDataDeleteStartLabel),
                selected: self.settings.delete_start(),
                enabled: can_pick,
                today,
                yesterday,
                first_weekday,
            },
            cx,
        );

        let end_picker = self.render_delete_date_field(
            ui,
            DeleteDateFieldSpec {
                field: DeleteDateField::End,
                label: self.formatter.text(Msg::SettingsDataDeleteEndLabel),
                selected: self.settings.delete_end(),
                enabled: can_pick,
                today,
                yesterday,
                first_weekday,
            },
            cx,
        );

        let delete_button: AnyElement = if data_op == DataOperation::Deleting {
            self.busy_button(
                ui,
                "settings-delete-range",
                Density::Standard,
                self.formatter.text(Msg::SettingsDataDeleteRunning),
            )
            .into_any_element()
        } else if validation.is_ok() && !busy && !confirm_open {
            ui.button(
                "settings-delete-range",
                Density::Standard,
                ButtonContent::IconLabel(
                    IconName::Trash,
                    self.formatter.text(Msg::SettingsDataDeleteAction),
                ),
            )
            .on_click(cx.listener(|this, _, _, cx| this.request_delete_range(cx)))
            .into_any_element()
        } else {
            ui.disabled_button(
                "settings-delete-range",
                Density::Standard,
                ButtonContent::IconLabel(
                    IconName::Trash,
                    self.formatter.text(Msg::SettingsDataDeleteAction),
                ),
            )
            .into_any_element()
        };

        let span_or_validation: Option<AnyElement> = match validation {
            Ok(validated) => Some(
                div()
                    .ramp(ui, Ramp::Caption)
                    .tabular_figures()
                    .text_color(palette.text_secondary)
                    .child(self.formatter.text_with(
                        Msg::SettingsDataDeleteSpanDays,
                        &[Arg::Count.with(&validated.days.to_string())],
                    ))
                    .into_any_element(),
            ),
            Err(ValidateDeleteRangeError::MissingDates) => None,
            Err(ValidateDeleteRangeError::EndBeforeStart) => Some(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.status_critical)
                    .child(self.formatter.text(Msg::SettingsDataDeleteValidationOrder))
                    .into_any_element(),
            ),
            Err(ValidateDeleteRangeError::EndAfterYesterday) => Some(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.status_critical)
                    .child(self.formatter.text(Msg::SettingsDataDeleteValidationToday))
                    .into_any_element(),
            ),
            Err(ValidateDeleteRangeError::ExceedsMaxSpan) => Some(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.status_critical)
                    .child(self.formatter.text_with(
                        Msg::SettingsDataDeleteValidationSpan,
                        &[Arg::Count.with(&MAX_DELETE_RANGE_DAYS.to_string())],
                    ))
                    .into_any_element(),
            ),
        };

        let confirm_block = match self.settings.delete_confirm() {
            DeleteRangeConfirm::None => None,
            DeleteRangeConfirm::Pending(range) => {
                let colors = ui.info_bar_colors(InfoBarStyle::Critical);
                let range_text = format!("{} – {}", range.start_inclusive, range.end_inclusive);
                let confirm_btn = if busy {
                    ui.disabled_button(
                        "settings-delete-confirm",
                        Density::Compact,
                        ButtonContent::Label(
                            self.formatter.text(Msg::SettingsDataDeleteConfirmAction),
                        ),
                    )
                } else {
                    self.destructive_button(
                        ui,
                        "settings-delete-confirm",
                        Density::Compact,
                        self.formatter.text(Msg::SettingsDataDeleteConfirmAction),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_delete_range(cx)))
                };
                let cancel_btn = if busy {
                    ui.disabled_button(
                        "settings-delete-cancel",
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::SettingsCancel)),
                    )
                } else {
                    ui.button(
                        "settings-delete-cancel",
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::SettingsCancel)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_delete_range(cx)))
                };
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space.s)
                        .p(space.m)
                        .rounded(ui.metrics.radius.control)
                        .bg(colors.background)
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_start()
                                .gap(space.s)
                                .child(ui.icon(
                                    IconName::TriangleAlert,
                                    Density::Compact,
                                    colors.icon,
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .whitespace_normal()
                                        .ramp(ui, Ramp::Body)
                                        .text_color(colors.foreground)
                                        .child(self.formatter.text_with(
                                            Msg::SettingsDataDeleteConfirmPrompt,
                                            &[
                                                Arg::Range.with(&range_text),
                                                Arg::Count.with(&range.days.to_string()),
                                            ],
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .items_center()
                                .gap(space.s)
                                .child(confirm_btn)
                                .child(cancel_btn),
                        ),
                )
            }
        };

        let delete_notice: Option<AnyElement> = match self.settings.delete_notice() {
            DeleteNotice::None => None,
            DeleteNotice::Succeeded { deleted_ms } => Some(
                div()
                    .ramp(ui, Ramp::Body)
                    .tabular_figures()
                    .text_color(palette.text_secondary)
                    .child(self.formatter.deleted_foreground_summary(*deleted_ms))
                    .into_any_element(),
            ),
            DeleteNotice::Failed(error) => {
                let msg =
                    if error.code() == LoadDataErrorCode::Protocol(wire::ErrorKind::BadRequest) {
                        Msg::SettingsDataDeleteBadRequest
                    } else {
                        Msg::SettingsDataDeleteFailed
                    };
                Some(self.render_inline_error(ui, msg, error).into_any_element())
            }
        };

        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .py(space.m)
            .border_t_1()
            .border_color(palette.stroke_divider)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space.xxs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Body)
                            .text_color(palette.text_primary)
                            .child(self.formatter.text(Msg::SettingsDataDeleteTitle)),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::SettingsDataDeleteDescription)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_end()
                    .gap(space.m)
                    .child(start_picker)
                    .child(end_picker)
                    .child(delete_button),
            )
            .children(span_or_validation)
            .children(confirm_block)
            .children(delete_notice)
    }

    fn render_delete_date_field(
        &self,
        ui: &Widgets<'_>,
        spec: DeleteDateFieldSpec,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let field = spec.field;
        let id = match field {
            DeleteDateField::Start => "settings-delete-start",
            DeleteDateField::End => "settings-delete-end",
        };
        let value_text = spec
            .selected
            .map(|date| date.to_string())
            .unwrap_or_else(|| self.formatter.text(Msg::SettingsDataDeletePickDate));
        let content = ButtonContent::IconLabel(IconName::Calendar, value_text);
        let button = if spec.enabled {
            ui.button(id, Density::Standard, content)
                .min_w(px(SETTINGS_DATE_FIELD_MIN_WIDTH))
                .tabular_figures()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_settings_calendar(field, cx);
                }))
        } else {
            ui.disabled_button(id, Density::Standard, content)
                .min_w(px(SETTINGS_DATE_FIELD_MIN_WIDTH))
                .tabular_figures()
        };

        let popover = self
            .settings_calendar
            .filter(|open| open.field == field)
            .and_then(|open| {
                Some(self.render_calendar_popover(
                    ui,
                    CalendarPopover {
                        month: open.month,
                        selected: spec.selected,
                        today: spec.today,
                        max_selectable: spec.yesterday?,
                        first_weekday: spec.first_weekday?,
                        left: SETTINGS_CALENDAR_LEFT,
                        target: field.calendar_target(),
                    },
                    cx,
                ))
            });

        div()
            .flex()
            .flex_col()
            .gap(space.xxs)
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(spec.label),
            )
            .child(div().relative().child(button).children(popover))
    }

    fn render_about_card(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        let ui_version_text = self.formatter.text_with(
            Msg::SettingsAboutVersionUi,
            &[Arg::Value.with(env!("CARGO_PKG_VERSION"))],
        );
        let core_version_text = match self.settings.core_status() {
            CoreStatusLoadState::Loading => self.formatter.text(Msg::SettingsAboutStatusLoading),
            CoreStatusLoadState::Failed(_) => self.formatter.text(Msg::SettingsAboutStatusFailed),
            CoreStatusLoadState::Ready(status) => self.formatter.text_with(
                Msg::SettingsAboutVersionCore,
                &[Arg::Value.with(&status.core_version.0)],
            ),
        };
        let version_row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x(space.l)
            .gap_y(space.xs)
            .py(space.m)
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(Msg::SettingsAboutVersionLabel)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.m)
                    .ramp(ui, Ramp::Body)
                    .tabular_figures()
                    .text_color(palette.text_secondary)
                    .child(ui_version_text)
                    .child(core_version_text),
            );

        let status_control: AnyElement = match self.settings.core_status() {
            CoreStatusLoadState::Loading => div()
                .ramp(ui, Ramp::Body)
                .text_color(palette.text_secondary)
                .child(self.formatter.text(Msg::SettingsAboutStatusLoading))
                .into_any_element(),
            CoreStatusLoadState::Failed(error) => div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(space.s)
                .child(
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(palette.status_critical)
                        .child(self.formatter.text(Msg::SettingsAboutStatusFailed)),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .font(ui.mono_font())
                        .text_color(palette.text_tertiary)
                        .child(error.code().to_string()),
                )
                .child(
                    ui.button(
                        "retry-core-status",
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::SettingsRetry)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.retry_core_status(cx))),
                )
                .into_any_element(),
            CoreStatusLoadState::Ready(status) => {
                let (dot_color, label) = if status.recording {
                    (
                        palette.status_success,
                        self.formatter.text(Msg::LinkRecording),
                    )
                } else if let Some(reason) = status.stop_reason {
                    let dot = if reason == wire::StopReason::PersistenceFailed {
                        palette.status_critical
                    } else {
                        palette.status_neutral
                    };
                    (dot, self.formatter.text(stop_reason_msg(reason)))
                } else {
                    (palette.status_neutral, self.formatter.text(Msg::StopTitle))
                };
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.s)
                    .child(
                        div()
                            .size(px(STATUS_DOT_SIZE))
                            .rounded_full()
                            .flex_shrink_0()
                            .bg(dot_color),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Body)
                            .text_color(palette.text_secondary)
                            .child(label),
                    )
                    .into_any_element()
            }
        };

        let status_row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x(space.l)
            .gap_y(space.xs)
            .py(space.m)
            .border_t_1()
            .border_color(palette.stroke_divider)
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(Msg::SettingsAboutStatusLabel)),
            )
            .child(status_control);

        let logs_row = match self.settings.data_info() {
            DataInfoLoadState::Ready(info) => {
                let path_for_copy = info.logs_path.clone();
                let path_for_open = info.logs_path.clone();
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_x(space.l)
                    .gap_y(space.xs)
                    .py(space.m)
                    .border_t_1()
                    .border_color(palette.stroke_divider)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(space.xxs)
                            .child(
                                div()
                                    .ramp(ui, Ramp::Body)
                                    .text_color(palette.text_primary)
                                    .child(self.formatter.text(Msg::SettingsAboutLogsLabel)),
                            )
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .font(ui.mono_font())
                                    .whitespace_normal()
                                    .text_color(palette.text_secondary)
                                    .child(info.logs_path.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(space.s)
                            .flex_shrink_0()
                            .child(
                                ui.button(
                                    "settings-copy-logs-path",
                                    Density::Compact,
                                    ButtonContent::IconLabel(
                                        IconName::Copy,
                                        self.formatter.text(Msg::SettingsDataCopyPath),
                                    ),
                                )
                                .on_click(cx.listener(
                                    move |_, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            path_for_copy.clone(),
                                        ));
                                    },
                                )),
                            )
                            .child(
                                ui.button(
                                    "settings-open-logs-folder",
                                    Density::Standard,
                                    ButtonContent::Label(
                                        self.formatter.text(Msg::SettingsAboutOpen),
                                    ),
                                )
                                .on_click(cx.listener(
                                    move |_, _, _, _| {
                                        open_folder(Path::new(&path_for_open));
                                    },
                                )),
                            ),
                    )
            }
            DataInfoLoadState::Loading => div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(space.l)
                .py(space.m)
                .border_t_1()
                .border_color(palette.stroke_divider)
                .child(
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(palette.text_primary)
                        .child(self.formatter.text(Msg::SettingsAboutLogsLabel)),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text(Msg::SettingsAboutStatusLoading)),
                ),
            DataInfoLoadState::Failed(_) => div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(space.l)
                .py(space.m)
                .border_t_1()
                .border_color(palette.stroke_divider)
                .child(
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(palette.text_primary)
                        .child(self.formatter.text(Msg::SettingsAboutLogsLabel)),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_tertiary)
                        .child(self.formatter.text(Msg::SettingsDataLoadFailedTitle)),
                ),
        };

        let local_note = div()
            .pt(space.m)
            .border_t_1()
            .border_color(palette.stroke_divider)
            .ramp(ui, Ramp::Caption)
            .text_color(palette.text_secondary)
            .child(self.formatter.text(Msg::SettingsAboutLocalOnly));

        ui.card()
            .flex()
            .flex_col()
            .w_full()
            .p(space.l)
            .gap(space.s)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(Msg::SettingsGroupAbout)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .child(version_row)
                    .child(status_row)
                    .child(logs_row)
                    .child(local_note),
            )
            .into_any_element()
    }

    fn destructive_button(
        &self,
        ui: &Widgets<'_>,
        id: &'static str,
        density: Density,
        label: String,
    ) -> gpui_kit::Stateful<Div> {
        ui.button(id, density, ButtonContent::Label(label)).when(
            ui.palette.contrast == ContrastMode::Standard,
            |btn| {
                btn.bg(ui.palette.status_critical)
                    .border_color(ui.palette.status_critical)
                    .text_color(ui.palette.text_on_critical)
            },
        )
    }

    fn busy_button(
        &self,
        ui: &Widgets<'_>,
        id: &'static str,
        density: Density,
        label: String,
    ) -> gpui_kit::Stateful<Div> {
        let space = &ui.metrics.space;
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(space.s)
            .min_h(ui.metrics.density(density).control_height)
            .px(space.m)
            .bg(ui.palette.fill_control_disabled)
            .border_1()
            .border_color(ui.palette.stroke_control)
            .rounded(ui.metrics.radius.control)
            .text_color(ui.palette.text_disabled)
            .ramp(ui, Ramp::Body)
            .cursor_default()
            .child(
                Spinner::new()
                    .icon(Icon::default().path(AppAssets::progress_path()))
                    .color(ui.palette.text_disabled)
                    .with_size(px(BUSY_SPINNER_SIZE)),
            )
            .child(label)
    }

    fn render_select_control<V: Clone + PartialEq + 'static>(
        &self,
        ui: &Widgets<'_>,
        id: &'static str,
        accessibility_label: String,
        state: Option<&Entity<SelectState<Vec<SelectOption<V>>>>>,
        save_activity: SaveActivity,
    ) -> AnyElement {
        let Some(state) = state else {
            return ui
                .skeleton_block(SkeletonTone::Subtle)
                .w(px(SETTINGS_SELECT_WIDTH))
                .h(ui.metrics.density(Density::Standard).control_height)
                .rounded(ui.metrics.radius.control)
                .into_any_element();
        };
        Select::new(state)
            .id(id)
            .accessibility_label(accessibility_label)
            .empty(|_, _| div())
            .disabled(save_activity == SaveActivity::Saving)
            .w(px(SETTINGS_SELECT_WIDTH))
            .bg(ui.palette.fill_control)
            .border_color(ui.palette.stroke_control)
            .text_color(ui.palette.text_primary)
            .ramp(ui, Ramp::Body)
            .into_any_element()
    }

    fn render_setting_row(
        &self,
        ui: &Widgets<'_>,
        label: String,
        description: String,
        control: AnyElement,
        indent: RowIndent,
        divider: RowDivider,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x(space.l)
            .gap_y(space.xs)
            .py(space.m);
        if indent == RowIndent::Nested {
            row = row.pl(space.l);
        }
        if divider == RowDivider::Top {
            row = row.border_t_1().border_color(palette.stroke_divider);
        }
        row.child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .gap(space.xxs)
                .child(
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(palette.text_primary)
                        .child(label),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_secondary)
                        .child(description),
                ),
        )
        .child(div().flex_shrink_0().child(control))
    }

    fn render_inline_error(&self, ui: &Widgets<'_>, message: Msg, error: &LoadDataError) -> Div {
        let space = &ui.metrics.space;
        let colors = ui.info_bar_colors(InfoBarStyle::Critical);
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(space.s)
            .px(space.m)
            .py(space.s)
            .rounded(ui.metrics.radius.control)
            .bg(colors.background)
            .border_1()
            .border_color(colors.border)
            .child(ui.icon(IconName::TriangleAlert, Density::Compact, colors.icon))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .ramp(ui, Ramp::Body)
                    .text_color(colors.foreground)
                    .child(self.formatter.text(message)),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .font(ui.mono_font())
                    .text_color(colors.secondary)
                    .child(error.code().to_string()),
            )
    }

    fn render_settings_loading_card(
        &self,
        ui: &Widgets<'_>,
        heading: Msg,
        row_count: usize,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        ui.card()
            .flex()
            .flex_col()
            .w_full()
            .p(space.l)
            .gap(space.s)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(heading)),
            )
            .children((0..row_count).map(|index| {
                let mut row = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(space.l)
                    .py(space.m);
                if index > 0 {
                    row = row.border_t_1().border_color(palette.stroke_divider);
                }
                row.child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .gap(space.xs)
                        .child(
                            ui.skeleton_block(SkeletonTone::Strong)
                                .w(px(SKELETON_LABEL_WIDTH))
                                .h(px(SKELETON_LABEL_HEIGHT))
                                .rounded(ui.metrics.radius.control),
                        )
                        .child(
                            ui.skeleton_block(SkeletonTone::Subtle)
                                .w(px(SKELETON_DESC_WIDTH))
                                .h(px(SKELETON_DESC_HEIGHT))
                                .rounded(ui.metrics.radius.control),
                        ),
                )
                .child(
                    ui.skeleton_block(SkeletonTone::Strong)
                        .w(px(SKELETON_CONTROL_WIDTH))
                        .h(px(SKELETON_CONTROL_HEIGHT))
                        .rounded(ui.metrics.radius.control),
                )
            }))
            .into_any_element()
    }

    fn render_settings_failed_card(
        &self,
        ui: &Widgets<'_>,
        error: &LoadDataError,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        ui.card()
            .flex()
            .flex_col()
            .w_full()
            .p(space.l)
            .gap(space.m)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(palette.text_primary)
                    .child(self.formatter.text(Msg::SettingsGroupGeneral)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.m)
                    .child(ui.icon(
                        IconName::TriangleAlert,
                        Density::Standard,
                        palette.status_caution,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(space.xxs)
                            .child(
                                div()
                                    .ramp(ui, Ramp::BodyStrong)
                                    .text_color(palette.text_primary)
                                    .child(self.formatter.text(Msg::SettingsLoadFailedTitle)),
                            )
                            .child(
                                div()
                                    .ramp(ui, Ramp::Body)
                                    .text_color(palette.text_secondary)
                                    .child(self.formatter.text(Msg::SettingsLoadFailedMessage)),
                            )
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .font(ui.mono_font())
                                    .text_color(palette.text_tertiary)
                                    .child(error.code().to_string()),
                            ),
                    )
                    .child(
                        ui.button(
                            "retry-settings",
                            Density::Standard,
                            ButtonContent::Label(self.formatter.text(Msg::SettingsRetry)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.retry_settings(cx))),
                    ),
            )
            .into_any_element()
    }
}
