//! Rule-set text editor pane: create/edit/override flows, multi-line monospace editor, debounced
//! validation with localized diagnostics, 7-day impact preview, stale reload, and dirty-leave
//! confirmation.

use std::{collections::BTreeMap, time::Duration};

use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::input::{Editor, EditorState, Input, InputEvent, InputState},
    div,
    prelude::FluentBuilder as _,
    px,
};
use quartz_protocol::{
    ErrorKind, PREVIEW_MAX_SAMPLES, RuleDiagnostic, RuleKind, RulePreviewSample,
    RulePreviewStatistics, RuleSetIdentityField, RuleSetSource, RuleView, RulesPreviewResponse,
};

use super::{AppShell, InputSyncState};
use crate::{
    app::{
        metrics::{Density, Ramp},
        tokens::ContrastMode,
        widgets::{ButtonContent, InfoBarStyle, RampExt as _, Segment, Widgets},
    },
    format::{Arg, Msg},
    layout::ChartArrangement,
    link::{LoadDataError, LoadDataErrorCode, RequestCoreError},
    model::{
        DirtyState, EditorLoadState, LeaveConfirmation, LeaveDecision, LeaveTarget,
        MutationActivity, PreviewState, RuleEditorEffect, RuleEditorMode, RuleEditorSession,
        SaveOutcome, StaleNotice, VALIDATE_DEBOUNCE_DELAY, ValidateRuleSetKeyError,
        ValidationState, valid_parents_for_kind,
    },
};

/// Height of the multi-line monospace TOML editor box in logical pixels.
const EDITOR_TEXTAREA_HEIGHT: f32 = 300.;
/// Maximum height of the scrollable preview samples list in logical pixels.
const PREVIEW_SAMPLES_MAX_HEIGHT: f32 = 320.;
/// Compact pill badge horizontal padding.
const EDITOR_BADGE_PADDING_X: f32 = 6.;
/// Compact pill badge vertical padding.
const EDITOR_BADGE_PADDING_Y: f32 = 2.;
/// Horizontal indentation per hierarchy depth inside the parent picker.
const PARENT_PICKER_INDENT_STEP: f32 = 12.;
/// Maximum hierarchy depth applied to parent picker indentation.
const MAX_PARENT_INDENT_DEPTH: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorBadgeTone {
    Neutral,
    Accent,
    Caution,
    Critical,
}

