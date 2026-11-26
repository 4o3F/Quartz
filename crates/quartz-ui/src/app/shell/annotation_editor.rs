//! Today saved-annotations side panel and general create/edit/delete annotation editor (M4-H13).

use std::time::Duration;

use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Stateful, StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::input::{Editor, EditorState, Input, InputEvent, InputState},
    div,
    prelude::FluentBuilder as _,
    px,
};
use quartz_protocol::{self as wire, LocalDate};

use super::{AppShell, InputSyncState};
use crate::{
    app::{
        metrics::{Density, Ramp},
        tokens::ContrastMode,
        widgets::{ButtonContent, InfoBarStyle, RampExt as _, Segment, SkeletonTone, Widgets},
    },
    format::{Arg, Msg},
    layout::PanelPlacement,
    link::{LoadDataError, RequestCoreError},
    model::{
        AnnotationCategoryDisplay, AnnotationDraft, AnnotationEditorEffect, AnnotationEditorMode,
        AnnotationList, AnnotationWriteNotice, DayTimeContext, DeleteConfirmState, EndpointRole,
        FoldChoice, LoadState, MutationActivity, SavedAnnotationRow, TodayPanelTab,
        ValidateAnnotationDraftError, ValidateAnnotationTimeError, format_note_excerpt,
        resolve_annotation_category, rules::CategoryArchiveState,
    },
};

/// Height of the multi-line note editor box in logical pixels.
const ANNOTATION_NOTE_HEIGHT: f32 = 96.;
/// Static skeleton row count while loading saved annotations.
const ANNOTATION_SKELETON_ROWS: usize = 5;
/// Height of one static annotation skeleton card in logical pixels.
const ANNOTATION_SKELETON_HEIGHT: f32 = 56.;
/// Horizontal padding inside compact kind/status badges.
const BADGE_PADDING_X: f32 = 6.;
/// Vertical padding inside compact kind/status badges.
const BADGE_PADDING_Y: f32 = 2.;
/// Minimum height for compact category picker chips.
const CATEGORY_CHIP_HEIGHT: f32 = 26.;
/// Border width for a selected category chip.
const SELECTED_CHIP_BORDER: f32 = 1.5;

impl AppShell {
    /// Synchronizes GPUI text/editor input entities with programmatic draft transitions during
    /// render where `Window` is live.
    pub(super) fn sync_annotation_inputs_if_needed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.annotation_inputs_sync != InputSyncState::NeedsSync {
            return;
        }
        self.annotation_inputs_sync = InputSyncState::Synced;
        let Some(draft) = self.annotation_editor.draft() else {
            return;
        };
        let start = draft.start_input.clone();
        let end = draft.end_input.clone();
        let label = draft.label_input.clone();
        let note = draft.note_input.clone();

        let time_placeholder = self
            .formatter
            .text(Msg::TodayAnnotationsEditorTimePlaceholder);
        let label_placeholder = self
            .formatter
            .text(Msg::TodayAnnotationsEditorLabelPlaceholder);
        let note_placeholder = self
            .formatter
            .text(Msg::TodayAnnotationsEditorNotePlaceholder);

