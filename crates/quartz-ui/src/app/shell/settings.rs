//! Settings page: left-aligned single column of group cards (07 section 7).
//!
//! Group order is General, Recording, Data, About; this batch implements only General.

use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::{
        Disableable as _, IndexPath,
        select::{Select, SelectEvent, SelectItem, SelectState},
        switch::Switch,
    },
    div, px,
};
use quartz_protocol as wire;

use super::{AppShell, Frame, InputSyncState, Page};
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, InfoBarStyle, RampExt as _, SkeletonTone, Widgets},
    },
    format::{Formatter, Msg},
    layout::WindowClass,
    link::{LoadDataError, RequestCoreError},
    model::{
        GeneralSettingChange, RowVisibility, SaveActivity, SettingsEffect, SettingsLoadState,
        SettingsValues,
    },
};

/// Maximum width of the left-aligned settings column in logical pixels.
const SETTINGS_COLUMN_MAX_WIDTH: f32 = 720.;
/// Width of dropdown select controls on settings rows in logical pixels.
const SETTINGS_SELECT_WIDTH: f32 = 168.;
/// Number of placeholder rows shown while initial `settings.get` is in flight.
const SKELETON_ROW_COUNT: usize = 5;
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

fn option_index<V: PartialEq>(options: &[SelectOption<V>], value: &V) -> Option<IndexPath> {
    options
        .iter()
        .position(|option| option.value == *value)
        .map(|row| IndexPath::default().row(row))
}

impl AppShell {
    /// Loads current settings via `settings.get` whenever the Settings page opens.
    pub(super) fn open_settings_page(&mut self, cx: &mut Context<Self>) {
        let effect = self.settings.open();
        self.run_settings_effect(effect, cx);
    }

    fn retry_settings(&mut self, cx: &mut Context<Self>) {
        let effect = self.settings.open();
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn change_general_setting(&mut self, change: GeneralSettingChange, cx: &mut Context<Self>) {
        let effect = self.settings.change_general(change);
        self.settings_selects_sync = InputSyncState::NeedsSync;
        self.run_settings_effect(effect, cx);
        cx.notify();
    }

    fn run_settings_effect(&mut self, effect: SettingsEffect, cx: &mut Context<Self>) {
        match effect {
            SettingsEffect::None => {}
            SettingsEffect::Load { generation } => {
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
            SettingsEffect::Save {
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

        self.settings_selects_sync = InputSyncState::Synced;
    }

    /// Renders the Settings page as a left-aligned single column of group cards.
    pub(super) fn render_settings(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let compact = frame.layout.class == WindowClass::Compact;

        let general_card: AnyElement = match self.settings.state() {
            SettingsLoadState::Loading => self.render_settings_loading_card(ui),
            SettingsLoadState::Failed(error) => self.render_settings_failed_card(ui, error, cx),
            SettingsLoadState::Ready(values) => self.render_general_card(ui, values, cx),
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
            .child(general_card);

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
            .map(|error| self.render_inline_error(ui, error));

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

    fn render_inline_error(&self, ui: &Widgets<'_>, error: &LoadDataError) -> Div {
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
                    .child(self.formatter.text(Msg::SettingsSaveFailed)),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .font(ui.mono_font())
                    .text_color(colors.secondary)
                    .child(error.code().to_string()),
            )
    }

    fn render_settings_loading_card(&self, ui: &Widgets<'_>) -> AnyElement {
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
                    .child(self.formatter.text(Msg::SettingsGroupGeneral)),
            )
            .children((0..SKELETON_ROW_COUNT).map(|index| {
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
