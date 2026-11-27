//! Rules page: rule-set tree, selected set detail, enable/disable toggle, confirmed deletion,
//! and the category editor (create, rename/recolor, tendency, inline-confirmed archive, reorder).

use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::input::{Input, InputEvent, InputState},
    div,
    prelude::FluentBuilder as _,
    px,
};
use quartz_protocol::{
    ErrorKind, RuleEntryKind, RuleEntryState, RuleKind, RuleProvenance, RuleSetSource, RuleView,
    RuleWarningCode, Uuid,
};

use super::{AppShell, Frame};
use crate::{
    app::{
        metrics::{Density, Ramp},
        tokens::ContrastMode,
        widgets::{ButtonContent, InfoBarStyle, RampExt as _, Segment, SkeletonTone, Widgets},
    },
    format::{Arg, Msg},
    layout::{ChartArrangement, WindowClass},
    link::{LoadDataError, LoadDataErrorCode, RequestCoreError},
    model::{
        ArchiveConfirmation, ArchivedVisibility, CategoryCatalog, CategoryColor,
        CategoryEditorDraft, CategoryEditorMode, CategoryItem, CategoryMutation, CategoryName,
        CategoryOrigin, DeleteConfirmation, FlatTreeRow, LeaveConfirmation, LeaveTarget, LoadState,
        MutationActivity, NodeEnablement, ReorderDirection, ResolvedCategory, RuleEntryDetail,
        RuleSetDetail, RuleSetEnablement, RuleTree, RuleTreeNode, RulesEffect, RulesKeyAction,
        RulesMutation, RulesTab, Tendency, TreeDirection, ValidateCategoryNameError,
    },
};

/// Fixed width of the left rule-set tree card when shown side-by-side (07 section 6).
const TREE_PANE_WIDTH: f32 = 300.;
/// Fixed width of the category editor card when shown side-by-side (mockup `--w-details`).
const CATEGORY_EDITOR_WIDTH: f32 = 360.;
/// Horizontal indentation per tree depth level.
const TREE_INDENT_STEP: f32 = 16.;
/// Horizontal indentation per category hierarchy level.
const CATEGORY_INDENT_STEP: f32 = 16.;
/// Maximum tree depth applied to horizontal indentation so deep user trees stay readable.
const MAX_INDENT_DEPTH: usize = 6;
/// Minimum height of one tree row; expands with text scale.
const TREE_ROW_MIN_HEIGHT: f32 = 44.;
/// Side of the category color swatch in the categories list (mockup `.cat-sw`).
const CATEGORY_SWATCH_SIZE: f32 = 14.;
/// Corner radius of the category color swatch.
const CATEGORY_SWATCH_RADIUS: f32 = 3.;
/// Width of one clickable swatch button inside the palette tray.
const PALETTE_SWATCH_WIDTH: f32 = 44.;
/// Height of one clickable swatch button inside the palette tray (mockup `.swatch-btn`).
const PALETTE_SWATCH_HEIGHT: f32 = 28.;
/// Border thickness of the selected swatch button in the palette tray.
const PALETTE_SWATCH_SELECTED_BORDER: f32 = 2.;
/// Fixed width of the tendency column in the categories list.
const CATEGORY_TENDENCY_COL_WIDTH: f32 = 96.;
/// Static skeleton row count while loading the rule-set tree.
const SKELETON_TREE_ROWS: usize = 5;
/// Height of one static tree skeleton placeholder.
const SKELETON_TREE_HEIGHT: f32 = 40.;
/// Static skeleton row count while loading rule-set entries.
const SKELETON_DETAIL_ROWS: usize = 4;
/// Height of one static detail skeleton placeholder.
const SKELETON_DETAIL_HEIGHT: f32 = 52.;
/// Static skeleton row count while loading the category catalog.
const SKELETON_CATEGORY_ROWS: usize = 6;
/// Height of one static category skeleton placeholder.
const SKELETON_CATEGORY_HEIGHT: f32 = 40.;
/// Width of the inline loading chip when rule-entry category metadata is still loading.
const SKELETON_CHIP_WIDTH: f32 = 64.;
/// Height of the inline loading chip when rule-entry category metadata is still loading.
const SKELETON_CHIP_HEIGHT: f32 = 18.;
/// Compact pill badge horizontal padding.
const BADGE_PADDING_X: f32 = 6.;
/// Compact pill badge vertical padding.
const BADGE_PADDING_Y: f32 = 2.;

/// Whether a key-down event was consumed by the Rules page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyHandled {
    Handled,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BadgeTone {
    Neutral,
    Accent,
    Caution,
    Critical,
}

impl AppShell {
    /// Starts loading the rule-set tree when opening the Rules page.
    pub(super) fn open_rules_page(&mut self, cx: &mut Context<Self>) {
        let effect = self.rules.open();
        self.run_rules_effect(effect, cx);
        if self.rules.active_tab() == RulesTab::Unclassified {
            self.open_unclassified_tab(cx);
        }
    }