        self.sync_annotation_start_input(&start, time_placeholder.clone(), window, cx);
        self.sync_annotation_end_input(&end, time_placeholder, window, cx);
        self.sync_annotation_label_input(&label, label_placeholder, window, cx);
        self.sync_annotation_note_input(&note, note_placeholder, window, cx);
    }

    fn sync_annotation_start_input(
        &mut self,
        initial: &str,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.annotation_start_input.clone() {
            input.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx);
                if state.value() != initial {
                    state.set_value(initial.to_owned(), window, cx);
                }
            });
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |state, cx| {
            state.set_value(initial.to_owned(), window, cx);
        });
        cx.subscribe(&input, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let value = state.read(cx).value().to_string();
                this.annotation_editor.set_start_input(value);
                cx.notify();
            }
        })
        .detach();
        self.annotation_start_input = Some(input);
    }

    fn sync_annotation_end_input(
        &mut self,
        initial: &str,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.annotation_end_input.clone() {
            input.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx);
                if state.value() != initial {
                    state.set_value(initial.to_owned(), window, cx);
                }
            });
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |state, cx| {
            state.set_value(initial.to_owned(), window, cx);
        });
        cx.subscribe(&input, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let value = state.read(cx).value().to_string();
                this.annotation_editor.set_end_input(value);
                cx.notify();
            }
        })
        .detach();
        self.annotation_end_input = Some(input);
    }

    fn sync_annotation_label_input(
        &mut self,
        initial: &str,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.annotation_label_input.clone() {
            input.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx);
                if state.value() != initial {
                    state.set_value(initial.to_owned(), window, cx);
                }
            });
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |state, cx| {
            state.set_value(initial.to_owned(), window, cx);
        });
        cx.subscribe(&input, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let value = state.read(cx).value().to_string();
                this.annotation_editor.set_label_input(value);
                cx.notify();
            }
        })
        .detach();
        self.annotation_label_input = Some(input);
    }

    fn sync_annotation_note_input(
        &mut self,
        initial: &str,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.annotation_note_input.clone() {
            editor.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx);
                if state.value() != initial {
                    state.set_value(initial.to_owned(), window, cx);
                }
            });
            return;
        }
        let editor = cx.new(|cx| EditorState::new(window, cx).placeholder(placeholder));
        editor.update(cx, |state, cx| {
            state.set_value(initial.to_owned(), window, cx);
        });
        cx.subscribe(&editor, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let value = state.read(cx).value().to_string();
                this.annotation_editor.set_note_input(value);
                cx.notify();
            }
        })
        .detach();
        self.annotation_note_input = Some(editor);
    }

    /// Toggles or opens the Today saved-annotations side panel from Today's date bar.
    pub(super) fn toggle_annotations_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.annotation_editor.is_panel_open()
            && self.annotation_editor.active_tab() == TodayPanelTab::Annotations
            && self.annotation_editor.draft().is_none()
        {
            self.annotation_editor.close_panel();
            if self.details.is_none() {
                window.focus(&self.root_focus, cx);
            }
            cx.notify();
            return;
        }
        if self.annotation_editor.day_context().is_none() {
            let context = self.day.live_time_context();
            let _ = self.annotation_editor.set_day_context(context);
        }
        let effect = self.annotation_editor.open_panel();
        self.run_annotation_editor_effect(effect, cx);
        self.ensure_categories_loaded_for_annotations(cx);
        self.hover = None;
        window.focus(&self.panel_focus, cx);
        cx.notify();
    }

    /// Switches between Details and Annotations inside Today's right-hand side panel.
    pub(super) fn switch_today_panel_tab(&mut self, tab: TodayPanelTab, cx: &mut Context<Self>) {
        if self.annotation_editor.day_context().is_none() {
            let context = self.day.live_time_context();
            let _ = self.annotation_editor.set_day_context(context);
        }
        let effect = self.annotation_editor.select_tab(tab);
        self.run_annotation_editor_effect(effect, cx);
        if tab == TodayPanelTab::Annotations {
            self.ensure_categories_loaded_for_annotations(cx);
        }
        cx.notify();
    }

    /// Ensures the shared `CategoryCatalog` is loaded so annotation rows and picker resolve
    /// active categories without duplicating catalog state.
    pub(super) fn ensure_categories_loaded_for_annotations(&mut self, cx: &mut Context<Self>) {
        let effect = self.rules.ensure_categories_loaded();
        self.run_rules_effect(effect, cx);
    }

    /// Updates the annotations model when the live day loads or changes dates.
    pub(super) fn on_live_day_context_changed(
        &mut self,
        context: Option<DayTimeContext>,
        cx: &mut Context<Self>,
    ) {
        let effect = self.annotation_editor.set_day_context(context);
        self.run_annotation_editor_effect(effect, cx);
    }

    /// Handles a `day` topic push for `date`, refreshing annotations if that day is shown.
    pub(super) fn on_annotations_day_changed(&mut self, date: LocalDate, cx: &mut Context<Self>) {
        let effect = self.annotation_editor.on_day_changed(date);
        self.run_annotation_editor_effect(effect, cx);
    }

    /// Handles `Esc` when the Annotations panel is active: cancels transient timeline state,
    /// delete confirm, closes draft, or closes the floating side panel.
    pub(super) fn handle_annotations_escape(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dismiss_timeline_transient(cx) {
            return;
        }
        if let Some(draft) = self.annotation_editor.draft() {
            if matches!(
                draft.mode,
                AnnotationEditorMode::Edit {
                    delete_confirm: DeleteConfirmState::Confirming,
                    ..
                }
            ) {
                self.annotation_editor.cancel_delete();
                cx.notify();
                return;
            }
            self.annotation_editor.close_draft();
            cx.notify();
            return;
        }
        self.close_details(window, cx);
    }

    /// Opens the annotation editor in create mode for a preselected UTC range and optional typed
    /// category UUID (H14 drag-select and H15 context menu "Other…").
    pub(super) fn open_create_annotation_range(
        &mut self,
        range: crate::link::UtcRange,
        category: Option<wire::Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.annotation_editor.day_context().is_none() {
            let context = self.day.live_time_context();
            let _ = self.annotation_editor.set_day_context(context);
        }
        self.annotation_editor.open_create(range, category);
        self.annotation_inputs_sync = InputSyncState::NeedsSync;
        self.ensure_categories_loaded_for_annotations(cx);
        self.hover = None;
        self.segment_menu = None;
        window.focus(&self.panel_focus, cx);
        cx.notify();
    }

    /// Directly creates a category-override annotation for `range` and active `category` (H15),
    /// then refreshes the shown day on success or surfaces the H13 write notice on failure.
    pub(super) fn quick_classify_range(
        &mut self,
        range: crate::link::UtcRange,
        category: wire::Uuid,
        cx: &mut Context<Self>,
    ) {
        if self.annotation_editor.day_context().is_none() {
            let context = self.day.live_time_context();
            let _ = self.annotation_editor.set_day_context(context);
        }
        let effect = self.annotation_editor.quick_classify(
            range,
            category,
            self.rules.categories(),
            generate_annotation_id,
        );
        self.run_annotation_editor_effect(effect, cx);
        cx.notify();
    }

    fn open_create_annotation_default(&mut self, cx: &mut Context<Self>) {
        if self.annotation_editor.day_context().is_none() {
            let context = self.day.live_time_context();
            let _ = self.annotation_editor.set_day_context(context);
        }
        self.annotation_editor.open_create_default();
        self.annotation_inputs_sync = InputSyncState::NeedsSync;
        self.ensure_categories_loaded_for_annotations(cx);
        cx.notify();
    }

    fn open_edit_annotation(&mut self, uuid: wire::AnnotationId, cx: &mut Context<Self>) {
        self.annotation_editor
            .open_edit(&uuid, self.rules.categories());
        self.annotation_inputs_sync = InputSyncState::NeedsSync;
        self.ensure_categories_loaded_for_annotations(cx);
        cx.notify();
    }

    fn retry_annotations_load(&mut self, cx: &mut Context<Self>) {
        let effect = self.annotation_editor.refresh();
        self.run_annotation_editor_effect(effect, cx);
        cx.notify();
    }

    fn submit_annotation_draft(&mut self, cx: &mut Context<Self>) {
        let effect = self.annotation_editor.submit(generate_annotation_id);
        self.run_annotation_editor_effect(effect, cx);
        cx.notify();
    }

    fn confirm_delete_annotation(&mut self, cx: &mut Context<Self>) {
        let effect = self.annotation_editor.confirm_delete();
        self.run_annotation_editor_effect(effect, cx);
        cx.notify();
    }

    fn run_annotation_editor_effect(
        &mut self,
        effect: AnnotationEditorEffect,
        cx: &mut Context<Self>,
    ) {
        match effect {
            AnnotationEditorEffect::None => {}
            AnnotationEditorEffect::Query {
                date,
                range,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.annotation_editor.apply_loaded(
                        generation,
                        date,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                let request = wire::QueryAnnotationsRequest {
                    start_utc_ms: range.start,
                    end_utc_ms: range.end,
                };
                cx.spawn(async move |this, cx| {
                    let result = link.query_annotations(request).await;
                    let _ = this.update(cx, |this, cx| {
                        this.annotation_editor
                            .apply_loaded(generation, date, result);
                        cx.notify();
                    });
                })
                .detach();
            }
            AnnotationEditorEffect::Create { request, date } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let _ = self.annotation_editor.apply_write_finished(
                        date,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.create_annotations(request).await.map(|_| ());
                    let _ = this.update(cx, |this, cx| {
                        let succeeded = result.is_ok();
                        let next = this.annotation_editor.apply_write_finished(date, result);
                        this.run_annotation_editor_effect(next, cx);
                        if succeeded && this.day.date() == date {
                            this.retry_day(cx);
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            AnnotationEditorEffect::Update { request, date } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let _ = self.annotation_editor.apply_write_finished(
                        date,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.update_annotations(request).await.map(|_| ());
                    let _ = this.update(cx, |this, cx| {
                        let succeeded = result.is_ok();
                        let next = this.annotation_editor.apply_write_finished(date, result);
                        this.run_annotation_editor_effect(next, cx);
                        if succeeded && this.day.date() == date {
                            this.retry_day(cx);
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            AnnotationEditorEffect::Delete { request, date } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let _ = self.annotation_editor.apply_write_finished(
                        date,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.delete_annotations(request).await.map(|_| ());
                    let _ = this.update(cx, |this, cx| {
                        let succeeded = result.is_ok();
                        let next = this.annotation_editor.apply_write_finished(date, result);
                        this.run_annotation_editor_effect(next, cx);
                        if succeeded && this.day.date() == date {
                            this.retry_day(cx);
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// Renders the `[Details | Annotations]` tab switcher inside Today's side panel whenever a
    /// live details subject is open.
    pub(super) fn render_today_panel_tabs(
        &self,
        ui: &Widgets<'_>,
        cx: &Context<Self>,
    ) -> Option<Div> {
        if self.details.is_none() || !matches!(self.day.source(), crate::model::DaySource::Live(_))
        {
            return None;
        }
        let selected = self.annotation_editor.active_tab();
        Some(
            div()
                .px(ui.metrics.space.l)
                .pb(ui.metrics.space.s)
                .border_b_1()
                .border_color(ui.palette.stroke_divider)
                .child(ui.segmented(
                    "today-side-panel-tabs",
                    [
                        Segment {
                            id: "today-panel-tab-details",
                            label: self.formatter.text(Msg::TodayAnnotationsTabDetails),
                            value: TodayPanelTab::Details,
                        },
                        Segment {
                            id: "today-panel-tab-annotations",
                            label: self.formatter.text(Msg::TodayAnnotationsTabAnnotations),
                            value: TodayPanelTab::Annotations,
                        },
                    ],
                    selected,
                    |tab| {
                        cx.listener(move |this, _, _, cx| {
                            this.switch_today_panel_tab(tab, cx);
                        })
                    },
                )),
        )
    }

    /// Renders the right-hand 360 px Today annotations side panel (list or create/edit editor).
    pub(super) fn render_annotations_panel(
        &self,
        ui: &Widgets<'_>,
        placement: PanelPlacement,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let palette = ui.palette;
        let (header, body, footer) = match self.annotation_editor.draft() {
            Some(draft) => (
                self.render_annotation_editor_header(ui, draft, cx),
                self.render_annotation_editor_body(ui, draft, cx),
                Some(self.render_annotation_editor_footer(ui, draft, cx)),
            ),
            None => (
                self.render_annotations_list_header(ui, cx),
                self.render_annotations_list_body(ui, cx),
                None,
            ),
        };

        div()
            .id("annotations-panel")
            .flex()
            .flex_col()
            .min_w(px(0.))
            .flex_shrink_0()
            .w(ui.metrics.layout.panel_width)
            .h_full()
            .bg(palette.fill_layer)
            .border_l_1()
            .border_color(palette.stroke_divider)
            .when(placement == PanelPlacement::Overlay, |panel| {
                panel.shadow(ui.shadows(&ui.metrics.shadow.overlay))
            })
            .track_focus(&self.panel_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.handle_annotations_escape(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .children(self.render_today_panel_tabs(ui, cx))
            .child(
                div()
                    .id("annotations-scroll")
                    .track_scroll(&self.details_scroll)
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .min_w(px(0.))
                    .p(ui.metrics.space.l)
                    .child(body),
            )
            .children(footer)
    }

    fn render_annotations_list_header(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let count_badge = self.annotation_editor.list().map(|list| {
            div()
                .px(px(BADGE_PADDING_X))
                .py(px(BADGE_PADDING_Y))
                .rounded(ui.metrics.radius.control)
                .bg(palette.fill_card_secondary)
                .border_1()
                .border_color(palette.stroke_card)
                .ramp(ui, Ramp::Caption)
                .tabular_figures()
                .text_color(palette.text_secondary)
                .child(list.items.len().to_string())
        });
        let can_create = self.annotation_editor.day_context().is_some();
        let new_label = self.formatter.text(Msg::TodayAnnotationsActionNew);
        let new_button = if can_create {
            ui.accent_button(
                "annotations-new",
                Density::Compact,
                ButtonContent::IconLabel(IconName::Plus, new_label),
            )
            .on_click(cx.listener(|this, _, _, cx| this.open_create_annotation_default(cx)))
        } else {
            ui.disabled_button(
                "annotations-new",
                Density::Compact,
                ButtonContent::IconLabel(IconName::Plus, new_label),
            )
        };

        div()
            .flex()
            .items_center()
            .gap(space.s)
            .p(space.l)
            .child(ui.icon(IconName::FileText, Density::Compact, palette.text_primary))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap(space.xs)
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .ramp(ui, Ramp::BodyStrong)
                            .child(self.formatter.text(Msg::TodayAnnotationsTitle)),
                    )
                    .children(count_badge),
            )
            .child(new_button)
            .child(
                ui.button(
                    "annotations-close",
                    Density::Compact,
                    ButtonContent::Icon(IconName::Close),
                )
                .on_click(cx.listener(|this, _, window, cx| this.close_details(window, cx))),
            )
    }

    fn render_annotations_list_body(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        match self.annotation_editor.list_state() {
            None | Some(LoadState::Loading) => {
                div()
                    .flex()
                    .flex_col()
                    .gap(space.s)
                    .children((0..ANNOTATION_SKELETON_ROWS).map(|_| {
                        ui.skeleton_block(SkeletonTone::Subtle)
                            .h(px(ANNOTATION_SKELETON_HEIGHT))
                            .w_full()
                            .rounded(ui.metrics.radius.overlay)
                    }))
            }
            Some(LoadState::Stale) => ui
                .card()
                .p(space.m)
                .flex()
                .flex_col()
                .gap(space.s)
                .child(
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text(Msg::TodayAnnotationsStaleMessage)),
                )
                .child(
                    div().child(
                        ui.button(
                            "annotations-retry-stale",
                            Density::Compact,
                            ButtonContent::Label(self.formatter.text(Msg::RetryDay)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.retry_annotations_load(cx))),
                    ),
                ),
            Some(LoadState::Failed(error)) => ui
                .card()
                .p(space.m)
                .flex()
                .flex_col()
                .gap(space.s)
                .child(
                    div()
                        .ramp(ui, Ramp::BodyStrong)
                        .child(self.formatter.text(Msg::TodayAnnotationsLoadFailedTitle)),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text(if error.is_busy() {
                            Msg::DayBusy
                        } else {
                            Msg::TodayAnnotationsLoadFailedMessage
                        })),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .font(ui.mono_font())
                        .text_color(palette.text_tertiary)
                        .child(error.code().to_string()),
                )
                .child(
                    div().child(
                        ui.button(
                            "annotations-retry-failed",
                            Density::Compact,
                            ButtonContent::Label(self.formatter.text(Msg::RetryDay)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.retry_annotations_load(cx))),
                    ),
                ),
            Some(LoadState::Ready(list)) => self.render_annotations_ready_list(ui, list, cx),
        }
    }

    fn render_annotations_ready_list(
        &self,
        ui: &Widgets<'_>,
        list: &AnnotationList,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        let truncated_banner = list.truncated.then(|| {
            ui.card()
                .p(space.m)
                .bg(palette.status_caution_bg)
                .border_color(
                    palette
                        .status_caution
                        .opacity(ui.metrics.opacity.notice_border),
                )
                .flex()
                .items_start()
                .gap(space.s)
                .child(ui.icon(
                    IconName::TriangleAlert,
                    Density::Compact,
                    palette.status_caution,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .whitespace_normal()
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_primary)
                        .child(self.formatter.text_with(
                            Msg::TodayAnnotationsTruncated,
                            &[Arg::Count.with(&list.items.len().to_string())],
                        )),
                )
        });

        if list.items.is_empty() {
            return div()
                .flex()
                .flex_col()
                .gap(space.m)
                .children(truncated_banner)
                .child(
                    ui.card()
                        .p(space.l)
                        .flex()
                        .flex_col()
                        .gap(space.s)
                        .child(
                            div()
                                .ramp(ui, Ramp::Body)
                                .text_color(palette.text_secondary)
                                .child(self.formatter.text(Msg::TodayAnnotationsEmpty)),
                        )
                        .child(
                            div().child(
                                ui.button(
                                    "annotations-empty-new",
                                    Density::Compact,
                                    ButtonContent::IconLabel(
                                        IconName::Plus,
                                        self.formatter.text(Msg::TodayAnnotationsActionNew),
                                    ),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.open_create_annotation_default(cx);
                                    },
                                )),
                            ),
                        ),
                );
        }

        let catalog = self.rules.categories();
        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .children(truncated_banner)
            .children(
                list.items
                    .iter()
                    .enumerate()
                    .map(|(index, row)| self.render_annotation_row(ui, index, row, catalog, cx)),
            )
    }

    fn render_annotation_row(
        &self,
        ui: &Widgets<'_>,
        index: usize,
        row: &SavedAnnotationRow,
        catalog: Option<&crate::model::CategoryCatalog>,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let time_range = self.format_annotation_range(row);
        let duration_label = u64::try_from(row.range.end.saturating_sub(row.range.start))
            .ok()
            .map(|ms| self.formatter.detail_duration(Duration::from_millis(ms)));
        let kind_label = self.formatter.text(annotation_kind_msg(row.kind));

        let category_row =
            row.category.as_ref().map(|category| {
                match resolve_annotation_category(category, catalog) {
                    AnnotationCategoryDisplay::Known {
                        name,
                        color,
                        archive_state,
                    } => div()
                        .flex()
                        .items_center()
                        .gap(space.xs)
                        .min_w(px(0.))
                        .child(ui.category_mark(ui.swatch(palette.category(color)), color))
                        .child(
                            div()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .ramp(ui, Ramp::Caption)
                                .text_color(palette.text_primary)
                                .child(self.formatter.category_name(&name)),
                        )
                        .when(archive_state == CategoryArchiveState::Archived, |row| {
                            row.child(
                                div()
                                    .px(px(BADGE_PADDING_X))
                                    .py(px(BADGE_PADDING_Y))
                                    .rounded(ui.metrics.radius.control)
                                    .bg(palette.fill_card_secondary)
                                    .border_1()
                                    .border_color(palette.stroke_card)
                                    .ramp(ui, Ramp::Caption)
                                    .text_color(palette.text_tertiary)
                                    .child(self.formatter.text(Msg::RulesCategoriesBadgeArchived)),
                            )
                        }),
                    AnnotationCategoryDisplay::Missing => div()
                        .flex()
                        .items_center()
                        .gap(space.xs)
                        .min_w(px(0.))
                        .child(ui.icon(
                            IconName::TriangleAlert,
                            Density::Compact,
                            palette.status_caution,
                        ))
                        .child(
                            div()
                                .min_w(px(0.))
                                .whitespace_normal()
                                .ramp(ui, Ramp::Caption)
                                .text_color(palette.status_caution)
                                .child(self.formatter.text(Msg::TodayAnnotationsCategoryMissing)),
                        ),
                }
            });

        let label_text = row.label.clone().or_else(|| {
            row.label_preset
                .map(|preset| self.formatter.text(Msg::from(preset)))
        });
        let note_excerpt = row.note.as_deref().and_then(format_note_excerpt);

        let uuid = row.uuid.clone();
        ui.card()
            .id(("annotation-row", index))
            .p(space.m)
            .flex()
            .flex_col()
            .min_w(px(0.))
            .gap(space.xs)
            .cursor_pointer()
            .hover(ui.subtle_hover())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_edit_annotation(uuid.clone(), cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(space.xs)
                    .child(
                        div()
                            .px(px(BADGE_PADDING_X))
                            .py(px(BADGE_PADDING_Y))
                            .rounded(ui.metrics.radius.control)
                            .bg(palette.fill_card_secondary)
                            .border_1()
                            .border_color(palette.stroke_card)
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(kind_label),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .font(ui.mono_font())
                            .tabular_figures()
                            .text_color(palette.text_primary)
                            .child(time_range),
                    )
                    .children(duration_label.map(|duration| {
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_tertiary)
                            .child(duration)
                    })),
            )
            .children(category_row)
            .children(label_text.map(|label| {
                div()
                    .min_w(px(0.))
                    .w_full()
                    .whitespace_normal()
                    .ramp(ui, Ramp::BodyStrong)
                    .text_color(palette.text_primary)
                    .child(label)
            }))
            .children(note_excerpt.map(|excerpt| {
                div()
                    .min_w(px(0.))
                    .w_full()
                    .whitespace_normal()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(excerpt)
            }))
    }

    fn format_annotation_range(&self, row: &SavedAnnotationRow) -> String {
        if let Some(ctx) = self.annotation_editor.day_context() {
            let start = ctx.format_endpoint_hm(row.range.start, EndpointRole::Start);
            let end = ctx.format_endpoint_hm(row.range.end, EndpointRole::End);
            return format!("{start} – {end}");
        }
        if matches!(self.day.source(), crate::model::DaySource::Live(_)) {
            return format!(
                "{} – {}",
                self.clock_utc(row.range.start),
                self.clock_utc(row.range.end)
            );
        }
        String::new()
    }

    fn render_annotation_editor_header(
        &self,
        ui: &Widgets<'_>,
        draft: &AnnotationDraft,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let busy = self.annotation_editor.mutating() == MutationActivity::Busy;
        let title_msg = match &draft.mode {
            AnnotationEditorMode::Create { .. } => Msg::TodayAnnotationsEditorCreateTitle,
            AnnotationEditorMode::Edit { .. } => Msg::TodayAnnotationsEditorEditTitle,
        };
        let back_button = if busy {
            ui.disabled_button(
                "annotation-editor-back",
                Density::Compact,
                ButtonContent::Icon(IconName::ChevronLeft),
            )
        } else {
            ui.button(
                "annotation-editor-back",
                Density::Compact,
                ButtonContent::Icon(IconName::ChevronLeft),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.annotation_editor.close_draft();
                cx.notify();
            }))
        };
        let close_button = if busy {
            ui.disabled_button(
                "annotation-editor-close",
                Density::Compact,
                ButtonContent::Icon(IconName::Close),
            )
        } else {
            ui.button(
                "annotation-editor-close",
                Density::Compact,
                ButtonContent::Icon(IconName::Close),
            )
            .on_click(cx.listener(|this, _, window, cx| this.close_details(window, cx)))
        };

        div()
            .flex()
            .items_center()
            .gap(space.s)
            .p(space.l)
            .child(back_button)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .ramp(ui, Ramp::BodyStrong)
                    .child(self.formatter.text(title_msg)),
            )
            .child(close_button)
    }

    /// Renders the H13 write-error notice banner when a direct quick-classify write (H15) fails
    /// outside the annotation editor form.
    pub(super) fn render_quick_annotation_notice(
        &self,
        ui: &Widgets<'_>,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let notice = self.annotation_editor.quick_notice()?;
        Some(
            self.render_annotation_write_notice(ui, notice).child(
                ui.button(
                    "quick-annotation-notice-dismiss",
                    Density::Compact,
                    ButtonContent::Icon(IconName::Close),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.annotation_editor.clear_quick_notice();
                    cx.notify();
                })),
            ),
        )
    }

    fn render_annotation_write_notice(
        &self,
        ui: &Widgets<'_>,
        notice: &AnnotationWriteNotice,
    ) -> Div {
        let space = &ui.metrics.space;
        let colors = ui.info_bar_colors(InfoBarStyle::Critical);
        let msg = match notice {
            AnnotationWriteNotice::BadRequest { .. } => Msg::TodayAnnotationsErrorBadRequest,
            AnnotationWriteNotice::InvalidRange { .. } => Msg::TodayAnnotationsErrorInvalidRange,
            AnnotationWriteNotice::Stale { .. } => Msg::TodayAnnotationsErrorStale,
            AnnotationWriteNotice::Busy { .. } => Msg::TodayAnnotationsErrorBusy,
            AnnotationWriteNotice::Generic { .. } => Msg::TodayAnnotationsErrorGeneric,
            AnnotationWriteNotice::CategoryUnavailable { .. } => {
                Msg::TodayAnnotationsErrorCategoryUnavailable
            }
        };
        ui.card()
            .p(space.m)
            .bg(colors.background)
            .border_color(colors.border)
            .text_color(colors.foreground)
            .flex()
            .items_start()
            .gap(space.s)
            .child(ui.icon(IconName::CircleX, Density::Compact, colors.icon))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(space.xxs)
                    .child(
                        div()
                            .whitespace_normal()
                            .ramp(ui, Ramp::Caption)
                            .child(self.formatter.text(msg)),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .font(ui.mono_font())
                            .text_color(colors.secondary)
                            .child(notice.code().to_string()),
                    ),
            )
    }

    fn render_annotation_editor_body(
        &self,
        ui: &Widgets<'_>,
        draft: &AnnotationDraft,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let busy = self.annotation_editor.mutating() == MutationActivity::Busy;
        let is_dismissed = draft.mode.kind() == wire::AnnotationKind::DismissedAway;

        let mutation_banner = draft
            .mutation_error
            .as_ref()
            .map(|notice| self.render_annotation_write_notice(ui, notice));

        let validation_banner = draft.validation_error.map(|error| {
            let text = self.format_validation_error(error);
            div()
                .p(space.s)
                .rounded(ui.metrics.radius.control)
                .bg(palette.status_critical_bg)
                .border_1()
                .border_color(
                    palette
                        .status_critical
                        .opacity(ui.metrics.opacity.notice_border),
                )
                .ramp(ui, Ramp::Caption)
                .text_color(palette.status_critical)
                .whitespace_normal()
                .child(text)
        });

        let kind_section = self.render_kind_field(ui, draft, busy, cx);
        let time_section = self.render_time_fields(ui, draft, busy, cx);

        let category_section =
            (!is_dismissed).then(|| self.render_category_picker(ui, draft, busy, cx));
        let label_section = (!is_dismissed).then(|| self.render_label_field(ui, draft, busy));
        let note_section = (!is_dismissed).then(|| self.render_note_field(ui, draft, busy));

        let delete_confirm_banner = matches!(
            draft.mode,
            AnnotationEditorMode::Edit {
                delete_confirm: DeleteConfirmState::Confirming,
                ..
            }
        )
        .then(|| self.render_delete_confirm_card(ui, busy, cx));

        div()
            .flex()
            .flex_col()
            .min_w(px(0.))
            .gap(space.m)
            .children(mutation_banner)
            .children(validation_banner)
            .child(kind_section)
            .child(time_section)
            .children(category_section)
            .children(label_section)
            .children(note_section)
            .children(delete_confirm_banner)
    }

    fn render_kind_field(
        &self,
        ui: &Widgets<'_>,
        draft: &AnnotationDraft,
        busy: bool,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        match &draft.mode {
            AnnotationEditorMode::Create { kind, .. } => {
                let control = ui.segmented(
                    "annotation-kind",
                    [
                        Segment {
                            id: "annotation-kind-category",
                            label: self
                                .formatter
                                .text(Msg::TodayAnnotationsKindCategoryOverride),
                            value: wire::AnnotationKind::CategoryOverride,
                        },
                        Segment {
                            id: "annotation-kind-offline",
                            label: self
                                .formatter
                                .text(Msg::TodayAnnotationsKindOfflineActivity),
                            value: wire::AnnotationKind::OfflineActivity,
                        },
                        Segment {
                            id: "annotation-kind-note",
                            label: self.formatter.text(Msg::TodayAnnotationsKindNote),
                            value: wire::AnnotationKind::Note,
                        },
                    ],
                    *kind,
                    |next_kind| {
                        cx.listener(move |this, _, _, cx| {
                            if !busy {
                                this.annotation_editor.set_kind(next_kind);
                                cx.notify();
                            }
                        })
                    },
                );
                div()
                    .flex()
                    .flex_col()
                    .gap(space.xs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::TodayAnnotationsEditorKindLabel)),
                    )
                    .child(control)
            }
            AnnotationEditorMode::Edit { kind, .. } => div()
                .p(space.s)
                .rounded(ui.metrics.radius.control)
                .bg(palette.fill_card_secondary)
                .border_1()
                .border_color(palette.stroke_card)
                .flex()
                .flex_wrap()
                .items_center()
                .gap(space.xs)
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text(Msg::TodayAnnotationsEditorKindLabel)),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::BodyStrong)
                        .text_color(palette.text_primary)
                        .child(self.formatter.text(annotation_kind_msg(*kind))),
                )
                .child(
                    div()
                        .px(px(BADGE_PADDING_X))
                        .py(px(BADGE_PADDING_Y))
                        .rounded(ui.metrics.radius.control)
                        .bg(palette.fill_card)
                        .border_1()
                        .border_color(palette.stroke_card)
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_tertiary)
                        .child(self.formatter.text(Msg::TodayAnnotationsEditorKindFixed)),
                ),
        }
    }

    fn render_time_fields(
        &self,
        ui: &Widgets<'_>,
        draft: &AnnotationDraft,
        busy: bool,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        let start_control = render_single_line_input(
            ui,
            self.annotation_start_input.as_ref(),
            &draft.start_input,
            busy,
        );
        let end_control = render_single_line_input(
            ui,
            self.annotation_end_input.as_ref(),
            &draft.end_input,
            busy,
        );

        let start_fold = self
            .annotation_editor
            .day_context()
            .and_then(|ctx| ctx.fold_offsets_for_input(&draft.start_input, EndpointRole::Start))
            .map(|(before, after)| {
                self.render_fold_selector(
                    ui,
                    "annotation-start-fold",
                    EndpointRole::Start,
                    before,
                    after,
                    draft.start_fold,
                    busy,
                    cx,
                )
            });
        let end_fold = self
            .annotation_editor
            .day_context()
            .and_then(|ctx| ctx.fold_offsets_for_input(&draft.end_input, EndpointRole::End))
            .map(|(before, after)| {
                self.render_fold_selector(
                    ui,
                    "annotation-end-fold",
                    EndpointRole::End,
                    before,
                    after,
                    draft.end_fold,
                    busy,
                    cx,
                )
            });

        div()
            .flex()
            .flex_col()
            .gap(space.xs)
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(self.formatter.text(Msg::TodayAnnotationsEditorTimeRange)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(space.s)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(space.xxs)
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .text_color(palette.text_tertiary)
                                    .child(
                                        self.formatter.text(Msg::TodayAnnotationsEditorStartLabel),
                                    ),
                            )
                            .child(start_control)
                            .children(start_fold),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(space.xxs)
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .text_color(palette.text_tertiary)
                                    .child(
                                        self.formatter.text(Msg::TodayAnnotationsEditorEndLabel),
                                    ),
                            )
                            .child(end_control)
                            .children(end_fold),
                    ),
            )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "renders a DST fold offset selector from explicit endpoint bounds and offsets"
    )]
    fn render_fold_selector(
        &self,
        ui: &Widgets<'_>,
        id: &'static str,
        role: EndpointRole,
        before: jiff::tz::Offset,
        after: jiff::tz::Offset,
        selected: FoldChoice,
        busy: bool,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let (earlier_id, later_id) = match role {
            EndpointRole::Start => (
                "annotation-start-fold-earlier",
                "annotation-start-fold-later",
            ),
            EndpointRole::End => ("annotation-end-fold-earlier", "annotation-end-fold-later"),
        };
        ui.segmented(
            id,
            [
                Segment {
                    id: earlier_id,
                    label: DayTimeContext::format_utc_offset(before),
                    value: FoldChoice::Earlier,
                },
                Segment {
                    id: later_id,
                    label: DayTimeContext::format_utc_offset(after),
                    value: FoldChoice::Later,
                },
            ],
            selected,
            |choice| {
                cx.listener(move |this, _, _, cx| {
                    if !busy {
                        this.annotation_editor.set_fold_choice(role, choice);
                        cx.notify();
                    }
                })
            },
        )
    }

    fn render_category_picker(
        &self,
        ui: &Widgets<'_>,
        draft: &AnnotationDraft,
        busy: bool,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        let (archived_note, tray) = match self.rules.categories_state() {
            LoadState::Loading => (
                None,
                div()
                    .p(space.s)
                    .rounded(ui.metrics.radius.control)
                    .bg(palette.bg_base)
                    .border_1()
                    .border_color(palette.stroke_card)
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(
                        self.formatter
                            .text(Msg::TodayAnnotationsEditorCategoryLoading),
                    ),
            ),
            LoadState::Stale | LoadState::Failed(_) => (
                None,
                div()
                    .p(space.s)
                    .rounded(ui.metrics.radius.control)
                    .bg(palette.bg_base)
                    .border_1()
                    .border_color(palette.stroke_card)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space.s)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::RulesCategoriesLoadFailedTitle)),
                    )
                    .child(
                        ui.button(
                            "annotation-reload-categories",
                            Density::Compact,
                            ButtonContent::Label(self.formatter.text(Msg::RetryDay)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let effect = this.rules.reload_categories();
                            this.run_rules_effect(effect, cx);
                            cx.notify();
                        })),
                    ),
            ),
            LoadState::Ready(catalog) => {
                let archived_count = catalog.archived_items().len();
                let archived_note = (archived_count > 0).then(|| {
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_tertiary)
                        .child(self.formatter.text_with(
                            Msg::TodayAnnotationsEditorCategoryArchivedNote,
                            &[Arg::Count.with(&archived_count.to_string())],
                        ))
                });
                let none_selected = draft.category.is_none();
                let none_chip = div()
                    .id("annotation-cat-none")
                    .flex()
                    .items_center()
                    .gap(space.xs)
                    .min_h(px(CATEGORY_CHIP_HEIGHT))
                    .px(space.s)
                    .rounded(ui.metrics.radius.control)
                    .bg(if none_selected {
                        palette.fill_card
                    } else {
                        palette.fill_control
                    })
                    .border(if none_selected {
                        px(SELECTED_CHIP_BORDER)
                    } else {
                        ui.metrics.layout.divider
                    })
                    .border_color(if none_selected {
                        palette.accent_fill
                    } else {
                        palette.stroke_control
                    })
                    .ramp(ui, Ramp::Caption)
                    .text_color(if busy {
                        palette.text_disabled
                    } else {
                        palette.text_primary
                    })
                    .when(!busy, |chip| {
                        chip.cursor_pointer()
                            .hover(ui.subtle_hover())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.annotation_editor.set_category(None);
                                cx.notify();
                            }))
                    })
                    .child(self.formatter.text(Msg::TodayAnnotationsEditorCategoryNone))
                    .when(none_selected, |chip| {
                        chip.child(ui.icon(IconName::Check, Density::Compact, palette.accent_fill))
                    });

                let chips =
                    catalog
                        .active_items()
                        .into_iter()
                        .enumerate()
                        .map(|(index, item)| {
                            let selected = draft.category.as_ref() == Some(&item.uuid);
                            let target_uuid = item.uuid.clone();
                            div()
                                .id(("annotation-cat-opt", index))
                                .flex()
                                .items_center()
                                .gap(space.xs)
                                .min_h(px(CATEGORY_CHIP_HEIGHT))
                                .px(space.s)
                                .max_w_full()
                                .rounded(ui.metrics.radius.control)
                                .bg(if selected {
                                    palette.fill_card
                                } else {
                                    palette.fill_control
                                })
                                .border(if selected {
                                    px(SELECTED_CHIP_BORDER)
                                } else {
                                    ui.metrics.layout.divider
                                })
                                .border_color(if selected {
                                    palette.accent_fill
                                } else {
                                    palette.stroke_control
                                })
                                .ramp(ui, Ramp::Caption)
                                .text_color(if busy {
                                    palette.text_disabled
                                } else {
                                    palette.text_primary
                                })
                                .when(!busy, |chip| {
                                    chip.cursor_pointer().hover(ui.subtle_hover()).on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.annotation_editor
                                                .set_category(Some(target_uuid.clone()));
                                            cx.notify();
                                        }),
                                    )
                                })
                                .child(ui.category_mark(
                                    ui.swatch(palette.category(item.color)),
                                    item.color,
                                ))
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(self.formatter.category_name(&item.display_name)),
                                )
                                .when(selected, |chip| {
                                    chip.child(ui.icon(
                                        IconName::Check,
                                        Density::Compact,
                                        palette.accent_fill,
                                    ))
                                })
                        });

                let tray = div()
                    .p(space.s)
                    .rounded(ui.metrics.radius.control)
                    .bg(palette.bg_base)
                    .border_1()
                    .border_color(palette.stroke_card)
                    .flex()
                    .flex_wrap()
                    .gap(space.xs)
                    .child(none_chip)
                    .children(chips);
                (archived_note, tray)
            }
        };

        div()
            .flex()
            .flex_col()
            .gap(space.xs)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .justify_between()
                    .gap(space.xs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(
                                self.formatter
                                    .text(Msg::TodayAnnotationsEditorCategoryLabel),
                            ),
                    )
                    .children(archived_note),
            )
            .child(tray)
    }

    fn render_label_field(&self, ui: &Widgets<'_>, draft: &AnnotationDraft, busy: bool) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let count = draft.label_input.chars().count();
        let over_limit = count > wire::ANNOTATION_MAX_LABEL_SCALARS;
        let counter_text = self.formatter.text_with(
            Msg::TodayAnnotationsEditorScalarCount,
            &[
                Arg::Count.with(&count.to_string()),
                Arg::Value.with(&wire::ANNOTATION_MAX_LABEL_SCALARS.to_string()),
            ],
        );
        let control = render_single_line_input(
            ui,
            self.annotation_label_input.as_ref(),
            &draft.label_input,
            busy,
        );

        div()
            .flex()
            .flex_col()
            .gap(space.xs)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .gap(space.s)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::TodayAnnotationsEditorLabelField)),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .tabular_figures()
                            .text_color(if over_limit {
                                palette.status_critical
                            } else {
                                palette.text_tertiary
                            })
                            .child(counter_text),
                    ),
            )
            .child(control)
    }

    fn render_note_field(&self, ui: &Widgets<'_>, draft: &AnnotationDraft, busy: bool) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let count = draft.note_input.chars().count();
        let over_limit = count > wire::ANNOTATION_MAX_NOTE_SCALARS;
        let counter_text = self.formatter.text_with(
            Msg::TodayAnnotationsEditorScalarCount,
            &[
                Arg::Count.with(&count.to_string()),
                Arg::Value.with(&wire::ANNOTATION_MAX_NOTE_SCALARS.to_string()),
            ],
        );
        let control = match &self.annotation_note_input {
            Some(editor_state) => Editor::new(editor_state)
                .h(px(ANNOTATION_NOTE_HEIGHT))
                .disabled(busy)
                .into_any_element(),
            None => div()
                .w_full()
                .h(px(ANNOTATION_NOTE_HEIGHT))
                .p(space.s)
                .rounded(ui.metrics.radius.control)
                .bg(palette.fill_card_secondary)
                .border_1()
                .border_color(palette.stroke_control)
                .ramp(ui, Ramp::Body)
                .text_color(palette.text_secondary)
                .child(draft.note_input.clone())
                .into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .gap(space.xs)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .gap(space.s)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::TodayAnnotationsEditorNoteField)),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .tabular_figures()
                            .text_color(if over_limit {
                                palette.status_critical
                            } else {
                                palette.text_tertiary
                            })
                            .child(counter_text),
                    ),
            )
            .child(control)
    }

    fn render_delete_confirm_card(&self, ui: &Widgets<'_>, busy: bool, cx: &Context<Self>) -> Div {
        let space = &ui.metrics.space;
        let colors = ui.info_bar_colors(InfoBarStyle::Critical);
        let cancel_label = self.formatter.text(Msg::TodayAnnotationsEditorActionCancel);
        let confirm_label = self
            .formatter
            .text(Msg::TodayAnnotationsEditorDeleteConfirmConfirm);

        let cancel_btn = if busy {
            ui.disabled_button(
                "annotation-delete-cancel",
                Density::Compact,
                ButtonContent::Label(cancel_label),
            )
        } else {
            ui.button(
                "annotation-delete-cancel",
                Density::Compact,
                ButtonContent::Label(cancel_label),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.annotation_editor.cancel_delete();
                cx.notify();
            }))
        };

        let confirm_btn = if busy {
            ui.disabled_button(
                "annotation-delete-confirm",
                Density::Compact,
                ButtonContent::Label(confirm_label),
            )
        } else {
            ui.button(
                "annotation-delete-confirm",
                Density::Compact,
                ButtonContent::Label(confirm_label),
            )
            .when(ui.palette.contrast == ContrastMode::Standard, |btn| {
                btn.bg(ui.palette.status_critical)
                    .border_color(ui.palette.status_critical)
                    .text_color(ui.palette.text_on_critical)
            })
            .on_click(cx.listener(|this, _, _, cx| this.confirm_delete_annotation(cx)))
        };

        ui.card()
            .p(space.m)
            .bg(colors.background)
            .border_color(colors.border)
            .text_color(colors.foreground)
            .flex()
            .flex_col()
            .gap(space.s)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(space.s)
                    .child(ui.icon(IconName::TriangleAlert, Density::Compact, colors.icon))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .whitespace_normal()
                            .ramp(ui, Ramp::Caption)
                            .child(
                                self.formatter
                                    .text(Msg::TodayAnnotationsEditorDeleteConfirmPrompt),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(space.s)
                    .child(cancel_btn)
                    .child(confirm_btn),
            )
    }

    fn render_annotation_editor_footer(
        &self,
        ui: &Widgets<'_>,
        draft: &AnnotationDraft,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let busy = self.annotation_editor.mutating() == MutationActivity::Busy;

        let save_msg = if busy {
            Msg::TodayAnnotationsEditorActionSaving
        } else if draft.mutation_error.is_some() {
            Msg::TodayAnnotationsEditorActionRetrySave
        } else {
            Msg::TodayAnnotationsEditorActionSave
        };
        let save_button = if busy {
            ui.disabled_button(
                "annotation-save",
                Density::Compact,
                ButtonContent::Label(self.formatter.text(save_msg)),
            )
        } else {
            ui.accent_button(
                "annotation-save",
                Density::Compact,
                ButtonContent::Label(self.formatter.text(save_msg)),
            )
            .on_click(cx.listener(|this, _, _, cx| this.submit_annotation_draft(cx)))
        };

        let cancel_label = self.formatter.text(Msg::TodayAnnotationsEditorActionCancel);
        let cancel_button = if busy {
            ui.disabled_button(
                "annotation-cancel",
                Density::Compact,
                ButtonContent::Label(cancel_label),
            )
        } else {
            ui.button(
                "annotation-cancel",
                Density::Compact,
                ButtonContent::Label(cancel_label),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.annotation_editor.close_draft();
                cx.notify();
            }))
        };

        let delete_button = matches!(draft.mode, AnnotationEditorMode::Edit { .. }).then(|| {
            let delete_label = self.formatter.text(Msg::TodayAnnotationsEditorActionDelete);
            if busy {
                ui.disabled_button(
                    "annotation-delete",
                    Density::Compact,
                    ButtonContent::IconLabel(IconName::Trash, delete_label),
                )
            } else {
                ui.button(
                    "annotation-delete",
                    Density::Compact,
                    ButtonContent::IconLabel(IconName::Trash, delete_label),
                )
                .text_color(palette.status_critical)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.annotation_editor.request_delete();
                    cx.notify();
                }))
            }
        });

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(space.s)
            .px(space.l)
            .py(space.m)
            .border_t_1()
            .border_color(palette.stroke_divider)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space.s)
                    .min_w(px(0.))
                    .child(save_button)
                    .child(cancel_button),
            )
            .children(delete_button)
    }

    fn format_validation_error(&self, error: ValidateAnnotationDraftError) -> String {
        match error {
            ValidateAnnotationDraftError::Time {
                source: ValidateAnnotationTimeError::InvalidFormat,
            } => self
                .formatter
                .text(Msg::TodayAnnotationsValidationInvalidTime),
            ValidateAnnotationDraftError::Time {
                source: ValidateAnnotationTimeError::DstGap,
            } => self.formatter.text(Msg::TodayAnnotationsValidationDstGap),
            ValidateAnnotationDraftError::Time {
                source: ValidateAnnotationTimeError::OutsideDay,
            } => self
                .formatter
                .text(Msg::TodayAnnotationsValidationOutsideDay),
            ValidateAnnotationDraftError::Time {
                source: ValidateAnnotationTimeError::EndNotAfterStart,
            } => self
                .formatter
                .text(Msg::TodayAnnotationsValidationEndBeforeStart),
            ValidateAnnotationDraftError::LabelTooLong => self.formatter.text_with(
                Msg::TodayAnnotationsValidationLabelTooLong,
                &[Arg::Count.with(&wire::ANNOTATION_MAX_LABEL_SCALARS.to_string())],
            ),
            ValidateAnnotationDraftError::NoteTooLong => self.formatter.text_with(
                Msg::TodayAnnotationsValidationNoteTooLong,
                &[Arg::Count.with(&wire::ANNOTATION_MAX_NOTE_SCALARS.to_string())],
            ),
            ValidateAnnotationDraftError::CategoryRequired => self
                .formatter
                .text(Msg::TodayAnnotationsValidationCategoryRequired),
            ValidateAnnotationDraftError::NoteContentRequired => self
                .formatter
                .text(Msg::TodayAnnotationsValidationNoteContentRequired),
            ValidateAnnotationDraftError::MissingDayContext => {
                self.formatter.text(Msg::TodayAnnotationsValidationNoDay)
            }
        }
    }
}

