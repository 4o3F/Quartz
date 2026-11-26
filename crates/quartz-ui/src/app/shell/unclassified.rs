//! Unclassified queue tab on the Rules page: last-7-logical-days `query.unclassified`,
//! application/document group toggle, debounced search input, sorted list with localized hidden
//! entity labels, truncation notice, and per-row "Create rule…" handoff to the H4 rule editor.

use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::input::{Input, InputEvent, InputState},
    div, px,
};
use quartz_protocol::{LocalDate, UnclassifiedGroupBy};

use super::{AppShell, InputSyncState, Page, rule_editor::ms_to_duration};
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, RampExt as _, Segment, SkeletonTone, Widgets},
    },
    format::{Arg, Msg},
    link::{LoadDataError, RequestCoreError},
    model::{
        LoadState, RulesTab, UNCLASSIFIED_DEBOUNCE_DELAY, UnclassifiedEffect,
        UnclassifiedEntityKind, UnclassifiedList, UnclassifiedRow, ValidateUnclassifiedSearchError,
        unclassified_last_seven_days,
    },
};

/// Minimum width of the unclassified search input in logical pixels.
const UNCLASSIFIED_SEARCH_MIN_WIDTH: f32 = 220.;
/// Static skeleton row count while loading the unclassified queue.
const SKELETON_UNCLASSIFIED_ROWS: usize = 6;
/// Height of one static unclassified skeleton row in logical pixels.
const SKELETON_UNCLASSIFIED_HEIGHT: f32 = 44.;

impl AppShell {
    /// Current logical today from the live core snapshot, falling back to the loaded day date.
    pub(super) fn current_logical_today(&self) -> LocalDate {
        self.day_snapshot()
            .map(|snapshot| snapshot.logical_date)
            .unwrap_or_else(|| self.day.date())
    }