    /// Reloads the rule-set tree on explicit user request.
    fn reload_rules_tree(&mut self, cx: &mut Context<Self>) {
        let effect = self.rules.reload_tree();
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    /// Retries loading the currently selected rule set's details.
    fn retry_rule_set_detail(&mut self, cx: &mut Context<Self>) {
        let effect = self.rules.open_selected();
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    fn select_rule_set(&mut self, key: String, cx: &mut Context<Self>) {
        if self.rule_editor.is_open() {
            self.leave_rule_editor_or(LeaveTarget::SelectRuleSet(key), cx);
            return;
        }
        if self.rule_import.is_open() {
            if self
                .rule_import
                .session()
                .is_some_and(|session| session.importing == MutationActivity::Busy)
            {
                return;
            }
            self.close_rule_import_silently();
        }
        let effect = self.rules.select(key);
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    fn toggle_rule_set_enabled(&mut self, key: &str, cx: &mut Context<Self>) {
        let effect = self.rules.toggle_enabled(key);
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    fn request_rule_set_delete(&mut self, key: &str, cx: &mut Context<Self>) {
        self.rules.request_delete(key);
        cx.notify();
    }

    fn cancel_rule_set_delete(&mut self, cx: &mut Context<Self>) {
        self.rules.cancel_delete();
        cx.notify();
    }

    fn confirm_rule_set_delete(&mut self, key: &str, cx: &mut Context<Self>) {
        let effect = self.rules.confirm_delete(key);
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    fn set_rules_tab(&mut self, tab: RulesTab, cx: &mut Context<Self>) {
        if self.rule_editor.is_open() {
            self.leave_rule_editor_or(LeaveTarget::SwitchTab(tab), cx);
            return;
        }
        if self.rule_import.is_open() {
            if self
                .rule_import
                .session()
                .is_some_and(|session| session.importing == MutationActivity::Busy)
            {
                return;
            }
            self.close_rule_import_silently();
        }
        self.rules.set_active_tab(tab);
        if tab == RulesTab::Unclassified {
            self.open_unclassified_tab(cx);
        }
        cx.notify();
    }

    fn reload_categories(&mut self, cx: &mut Context<Self>) {
        let effect = self.rules.reload_categories();
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    fn toggle_archived_categories(&mut self, cx: &mut Context<Self>) {
        self.rules.toggle_archived_visibility();
        cx.notify();
    }

    pub(super) fn sync_category_name_input(
        &mut self,
        initial: &str,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.category_name_input.clone() {
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
                this.rules.set_category_editor_name(value);
                cx.notify();
            }
        })
        .detach();
        self.category_name_input = Some(input);
    }

    fn open_create_category(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(uuid) = generate_category_uuid() else {
            return;
        };
        self.rules.begin_create_category(uuid);
        let placeholder = self
            .formatter
            .text(Msg::RulesCategoriesEditorNamePlaceholder);
        self.sync_category_name_input("", placeholder, window, cx);
        cx.notify();
    }

    fn open_edit_category(&mut self, uuid: &Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.rules.begin_edit_category(uuid);
        let Some(draft) = self.rules.category_editor() else {
            return;
        };
        let initial = draft.name_input.clone();
        let placeholder = match &draft.mode {
            CategoryEditorMode::Edit {
                origin: CategoryOrigin::Builtin(builtin),
                ..
            } => self
                .formatter
                .category_name(&CategoryName::Builtin(*builtin)),
            CategoryEditorMode::Edit {
                origin: CategoryOrigin::User,
                ..
            }
            | CategoryEditorMode::Create { .. } => self
                .formatter
                .text(Msg::RulesCategoriesEditorNamePlaceholder),
        };
        self.sync_category_name_input(&initial, placeholder, window, cx);
        cx.notify();
    }

    fn close_category_editor(&mut self, cx: &mut Context<Self>) {
        self.rules.close_category_editor();
        cx.notify();
    }

    fn reset_builtin_category_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rules.reset_builtin_editor_name();
        if let Some(input) = self.category_name_input.clone() {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }
        cx.notify();
    }

    fn set_category_editor_color(&mut self, color: CategoryColor, cx: &mut Context<Self>) {
        self.rules.set_category_editor_color(color);
        cx.notify();
    }

    fn set_category_editor_tendency(&mut self, tendency: Tendency, cx: &mut Context<Self>) {
        self.rules.set_category_editor_tendency(tendency);
        cx.notify();
    }

    fn submit_category_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = self.category_name_input.as_ref() {
            let value = input.read(cx).value().to_string();
            self.rules.set_category_editor_name(value);
        }
        let effect = self.rules.submit_category_editor();
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    fn request_archive_category(&mut self, uuid: &Uuid, cx: &mut Context<Self>) {
        self.rules.request_archive_category(uuid);
        cx.notify();
    }

    fn cancel_archive_category(&mut self, cx: &mut Context<Self>) {
        self.rules.cancel_archive_category();
        cx.notify();
    }

    fn confirm_archive_category(&mut self, uuid: &Uuid, cx: &mut Context<Self>) {
        let effect = self.rules.confirm_archive_category(uuid);
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    fn restore_category(&mut self, uuid: &Uuid, cx: &mut Context<Self>) {
        let effect = self.rules.restore_category(uuid);
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    fn reorder_category(
        &mut self,
        uuid: &Uuid,
        direction: ReorderDirection,
        cx: &mut Context<Self>,
    ) {
        let effect = self.rules.reorder_category(uuid, direction);
        self.run_rules_effect(effect, cx);
        cx.notify();
    }

    /// Handles Up/Down/Enter/Space/Escape on the Rules page.
    pub(super) fn handle_rules_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> KeyHandled {
        if self.rule_editor.is_open() {
            if event.keystroke.key == "escape" {
                if matches!(
                    self.rule_editor
                        .session()
                        .map(|session| &session.leave_confirm),
                    Some(LeaveConfirmation::Pending { .. })
                ) {
                    self.cancel_leave_rule_editor(cx);
                } else {
                    self.request_close_rule_editor(cx);
                }
                return KeyHandled::Handled;
            }
            return KeyHandled::Ignored;
        }
        if self.rule_import.is_open() {
            if event.keystroke.key == "escape" {
                self.close_rule_import(cx);
                return KeyHandled::Handled;
            }
            return KeyHandled::Ignored;
        }
        if self.rules.active_tab() == RulesTab::Unclassified {
            return KeyHandled::Ignored;
        }
        if self.rules.active_tab() == RulesTab::Categories {
            if event.keystroke.key == "escape"
                && (matches!(
                    self.rules.archive_confirmation(),
                    ArchiveConfirmation::Pending { .. }
                ) || self.rules.category_editor().is_some())
            {
                let effect = self.rules.handle_key(RulesKeyAction::Cancel);
                self.run_rules_effect(effect, cx);
                cx.notify();
                return KeyHandled::Handled;
            }
            return KeyHandled::Ignored;
        }
        let action = match event.keystroke.key.as_str() {
            "up" => RulesKeyAction::Move(TreeDirection::Up),
            "down" => RulesKeyAction::Move(TreeDirection::Down),
            "enter" => RulesKeyAction::Open,
            "space" | " " => RulesKeyAction::ToggleEnabled,
            "escape" => {
                if matches!(
                    self.rules.delete_confirmation(),
                    DeleteConfirmation::Pending { .. }
                ) {
                    RulesKeyAction::Cancel
                } else {
                    return KeyHandled::Ignored;
                }
            }
            _ => return KeyHandled::Ignored,
        };
        let effect = self.rules.handle_key(action);
        self.run_rules_effect(effect, cx);
        cx.notify();
        KeyHandled::Handled
    }

    fn spawn_load_rule_trees(
        &mut self,
        generation: crate::model::rules::RulesGeneration,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
            let next = self.rules.apply_tree_loaded(
                generation,
                Err(LoadDataError::from(RequestCoreError::Disconnected)),
            );
            if !matches!(next, RulesEffect::None) {
                self.run_rules_effect(next, cx);
            }
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = link.load_rule_trees().await;
            let _ = this.update(cx, |this, cx| {
                let next = this.rules.apply_tree_loaded(generation, result);
                this.run_rules_effect(next, cx);
                this.sync_rule_import_with_tree(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn spawn_load_categories(
        &mut self,
        generation: crate::model::rules::RulesGeneration,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
            self.rules.apply_categories_loaded(
                generation,
                Err(LoadDataError::from(RequestCoreError::Disconnected)),
            );
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = link.list_categories().await;
            let _ = this.update(cx, |this, cx| {
                this.rules.apply_categories_loaded(generation, result);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn run_rules_effect(&mut self, effect: RulesEffect, cx: &mut Context<Self>) {
        match effect {
            RulesEffect::None => {}
            RulesEffect::LoadPage {
                tree_generation,
                categories_generation,
            } => {
                self.spawn_load_rule_trees(tree_generation, cx);
                self.spawn_load_categories(categories_generation, cx);
            }
            RulesEffect::LoadTree { generation } => {
                self.spawn_load_rule_trees(generation, cx);
            }
            RulesEffect::LoadDetail { key, generation } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    self.rules.apply_detail_loaded(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = link.get_rule_set(key, RuleView::Own).await;
                    let _ = this.update(cx, |this, cx| {
                        this.rules.apply_detail_loaded(generation, result);
                        cx.notify();
                    });
                })
                .detach();
            }
            RulesEffect::Mutate {
                mutation,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let _ = self.rules.apply_mutation_finished(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = match mutation {
                        RulesMutation::SetEnabled(request) => {
                            link.set_rule_set_enabled(request).await
                        }
                        RulesMutation::Delete(request) => link.delete_rule_set(request).await,
                    };
                    let _ = this.update(cx, |this, cx| {
                        let succeeded = result.is_ok();
                        let next = this.rules.apply_mutation_finished(generation, result);
                        this.run_rules_effect(next, cx);
                        if succeeded {
                            this.on_rules_mutated(cx);
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            RulesEffect::LoadCategories { generation } => {
                self.spawn_load_categories(generation, cx);
            }
            RulesEffect::MutateCategory {
                mutation,
                generation,
            } => {
                let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
                    let _ = self.rules.apply_category_mutation_finished(
                        generation,
                        Err(LoadDataError::from(RequestCoreError::Disconnected)),
                    );
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = match mutation {
                        CategoryMutation::Create(request) => {
                            link.create_category(request).await.map(|_| ())
                        }
                        CategoryMutation::Update(request) => {
                            link.update_category(request).await.map(|_| ())
                        }
                        CategoryMutation::Archive(request) => {
                            link.archive_category(request).await.map(|_| ())
                        }
                        CategoryMutation::Reorder(request) => {
                            link.reorder_categories(request).await.map(|_| ())
                        }
                    };
                    let _ = this.update(cx, |this, cx| {
                        let next = this
                            .rules
                            .apply_category_mutation_finished(generation, result);
                        this.run_rules_effect(next, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// Renders the Rules page: header with tab switch and active section (rule sets or categories).
    pub(super) fn render_rules(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let arrangement = frame.layout.class.chart_arrangement();
        let active_tab = self.rules.active_tab();
        let tree_busy = matches!(self.rules.tree_state(), LoadState::Loading)
            || self.rules.mutating() == MutationActivity::Busy;
        let categories_busy = matches!(self.rules.categories_state(), LoadState::Loading)
            || self.rules.category_mutating() == MutationActivity::Busy;
        let unclassified_busy = matches!(self.unclassified.state(), LoadState::Loading);
        let reload_busy = match active_tab {
            RulesTab::RuleSets => tree_busy,
            RulesTab::Categories => categories_busy,
            RulesTab::Unclassified => unclassified_busy,
        };

        let tab_switch = ui.segmented(
            "rules-tab",
            [
                Segment {
                    id: "rules-tab-sets",
                    label: self.formatter.text(Msg::RulesTabRuleSets),
                    value: RulesTab::RuleSets,
                },
                Segment {
                    id: "rules-tab-categories",
                    label: self.formatter.text(Msg::RulesTabCategories),
                    value: RulesTab::Categories,
                },
                Segment {
                    id: "rules-tab-unclassified",
                    label: self.formatter.text(Msg::RulesTabUnclassified),
                    value: RulesTab::Unclassified,
                },
            ],
            active_tab,
            |tab| cx.listener(move |this, _, _, cx| this.set_rules_tab(tab, cx)),
        );

        let new_rule_set_button = (active_tab == RulesTab::RuleSets).then(|| {
            let content = ButtonContent::IconLabel(
                IconName::Plus,
                self.formatter.text(Msg::RulesActionNewSet),
            );
            if tree_busy || self.rule_editor.is_open() {
                ui.disabled_button("rules-new-set", Density::Compact, content)
            } else {
                ui.accent_button("rules-new-set", Density::Compact, content)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_new_rule_set(cx);
                    }))
            }
        });

        let new_category_button = (active_tab == RulesTab::Categories).then(|| {
            let content = ButtonContent::IconLabel(
                IconName::Plus,
                self.formatter.text(Msg::RulesCategoriesActionNew),
            );
            if categories_busy {
                ui.disabled_button("categories-new", Density::Compact, content)
            } else {
                ui.accent_button("categories-new", Density::Compact, content)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_create_category(window, cx);
                    }))
            }
        });

        let reload_button = if reload_busy {
            ui.disabled_button(
                "rules-reload",
                Density::Compact,
                ButtonContent::Label(self.formatter.text(Msg::RulesActionReload)),
            )
        } else {
            ui.button(
                "rules-reload",
                Density::Compact,
                ButtonContent::Label(self.formatter.text(Msg::RulesActionReload)),
            )
            .on_click(cx.listener(move |this, _, _, cx| match active_tab {
                RulesTab::RuleSets => this.reload_rules_tree(cx),
                RulesTab::Categories => this.reload_categories(cx),
                RulesTab::Unclassified => this.reload_unclassified(cx),
            }))
        };

        let header = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(space.s)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.m)
                    .child(
                        div()
                            .ramp(ui, Ramp::Title)
                            .text_color(ui.palette.text_primary)
                            .child(self.formatter.text(Msg::NavRules)),
                    )
                    .child(tab_switch),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.s)
                    .children(new_rule_set_button)
                    .children(new_category_button)
                    .child(reload_button),
            );

        let body = match active_tab {
            RulesTab::RuleSets => {
                let tree_pane = self.render_rules_tree_pane(ui, arrangement, cx);
                let right_pane = if self.rule_editor.is_open() {
                    self.render_rule_editor_pane(ui, arrangement, cx)
                } else if self.rule_import.is_open() {
                    self.render_rule_import_pane(ui, arrangement, cx)
                } else {
                    self.render_rules_detail_pane(ui, arrangement, cx)
                };
                match arrangement {
                    ChartArrangement::SideBySide => div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(space.s)
                        .child(tree_pane)
                        .child(right_pane),
                    ChartArrangement::Stacked => div()
                        .flex()
                        .flex_col()
                        .gap(space.s)
                        .child(tree_pane)
                        .child(right_pane),
                }
                .into_any_element()
            }
            RulesTab::Categories => self.render_categories_section(ui, arrangement, cx),
            RulesTab::Unclassified => self.render_unclassified_section(ui, cx),
        };

        let content = div()
            .flex()
            .flex_col()
            .gap(space.m)
            .w_full()
            .max_w(ui.metrics.layout.max_content_width)
            .mx_auto()
            .px(if frame.layout.class == WindowClass::Compact {
                space.l
            } else {
                space.xxl
            })
            .pt(if frame.layout.class == WindowClass::Compact {
                space.l
            } else {
                space.xxxl
            })
            .pb(space.xxl)
            .child(header)
            .child(body);

        div()
            .id("rules-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(content)
    }

    fn render_rules_tree_pane(
        &self,
        ui: &Widgets<'_>,
        arrangement: ChartArrangement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let body = match self.rules.tree_state() {
            LoadState::Loading => div()
                .flex()
                .flex_col()
                .gap(space.xs)
                .children((0..SKELETON_TREE_ROWS).map(|_| {
                    ui.skeleton_block(SkeletonTone::Subtle)
                        .h(px(SKELETON_TREE_HEIGHT))
                        .w_full()
                        .rounded(ui.metrics.radius.control)
                }))
                .into_any_element(),
            LoadState::Failed(error) => self
                .render_rules_error_block(
                    ui,
                    Msg::RulesLoadFailedTitle,
                    if error.is_busy() {
                        Msg::DayBusy
                    } else {
                        Msg::RulesLoadFailedMessage
                    },
                    error,
                    "retry-rules-tree",
                    cx.listener(|this, _, _, cx| this.reload_rules_tree(cx)),
                )
                .into_any_element(),
            LoadState::Stale => self
                .render_rules_stale_block(ui, "reload-stale-tree", cx)
                .into_any_element(),
            LoadState::Ready(tree) => self.render_rules_tree_list(ui, tree, cx).into_any_element(),
        };

        let tree_busy = matches!(self.rules.tree_state(), LoadState::Loading)
            || self.rules.mutating() == MutationActivity::Busy;
        let import_content = ButtonContent::IconLabel(
            IconName::FileText,
            self.formatter.text(Msg::RulesActionPasteImport),
        );
        let paste_import_button = if tree_busy || self.rule_import.is_open() {
            ui.disabled_button("rules-paste-import", Density::Compact, import_content)
        } else {
            ui.button("rules-paste-import", Density::Compact, import_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.open_rule_import(cx);
                }))
        };

        ui.card()
            .id("rules-tree-pane")
            .flex()
            .flex_col()
            .gap(space.s)
            .p(space.m)
            .map(|card| match arrangement {
                ChartArrangement::SideBySide => card.w(px(TREE_PANE_WIDTH)).flex_shrink_0(),
                ChartArrangement::Stacked => card.w_full(),
            })
            .track_focus(&self.rules_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.handle_rules_key(event, cx) == KeyHandled::Handled {
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(space.xs)
                    .px(space.xs)
                    .child(
                        div()
                            .ramp(ui, Ramp::BodyStrong)
                            .text_color(ui.palette.text_primary)
                            .child(self.formatter.text(Msg::RulesTreeTitle)),
                    )
                    .child(paste_import_button),
            )
            .child(body)
            .into_any_element()
    }

    fn render_rules_tree_list(&self, ui: &Widgets<'_>, tree: &RuleTree, cx: &Context<Self>) -> Div {
        let space = &ui.metrics.space;
        let rows = tree.flatten();
        let mut container = div().flex().flex_col().gap(space.xxs);

        if tree.truncated {
            container = container.child(self.render_truncated_notice(
                ui,
                Msg::RulesTreeTruncated,
                tree.node_count(),
            ));
        }

        if rows.is_empty() {
            return container.child(
                div()
                    .p(space.s)
                    .ramp(ui, Ramp::Body)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesTreeEmpty)),
            );
        }

        container.children(
            rows.into_iter()
                .enumerate()
                .map(|(index, row)| self.render_tree_row(ui, index, row, cx)),
        )
    }

    fn render_tree_row(
        &self,
        ui: &Widgets<'_>,
        index: usize,
        row: FlatTreeRow<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let node = row.node;
        let palette = ui.palette;
        let space = &ui.metrics.space;
        let selected = self.rules.selected_key() == Some(node.key.as_str());
        let effective = node.enablement.is_effective();
        let clamped_depth = row.depth.min(MAX_INDENT_DEPTH);
        let indent = f32::from(u16::try_from(clamped_depth).unwrap_or(0)) * TREE_INDENT_STEP;
        let row_height = px(TREE_ROW_MIN_HEIGHT);
        let title_color = if effective {
            palette.text_primary
        } else {
            palette.text_disabled
        };
        let meta_color = if effective {
            palette.text_secondary
        } else {
            palette.text_disabled
        };

        let mut badges = div().flex().flex_wrap().items_center().gap(space.xs);
        if node.counts.added > 0 {
            badges = badges.child(Self::badge(
                ui,
                self.formatter.text_with(
                    Msg::RulesCountsAdded,
                    &[Arg::Count.with(&node.counts.added.to_string())],
                ),
                BadgeTone::Neutral,
            ));
        }
        if node.counts.overridden > 0 {
            badges = badges.child(Self::badge(
                ui,
                self.formatter.text_with(
                    Msg::RulesCountsOverridden,
                    &[Arg::Count.with(&node.counts.overridden.to_string())],
                ),
                BadgeTone::Caution,
            ));
        }
        if node.counts.disabled > 0 {
            badges = badges.child(Self::badge(
                ui,
                self.formatter.text_with(
                    Msg::RulesCountsDisabled,
                    &[Arg::Count.with(&node.counts.disabled.to_string())],
                ),
                BadgeTone::Neutral,
            ));
        }
        if node.override_marker.is_overridden() {
            badges = badges.child(Self::badge(
                ui,
                self.formatter.text(Msg::RulesBadgeOverridden),
                BadgeTone::Accent,
            ));
        }
        if !effective {
            badges = badges.child(Self::badge(
                ui,
                self.formatter.text(Msg::RulesBadgeDisabled),
                BadgeTone::Neutral,
            ));
        }
        for warning in &node.warnings {
            let (msg, tone) = match warning.code {
                RuleWarningCode::TargetDeleted => {
                    (Msg::RulesWarningTargetDeleted, BadgeTone::Critical)
                }
                RuleWarningCode::Conflict => (Msg::RulesWarningConflict, BadgeTone::Caution),
            };
            badges = badges.child(Self::badge(ui, self.formatter.text(msg), tone));
        }

        let key = node.key.clone();
        div()
            .id(("rules-tree-row", index))
            .relative()
            .flex()
            .flex_row()
            .items_start()
            .gap(space.s)
            .min_h(row_height)
            .pl(px(indent) + space.s)
            .pr(space.s)
            .py(space.xs)
            .rounded(ui.metrics.radius.control)
            .cursor_pointer()
            .when(selected, |item| {
                item.bg(palette.fill_subtle_hover)
                    .child(ui.indicator_bar(palette.accent_fill, row_height))
            })
            .when(!selected, |item| item.hover(ui.subtle_hover()))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.rules_focus, cx);
                this.select_rule_set(key.clone(), cx);
            }))
            .child(div().pt(space.xxs).flex_shrink_0().child(ui.icon(
                rule_kind_icon(node.kind),
                Density::Compact,
                meta_color,
            )))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(space.xxs)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(space.xs)
                            .min_w(px(0.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .ramp(
                                        ui,
                                        if effective {
                                            Ramp::BodyStrong
                                        } else {
                                            Ramp::Body
                                        },
                                    )
                                    .text_color(title_color)
                                    .child(node.name.clone()),
                            )
                            .when(node.source == RuleSetSource::Builtin, |header| {
                                header.child(Self::badge(
                                    ui,
                                    self.formatter.text(Msg::RulesSourceBuiltin),
                                    BadgeTone::Neutral,
                                ))
                            }),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .ramp(ui, Ramp::Caption)
                            .font(ui.mono_font())
                            .text_color(meta_color)
                            .child(node.key.clone()),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(meta_color)
                            .child(format!(
                                "{} · {}",
                                self.formatter.text(rule_kind_msg(node.kind)),
                                self.formatter.text_with(
                                    Msg::RulesVersion,
                                    &[Arg::Value.with(&node.version.to_string())],
                                ),
                            )),
                    )
                    .child(badges),
            )
    }

    fn render_rules_detail_pane(
        &self,
        ui: &Widgets<'_>,
        arrangement: ChartArrangement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let card =
            ui.card()
                .flex()
                .flex_col()
                .gap(space.m)
                .p(space.l)
                .map(|card| match arrangement {
                    ChartArrangement::SideBySide => card.flex_1().min_w(px(0.)),
                    ChartArrangement::Stacked => card.w_full(),
                });

        let Some(node) = self.rules.selected_node() else {
            return card
                .child(
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(ui.palette.text_secondary)
                        .child(self.formatter.text(Msg::RulesDetailSelectPrompt)),
                )
                .into_any_element();
        };

        let detail_meta = match self.rules.detail_state() {
            LoadState::Ready(detail) => Some(detail),
            LoadState::Loading | LoadState::Failed(_) | LoadState::Stale => None,
        };

        let header = self.render_detail_header(ui, node, detail_meta, cx);
        let notices = self.render_detail_notices(ui, node, cx);

        let body = match self.rules.detail_state() {
            LoadState::Loading => div()
                .flex()
                .flex_col()
                .gap(space.s)
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(ui.palette.text_secondary)
                        .child(self.formatter.text(Msg::RulesDetailLoading)),
                )
                .children((0..SKELETON_DETAIL_ROWS).map(|_| {
                    ui.skeleton_block(SkeletonTone::Subtle)
                        .h(px(SKELETON_DETAIL_HEIGHT))
                        .w_full()
                        .rounded(ui.metrics.radius.control)
                }))
                .into_any_element(),
            LoadState::Failed(error) => self
                .render_rules_error_block(
                    ui,
                    Msg::RulesDetailFailedTitle,
                    if error.is_busy() {
                        Msg::DayBusy
                    } else {
                        Msg::RulesDetailFailedMessage
                    },
                    error,
                    "retry-rules-detail",
                    cx.listener(|this, _, _, cx| this.retry_rule_set_detail(cx)),
                )
                .into_any_element(),
            LoadState::Stale => self
                .render_rules_stale_block(ui, "reload-stale-detail", cx)
                .into_any_element(),
            LoadState::Ready(detail) => self
                .render_detail_entries(ui, node, detail, cx)
                .into_any_element(),
        };

        card.child(header)
            .children(notices)
            .child(body)
            .into_any_element()
    }

    fn render_detail_header(
        &self,
        ui: &Widgets<'_>,
        node: &RuleTreeNode,
        detail: Option<&RuleSetDetail>,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let busy = self.rules.mutating() == MutationActivity::Busy;
        let is_user = node.source == RuleSetSource::User;
        let display_key = detail.map_or(node.key.as_str(), |loaded| loaded.key.as_str());
        let display_name = detail.map_or(node.name.as_str(), |loaded| loaded.name.as_str());
        let display_version = detail.map_or(node.version, |loaded| loaded.version);
        let parent_version = detail.and_then(|loaded| loaded.parent_version);
        let toggle_label = match node.self_enablement {
            RuleSetEnablement::Enabled => self.formatter.text(Msg::RulesActionDisable),
            RuleSetEnablement::Disabled => self.formatter.text(Msg::RulesActionEnable),
        };
        let edit_button = is_user.then(|| {
            let content =
                ButtonContent::IconLabel(IconName::Code, self.formatter.text(Msg::RulesActionEdit));
            if busy || display_version == 0 {
                ui.disabled_button("rules-edit", Density::Compact, content)
            } else {
                let key_for_edit = display_key.to_owned();
                let kind_for_edit = node.kind;
                ui.accent_button("rules-edit", Density::Compact, content)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_edit_rule_set(
                            key_for_edit.clone(),
                            kind_for_edit,
                            display_version,
                            cx,
                        );
                    }))
            }
        });

        let key_for_toggle = node.key.clone();
        let toggle_button = if is_user && !busy && node.version > 0 {
            ui.button(
                "rules-toggle-enabled",
                Density::Compact,
                ButtonContent::Label(toggle_label),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_rule_set_enabled(&key_for_toggle, cx);
            }))
        } else {
            ui.disabled_button(
                "rules-toggle-enabled",
                Density::Compact,
                ButtonContent::Label(toggle_label),
            )
        };