fn render_single_line_input(
    ui: &Widgets<'_>,
    input: Option<&gpui_kit::Entity<InputState>>,
    fallback_text: &str,
    busy: bool,
) -> AnyElement {
    let space = &ui.metrics.space;
    let palette = ui.palette;
    match (input, busy) {
        (Some(input_state), false) => Input::new(input_state).into_any_element(),
        _ => div()
            .w_full()
            .min_h(ui.metrics.density(Density::Standard).control_height)
            .px(space.s)
            .py(space.xs)
            .rounded(ui.metrics.radius.control)
            .bg(palette.fill_control_disabled)
            .border_1()
            .border_color(palette.stroke_control)
            .ramp(ui, Ramp::Body)
            .text_color(palette.text_disabled)
            .child(fallback_text.to_owned())
            .into_any_element(),
    }
}

const fn annotation_kind_msg(kind: wire::AnnotationKind) -> Msg {
    match kind {
        wire::AnnotationKind::CategoryOverride => Msg::TodayAnnotationsKindCategoryOverride,
        wire::AnnotationKind::OfflineActivity => Msg::TodayAnnotationsKindOfflineActivity,
        wire::AnnotationKind::ActiveOverride => Msg::TodayAnnotationsKindActiveOverride,
        wire::AnnotationKind::DismissedAway => Msg::TodayAnnotationsKindDismissedAway,
        wire::AnnotationKind::Note => Msg::TodayAnnotationsKindNote,
    }
}

fn generate_annotation_id() -> Option<wire::AnnotationId> {
    match windows::core::GUID::new().map(|id| format!("{id:?}")) {
        Ok(text) => match text.parse() {
            Ok(uuid) => Some(wire::AnnotationId(uuid)),
            Err(error) => {
                tracing::warn!(%error, "could not parse annotation identity");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, "could not create annotation identity");
            None
        }
    }
}