impl AppShell {
    /// Synchronizes GPUI input entities with pending programmatic model updates (such as async
    /// export loads, stale reloads, or template switches) during render where `Window` is live.
    pub(super) fn sync_rule_editor_inputs_if_needed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rule_editor_key_sync == InputSyncState::NeedsSync {
            self.rule_editor_key_sync = InputSyncState::Synced;
            if let Some(session) = self.rule_editor.session()
                && let RuleEditorMode::CreateNew { key, .. } = &session.mode
            {
                let initial = key.clone();
                let placeholder = self.formatter.text(Msg::RulesEditorKeyPlaceholder);
                self.sync_rule_editor_key_input(&initial, placeholder, window, cx);
            }
        }
        if self.rule_editor_text_sync == InputSyncState::NeedsSync {
            self.rule_editor_text_sync = InputSyncState::Synced;
            if let Some(session) = self.rule_editor.session() {
                let initial = session.text().to_owned();
                let placeholder = self.formatter.text(Msg::RulesEditorTextPlaceholder);
                self.sync_rule_editor_text_input(&initial, placeholder, window, cx);
            }
        }
    }

    fn sync_rule_editor_key_input(
        &mut self,
        initial: &str,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.rule_editor_key_input.clone() {
            input.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx);
                state.set_value(initial.to_owned(), window, cx);
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
                let before_text = this
                    .rule_editor
                    .session()
                    .map(|session| session.text().to_owned());
                let effect = this.rule_editor.set_new_key(value, this.rules.tree());
                let after_text = this
                    .rule_editor
                    .session()
                    .map(|session| session.text().to_owned());
                if before_text != after_text {
                    this.rule_editor_text_sync = InputSyncState::NeedsSync;
                }
                this.run_rule_editor_effect(effect, cx);
                cx.notify();
            }
        })
        .detach();
        self.rule_editor_key_input = Some(input);
    }

    fn sync_rule_editor_text_input(
        &mut self,
        initial: &str,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.rule_editor_text_input.clone() {
            editor.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx);
                state.set_value(initial.to_owned(), window, cx);
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
                let effect = this.rule_editor.set_text(value);
                this.run_rule_editor_effect(effect, cx);
                cx.notify();
            }
        })
        .detach();
        self.rule_editor_text_input = Some(editor);
    }

    /// Opens the text editor for an existing user rule set and loads its canonical text.
    pub(super) fn open_edit_rule_set(
        &mut self,
        key: String,
        kind: RuleKind,
        base_version: u32,
        cx: &mut Context<Self>,
    ) {
        self.rule_editor_validate_task = None;
        self.pending_leave_page = None;
        let effect = self.rule_editor.open_edit(key, kind, base_version);
        self.rule_editor_text_sync = InputSyncState::NeedsSync;
        self.run_rule_editor_effect(effect, cx);
        cx.notify();
    }

    /// Opens the text editor in create mode for a brand-new `user.<slug>` rule set.
    pub(super) fn open_new_rule_set(&mut self, cx: &mut Context<Self>) {
        self.rule_editor_validate_task = None;
        self.pending_leave_page = None;
        let default_kind = self
            .rules
            .selected_node()
            .map_or(RuleKind::Classification, |node| node.kind);
        self.rule_editor.open_new(default_kind, self.rules.tree());
        self.rule_editor_key_sync = InputSyncState::NeedsSync;
        self.rule_editor_text_sync = InputSyncState::NeedsSync;
        cx.notify();
    }

    /// Opens the text editor to create a user child set overriding or disabling a built-in rule.
    pub(super) fn open_override_child_rule_set(
        &mut self,
        parent: String,
        kind: RuleKind,
        rule_id: String,
        target: String,
        cx: &mut Context<Self>,
    ) {
        self.rule_editor_validate_task = None;
        self.pending_leave_page = None;
        let effect =
            self.rule_editor
                .open_override_child(parent, kind, &rule_id, target, self.rules.tree());
        self.rule_editor_key_sync = InputSyncState::NeedsSync;
        self.rule_editor_text_sync = InputSyncState::NeedsSync;
        self.run_rule_editor_effect(effect, cx);
        cx.notify();
    }

    /// Guards navigation or closing while the rule editor is open with unsaved edits.
    pub(super) fn leave_rule_editor_or(&mut self, target: LeaveTarget, cx: &mut Context<Self>) {
        match self.rule_editor.request_leave(target) {
            LeaveDecision::Proceed(target) => {
                self.rule_editor_validate_task = None;
                self.pending_leave_page = None;
                self.apply_leave_target(target, cx);
            }
            LeaveDecision::NeedsConfirm => {
                cx.notify();
            }
            LeaveDecision::Busy => {}
        }
    }

    /// Requests closing the rule editor (asks inline confirm if dirty).
    pub(super) fn request_close_rule_editor(&mut self, cx: &mut Context<Self>) {
        self.leave_rule_editor_or(LeaveTarget::CloseEditor, cx);
    }

    /// Cancels a pending unsaved-change confirmation and stays in the editor.
    pub(super) fn cancel_leave_rule_editor(&mut self, cx: &mut Context<Self>) {
        self.pending_leave_page = None;
        self.rule_editor.cancel_leave();
        cx.notify();
    }

    fn confirm_leave_rule_editor(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.rule_editor.confirm_leave() else {
            return;
        };
        self.rule_editor_validate_task = None;
        self.apply_leave_target(target, cx);
    }

    fn apply_leave_target(&mut self, target: LeaveTarget, cx: &mut Context<Self>) {
        match target {
            LeaveTarget::CloseEditor => {
                self.pending_leave_page = None;
                cx.notify();
            }
            LeaveTarget::SelectRuleSet(key) => {
                self.pending_leave_page = None;
                let effect = self.rules.select(key);
                self.run_rules_effect(effect, cx);
                cx.notify();
            }
            LeaveTarget::SwitchTab(tab) => {
                self.pending_leave_page = None;
                self.rules.set_active_tab(tab);
                cx.notify();
            }
            LeaveTarget::LeavePage => {
                if let Some(page) = self.pending_leave_page.take() {
                    self.page = page;
                    self.hover = None;
                    self.cancel_details(cx);
                }
                cx.notify();
            }
        }
    }

    fn set_rule_editor_kind(&mut self, kind: RuleKind, cx: &mut Context<Self>) {
        let before_text = self
            .rule_editor
            .session()
            .map(|session| session.text().to_owned());
        let effect = self.rule_editor.set_new_kind(kind, self.rules.tree());
        let after_text = self
            .rule_editor
            .session()
            .map(|session| session.text().to_owned());
        if before_text != after_text {
            self.rule_editor_text_sync = InputSyncState::NeedsSync;
        }
        self.run_rule_editor_effect(effect, cx);
        cx.notify();
    }

    fn set_rule_editor_parent(&mut self, parent: Option<String>, cx: &mut Context<Self>) {
        let before_text = self
            .rule_editor
            .session()
            .map(|session| session.text().to_owned());
        let effect = self.rule_editor.set_new_parent(parent, self.rules.tree());
        let after_text = self
            .rule_editor
            .session()
            .map(|session| session.text().to_owned());
        if before_text != after_text {
            self.rule_editor_text_sync = InputSyncState::NeedsSync;
        }
        self.run_rule_editor_effect(effect, cx);
        cx.notify();
    }

    fn retry_rule_editor_load(&mut self, cx: &mut Context<Self>) {
        let effect = self.rule_editor.retry_load();
        self.run_rule_editor_effect(effect, cx);
        cx.notify();
    }

    fn reload_rule_editor_stale(&mut self, cx: &mut Context<Self>) {
        self.rule_editor_validate_task = None;
        let effect = self.rule_editor.reload_after_stale();
        let tree_effect = self.rules.reload_tree();
        self.run_rules_effect(tree_effect, cx);
        self.run_rule_editor_effect(effect, cx);
        cx.notify();
    }

    fn preview_rule_editor(&mut self, cx: &mut Context<Self>) {
        let today = self
            .day_snapshot()
            .map(|snapshot| snapshot.logical_date)
            .unwrap_or_else(|| self.day.date());
        let effect = self.rule_editor.request_preview(today);
        self.run_rule_editor_effect(effect, cx);
        cx.notify();
    }

    fn save_rule_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(key_input) = self.rule_editor_key_input.as_ref()
            && matches!(
                self.rule_editor.session().map(|session| &session.mode),
                Some(RuleEditorMode::CreateNew { .. })
            )
        {
            let value = key_input.read(cx).value().to_string();
            let _ = self.rule_editor.set_new_key(value, self.rules.tree());
        }
        if let Some(text_input) = self.rule_editor_text_input.as_ref()
            && matches!(
                self.rule_editor
                    .session()
                    .map(|session| &session.load_state),
                Some(EditorLoadState::Ready)
            )
        {
            let value = text_input.read(cx).value().to_string();
            let _ = self.rule_editor.set_text(value);
        }
        let effect = self.rule_editor.submit_save(self.rules.tree());
        self.run_rule_editor_effect(effect, cx);
        cx.notify();
    }

    fn run_rule_editor_effect(&mut self, effect: RuleEditorEffect, cx: &mut Context<Self>) {
        match effect {
            RuleEditorEffect::None => {}
            RuleEditorEffect::ExportText { key, generation } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let next = self.rule_editor.apply_export_loaded(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    self.run_rule_editor_effect(next, cx);
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.export_rule_set_text(key).await;
                    let _ = this.update(cx, |this, cx| {
                        let loaded_ok = result.is_ok();
                        let next = this.rule_editor.apply_export_loaded(generation, result);
                        if loaded_ok {
                            this.rule_editor_text_sync = InputSyncState::NeedsSync;
                        }
                        this.run_rule_editor_effect(next, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            RuleEditorEffect::ReloadStale { key, generation } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let next = self.rule_editor.apply_stale_reloaded(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    self.run_rule_editor_effect(next, cx);
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = async {
                        let detail = link.get_rule_set(key.clone(), RuleView::Own).await?;
                        let exported = link.export_rule_set_text(key).await?;
                        if detail.versions != exported.versions {
                            return Err(LoadDataError::Stale);
                        }
                        Ok((detail.version, exported))
                    }
                    .await;
                    let _ = this.update(cx, |this, cx| {
                        let loaded_ok = result.is_ok();
                        let next = this.rule_editor.apply_stale_reloaded(generation, result);
                        if loaded_ok {
                            this.rule_editor_text_sync = InputSyncState::NeedsSync;
                        }
                        this.run_rule_editor_effect(next, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            RuleEditorEffect::ScheduleValidate { generation } => {
                let delay = cx.background_executor().timer(VALIDATE_DEBOUNCE_DELAY);
                self.rule_editor_validate_task = Some(cx.spawn(async move |this, cx| {
                    delay.await;
                    let _ = this.update(cx, |this, cx| {
                        let next = this.rule_editor.trigger_validate(generation);
                        this.run_rule_editor_effect(next, cx);
                        cx.notify();
                    });
                }));
            }
            RuleEditorEffect::Validate {
                request,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.rule_editor.apply_validation_finished(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.validate_rule_set_text(request).await;
                    let _ = this.update(cx, |this, cx| {
                        this.rule_editor
                            .apply_validation_finished(generation, result);
                        cx.notify();
                    });
                })
                .detach();
            }
            RuleEditorEffect::Preview {
                request,
                buffer_generation,
                preview_generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.rule_editor.apply_preview_finished(
                        buffer_generation,
                        preview_generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.preview_rules(request).await;
                    let _ = this.update(cx, |this, cx| {
                        this.rule_editor.apply_preview_finished(
                            buffer_generation,
                            preview_generation,
                            result,
                        );
                        cx.notify();
                    });
                })
                .detach();
            }
            RuleEditorEffect::Save {
                request,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let _ = self.rule_editor.apply_save_finished(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.edit_rule_set(request).await;
                    let _ = this.update(cx, |this, cx| {
                        match this.rule_editor.apply_save_finished(generation, result) {
                            SaveOutcome::Saved { key } => {
                                this.rule_editor_validate_task = None;
                                this.pending_leave_page = None;
                                let next = this.rules.reload_tree_selecting(key);
                                this.run_rules_effect(next, cx);
                            }
                            SaveOutcome::Ignored | SaveOutcome::Failed => {}
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// Renders the rule-set text editor pane (header, notices, create metadata, monospace editor,
    /// diagnostics list, action bar, and 7-day preview).
    pub(super) fn render_rule_editor_pane(
        &self,
        ui: &Widgets<'_>,
        arrangement: ChartArrangement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let card = ui
            .card()
            .id("rules-text-editor-pane")
            .flex()
            .flex_col()
            .gap(space.m)
            .p(space.l)
            .map(|card| match arrangement {
                ChartArrangement::SideBySide => card.flex_1().min_w(px(0.)),
                ChartArrangement::Stacked => card.w_full(),
            });

        let Some(session) = self.rule_editor.session() else {
            return card.into_any_element();
        };

        let header = self.render_editor_header(ui, session, cx);
        let notices = self.render_editor_notices(ui, session, cx);

        let body = match &session.load_state {
            EditorLoadState::Loading => div()
                .py(space.l)
                .ramp(ui, Ramp::Body)
                .text_color(ui.palette.text_secondary)
                .child(self.formatter.text(Msg::RulesEditorLoading))
                .into_any_element(),
            EditorLoadState::Failed(error) => self
                .render_editor_load_failed(ui, error, cx)
                .into_any_element(),
            EditorLoadState::Ready => self
                .render_editor_ready_body(ui, session, cx)
                .into_any_element(),
        };

        card.child(header)
            .children(notices)
            .child(body)
            .into_any_element()
    }

    fn render_editor_header(
        &self,
        ui: &Widgets<'_>,
        session: &RuleEditorSession,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let busy = session.saving == MutationActivity::Busy;
        let (title, kind, version_badge) = match &session.mode {
            RuleEditorMode::EditExisting {
                key,
                kind,
                base_version,
            } => (
                self.formatter
                    .text_with(Msg::RulesEditorTitleEdit, &[Arg::Name.with(key)]),
                *kind,
                Some(self.formatter.text_with(
                    Msg::RulesVersion,
                    &[Arg::Value.with(&base_version.to_string())],
                )),
            ),
            RuleEditorMode::CreateNew { kind, .. } => {
                (self.formatter.text(Msg::RulesEditorTitleNew), *kind, None)
            }
        };

        let close_btn = if busy {
            ui.disabled_button(
                "rule-editor-close",
                Density::Compact,
                ButtonContent::IconLabel(
                    IconName::Close,
                    self.formatter.text(Msg::RulesEditorActionClose),
                ),
            )
        } else {
            ui.button(
                "rule-editor-close",
                Density::Compact,
                ButtonContent::IconLabel(
                    IconName::Close,
                    self.formatter.text(Msg::RulesEditorActionClose),
                ),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.request_close_rule_editor(cx);
            }))
        };

        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(space.s)
            .pb(space.s)
            .border_b_1()
            .border_color(palette.stroke_divider)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.xs)
                    .child(ui.icon(
                        editor_rule_kind_icon(kind),
                        Density::Standard,
                        palette.text_primary,
                    ))
                    .child(
                        div()
                            .ramp(ui, Ramp::Subtitle)
                            .text_color(palette.text_primary)
                            .child(title),
                    )
                    .children(
                        version_badge
                            .map(|text| Self::editor_badge(ui, text, EditorBadgeTone::Neutral)),
                    )
                    .when(session.dirty == DirtyState::Dirty, |row| {
                        row.child(Self::editor_badge(
                            ui,
                            self.formatter.text(Msg::RulesEditorBadgeDirty),
                            EditorBadgeTone::Caution,
                        ))
                    }),
            )
            .child(close_btn)
    }

    fn render_editor_notices(
        &self,
        ui: &Widgets<'_>,
        session: &RuleEditorSession,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let space = &ui.metrics.space;
        let mut notices = Vec::new();

        if matches!(session.leave_confirm, LeaveConfirmation::Pending { .. }) {
            let colors = ui.info_bar_colors(InfoBarStyle::Critical);
            notices.push(
                ui.card()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(space.s)
                    .p(space.m)
                    .bg(colors.background)
                    .border_color(colors.border)
                    .text_color(colors.foreground)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(space.s)
                            .flex_1()
                            .min_w(px(0.))
                            .child(ui.icon(IconName::TriangleAlert, Density::Compact, colors.icon))
                            .child(
                                div()
                                    .ramp(ui, Ramp::BodyStrong)
                                    .child(self.formatter.text(Msg::RulesEditorUnsavedPrompt)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(space.s)
                            .child(
                                ui.accent_button(
                                    "rule-editor-unsaved-discard",
                                    Density::Compact,
                                    ButtonContent::Label(
                                        self.formatter.text(Msg::RulesEditorUnsavedDiscard),
                                    ),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.confirm_leave_rule_editor(cx);
                                    },
                                )),
                            )
                            .child(
                                ui.button(
                                    "rule-editor-unsaved-stay",
                                    Density::Compact,
                                    ButtonContent::Label(
                                        self.formatter.text(Msg::RulesEditorUnsavedStay),
                                    ),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.cancel_leave_rule_editor(cx);
                                    },
                                )),
                            ),
                    )
                    .into_any_element(),
            );
        }

        if let Some(StaleNotice { current_version }) = session.stale_notice {
            let colors = ui.info_bar_colors(InfoBarStyle::Critical);
            notices.push(
                ui.card()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(space.s)
                    .p(space.m)
                    .bg(colors.background)
                    .border_color(colors.border)
                    .text_color(colors.foreground)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(space.s)
                            .flex_1()
                            .min_w(px(0.))
                            .child(ui.icon(IconName::TriangleAlert, Density::Compact, colors.icon))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(space.xxs)
                                    .child(
                                        div()
                                            .ramp(ui, Ramp::BodyStrong)
                                            .child(self.formatter.text(Msg::RulesEditorStaleTitle)),
                                    )
                                    .child(div().ramp(ui, Ramp::Body).child(
                                        self.formatter.text_with(
                                            Msg::RulesEditorStaleMessage,
                                            &[Arg::Value.with(&current_version.to_string())],
                                        ),
                                    )),
                            ),
                    )
                    .child(
                        ui.accent_button(
                            "rule-editor-reload-stale",
                            Density::Compact,
                            ButtonContent::Label(self.formatter.text(Msg::RulesActionReload)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reload_rule_editor_stale(cx);
                        })),
                    )
                    .into_any_element(),
            );
        }

        if let Some(error) = &session.save_error {
            let msg = match error.code() {
                LoadDataErrorCode::Protocol(ErrorKind::RuleSetReadOnly { .. }) => {
                    Msg::RulesMutationFailedReadonly
                }
                LoadDataErrorCode::Protocol(ErrorKind::RuleSetIdentity {
                    field: RuleSetIdentityField::Kind,
                    ..
                }) => Msg::RulesEditorSaveFailedIdentityKind,
                LoadDataErrorCode::Protocol(ErrorKind::RuleSetIdentity {
                    field: RuleSetIdentityField::Parent,
                    ..
                }) => Msg::RulesEditorSaveFailedIdentityParent,
                LoadDataErrorCode::Protocol(ErrorKind::Busy) => Msg::DayBusy,
                _ => Msg::RulesEditorSaveFailedGeneric,
            };
            let colors = ui.info_bar_colors(InfoBarStyle::Critical);
            notices.push(
                ui.card()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(space.s)
                    .p(space.m)
                    .bg(colors.background)
                    .border_color(colors.border)
                    .text_color(colors.foreground)
                    .child(ui.icon(IconName::TriangleAlert, Density::Compact, colors.icon))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(space.xxs)
                            .child(div().ramp(ui, Ramp::Body).child(self.formatter.text(msg)))
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .font(ui.mono_font())
                                    .text_color(colors.secondary)
                                    .child(error.code().to_string()),
                            ),
                    )
                    .into_any_element(),
            );
        }

        notices
    }

    fn render_editor_load_failed(
        &self,
        ui: &Widgets<'_>,
        error: &LoadDataError,
        cx: &Context<Self>,
    ) -> Div {
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
                            .ramp(ui, Ramp::Body)
                            .text_color(ui.palette.text_primary)
                            .child(self.formatter.text(if error.is_busy() {
                                Msg::DayBusy
                            } else {
                                Msg::RulesEditorLoadFailed
                            })),
                    ),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .font(ui.mono_font())
                    .text_color(ui.palette.text_tertiary)
                    .child(error.code().to_string()),
            )
            .child(
                div().child(
                    ui.button(
                        "rule-editor-retry-load",
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::RetryDay)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.retry_rule_editor_load(cx))),
                ),
            )
    }

    fn render_editor_ready_body(
        &self,
        ui: &Widgets<'_>,
        session: &RuleEditorSession,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let busy = session.saving == MutationActivity::Busy;

        let create_fields = match &session.mode {
            RuleEditorMode::CreateNew {
                key,
                kind,
                parent,
                key_error,
                ..
            } => Some(self.render_create_metadata_fields(
                ui,
                key,
                *kind,
                parent.as_deref(),
                *key_error,
                busy,
                cx,
            )),
            RuleEditorMode::EditExisting { .. } => None,
        };

        let text_control = match &self.rule_editor_text_input {
            Some(editor_state) => Editor::new(editor_state)
                .h(px(EDITOR_TEXTAREA_HEIGHT))
                .font(ui.mono_font())
                .disabled(busy)
                .into_any_element(),
            None => div()
                .w_full()
                .h(px(EDITOR_TEXTAREA_HEIGHT))
                .p(space.s)
                .rounded(ui.metrics.radius.control)
                .bg(palette.fill_card_secondary)
                .border_1()
                .border_color(palette.stroke_control)
                .font(ui.mono_font())
                .ramp(ui, Ramp::Caption)
                .text_color(palette.text_secondary)
                .child(session.text().to_owned())
                .into_any_element(),
        };

        let validation_section = self.render_validation_section(ui, session);
        let action_bar = self.render_editor_action_bar(ui, session, cx);
        let preview_section = self.render_preview_section(ui, session);

        div()
            .flex()
            .flex_col()
            .gap(space.m)
            .children(create_fields)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space.xs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::RulesEditorTextLabel)),
                    )
                    .child(text_control),
            )
            .child(validation_section)
            .child(action_bar)
            .children(preview_section)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "renders all metadata controls for create mode from disjoint borrowed fields"
    )]
    fn render_create_metadata_fields(
        &self,
        ui: &Widgets<'_>,
        key: &str,
        kind: RuleKind,
        selected_parent: Option<&str>,
        key_error: Option<ValidateRuleSetKeyError>,
        busy: bool,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        let kind_control = ui.segmented(
            "rule-editor-kind",
            [
                Segment {
                    id: "rule-editor-kind-classification",
                    label: self.formatter.text(Msg::RulesKindClassification),
                    value: RuleKind::Classification,
                },
                Segment {
                    id: "rule-editor-kind-extraction",
                    label: self.formatter.text(Msg::RulesKindExtraction),
                    value: RuleKind::Extraction,
                },
                Segment {
                    id: "rule-editor-kind-privacy",
                    label: self.formatter.text(Msg::RulesKindPrivacy),
                    value: RuleKind::Privacy,
                },
            ],
            kind,
            |next_kind| {
                cx.listener(move |this, _, _, cx| {
                    this.set_rule_editor_kind(next_kind, cx);
                })
            },
        );

        let key_control = match (&self.rule_editor_key_input, busy) {
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
                .font(ui.mono_font())
                .ramp(ui, Ramp::Body)
                .text_color(palette.text_disabled)
                .child(key.to_owned())
                .into_any_element(),
        };

        let key_error_line = key_error.map(|error| {
            let msg = match error {
                ValidateRuleSetKeyError::Empty => Msg::RulesEditorKeyErrorEmpty,
                ValidateRuleSetKeyError::MissingUserPrefix => Msg::RulesEditorKeyErrorPrefix,
                ValidateRuleSetKeyError::MissingSlug => Msg::RulesEditorKeyErrorMissingSlug,
                ValidateRuleSetKeyError::InvalidCharacters => Msg::RulesEditorKeyErrorInvalidChars,
                ValidateRuleSetKeyError::AlreadyExists => Msg::RulesEditorKeyErrorExists,
            };
            div()
                .ramp(ui, Ramp::Caption)
                .text_color(palette.status_critical)
                .child(self.formatter.text(msg))
        });

        let parent_options = self
            .rules
            .tree()
            .map_or_else(Vec::new, |tree| valid_parents_for_kind(tree, kind));

        let none_selected = selected_parent.is_none();
        let none_label = self.formatter.text(Msg::RulesEditorParentNone);
        let none_button = if busy {
            ui.disabled_button(
                "rule-editor-parent-none",
                Density::Compact,
                ButtonContent::Label(none_label),
            )
        } else if none_selected {
            ui.accent_button(
                "rule-editor-parent-none",
                Density::Compact,
                ButtonContent::Label(none_label),
            )
        } else {
            ui.button(
                "rule-editor-parent-none",
                Density::Compact,
                ButtonContent::Label(none_label),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.set_rule_editor_parent(None, cx);
            }))
        };

        let parent_buttons = parent_options
            .into_iter()
            .enumerate()
            .map(|(index, option)| {
                let is_selected = selected_parent == Some(option.key.as_str());
                let clamped = option.depth.min(MAX_PARENT_INDENT_DEPTH);
                let indent =
                    f32::from(u16::try_from(clamped).unwrap_or(0)) * PARENT_PICKER_INDENT_STEP;
                let suffix = match option.source {
                    RuleSetSource::Builtin => self.formatter.text(Msg::RulesSourceBuiltin),
                    RuleSetSource::User => self.formatter.text(Msg::RulesSourceUser),
                };
                let label = format!("{} ({suffix})", option.key);
                let parent_key = option.key;
                let button = if busy {
                    ui.disabled_button(
                        ("rule-editor-parent-opt", index),
                        Density::Compact,
                        ButtonContent::Label(label),
                    )
                } else if is_selected {
                    ui.accent_button(
                        ("rule-editor-parent-opt", index),
                        Density::Compact,
                        ButtonContent::Label(label),
                    )
                } else {
                    ui.button(
                        ("rule-editor-parent-opt", index),
                        Density::Compact,
                        ButtonContent::Label(label),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_rule_editor_parent(Some(parent_key.clone()), cx);
                    }))
                };
                div().pl(px(indent)).child(button)
            });

        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .p(space.m)
            .rounded(ui.metrics.radius.control)
            .bg(palette.fill_card_secondary)
            .border_1()
            .border_color(palette.stroke_card)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space.xs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::RulesEditorKindLabel)),
                    )
                    .child(kind_control),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space.xs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::RulesEditorKeyLabel)),
                    )
                    .child(key_control)
                    .children(key_error_line),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space.xs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::RulesEditorParentLabel)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(space.xs)
                            .child(none_button)
                            .children(parent_buttons),
                    ),
            )
    }

    fn render_validation_section(&self, ui: &Widgets<'_>, session: &RuleEditorSession) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        let mut section = div().flex().flex_col().gap(space.xs);
        match &session.validation {
            ValidationState::Idle => {}
            ValidationState::Debouncing => {
                section = section.child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text(Msg::RulesEditorValidationPending)),
                );
            }
            ValidationState::Validating => {
                section = section.child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text(Msg::RulesEditorValidationChecking)),
                );
            }
            ValidationState::Valid => {
                section = section.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(space.xs)
                        .child(ui.icon(IconName::Check, Density::Compact, palette.accent_fill))
                        .child(
                            div()
                                .ramp(ui, Ramp::Caption)
                                .text_color(palette.text_primary)
                                .child(self.formatter.text(Msg::RulesEditorValidationValid)),
                        ),
                );
            }
            ValidationState::Invalid {
                diagnostics,
                truncated,
            } => {
                section = section.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(space.xs)
                        .child(ui.icon(
                            IconName::CircleX,
                            Density::Compact,
                            palette.status_critical,
                        ))
                        .child(
                            div()
                                .ramp(ui, Ramp::BodyStrong)
                                .text_color(palette.status_critical)
                                .child(self.formatter.text_with(
                                    Msg::RulesEditorValidationInvalidCount,
                                    &[Arg::Count.with(&diagnostics.len().to_string())],
                                )),
                        ),
                );
                if *truncated {
                    section = section.child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text_with(
                                Msg::RulesEditorDiagnosticsTruncated,
                                &[Arg::Count.with(&diagnostics.len().to_string())],
                            )),
                    );
                }
                section =
                    section.children(diagnostics.iter().enumerate().map(|(index, diagnostic)| {
                        self.render_diagnostic_row(ui, index, diagnostic)
                    }));
            }
            ValidationState::Failed(error) => {
                let colors = ui.info_bar_colors(InfoBarStyle::Critical);
                section = section.child(
                    ui.card()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(space.s)
                        .p(space.s)
                        .bg(colors.background)
                        .border_color(colors.border)
                        .text_color(colors.foreground)
                        .child(ui.icon(IconName::TriangleAlert, Density::Compact, colors.icon))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(space.xxs)
                                .child(div().ramp(ui, Ramp::Caption).child(self.formatter.text(
                                    if error.is_busy() {
                                        Msg::DayBusy
                                    } else {
                                        Msg::RulesEditorValidationFailed
                                    },
                                )))
                                .child(
                                    div()
                                        .ramp(ui, Ramp::Caption)
                                        .font(ui.mono_font())
                                        .text_color(colors.secondary)
                                        .child(error.code().to_string()),
                                ),
                        ),
                );
            }
        }
        section
    }

    fn render_diagnostic_row(
        &self,
        ui: &Widgets<'_>,
        index: usize,
        diagnostic: &RuleDiagnostic,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let location = diagnostic.location.as_ref().map(|loc| {
            self.formatter.text_with(
                Msg::RulesEditorDiagnosticLocation,
                &[
                    Arg::Line.with(&loc.line.to_string()),
                    Arg::Column.with(&loc.column.to_string()),
                ],
            )
        });
        let message = self.formatter.rule_diagnostic_message(diagnostic);

        div()
            .id(("rule-editor-diag", index))
            .flex()
            .flex_col()
            .gap(space.xxs)
            .p(space.s)
            .rounded(ui.metrics.radius.control)
            .bg(palette.fill_card_secondary)
            .border_1()
            .border_color(palette.stroke_card)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.xs)
                    .child(Self::editor_badge(
                        ui,
                        diagnostic.code.clone(),
                        EditorBadgeTone::Critical,
                    ))
                    .children(location.map(|text| {
                        div()
                            .ramp(ui, Ramp::Caption)
                            .tabular_figures()
                            .text_color(palette.text_secondary)
                            .child(text)
                    }))
                    .when(!diagnostic.path.is_empty(), |row| {
                        row.child(
                            div()
                                .ramp(ui, Ramp::Caption)
                                .font(ui.mono_font())
                                .text_color(palette.text_tertiary)
                                .child(diagnostic.path.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(palette.text_primary)
                    .whitespace_normal()
                    .child(message),
            )
    }

    fn render_editor_action_bar(
        &self,
        ui: &Widgets<'_>,
        session: &RuleEditorSession,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let can_preview = self.rule_editor.can_preview();
        let can_save = self.rule_editor.can_save();
        let saving = session.saving == MutationActivity::Busy;

        let preview_content = ButtonContent::IconLabel(
            IconName::Clock,
            self.formatter.text(Msg::RulesEditorActionPreview),
        );
        let preview_btn = if can_preview {
            ui.button("rule-editor-preview", Density::Compact, preview_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.preview_rule_editor(cx);
                }))
        } else {
            ui.disabled_button("rule-editor-preview", Density::Compact, preview_content)
        };

        let save_label = if saving {
            self.formatter.text(Msg::RulesEditorActionSaving)
        } else {
            self.formatter.text(Msg::RulesEditorActionSave)
        };
        let save_content = ButtonContent::IconLabel(IconName::Check, save_label);
        let save_btn = if can_save {
            ui.accent_button("rule-editor-save", Density::Compact, save_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.save_rule_editor(cx);
                }))
        } else {
            ui.disabled_button("rule-editor-save", Density::Compact, save_content)
        };

        let cancel_content =
            ButtonContent::IconLabel(IconName::Close, self.formatter.text(Msg::RulesActionCancel));
        let cancel_btn = if saving {
            ui.disabled_button("rule-editor-cancel", Density::Compact, cancel_content)
        } else {
            ui.button("rule-editor-cancel", Density::Compact, cancel_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.request_close_rule_editor(cx);
                }))
        };

        let preview_hint = (!can_preview && !matches!(session.preview, PreviewState::Loading))
            .then(|| {
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_tertiary)
                    .child(self.formatter.text(Msg::RulesPreviewHintValidateFirst))
            });

        div()
            .flex()
            .flex_col()
            .gap(space.xs)
            .pt(space.s)
            .border_t_1()
            .border_color(palette.stroke_divider)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.s)
                    .child(save_btn)
                    .child(preview_btn)
                    .child(cancel_btn),
            )
            .children(preview_hint)
    }

    fn render_preview_section(&self, ui: &Widgets<'_>, session: &RuleEditorSession) -> Option<Div> {
        let space = &ui.metrics.space;
        let palette = ui.palette;

        match &session.preview {
            PreviewState::Idle => None,
            PreviewState::Loading => Some(
                div()
                    .p(space.m)
                    .rounded(ui.metrics.radius.control)
                    .bg(palette.fill_card_secondary)
                    .border_1()
                    .border_color(palette.stroke_card)
                    .ramp(ui, Ramp::Body)
                    .text_color(palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesPreviewLoading)),
            ),
            PreviewState::Failed(error) => {
                let colors = ui.info_bar_colors(InfoBarStyle::Critical);
                Some(
                    ui.card()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(space.s)
                        .p(space.m)
                        .bg(colors.background)
                        .border_color(colors.border)
                        .text_color(colors.foreground)
                        .child(ui.icon(IconName::TriangleAlert, Density::Compact, colors.icon))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(space.xxs)
                                .child(div().ramp(ui, Ramp::Body).child(self.formatter.text(
                                    if error.is_busy() {
                                        Msg::DayBusy
                                    } else {
                                        Msg::RulesPreviewFailed
                                    },
                                )))
                                .child(
                                    div()
                                        .ramp(ui, Ramp::Caption)
                                        .font(ui.mono_font())
                                        .text_color(colors.secondary)
                                        .child(error.code().to_string()),
                                ),
                        ),
                )
            }
            PreviewState::Ready(response) => {
                Some(self.render_preview_results(ui, session, response))
            }
        }
    }

    fn render_preview_results(
        &self,
        ui: &Widgets<'_>,
        session: &RuleEditorSession,
        response: &RulesPreviewResponse,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let range_label = match session.preview_range {
            Some((start, end)) => {
                let last_day = end.yesterday().unwrap_or(end);
                format!("{start} – {last_day}")
            }
            None => String::new(),
        };

        let before_dur = self
            .formatter
            .detail_duration(ms_to_duration(response.totals.before.active_ms));
        let after_dur = self
            .formatter
            .detail_duration(ms_to_duration(response.totals.after.active_ms));

        let summary_row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(space.xs)
            .child(Self::editor_badge(
                ui,
                self.formatter.text_with(
                    Msg::RulesPreviewSummaryBefore,
                    &[Arg::Duration.with(&before_dur)],
                ),
                EditorBadgeTone::Neutral,
            ))
            .child(Self::editor_badge(
                ui,
                self.formatter.text_with(
                    Msg::RulesPreviewSummaryAfter,
                    &[Arg::Duration.with(&after_dur)],
                ),
                EditorBadgeTone::Accent,
            ))
            .child(Self::editor_badge(
                ui,
                self.formatter.text_with(
                    Msg::RulesPreviewSummaryDocumentChanges,
                    &[Arg::Count.with(&response.document_change_count.to_string())],
                ),
                EditorBadgeTone::Neutral,
            ))
            .child(Self::editor_badge(
                ui,
                self.formatter.text_with(
                    Msg::RulesPreviewSummaryFurtherHidden,
                    &[Arg::Count.with(&response.further_hidden_count.to_string())],
                ),
                if response.further_hidden_count > 0 {
                    EditorBadgeTone::Caution
                } else {
                    EditorBadgeTone::Neutral
                },
            ));

        let category_rows = self.render_preview_category_totals(ui, response);
        let rules_block = self.render_preview_rules(ui, &response.rules);
        let samples_block =
            self.render_preview_samples(ui, &response.samples, response.samples_truncated);

        div()
            .flex()
            .flex_col()
            .gap(space.m)
            .p(space.m)
            .rounded(ui.metrics.radius.control)
            .bg(palette.fill_card_secondary)
            .border_1()
            .border_color(palette.stroke_card)
            .child(
                div()
                    .ramp(ui, Ramp::BodyStrong)
                    .text_color(palette.text_primary)
                    .child(
                        self.formatter
                            .text_with(Msg::RulesPreviewHeading, &[Arg::Range.with(&range_label)]),
                    ),
            )
            .child(summary_row)
            .children(category_rows)
            .child(rules_block)
            .child(samples_block)
    }

    fn render_preview_category_totals(
        &self,
        ui: &Widgets<'_>,
        response: &RulesPreviewResponse,
    ) -> Option<Div> {
        let mut totals: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for rank in &response.totals.before.categories {
            let name = self.formatter.preview_category_ref(&rank.category);
            let entry = totals.entry(name).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(rank.active_ms);
        }
        for rank in &response.totals.after.categories {
            let name = self.formatter.preview_category_ref(&rank.category);
            let entry = totals.entry(name).or_insert((0, 0));
            entry.1 = entry.1.saturating_add(rank.active_ms);
        }
        if totals.is_empty() {
            return None;
        }
        let space = &ui.metrics.space;
        let palette = ui.palette;
        Some(
            div()
                .flex()
                .flex_col()
                .gap(space.xs)
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text(Msg::RulesPreviewCategoriesHeading)),
                )
                .child(div().flex().flex_row().flex_wrap().gap(space.xs).children(
                    totals.into_iter().map(|(name, (before_ms, after_ms))| {
                        let value = format!(
                            "{} → {}",
                            self.formatter.detail_duration(ms_to_duration(before_ms)),
                            self.formatter.detail_duration(ms_to_duration(after_ms)),
                        );
                        Self::editor_badge(
                            ui,
                            self.formatter.text_with(
                                Msg::RulesPreviewCategoriesBeforeAfter,
                                &[Arg::Name.with(&name), Arg::Value.with(&value)],
                            ),
                            if before_ms != after_ms {
                                EditorBadgeTone::Caution
                            } else {
                                EditorBadgeTone::Neutral
                            },
                        )
                    }),
                )),
        )
    }

    fn render_preview_rules(&self, ui: &Widgets<'_>, rules: &[RulePreviewStatistics]) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let mut block = div().flex().flex_col().gap(space.xs).child(
            div()
                .ramp(ui, Ramp::Caption)
                .text_color(palette.text_secondary)
                .child(self.formatter.text_with(
                    Msg::RulesPreviewRulesHeading,
                    &[Arg::Count.with(&rules.len().to_string())],
                )),
        );
        if rules.is_empty() {
            return block.child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_tertiary)
                    .child(self.formatter.text(Msg::RulesPreviewRulesEmpty)),
            );
        }
        block = block.children(rules.iter().enumerate().map(|(index, stat)| {
            let matched = self
                .formatter
                .detail_duration(ms_to_duration(stat.matched_ms));
            let effective = self
                .formatter
                .detail_duration(ms_to_duration(stat.effective_ms));
            let stats_line = self.formatter.text_with(
                Msg::RulesPreviewRuleStats,
                &[
                    Arg::Count.with(&stat.matched_count.to_string()),
                    Arg::Duration.with(&matched),
                    Arg::Value.with(&effective),
                ],
            );
            div()
                .id(("rule-preview-stat", index))
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(space.s)
                .px(space.s)
                .py(space.xs)
                .rounded(ui.metrics.radius.control)
                .bg(palette.fill_card)
                .border_1()
                .border_color(palette.stroke_card)
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .font(ui.mono_font())
                        .text_color(palette.text_primary)
                        .child(stat.rule.clone()),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .tabular_figures()
                        .text_color(palette.text_secondary)
                        .child(stats_line),
                )
        }));
        block
    }

    fn render_preview_samples(
        &self,
        ui: &Widgets<'_>,
        samples: &[RulePreviewSample],
        truncated: bool,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let visible_count = samples.len().min(PREVIEW_MAX_SAMPLES);
        let mut block = div().flex().flex_col().gap(space.xs).child(
            div()
                .ramp(ui, Ramp::Caption)
                .text_color(palette.text_secondary)
                .child(self.formatter.text_with(
                    Msg::RulesPreviewSamplesHeading,
                    &[Arg::Count.with(&visible_count.to_string())],
                )),
        );
        if truncated {
            block = block.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.xs)
                    .px(space.s)
                    .py(space.xs)
                    .rounded(ui.metrics.radius.control)
                    .bg(palette.fill_subtle_hover)
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(ui.icon(
                        IconName::TriangleAlert,
                        Density::Compact,
                        palette.status_caution,
                    ))
                    .child(self.formatter.text_with(
                        Msg::RulesPreviewSamplesTruncated,
                        &[Arg::Count.with(&visible_count.to_string())],
                    )),
            );
        }
        if visible_count == 0 {
            return block.child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_tertiary)
                    .child(self.formatter.text(Msg::RulesPreviewSamplesEmpty)),
            );
        }
        block.child(
            div()
                .id("rule-preview-samples-scroll")
                .flex()
                .flex_col()
                .gap(space.xs)
                .max_h(px(PREVIEW_SAMPLES_MAX_HEIGHT))
                .overflow_y_scroll()
                .children(
                    samples
                        .iter()
                        .take(PREVIEW_MAX_SAMPLES)
                        .enumerate()
                        .map(|(index, sample)| self.render_preview_sample_row(ui, index, sample)),
                ),
        )
    }

    fn render_preview_sample_row(
        &self,
        ui: &Widgets<'_>,
        index: usize,
        sample: &RulePreviewSample,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let duration = self.formatter.detail_duration(ms_to_duration(
            sample.end_utc_ms.saturating_sub(sample.start_utc_ms),
        ));
        let change = format!(
            "{} → {}",
            self.formatter.preview_output(&sample.before),
            self.formatter.preview_output(&sample.after),
        );
        div()
            .id(("rule-preview-sample", index))
            .flex()
            .flex_col()
            .gap(space.xxs)
            .px(space.s)
            .py(space.xs)
            .rounded(ui.metrics.radius.control)
            .bg(palette.fill_card)
            .border_1()
            .border_color(palette.stroke_card)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(space.xs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .font(ui.mono_font())
                            .text_color(palette.text_primary)
                            .child(sample.rule.clone()),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .tabular_figures()
                            .text_color(palette.text_tertiary)
                            .child(duration),
                    ),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .whitespace_normal()
                    .child(
                        self.formatter
                            .text_with(Msg::RulesPreviewSampleChange, &[Arg::Value.with(&change)]),
                    ),
            )
    }

    fn editor_badge(ui: &Widgets<'_>, text: String, tone: EditorBadgeTone) -> Div {
        let palette = ui.palette;
        let high_contrast = palette.contrast == ContrastMode::High;
        let (bg, fg) = match tone {
            EditorBadgeTone::Neutral => {
                if high_contrast {
                    (palette.bg_base, palette.text_primary)
                } else {
                    (palette.fill_subtle_hover, palette.text_secondary)
                }
            }
            EditorBadgeTone::Accent => (palette.accent_fill, palette.text_on_accent),
            EditorBadgeTone::Caution => {
                if high_contrast {
                    (palette.status_caution_bg, palette.text_on_accent)
                } else {
                    (palette.status_caution_bg, palette.status_caution)
                }
            }
            EditorBadgeTone::Critical => {
                if high_contrast {
                    (palette.status_critical_bg, palette.text_on_accent)
                } else {
                    (palette.status_critical_bg, palette.status_critical)
                }
            }
        };
        div()
            .flex_shrink_0()
            .px(px(EDITOR_BADGE_PADDING_X))
            .py(px(EDITOR_BADGE_PADDING_Y))
            .rounded(ui.metrics.radius.control)
            .bg(bg)
            .when(high_contrast, |pill| {
                pill.border_1().border_color(palette.stroke_card)
            })
            .ramp(ui, Ramp::Caption)
            .text_color(fg)
            .child(text)
    }
}

fn ms_to_duration(millis: i64) -> Duration {
    Duration::from_millis(u64::try_from(millis.max(0)).unwrap_or(0))
}

const fn editor_rule_kind_icon(kind: RuleKind) -> IconName {
    match kind {
        RuleKind::Classification => IconName::Tag,
        RuleKind::Extraction => IconName::FileText,
        RuleKind::Privacy => IconName::Shield,
    }
}