        let delete_pending = matches!(
            self.rules.delete_confirmation(),
            DeleteConfirmation::Pending { key } if key == &node.key
        );
        let delete_button = (is_user && !delete_pending).then(|| {
            let can_delete = node.children.is_empty() && !busy && node.version > 0;
            let content = ButtonContent::IconLabel(
                IconName::Trash,
                self.formatter.text(Msg::RulesActionDelete),
            );
            if can_delete {
                let key_for_delete = node.key.clone();
                ui.button("rules-delete-request", Density::Compact, content)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.request_rule_set_delete(&key_for_delete, cx);
                    }))
            } else {
                ui.disabled_button("rules-delete-request", Density::Compact, content)
            }
        });

        let source_msg = match node.source {
            RuleSetSource::Builtin => Msg::RulesSourceBuiltin,
            RuleSetSource::User => Msg::RulesSourceUser,
        };
        let mut provenance_parts = vec![
            self.formatter.text(source_msg),
            self.formatter.text(rule_kind_msg(node.kind)),
            self.formatter.text_with(
                Msg::RulesVersion,
                &[Arg::Value.with(&display_version.to_string())],
            ),
        ];
        if let Some(parent) = &node.parent {
            let parent_text = match parent_version {
                Some(version) => self.formatter.text_with(
                    Msg::RulesDetailParentVersion,
                    &[
                        Arg::Name.with(parent),
                        Arg::Value.with(&version.to_string()),
                    ],
                ),
                None => self
                    .formatter
                    .text_with(Msg::RulesDetailParent, &[Arg::Name.with(parent)]),
            };
            provenance_parts.push(parent_text);
        }

        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_start()
            .justify_between()
            .gap(space.m)
            .pb(space.s)
            .border_b_1()
            .border_color(palette.stroke_divider)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(space.xs)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(space.s)
                            .child(ui.icon(
                                rule_kind_icon(node.kind),
                                Density::Standard,
                                palette.text_primary,
                            ))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .ramp(ui, Ramp::Subtitle)
                                    .text_color(palette.text_primary)
                                    .child(display_name.to_owned()),
                            )
                            .child(Self::badge(
                                ui,
                                self.formatter.text(source_msg),
                                BadgeTone::Neutral,
                            ))
                            .when(!node.enablement.is_effective(), |row| {
                                row.child(Self::badge(
                                    ui,
                                    self.formatter.text(Msg::RulesBadgeDisabled),
                                    BadgeTone::Neutral,
                                ))
                            }),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .font(ui.mono_font())
                            .text_color(palette.text_secondary)
                            .child(display_key.to_owned()),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(provenance_parts.join(" · ")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.s)
                    .children(edit_button)
                    .child(toggle_button)
                    .children(delete_button),
            )
    }

    fn render_detail_notices(
        &self,
        ui: &Widgets<'_>,
        node: &RuleTreeNode,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let space = &ui.metrics.space;
        let mut notices = Vec::new();

        if let DeleteConfirmation::Pending { key } = self.rules.delete_confirmation()
            && key == &node.key
        {
            let colors = ui.info_bar_colors(InfoBarStyle::Critical);
            let confirm_key = node.key.clone();
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
                            .child(div().ramp(ui, Ramp::BodyStrong).child(
                                self.formatter.text_with(
                                    Msg::RulesActionDeleteConfirmPrompt,
                                    &[Arg::Name.with(&node.key)],
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(space.s)
                            .child(
                                ui.accent_button(
                                    "rules-delete-confirm",
                                    Density::Compact,
                                    ButtonContent::IconLabel(
                                        IconName::Check,
                                        self.formatter.text(Msg::RulesActionDeleteConfirm),
                                    ),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.confirm_rule_set_delete(&confirm_key, cx);
                                    },
                                )),
                            )
                            .child(
                                ui.button(
                                    "rules-delete-cancel",
                                    Density::Compact,
                                    ButtonContent::IconLabel(
                                        IconName::Close,
                                        self.formatter.text(Msg::RulesActionCancel),
                                    ),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.cancel_rule_set_delete(cx);
                                    },
                                )),
                            ),
                    )
                    .into_any_element(),
            );
        }

        if let Some(notice) = self.rules.mutation_notice() {
            let message = match notice.error.code() {
                LoadDataErrorCode::Protocol(ErrorKind::RuleSetReadOnly { .. }) => {
                    Msg::RulesMutationFailedReadonly
                }
                LoadDataErrorCode::Protocol(ErrorKind::RuleSetHasChildren { .. }) => {
                    Msg::RulesMutationFailedHasChildren
                }
                LoadDataErrorCode::Protocol(ErrorKind::RuleSetNotFound { .. }) => {
                    Msg::RulesMutationFailedNotFound
                }
                _ => Msg::RulesMutationFailedGeneric,
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
                            .child(
                                div()
                                    .ramp(ui, Ramp::Body)
                                    .child(self.formatter.text(message)),
                            )
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .font(ui.mono_font())
                                    .text_color(colors.secondary)
                                    .child(notice.error.code().to_string()),
                            ),
                    )
                    .into_any_element(),
            );
        }

        if node.source == RuleSetSource::Builtin {
            notices.push(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesDetailBuiltinReadonly))
                    .into_any_element(),
            );
        }

        if node.enablement == NodeEnablement::DisabledByParent {
            notices.push(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesDetailDisabledByParent))
                    .into_any_element(),
            );
        }

        if node.source == RuleSetSource::User && !node.children.is_empty() {
            notices.push(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesActionDeleteHasChildren))
                    .into_any_element(),
            );
        }

        notices
    }

    fn render_detail_entries(
        &self,
        ui: &Widgets<'_>,
        node: &RuleTreeNode,
        detail: &RuleSetDetail,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let mut container = div().flex().flex_col().gap(space.s).child(
            div()
                .ramp(ui, Ramp::BodyStrong)
                .text_color(ui.palette.text_primary)
                .child(self.formatter.text_with(
                    Msg::RulesDetailEntriesHeading,
                    &[Arg::Count.with(&detail.entries.len().to_string())],
                )),
        );

        if detail.truncated {
            container = container.child(self.render_truncated_notice(
                ui,
                Msg::RulesDetailTruncated,
                detail.entries.len(),
            ));
        }

        if detail.entries.is_empty() {
            return container.child(
                div()
                    .py(space.m)
                    .ramp(ui, Ramp::Body)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesDetailEmpty)),
            );
        }

        container.children(
            detail
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| self.render_entry_row(ui, node, index, entry, cx)),
        )
    }

    fn render_entry_row(
        &self,
        ui: &Widgets<'_>,
        node: &RuleTreeNode,
        index: usize,
        entry: &RuleEntryDetail,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let inactive = !matches!(entry.state, RuleEntryState::Effective);
        let label_color = if inactive {
            palette.text_secondary
        } else {
            palette.text_primary
        };

        let (kind_msg, kind_tone) = match entry.entry_kind {
            RuleEntryKind::Rule => (Msg::RulesEntryKindRule, BadgeTone::Neutral),
            RuleEntryKind::Override => (Msg::RulesEntryKindOverride, BadgeTone::Caution),
            RuleEntryKind::Disable => (Msg::RulesEntryKindDisable, BadgeTone::Neutral),
        };
        let (state_msg, state_tone) = match entry.state {
            RuleEntryState::Effective => (Msg::RulesEntryStateEffective, BadgeTone::Accent),
            RuleEntryState::Disabled => (Msg::RulesEntryStateDisabled, BadgeTone::Neutral),
            RuleEntryState::Overridden => (Msg::RulesEntryStateOverridden, BadgeTone::Caution),
            RuleEntryState::TargetMissing => {
                (Msg::RulesEntryStateTargetMissing, BadgeTone::Critical)
            }
        };

        let category_chip = entry.category.as_ref().map(|uuid| {
            if matches!(self.rules.categories_state(), LoadState::Loading) {
                return ui
                    .skeleton_block(SkeletonTone::Subtle)
                    .w(px(SKELETON_CHIP_WIDTH))
                    .h(px(SKELETON_CHIP_HEIGHT))
                    .rounded(ui.metrics.radius.control)
                    .into_any_element();
            }
            match self.rules.resolve_entry_category(uuid) {
                ResolvedCategory::Active { name, color } => div()
                    .flex_shrink_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.xs)
                    .px(px(BADGE_PADDING_X))
                    .py(px(BADGE_PADDING_Y))
                    .rounded(ui.metrics.radius.control)
                    .bg(palette.fill_subtle_hover)
                    .when(palette.contrast == ContrastMode::High, |chip| {
                        chip.border_1().border_color(palette.stroke_card)
                    })
                    .child(ui.category_mark(ui.swatch(palette.category(color)), color))
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_primary)
                            .child(self.formatter.category_name(&name)),
                    )
                    .into_any_element(),
                ResolvedCategory::Missing => {
                    let fg = if palette.contrast == ContrastMode::High {
                        palette.text_on_accent
                    } else {
                        palette.status_caution
                    };
                    div()
                        .flex_shrink_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(space.xxs)
                        .px(px(BADGE_PADDING_X))
                        .py(px(BADGE_PADDING_Y))
                        .rounded(ui.metrics.radius.control)
                        .bg(palette.status_caution_bg)
                        .when(palette.contrast == ContrastMode::High, |chip| {
                            chip.border_1().border_color(palette.stroke_card)
                        })
                        .child(ui.icon(IconName::TriangleAlert, Density::Compact, fg))
                        .child(
                            div()
                                .ramp(ui, Ramp::Caption)
                                .text_color(fg)
                                .child(self.formatter.text(Msg::RulesEntryCategoryMissing)),
                        )
                        .into_any_element()
                }
            }
        });

        let target_line = entry
            .target
            .as_deref()
            .filter(|_| entry.entry_kind == RuleEntryKind::Override)
            .map(|target| {
                div()
                    .ramp(ui, Ramp::Caption)
                    .font(ui.mono_font())
                    .text_color(palette.text_secondary)
                    .child(
                        self.formatter
                            .text_with(Msg::RulesEntryTarget, &[Arg::Value.with(target)]),
                    )
            });

        let provenance_line = (!entry.provenance.is_empty()).then(|| {
            let chain = format_provenance_chain(&entry.provenance);
            div()
                .ramp(ui, Ramp::Caption)
                .text_color(palette.text_tertiary)
                .child(
                    self.formatter
                        .text_with(Msg::RulesEntryProvenance, &[Arg::Value.with(&chain)]),
                )
        });

        let override_child_btn = (node.source == RuleSetSource::Builtin
            && entry.entry_kind == RuleEntryKind::Rule)
            .then(|| entry.qualified_target(&node.key))
            .flatten()
            .map(|target| {
                let parent_key = node.key.clone();
                let kind = node.kind;
                let rule_id = entry.rule_id.clone().unwrap_or_else(|| target.clone());
                ui.button(
                    ("rules-override-child", index),
                    Density::Compact,
                    ButtonContent::Label(self.formatter.text(Msg::RulesActionOverrideInChild)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_override_child_rule_set(
                        parent_key.clone(),
                        kind,
                        rule_id.clone(),
                        target.clone(),
                        cx,
                    );
                }))
            });

        div()
            .id(("rules-entry", index))
            .flex()
            .flex_col()
            .gap(space.xs)
            .p(space.m)
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
                    .gap(space.s)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .ramp(
                                ui,
                                if inactive {
                                    Ramp::Body
                                } else {
                                    Ramp::BodyStrong
                                },
                            )
                            .text_color(label_color)
                            .child(self.formatter.rule_entry_label(&entry.label)),
                    )
                    .children(category_chip)
                    .child(Self::badge(ui, self.formatter.text(kind_msg), kind_tone))
                    .child(Self::badge(ui, self.formatter.text(state_msg), state_tone))
                    .when_some(entry.priority, |row, priority| {
                        row.child(
                            div()
                                .ramp(ui, Ramp::Caption)
                                .tabular_figures()
                                .text_color(palette.text_secondary)
                                .child(self.formatter.text_with(
                                    Msg::RulesEntryPriority,
                                    &[Arg::Priority.with(&priority.to_string())],
                                )),
                        )
                    }),
            )
            .children(target_line)
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .font(ui.mono_font())
                    .text_color(palette.text_secondary)
                    .whitespace_normal()
                    .child(
                        self.formatter
                            .entry_condition_summary(&entry.summary, self.rules.categories()),
                    ),
            )
            .children(provenance_line)
            .children(override_child_btn.map(|btn| div().pt(space.xxs).child(btn)))
    }

    pub(super) fn render_truncated_notice(&self, ui: &Widgets<'_>, msg: Msg, count: usize) -> Div {
        let space = &ui.metrics.space;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(space.xs)
            .px(space.s)
            .py(space.xs)
            .rounded(ui.metrics.radius.control)
            .bg(ui.palette.fill_subtle_hover)
            .ramp(ui, Ramp::Caption)
            .text_color(ui.palette.text_secondary)
            .child(ui.icon(
                IconName::TriangleAlert,
                Density::Compact,
                ui.palette.status_caution,
            ))
            .child(
                self.formatter
                    .text_with(msg, &[Arg::Count.with(&count.to_string())]),
            )
    }

    pub(super) fn render_rules_error_block<L>(
        &self,
        ui: &Widgets<'_>,
        title: Msg,
        message: Msg,
        error: &LoadDataError,
        retry_id: &'static str,
        on_retry: L,
    ) -> Div
    where
        L: Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
    {
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
                            .child(self.formatter.text(title)),
                    ),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(message)),
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
                        retry_id,
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::RetryDay)),
                    )
                    .on_click(on_retry),
                ),
            )
    }

    fn render_rules_stale_block(
        &self,
        ui: &Widgets<'_>,
        button_id: &'static str,
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
                        button_id,
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::RulesActionReload)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.reload_rules_tree(cx))),
                ),
            )
    }

    fn badge(ui: &Widgets<'_>, text: String, tone: BadgeTone) -> Div {
        let palette = ui.palette;
        let high_contrast = palette.contrast == ContrastMode::High;
        let (bg, fg) = match tone {
            BadgeTone::Neutral => {
                if high_contrast {
                    (palette.bg_base, palette.text_primary)
                } else {
                    (palette.fill_subtle_hover, palette.text_secondary)
                }
            }
            BadgeTone::Accent => (palette.accent_fill, palette.text_on_accent),
            BadgeTone::Caution => {
                if high_contrast {
                    (palette.status_caution_bg, palette.text_on_accent)
                } else {
                    (palette.status_caution_bg, palette.status_caution)
                }
            }
            BadgeTone::Critical => {
                if high_contrast {
                    (palette.status_critical_bg, palette.text_on_accent)
                } else {
                    (palette.status_critical_bg, palette.status_critical)
                }
            }
        };
        div()
            .flex_shrink_0()
            .px(px(BADGE_PADDING_X))
            .py(px(BADGE_PADDING_Y))
            .rounded(ui.metrics.radius.control)
            .bg(bg)
            .when(high_contrast, |pill| {
                pill.border_1().border_color(palette.stroke_card)
            })
            .ramp(ui, Ramp::Caption)
            .text_color(fg)
            .child(text)
    }

    fn render_categories_section(
        &self,
        ui: &Widgets<'_>,
        arrangement: ChartArrangement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let list_card = self.render_categories_card(ui, arrangement, cx);
        let editor_card = self
            .rules
            .category_editor()
            .map(|draft| self.render_category_editor_pane(ui, draft, arrangement, cx));

        match (editor_card, arrangement) {
            (Some(editor), ChartArrangement::SideBySide) => div()
                .flex()
                .flex_row()
                .items_start()
                .gap(space.s)
                .child(list_card)
                .child(editor)
                .into_any_element(),
            (Some(editor), ChartArrangement::Stacked) => div()
                .flex()
                .flex_col()
                .gap(space.s)
                .child(list_card)
                .child(editor)
                .into_any_element(),
            (None, _) => list_card,
        }
    }

    fn render_categories_card(
        &self,
        ui: &Widgets<'_>,
        arrangement: ChartArrangement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let has_editor = self.rules.category_editor().is_some();
        let body = match self.rules.categories_state() {
            LoadState::Loading => div()
                .flex()
                .flex_col()
                .gap(space.xs)
                .children((0..SKELETON_CATEGORY_ROWS).map(|_| {
                    ui.skeleton_block(SkeletonTone::Subtle)
                        .h(px(SKELETON_CATEGORY_HEIGHT))
                        .w_full()
                        .rounded(ui.metrics.radius.control)
                }))
                .into_any_element(),
            LoadState::Failed(error) => self
                .render_rules_error_block(
                    ui,
                    Msg::RulesCategoriesLoadFailedTitle,
                    if error.is_busy() {
                        Msg::DayBusy
                    } else {
                        Msg::RulesCategoriesLoadFailedMessage
                    },
                    error,
                    "retry-categories-list",
                    cx.listener(|this, _, _, cx| this.reload_categories(cx)),
                )
                .into_any_element(),
            LoadState::Stale => self
                .render_categories_stale_block(ui, cx)
                .into_any_element(),
            LoadState::Ready(catalog) => self
                .render_categories_list(ui, catalog, cx)
                .into_any_element(),
        };

        ui.card()
            .id("rules-categories-card")
            .flex()
            .flex_col()
            .gap(space.s)
            .p(space.l)
            .map(|card| match (has_editor, arrangement) {
                (true, ChartArrangement::SideBySide) => card.flex_1().min_w(px(0.)),
                _ => card.w_full(),
            })
            .child(
                div()
                    .ramp(ui, Ramp::BodyStrong)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(Msg::RulesCategoriesHeading)),
            )
            .child(body)
            .into_any_element()
    }

    fn render_categories_list(
        &self,
        ui: &Widgets<'_>,
        catalog: &CategoryCatalog,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let active = self.rules.active_categories();
        let notices = self.render_categories_notices(ui, catalog, cx);
        let mut container = div().flex().flex_col().gap(space.s).children(notices);

        if catalog.truncated {
            container = container.child(self.render_truncated_notice(
                ui,
                Msg::RulesCategoriesTruncated,
                catalog.items.len(),
            ));
        }

        let header_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(space.s)
            .px(space.s)
            .pb(space.xs)
            .border_b_1()
            .border_color(palette.stroke_divider)
            .ramp(ui, Ramp::Caption)
            .text_color(palette.text_secondary)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .child(self.formatter.text(Msg::RulesCategoriesColName)),
            )
            .child(
                div()
                    .w(px(CATEGORY_TENDENCY_COL_WIDTH))
                    .flex_shrink_0()
                    .child(self.formatter.text(Msg::RulesCategoriesColTendency)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .child(self.formatter.text(Msg::RulesCategoriesColActions)),
            );

        container = container.child(header_row);

        if active.is_empty() {
            container = container.child(
                div()
                    .py(space.m)
                    .px(space.s)
                    .ramp(ui, Ramp::Body)
                    .text_color(palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesCategoriesEmpty)),
            );
        } else {
            container = container.children(active.into_iter().enumerate().map(|(index, item)| {
                self.render_active_category_row(ui, catalog, index, item, cx)
            }));
        }

        container.child(self.render_archived_section(ui, cx))
    }

    fn render_categories_notices(
        &self,
        ui: &Widgets<'_>,
        catalog: &CategoryCatalog,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let space = &ui.metrics.space;
        let mut notices = Vec::new();

        if let ArchiveConfirmation::Pending { uuid } = self.rules.archive_confirmation()
            && let Some(item) = catalog.find(uuid)
        {
            let colors = ui.info_bar_colors(InfoBarStyle::Critical);
            let display_name = self.formatter.category_name(&item.display_name);
            let confirm_uuid = item.uuid.clone();
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
                            .child(div().ramp(ui, Ramp::BodyStrong).child(
                                self.formatter.text_with(
                                    Msg::RulesCategoriesArchiveConfirmPrompt,
                                    &[Arg::Name.with(&display_name)],
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(space.s)
                            .child(
                                ui.accent_button(
                                    "category-archive-confirm",
                                    Density::Compact,
                                    ButtonContent::IconLabel(
                                        IconName::Check,
                                        self.formatter.text(Msg::RulesCategoriesArchiveConfirm),
                                    ),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.confirm_archive_category(&confirm_uuid, cx);
                                    },
                                )),
                            )
                            .child(
                                ui.button(
                                    "category-archive-cancel",
                                    Density::Compact,
                                    ButtonContent::IconLabel(
                                        IconName::Close,
                                        self.formatter.text(Msg::RulesActionCancel),
                                    ),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.cancel_archive_category(cx);
                                    },
                                )),
                            ),
                    )
                    .into_any_element(),
            );
        }

        if let Some(notice) = self.rules.category_mutation_notice() {
            let message = match notice.error.code() {
                LoadDataErrorCode::Protocol(ErrorKind::BadRequest) => {
                    Msg::RulesCategoriesMutationFailedInvalid
                }
                LoadDataErrorCode::Protocol(
                    ErrorKind::Forbidden | ErrorKind::RuleSetReadOnly { .. },
                ) => Msg::RulesCategoriesMutationFailedReadonly,
                LoadDataErrorCode::Protocol(ErrorKind::Busy) => {
                    Msg::RulesCategoriesMutationFailedBusy
                }
                LoadDataErrorCode::Protocol(ErrorKind::CategoryNotFound) => {
                    Msg::RulesCategoriesMutationFailedNotFound
                }
                _ => Msg::RulesCategoriesMutationFailedGeneric,
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
                            .child(
                                div()
                                    .ramp(ui, Ramp::Body)
                                    .child(self.formatter.text(message)),
                            )
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .font(ui.mono_font())
                                    .text_color(colors.secondary)
                                    .child(notice.error.code().to_string()),
                            ),
                    )
                    .into_any_element(),
            );
        }

        notices
    }

    fn render_active_category_row(
        &self,
        ui: &Widgets<'_>,
        catalog: &CategoryCatalog,
        index: usize,
        item: &CategoryItem,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let busy = self.rules.category_mutating() == MutationActivity::Busy;
        let is_editing = matches!(
            self.rules.category_editor().map(|draft| &draft.mode),
            Some(CategoryEditorMode::Edit { uuid, .. }) if uuid == &item.uuid
        );
        let archive_pending = matches!(
            self.rules.archive_confirmation(),
            ArchiveConfirmation::Pending { uuid } if uuid == &item.uuid
        );
        let clamped_depth = item.depth.min(MAX_INDENT_DEPTH);
        let indent = f32::from(u16::try_from(clamped_depth).unwrap_or(0)) * CATEGORY_INDENT_STEP;
        let row_height = ui.metrics.density(Density::Standard).row_height;

        let can_up = !busy && catalog.can_reorder(&item.uuid, ReorderDirection::Up);
        let can_down = !busy && catalog.can_reorder(&item.uuid, ReorderDirection::Down);

        let up_label = self.formatter.text(Msg::RulesCategoriesActionMoveUp);
        let up_btn = if can_up {
            let uuid = item.uuid.clone();
            ui.button(
                ("category-up", index),
                Density::Compact,
                ButtonContent::Label(up_label),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.reorder_category(&uuid, ReorderDirection::Up, cx);
            }))
        } else {
            ui.disabled_button(
                ("category-up", index),
                Density::Compact,
                ButtonContent::Label(up_label),
            )
        };

        let down_label = self.formatter.text(Msg::RulesCategoriesActionMoveDown);
        let down_btn = if can_down {
            let uuid = item.uuid.clone();
            ui.button(
                ("category-down", index),
                Density::Compact,
                ButtonContent::Label(down_label),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.reorder_category(&uuid, ReorderDirection::Down, cx);
            }))
        } else {
            ui.disabled_button(
                ("category-down", index),
                Density::Compact,
                ButtonContent::Label(down_label),
            )
        };

        let edit_label = self.formatter.text(Msg::RulesCategoriesActionEdit);
        let edit_btn = if busy {
            ui.disabled_button(
                ("category-edit", index),
                Density::Compact,
                ButtonContent::Label(edit_label),
            )
        } else {
            let uuid = item.uuid.clone();
            ui.button(
                ("category-edit", index),
                Density::Compact,
                ButtonContent::Label(edit_label),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_edit_category(&uuid, window, cx);
            }))
        };

        let archive_label = self.formatter.text(Msg::RulesCategoriesActionArchive);
        let archive_btn = if busy || archive_pending {
            ui.disabled_button(
                ("category-archive", index),
                Density::Compact,
                ButtonContent::Label(archive_label),
            )
        } else {
            let uuid = item.uuid.clone();
            ui.button(
                ("category-archive", index),
                Density::Compact,
                ButtonContent::Label(archive_label),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.request_archive_category(&uuid, cx);
            }))
        };

        let origin_badge = match item.origin {
            CategoryOrigin::Builtin(_) => Self::badge(
                ui,
                self.formatter.text(Msg::RulesSourceBuiltin),
                BadgeTone::Neutral,
            ),
            CategoryOrigin::User => Self::badge(
                ui,
                self.formatter.text(Msg::RulesSourceUser),
                BadgeTone::Neutral,
            ),
        };
        let renamed_badge = (matches!(item.origin, CategoryOrigin::Builtin(_))
            && item.custom_name.is_some())
        .then(|| {
            Self::badge(
                ui,
                self.formatter.text(Msg::RulesCategoriesBadgeRenamed),
                BadgeTone::Caution,
            )
        });

        let swatch = ui.category_mark(
            div()
                .flex_shrink_0()
                .size(px(CATEGORY_SWATCH_SIZE))
                .rounded(px(CATEGORY_SWATCH_RADIUS))
                .border_1()
                .border_color(palette.stroke_control),
            item.color,
        );

        div()
            .id(("category-row", index))
            .relative()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(space.s)
            .min_h(row_height)
            .pl(px(indent) + space.s)
            .pr(space.s)
            .py(space.xs)
            .rounded(ui.metrics.radius.control)
            .when(is_editing, |row| {
                row.bg(palette.fill_subtle_hover)
                    .child(ui.indicator_bar(palette.accent_fill, row_height))
            })
            .when(!is_editing, |row| row.hover(ui.subtle_hover()))
            .child(swatch)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(space.xs)
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .ramp(
                                ui,
                                if item.depth == 0 {
                                    Ramp::BodyStrong
                                } else {
                                    Ramp::Body
                                },
                            )
                            .text_color(palette.text_primary)
                            .child(self.formatter.category_name(&item.display_name)),
                    )
                    .child(origin_badge)
                    .children(renamed_badge),
            )
            .child(
                div()
                    .w(px(CATEGORY_TENDENCY_COL_WIDTH))
                    .flex_shrink_0()
                    .child(Self::badge(
                        ui,
                        self.formatter.tendency(item.tendency),
                        BadgeTone::Neutral,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.xs)
                    .child(up_btn)
                    .child(down_btn)
                    .child(edit_btn)
                    .child(archive_btn),
            )
    }

    fn render_archived_section(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let archived_count = self.rules.archived_category_count();
        let visibility = self.rules.archived_visibility();
        let (toggle_icon, toggle_msg) = match visibility {
            ArchivedVisibility::Hidden => (
                IconName::ChevronRight,
                Msg::RulesCategoriesActionShowArchived,
            ),
            ArchivedVisibility::Visible => (
                IconName::ChevronDown,
                Msg::RulesCategoriesActionHideArchived,
            ),
        };
        let toggle_label = self
            .formatter
            .text_with(toggle_msg, &[Arg::Count.with(&archived_count.to_string())]);

        let mut section = div()
            .mt(space.s)
            .pt(space.s)
            .border_t_1()
            .border_color(palette.stroke_divider)
            .flex()
            .flex_col()
            .gap(space.s)
            .child(
                div().flex().flex_row().items_center().child(
                    ui.button(
                        "categories-toggle-archived",
                        Density::Compact,
                        ButtonContent::IconLabel(toggle_icon, toggle_label),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_archived_categories(cx);
                    })),
                ),
            );

        if visibility == ArchivedVisibility::Visible {
            section = section.child(
                div()
                    .px(space.s)
                    .py(space.xs)
                    .rounded(ui.metrics.radius.control)
                    .bg(palette.fill_card_secondary)
                    .border_1()
                    .border_color(palette.stroke_card)
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesCategoriesArchivedInfo)),
            );
            let archived_items = self.rules.visible_archived_categories();
            if archived_items.is_empty() {
                section = section.child(
                    div()
                        .px(space.s)
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text(Msg::RulesCategoriesArchivedEmpty)),
                );
            } else {
                section =
                    section.children(archived_items.into_iter().enumerate().map(
                        |(index, item)| self.render_archived_category_row(ui, index, item, cx),
                    ));
            }
        }

        section
    }

    fn render_archived_category_row(
        &self,
        ui: &Widgets<'_>,
        index: usize,
        item: &CategoryItem,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let busy = self.rules.category_mutating() == MutationActivity::Busy;
        let restore_label = self.formatter.text(Msg::RulesCategoriesActionRestore);
        let restore_btn = if busy {
            ui.disabled_button(
                ("category-restore", index),
                Density::Compact,
                ButtonContent::Label(restore_label),
            )
        } else {
            let uuid = item.uuid.clone();
            ui.button(
                ("category-restore", index),
                Density::Compact,
                ButtonContent::Label(restore_label),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.restore_category(&uuid, cx);
            }))
        };

        let origin_badge = match item.origin {
            CategoryOrigin::Builtin(_) => Self::badge(
                ui,
                self.formatter.text(Msg::RulesSourceBuiltin),
                BadgeTone::Neutral,
            ),
            CategoryOrigin::User => Self::badge(
                ui,
                self.formatter.text(Msg::RulesSourceUser),
                BadgeTone::Neutral,
            ),
        };

        let swatch = ui.category_mark(
            div()
                .flex_shrink_0()
                .size(px(CATEGORY_SWATCH_SIZE))
                .rounded(px(CATEGORY_SWATCH_RADIUS))
                .border_1()
                .border_color(palette.stroke_control),
            item.color,
        );

        div()
            .id(("category-archived-row", index))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(space.s)
            .p(space.s)
            .rounded(ui.metrics.radius.control)
            .bg(palette.fill_card_secondary)
            .border_1()
            .border_color(palette.stroke_card)
            .child(swatch)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(space.xs)
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .ramp(ui, Ramp::Body)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.category_name(&item.display_name)),
                    )
                    .child(Self::badge(
                        ui,
                        self.formatter.text(Msg::RulesCategoriesBadgeArchived),
                        BadgeTone::Neutral,
                    ))
                    .child(origin_badge)
                    .child(Self::badge(
                        ui,
                        self.formatter.tendency(item.tendency),
                        BadgeTone::Neutral,
                    )),
            )
            .child(restore_btn)
    }

    fn render_category_editor_pane(
        &self,
        ui: &Widgets<'_>,
        draft: &CategoryEditorDraft,
        arrangement: ChartArrangement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let busy = self.rules.category_mutating() == MutationActivity::Busy;
        let (title_msg, submit_msg, origin_badge, is_builtin) = match &draft.mode {
            CategoryEditorMode::Create { .. } => (
                Msg::RulesCategoriesEditorCreateTitle,
                Msg::RulesCategoriesEditorCreateSubmit,
                None,
                false,
            ),
            CategoryEditorMode::Edit { origin, .. } => {
                let badge = match origin {
                    CategoryOrigin::Builtin(_) => Msg::RulesSourceBuiltin,
                    CategoryOrigin::User => Msg::RulesSourceUser,
                };
                (
                    Msg::RulesCategoriesEditorEditTitle,
                    Msg::RulesCategoriesEditorSaveSubmit,
                    Some(badge),
                    matches!(origin, CategoryOrigin::Builtin(_)),
                )
            }
        };

        let reset_default_btn = (is_builtin && !draft.name_input.trim().is_empty()).then(|| {
            let label = self.formatter.text(Msg::RulesCategoriesEditorResetDefault);
            if busy {
                ui.disabled_button(
                    "category-reset-name",
                    Density::Compact,
                    ButtonContent::Label(label),
                )
            } else {
                ui.button(
                    "category-reset-name",
                    Density::Compact,
                    ButtonContent::Label(label),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.reset_builtin_category_name(window, cx);
                }))
            }
        });

        let name_control = match (&self.category_name_input, busy) {
            (Some(input_state), false) => Input::new(input_state)
                .ramp(ui, Ramp::Body)
                .into_any_element(),
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
                .child(draft.name_input.clone())
                .into_any_element(),
        };

        let validation_notice = draft.validation_error.map(|error| {
            let msg = match error {
                ValidateCategoryNameError::Blank => Msg::RulesCategoriesValidationBlank,
                ValidateCategoryNameError::TooLong => Msg::RulesCategoriesValidationTooLong,
                ValidateCategoryNameError::Reserved => Msg::RulesCategoriesValidationReserved,
            };
            div()
                .ramp(ui, Ramp::Caption)
                .text_color(palette.status_critical)
                .child(self.formatter.text(msg))
        });

        let builtin_hint = is_builtin.then(|| {
            div()
                .ramp(ui, Ramp::Caption)
                .text_color(palette.text_secondary)
                .child(self.formatter.text(Msg::RulesCategoriesEditorBuiltinHint))
        });

        let palette_tray = div()
            .p(space.s)
            .rounded(ui.metrics.radius.control)
            .bg(palette.bg_base)
            .border_1()
            .border_color(palette.stroke_card)
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(space.s)
            .children(
                CategoryColor::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, color)| {
                        let selected = draft.color == color;
                        let fill = palette.category(color);
                        let check_color = palette.text_on(fill);
                        let swatch = ui
                            .category_mark(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(PALETTE_SWATCH_WIDTH))
                                    .h(px(PALETTE_SWATCH_HEIGHT))
                                    .rounded(ui.metrics.radius.control)
                                    .border(if selected {
                                        px(PALETTE_SWATCH_SELECTED_BORDER)
                                    } else {
                                        ui.metrics.layout.divider
                                    })
                                    .border_color(if selected {
                                        palette.stroke_focus_outer
                                    } else {
                                        palette.stroke_control
                                    }),
                                color,
                            )
                            .id(("category-swatch", index))
                            .when(selected, |item| {
                                item.child(ui.icon(IconName::Check, Density::Compact, check_color))
                            });
                        if busy {
                            swatch.cursor_default().into_any_element()
                        } else {
                            swatch
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_category_editor_color(color, cx);
                                }))
                                .into_any_element()
                        }
                    }),
            );

        let tendency_control = ui.segmented(
            "category-editor-tendency",
            [
                Segment {
                    id: "category-tendency-focus",
                    label: self.formatter.tendency(Tendency::Focus),
                    value: Tendency::Focus,
                },
                Segment {
                    id: "category-tendency-neutral",
                    label: self.formatter.tendency(Tendency::Neutral),
                    value: Tendency::Neutral,
                },
                Segment {
                    id: "category-tendency-distraction",
                    label: self.formatter.tendency(Tendency::Distraction),
                    value: Tendency::Distraction,
                },
            ],
            draft.tendency,
            |tendency| {
                cx.listener(move |this, _, _, cx| {
                    if this.rules.category_mutating() == MutationActivity::Idle {
                        this.set_category_editor_tendency(tendency, cx);
                    }
                })
            },
        );

        let submit_content =
            ButtonContent::IconLabel(IconName::Check, self.formatter.text(submit_msg));
        let submit_btn = if busy {
            ui.disabled_button("category-editor-submit", Density::Compact, submit_content)
        } else {
            ui.accent_button("category-editor-submit", Density::Compact, submit_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.submit_category_editor(cx);
                }))
        };

        let cancel_content =
            ButtonContent::IconLabel(IconName::Close, self.formatter.text(Msg::RulesActionCancel));
        let cancel_btn = if busy {
            ui.disabled_button("category-editor-cancel", Density::Compact, cancel_content)
        } else {
            ui.button("category-editor-cancel", Density::Compact, cancel_content)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_category_editor(cx);
                }))
        };

        let close_btn = if busy {
            ui.disabled_button(
                "category-editor-close",
                Density::Compact,
                ButtonContent::Icon(IconName::Close),
            )
        } else {
            ui.button(
                "category-editor-close",
                Density::Compact,
                ButtonContent::Icon(IconName::Close),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_category_editor(cx);
            }))
        };

        ui.card()
            .id("rules-category-editor")
            .flex()
            .flex_col()
            .gap(space.m)
            .p(space.l)
            .map(|card| match arrangement {
                ChartArrangement::SideBySide => card.w(px(CATEGORY_EDITOR_WIDTH)).flex_shrink_0(),
                ChartArrangement::Stacked => card.w_full(),
            })
            .child(
                div()
                    .flex()
                    .flex_row()
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
                            .child(
                                div()
                                    .ramp(ui, Ramp::BodyStrong)
                                    .text_color(palette.text_primary)
                                    .child(self.formatter.text(title_msg)),
                            )
                            .children(origin_badge.map(|msg| {
                                Self::badge(ui, self.formatter.text(msg), BadgeTone::Neutral)
                            })),
                    )
                    .child(close_btn),
            )
            .child(
                div()
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
                            .gap(space.xs)
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .text_color(palette.text_secondary)
                                    .child(
                                        self.formatter.text(Msg::RulesCategoriesEditorNameLabel),
                                    ),
                            )
                            .children(reset_default_btn),
                    )
                    .child(name_control)
                    .children(validation_notice)
                    .children(builtin_hint),
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
                            .child(self.formatter.text(Msg::RulesCategoriesEditorColorLabel)),
                    )
                    .child(palette_tray),
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
                            .child(self.formatter.text(Msg::RulesCategoriesEditorTendencyLabel)),
                    )
                    .child(tendency_control),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.s)
                    .pt(space.s)
                    .border_t_1()
                    .border_color(palette.stroke_divider)
                    .child(submit_btn)
                    .child(cancel_btn),
            )
            .into_any_element()
    }

    fn render_categories_stale_block(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> Div {
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
                            .child(self.formatter.text(Msg::RulesCategoriesStaleTitle)),
                    ),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(Msg::RulesCategoriesStaleMessage)),
            )
            .child(
                div().child(
                    ui.button(
                        "reload-stale-categories",
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::RulesActionReload)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.reload_categories(cx))),
                ),
            )
    }
}

fn generate_category_uuid() -> Option<Uuid> {
    match windows::core::GUID::new().map(|id| format!("{id:?}")) {
        Ok(text) => match text.parse() {
            Ok(uuid) => Some(uuid),
            Err(error) => {
                tracing::warn!(%error, "could not parse category identity");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, "could not create category identity");
            None
        }
    }
}

const fn rule_kind_icon(kind: RuleKind) -> IconName {
    match kind {
        RuleKind::Classification => IconName::Tag,
        RuleKind::Extraction => IconName::FileText,
        RuleKind::Privacy => IconName::Shield,
    }
}

const fn rule_kind_msg(kind: RuleKind) -> Msg {
    match kind {
        RuleKind::Classification => Msg::RulesKindClassification,
        RuleKind::Extraction => Msg::RulesKindExtraction,
        RuleKind::Privacy => Msg::RulesKindPrivacy,
    }
}

fn format_provenance_chain(provenance: &[RuleProvenance]) -> String {
    provenance
        .iter()
        .map(|step| format!("{} v{}", step.key, step.version))
        .collect::<Vec<_>>()
        .join(" → ")
}