    /// Synchronizes the unclassified search GPUI input entity when the Unclassified tab is active.
    pub(super) fn sync_unclassified_search_input_if_needed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Rules || self.rules.active_tab() != RulesTab::Unclassified {
            return;
        }
        if self.unclassified_search_input.is_some()
            && self.unclassified_search_sync != InputSyncState::NeedsSync
        {
            return;
        }
        self.unclassified_search_sync = InputSyncState::Synced;
        let initial = self.unclassified.search_text().to_owned();
        let placeholder = self.formatter.text(Msg::RulesUnclassifiedSearchPlaceholder);
        if let Some(input) = self.unclassified_search_input.clone() {
            input.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx);
                if state.value() != initial {
                    state.set_value(initial, window, cx);
                }
            });
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        if !initial.is_empty() {
            input.update(cx, |state, cx| {
                state.set_value(initial, window, cx);
            });
        }
        cx.subscribe(&input, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let value = state.read(cx).value().to_string();
                this.set_unclassified_search(value, cx);
            }
        })
        .detach();
        self.unclassified_search_input = Some(input);
    }

    /// Refreshes the unclassified queue when opening or switching to the Unclassified tab.
    pub(super) fn open_unclassified_tab(&mut self, cx: &mut Context<Self>) {
        self.unclassified_query_task = None;
        self.unclassified_search_sync = InputSyncState::NeedsSync;
        let today = self.current_logical_today();
        let effect = self.unclassified.open(today);
        self.run_unclassified_effect(effect, cx);
    }

    /// Reloads the unclassified queue on explicit user request.
    pub(super) fn reload_unclassified(&mut self, cx: &mut Context<Self>) {
        self.unclassified_query_task = None;
        let today = self.current_logical_today();
        let effect = self.unclassified.reload(today);
        self.run_unclassified_effect(effect, cx);
        cx.notify();
    }

    fn set_unclassified_group_by(&mut self, group_by: UnclassifiedGroupBy, cx: &mut Context<Self>) {
        self.unclassified_query_task = None;
        let today = self.current_logical_today();
        let effect = self.unclassified.set_group_by(group_by, today);
        self.run_unclassified_effect(effect, cx);
        cx.notify();
    }

    fn set_unclassified_search(&mut self, raw: String, cx: &mut Context<Self>) {
        let effect = self.unclassified.set_search(raw);
        self.run_unclassified_effect(effect, cx);
        cx.notify();
    }

    /// Handles a `day` topic change notification from the core link. When the Unclassified tab is
    /// visible and `date` falls inside the covered 7-day range, schedules a coalesced refresh.
    pub(crate) fn day_changed(&mut self, date: LocalDate, cx: &mut Context<Self>) {
        self.on_annotations_day_changed(date, cx);
        if self.page == Page::Rules && self.rules.active_tab() == RulesTab::Unclassified {
            let today = self.current_logical_today();
            let effect = self.unclassified.on_day_changed(date, today);
            self.run_unclassified_effect(effect, cx);
            cx.notify();
        } else {
            self.unclassified.mark_stale();
            if self.page == Page::Today {
                cx.notify();
            }
        }
    }

    /// Refreshes or marks the unclassified queue stale after a rule save, import, toggle, or
    /// delete completes.
    pub(super) fn on_rules_mutated(&mut self, cx: &mut Context<Self>) {
        self.unclassified.mark_stale();
        if self.page == Page::Rules && self.rules.active_tab() == RulesTab::Unclassified {
            self.reload_unclassified(cx);
        }
    }

    fn run_unclassified_effect(&mut self, effect: UnclassifiedEffect, cx: &mut Context<Self>) {
        match effect {
            UnclassifiedEffect::None => {}
            UnclassifiedEffect::ScheduleQuery { generation } => {
                let delay = cx.background_executor().timer(UNCLASSIFIED_DEBOUNCE_DELAY);
                self.unclassified_query_task = Some(cx.spawn(async move |this, cx| {
                    delay.await;
                    let _ = this.update(cx, |this, cx| {
                        let today = this.current_logical_today();
                        let next = this.unclassified.trigger_debounced_query(generation, today);
                        this.run_unclassified_effect(next, cx);
                        cx.notify();
                    });
                }));
            }
            UnclassifiedEffect::Query {
                request,
                range,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.unclassified.apply_loaded(
                        generation,
                        range,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.query_unclassified(request).await;
                    let _ = this.update(cx, |this, cx| {
                        this.unclassified.apply_loaded(generation, range, result);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// Renders the Rules page "Unclassified" tab.
    pub(super) fn render_unclassified_section(
        &self,
        ui: &Widgets<'_>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        let range = self
            .unclassified
            .loaded_range()
            .or_else(|| unclassified_last_seven_days(self.current_logical_today()));
        let range_label = match range {
            Some((start, end)) => {
                let last_day = end.yesterday().unwrap_or(end);
                format!("{start} – {last_day}")
            }
            None => String::new(),
        };

        let group_toggle = ui.segmented(
            "unclassified-group-by",
            [
                Segment {
                    id: "unclassified-group-app",
                    label: self.formatter.text(Msg::RulesUnclassifiedGroupApp),
                    value: UnclassifiedGroupBy::App,
                },
                Segment {
                    id: "unclassified-group-document",
                    label: self.formatter.text(Msg::RulesUnclassifiedGroupDocument),
                    value: UnclassifiedGroupBy::Document,
                },
            ],
            self.unclassified.group_by(),
            |group_by| {
                cx.listener(move |this, _, _, cx| {
                    this.set_unclassified_group_by(group_by, cx);
                })
            },
        );

        let search_input = match &self.unclassified_search_input {
            Some(input_state) => div()
                .flex_1()
                .min_w(px(UNCLASSIFIED_SEARCH_MIN_WIDTH))
                .child(Input::new(input_state))
                .into_any_element(),
            None => div()
                .flex_1()
                .min_w(px(UNCLASSIFIED_SEARCH_MIN_WIDTH))
                .min_h(ui.metrics.density(Density::Standard).control_height)
                .px(space.s)
                .py(space.xs)
                .rounded(ui.metrics.radius.control)
                .bg(palette.fill_control)
                .border_1()
                .border_color(palette.stroke_control)
                .ramp(ui, Ramp::Body)
                .text_color(palette.text_tertiary)
                .child(self.formatter.text(Msg::RulesUnclassifiedSearchPlaceholder))
                .into_any_element(),
        };

        let search_error = self.unclassified.search_error().map(|error| {
            let msg = match error {
                ValidateUnclassifiedSearchError::TooLong => Msg::RulesUnclassifiedSearchTooLong,
            };
            div()
                .ramp(ui, Ramp::Caption)
                .text_color(palette.status_critical)
                .child(self.formatter.text(msg))
        });

        let toolbar = div()
            .flex()
            .flex_col()
            .gap(space.xs)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(space.s)
                    .child(group_toggle)
                    .child(search_input),
            )
            .children(search_error);

        let body = match self.unclassified.state() {
            LoadState::Loading => div()
                .flex()
                .flex_col()
                .gap(space.xs)
                .children((0..SKELETON_UNCLASSIFIED_ROWS).map(|_| {
                    ui.skeleton_block(SkeletonTone::Subtle)
                        .h(px(SKELETON_UNCLASSIFIED_HEIGHT))
                        .w_full()
                        .rounded(ui.metrics.radius.control)
                }))
                .into_any_element(),
            LoadState::Failed(error) => self
                .render_rules_error_block(
                    ui,
                    Msg::RulesUnclassifiedLoadFailedTitle,
                    if error.is_busy() {
                        Msg::DayBusy
                    } else {
                        Msg::RulesUnclassifiedLoadFailedMessage
                    },
                    error,
                    "retry-unclassified",
                    cx.listener(|this, _, _, cx| this.reload_unclassified(cx)),
                )
                .into_any_element(),
            LoadState::Stale => self
                .render_unclassified_stale_block(ui, cx)
                .into_any_element(),
            LoadState::Ready(list) => self
                .render_unclassified_list(ui, list, cx)
                .into_any_element(),
        };

        ui.card()
            .id("rules-unclassified-pane")
            .flex()
            .flex_col()
            .gap(space.m)
            .p(space.l)
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_baseline()
                    .justify_between()
                    .gap(space.s)
                    .child(
                        div()
                            .ramp(ui, Ramp::Subtitle)
                            .text_color(palette.text_primary)
                            .child(self.formatter.text(Msg::RulesUnclassifiedHeading)),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .tabular_figures()
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text_with(
                                Msg::RulesUnclassifiedRange,
                                &[Arg::Range.with(&range_label)],
                            )),
                    ),
            )
            .child(toolbar)
            .child(body)
            .into_any_element()
    }

    fn render_unclassified_list(
        &self,
        ui: &Widgets<'_>,
        list: &UnclassifiedList,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let mut container = div().flex().flex_col().gap(space.xs);

        if list.truncated {
            container = container.child(self.render_truncated_notice(
                ui,
                Msg::RulesUnclassifiedTruncated,
                list.items.len(),
            ));
        }

        if list.items.is_empty() {
            let empty_msg = if self.unclassified.has_search_filter() {
                Msg::RulesUnclassifiedSearchEmpty
            } else {
                Msg::RulesUnclassifiedEmpty
            };
            return container.child(
                div()
                    .py(space.l)
                    .ramp(ui, Ramp::Body)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(empty_msg)),
            );
        }

        container.children(
            list.items
                .iter()
                .enumerate()
                .map(|(index, row)| self.render_unclassified_row(ui, index, row, cx)),
        )
    }

    fn render_unclassified_row(
        &self,
        ui: &Widgets<'_>,
        index: usize,
        row: &UnclassifiedRow,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let icon = match row.kind {
            UnclassifiedEntityKind::App => IconName::SquareTerminal,
            UnclassifiedEntityKind::Document => IconName::FileText,
        };
        let label = self.formatter.unclassified_entity_label(&row.label);
        let duration = self
            .formatter
            .detail_duration(ms_to_duration(row.active_ms));
        let action_content = ButtonContent::IconLabel(
            IconName::Plus,
            self.formatter.text(Msg::RulesUnclassifiedActionCreateRule),
        );
        let action_btn = match &row.prefill {
            Some(prefill) => {
                let prefill = prefill.clone();
                ui.button(
                    ("unclassified-create-rule", index),
                    Density::Compact,
                    action_content,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_prefilled_classification_rule_set(prefill.clone(), cx);
                }))
            }
            None => ui.disabled_button(
                ("unclassified-create-rule", index),
                Density::Compact,
                action_content,
            ),
        };

        div()
            .id(("unclassified-row", index))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(space.m)
            .px(space.m)
            .py(space.s)
            .rounded(ui.metrics.radius.control)
            .bg(palette.fill_card_secondary)
            .border_1()
            .border_color(palette.stroke_card)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.s)
                    .flex_1()
                    .min_w(px(0.))
                    .child(ui.icon(icon, Density::Compact, palette.text_secondary))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(space.xxs)
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .ramp(ui, Ramp::BodyStrong)
                                    .text_color(palette.text_primary)
                                    .child(label),
                            )
                            .children(row.secondary.as_ref().map(|meta| {
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .ramp(ui, Ramp::Caption)
                                    .font(ui.mono_font())
                                    .text_color(palette.text_secondary)
                                    .child(meta.clone())
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.m)
                    .child(
                        div()
                            .ramp(ui, Ramp::Body)
                            .tabular_figures()
                            .text_color(palette.text_secondary)
                            .child(duration),
                    )
                    .child(action_btn),
            )
    }

    fn render_unclassified_stale_block(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> Div {
        let space = &ui.metrics.space;
        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .p(space.s)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.s)
                    .child(ui.icon(
                        IconName::TriangleAlert,
                        Density::Standard,
                        ui.palette.status_caution,
                    ))
                    .child(
                        div()
                            .ramp(ui, Ramp::BodyStrong)
                            .text_color(ui.palette.text_primary)
                            .child(self.formatter.text(Msg::RulesStaleTitle)),
                    ),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesStaleMessage)),
            )
            .child(
                div().child(
                    ui.button(
                        "reload-stale-unclassified",
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::RulesActionReload)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.reload_unclassified(cx))),
                ),
            )
    }
}
