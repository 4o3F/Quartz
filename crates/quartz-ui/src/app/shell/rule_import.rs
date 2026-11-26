//! Paste-import pane for the Rules page: multi-line monospace editor for pasted `quartz-rules/1`
//! text, derived create-vs-replace target summary, debounced validation with shared H4
//! diagnostics, 7-day `RulePreviewMutation::Import` preview, stale reload, and `rules_text.import`.

use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::input::{Editor, EditorState, InputEvent},
    div,
    prelude::FluentBuilder as _,
    px,
};
use quartz_protocol::{ErrorKind, RuleSetIdentityField};

use super::{
    AppShell, InputSyncState,
    rule_editor::{EDITOR_TEXTAREA_HEIGHT, EditorBadgeTone},
};
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, InfoBarStyle, RampExt as _, Widgets},
    },
    format::{Arg, Msg},
    layout::ChartArrangement,
    link::{LoadDataError, LoadDataErrorCode, RequestCoreError},
    model::{
        ExtractImportKeyError, ImportAction, ImportOutcome, ImportTarget, LeaveTarget,
        MutationActivity, PreviewState, RuleImportEffect, RuleImportSession, StaleNotice,
        VALIDATE_DEBOUNCE_DELAY,
    },
};

impl AppShell {
    /// Synchronizes the paste-import GPUI multi-line editor entity with programmatic model updates
    /// during render where `Window` is available.
    pub(super) fn sync_rule_import_input_if_needed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rule_import_text_sync != InputSyncState::NeedsSync {
            return;
        }
        self.rule_import_text_sync = InputSyncState::Synced;
        let Some(session) = self.rule_import.session() else {
            return;
        };
        let initial = session.text().to_owned();
        let placeholder = self.formatter.text(Msg::RulesImportTextPlaceholder);
        self.sync_rule_import_text_input(&initial, placeholder, window, cx);
    }

    fn sync_rule_import_text_input(
        &mut self,
        initial: &str,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.rule_import_text_input.clone() {
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
                let effect = this.rule_import.set_text(value, this.rules.tree());
                this.run_rule_import_effect(effect, cx);
                cx.notify();
            }
        })
        .detach();
        self.rule_import_text_input = Some(editor);
    }

    /// Opens the paste-import pane, prompting inline unsaved-change confirmation if the H4 rule
    /// editor currently has unsaved edits.
    pub(super) fn open_rule_import(&mut self, cx: &mut Context<Self>) {
        if self.rule_editor.is_open() {
            self.leave_rule_editor_or(LeaveTarget::OpenImport, cx);
            return;
        }
        self.open_rule_import_now(cx);
    }

    /// Opens a fresh paste-import pane immediately.
    pub(super) fn open_rule_import_now(&mut self, cx: &mut Context<Self>) {
        self.rule_import_validate_task = None;
        self.pending_leave_page = None;
        self.rule_import.open();
        self.rule_import_text_sync = InputSyncState::NeedsSync;
        cx.notify();
    }

    /// Closes the paste-import pane unless an import write is currently in flight.
    pub(super) fn close_rule_import(&mut self, cx: &mut Context<Self>) {
        if self
            .rule_import
            .session()
            .is_some_and(|session| session.importing == MutationActivity::Busy)
        {
            return;
        }
        self.close_rule_import_silently();
        cx.notify();
    }

    /// Closes the paste-import pane without triggering a separate notify.
    pub(super) fn close_rule_import_silently(&mut self) {
        if self
            .rule_import
            .session()
            .is_some_and(|session| session.importing == MutationActivity::Busy)
        {
            return;
        }
        self.rule_import_validate_task = None;
        self.rule_import.close();
    }

    /// Recomputes create-vs-replace and `base_version` for an open import pane after the tree
    /// finishes loading (for example after clicking Reload on a stale notice).
    pub(super) fn sync_rule_import_with_tree(&mut self, cx: &mut Context<Self>) {
        if !self.rule_import.is_open() {
            return;
        }
        let effect = self.rule_import.sync_tree(self.rules.tree());
        self.run_rule_import_effect(effect, cx);
    }

    fn reload_rule_import_stale(&mut self, cx: &mut Context<Self>) {
        self.rule_import_validate_task = None;
        let tree_effect = self.rules.reload_tree();
        self.run_rules_effect(tree_effect, cx);
        cx.notify();
    }

    fn preview_rule_import(&mut self, cx: &mut Context<Self>) {
        let today = self.current_logical_today();
        let effect = self.rule_import.request_preview(today);
        self.run_rule_import_effect(effect, cx);
        cx.notify();
    }

    fn submit_rule_import(&mut self, cx: &mut Context<Self>) {
        if let Some(text_input) = self.rule_import_text_input.as_ref() {
            let value = text_input.read(cx).value().to_string();
            let _ = self.rule_import.set_text(value, self.rules.tree());
        }
        let effect = self.rule_import.submit_import(self.rules.tree());
        self.run_rule_import_effect(effect, cx);
        cx.notify();
    }

    fn run_rule_import_effect(&mut self, effect: RuleImportEffect, cx: &mut Context<Self>) {
        match effect {
            RuleImportEffect::None => {}
            RuleImportEffect::ScheduleValidate { generation } => {
                let delay = cx.background_executor().timer(VALIDATE_DEBOUNCE_DELAY);
                self.rule_import_validate_task = Some(cx.spawn(async move |this, cx| {
                    delay.await;
                    let _ = this.update(cx, |this, cx| {
                        let next = this.rule_import.trigger_validate(generation);
                        this.run_rule_import_effect(next, cx);
                        cx.notify();
                    });
                }));
            }
            RuleImportEffect::Validate {
                request,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.rule_import.apply_validation_finished(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.validate_rule_set_text(request).await;
                    let _ = this.update(cx, |this, cx| {
                        this.rule_import
                            .apply_validation_finished(generation, result);
                        cx.notify();
                    });
                })
                .detach();
            }
            RuleImportEffect::Preview {
                request,
                buffer_generation,
                preview_generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.rule_import.apply_preview_finished(
                        buffer_generation,
                        preview_generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.preview_rules(request).await;
                    let _ = this.update(cx, |this, cx| {
                        this.rule_import.apply_preview_finished(
                            buffer_generation,
                            preview_generation,
                            result,
                        );
                        cx.notify();
                    });
                })
                .detach();
            }
            RuleImportEffect::Import {
                request,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let _ = self.rule_import.apply_import_finished(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.import_rule_set_text(request).await;
                    let _ = this.update(cx, |this, cx| {
                        match this.rule_import.apply_import_finished(generation, result) {
                            ImportOutcome::Imported { key } => {
                                this.rule_import_validate_task = None;
                                let next = this.rules.reload_tree_selecting(key);
                                this.run_rules_effect(next, cx);
                                this.on_rules_mutated(cx);
                            }
                            ImportOutcome::Ignored | ImportOutcome::Failed => {}
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// Renders the paste-import pane: header, target create/replace summary, error and stale
    /// notices, multi-line editor, shared validation diagnostics, action bar, and 7-day preview.
    pub(super) fn render_rule_import_pane(
        &self,
        ui: &Widgets<'_>,
        arrangement: ChartArrangement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let card = ui
            .card()
            .id("rules-paste-import-pane")
            .flex()
            .flex_col()
            .gap(space.m)
            .p(space.l)
            .map(|card| match arrangement {
                ChartArrangement::SideBySide => card.flex_1().min_w(px(0.)),
                ChartArrangement::Stacked => card.w_full(),
            });

        let Some(session) = self.rule_import.session() else {
            return card.into_any_element();
        };

        let busy = session.importing == MutationActivity::Busy;
        let header = self.render_import_header(ui, busy, cx);
        let notices = self.render_import_notices(ui, session, cx);
        let target_banner = self.render_import_target_banner(ui, &session.target);

        let text_control = match &self.rule_import_text_input {
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

        let validation_section = self.render_validation_state(ui, &session.validation);
        let action_bar = self.render_import_action_bar(ui, session, cx);
        let preview_section =
            self.render_preview_state(ui, &session.preview, session.preview_range);

        card.child(header)
            .children(notices)
            .children(target_banner)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space.xs)
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.text(Msg::RulesImportTextLabel)),
                    )
                    .child(text_control),
            )
            .child(validation_section)
            .child(action_bar)
            .children(preview_section)
            .into_any_element()
    }

    fn render_import_header(&self, ui: &Widgets<'_>, busy: bool, cx: &Context<Self>) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let close_content = ButtonContent::IconLabel(
            IconName::Close,
            self.formatter.text(Msg::RulesEditorActionClose),
        );
        let close_btn = if busy {
            ui.disabled_button("rule-import-close", Density::Compact, close_content)
        } else {
            ui.button("rule-import-close", Density::Compact, close_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_rule_import(cx);
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
                    .items_center()
                    .gap(space.xs)
                    .child(ui.icon(IconName::FileText, Density::Standard, palette.text_primary))
                    .child(
                        div()
                            .ramp(ui, Ramp::Subtitle)
                            .text_color(palette.text_primary)
                            .child(self.formatter.text(Msg::RulesImportTitle)),
                    ),
            )
            .child(close_btn)
    }

    fn render_import_target_banner(
        &self,
        ui: &Widgets<'_>,
        target: &ImportTarget,
    ) -> Option<AnyElement> {
        let space = &ui.metrics.space;
        match target {
            ImportTarget::Empty => None,
            ImportTarget::InvalidKey(error) => {
                let msg = match error {
                    ExtractImportKeyError::MissingKey => Msg::RulesImportKeyErrorMissing,
                    ExtractImportKeyError::InvalidKey => Msg::RulesImportKeyErrorInvalid,
                    ExtractImportKeyError::ReservedBuiltinPrefix => Msg::RulesImportKeyErrorBuiltin,
                };
                let colors = ui.info_bar_colors(InfoBarStyle::Critical);
                Some(
                    ui.card()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(space.s)
                        .p(space.m)
                        .bg(colors.background)
                        .border_color(colors.border)
                        .text_color(colors.foreground)
                        .child(ui.icon(IconName::TriangleAlert, Density::Compact, colors.icon))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .ramp(ui, Ramp::Body)
                                .child(self.formatter.text(msg)),
                        )
                        .into_any_element(),
                )
            }
            ImportTarget::Resolved(plan) => {
                let (message, tone) = match plan.action {
                    ImportAction::Create => (
                        self.formatter
                            .text_with(Msg::RulesImportPlanCreate, &[Arg::Name.with(&plan.key)]),
                        EditorBadgeTone::Accent,
                    ),
                    ImportAction::Replace { base_version } => (
                        self.formatter.text_with(
                            Msg::RulesImportPlanReplace,
                            &[
                                Arg::Name.with(&plan.key),
                                Arg::Value.with(&base_version.to_string()),
                            ],
                        ),
                        EditorBadgeTone::Caution,
                    ),
                };
                Some(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .gap(space.s)
                        .p(space.s)
                        .rounded(ui.metrics.radius.control)
                        .bg(ui.palette.fill_card_secondary)
                        .border_1()
                        .border_color(ui.palette.stroke_card)
                        .child(Self::editor_badge(ui, plan.key.clone(), tone))
                        .child(
                            div()
                                .ramp(ui, Ramp::Body)
                                .text_color(ui.palette.text_primary)
                                .child(message),
                        )
                        .into_any_element(),
                )
            }
        }
    }

    fn render_import_notices(
        &self,
        ui: &Widgets<'_>,
        session: &RuleImportSession,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let space = &ui.metrics.space;
        let mut notices = Vec::new();

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
                                            Msg::RulesImportStaleMessage,
                                            &[Arg::Value.with(&current_version.to_string())],
                                        ),
                                    )),
                            ),
                    )
                    .child(
                        ui.accent_button(
                            "rule-import-reload-stale",
                            Density::Compact,
                            ButtonContent::Label(self.formatter.text(Msg::RulesActionReload)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reload_rule_import_stale(cx);
                        })),
                    )
                    .into_any_element(),
            );
        }

        if let Some(error) = &session.import_error {
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
                _ => Msg::RulesImportFailedGeneric,
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

    fn render_import_action_bar(
        &self,
        ui: &Widgets<'_>,
        session: &RuleImportSession,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let can_preview = self.rule_import.can_preview();
        let can_import = self.rule_import.can_import();
        let importing = session.importing == MutationActivity::Busy;

        let import_label = if importing {
            self.formatter.text(Msg::RulesImportActionImporting)
        } else {
            self.formatter.text(Msg::RulesImportActionImport)
        };
        let import_content = ButtonContent::IconLabel(IconName::Check, import_label);
        let import_btn = if can_import {
            ui.accent_button("rule-import-submit", Density::Compact, import_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.submit_rule_import(cx);
                }))
        } else {
            ui.disabled_button("rule-import-submit", Density::Compact, import_content)
        };

        let preview_content = ButtonContent::IconLabel(
            IconName::Clock,
            self.formatter.text(Msg::RulesEditorActionPreview),
        );
        let preview_btn = if can_preview {
            ui.button("rule-import-preview", Density::Compact, preview_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.preview_rule_import(cx);
                }))
        } else {
            ui.disabled_button("rule-import-preview", Density::Compact, preview_content)
        };

        let cancel_content =
            ButtonContent::IconLabel(IconName::Close, self.formatter.text(Msg::RulesActionCancel));
        let cancel_btn = if importing {
            ui.disabled_button("rule-import-cancel", Density::Compact, cancel_content)
        } else {
            ui.button("rule-import-cancel", Density::Compact, cancel_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_rule_import(cx);
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
                    .child(import_btn)
                    .child(preview_btn)
                    .child(cancel_btn),
            )
            .children(preview_hint)
    }
}
