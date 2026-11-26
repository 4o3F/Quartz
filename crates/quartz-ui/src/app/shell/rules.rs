//! Rules page: rule-set tree, selected set detail, enable/disable toggle, and confirmed deletion.

use gpui_kit::{
    AnyElement, Context, Div, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px,
};
use quartz_protocol::{
    ErrorKind, RuleEntryKind, RuleEntryState, RuleKind, RuleProvenance, RuleSetSource, RuleView,
    RuleWarningCode,
};

use super::{AppShell, Frame};
use crate::{
    app::{
        metrics::{Density, Ramp},
        tokens::ContrastMode,
        widgets::{ButtonContent, InfoBarStyle, RampExt as _, SkeletonTone, Widgets},
    },
    format::{Arg, Msg},
    layout::{ChartArrangement, WindowClass},
    link::{LoadDataError, LoadDataErrorCode, RequestCoreError},
    model::{
        DeleteConfirmation, FlatTreeRow, LoadState, MutationActivity, NodeEnablement,
        RuleEntryDetail, RuleSetDetail, RuleSetEnablement, RuleTree, RuleTreeNode, RulesEffect,
        RulesKeyAction, RulesMutation, TreeDirection,
    },
};

/// Fixed width of the left rule-set tree card when shown side-by-side (07 section 6).
const TREE_PANE_WIDTH: f32 = 300.;
/// Horizontal indentation per tree depth level.
const TREE_INDENT_STEP: f32 = 16.;
/// Maximum tree depth applied to horizontal indentation so deep user trees stay readable.
const MAX_INDENT_DEPTH: usize = 6;
/// Minimum height of one tree row; expands with text scale.
const TREE_ROW_MIN_HEIGHT: f32 = 44.;
/// Static skeleton row count while loading the rule-set tree.
const SKELETON_TREE_ROWS: usize = 5;
/// Height of one static tree skeleton placeholder.
const SKELETON_TREE_HEIGHT: f32 = 40.;
/// Static skeleton row count while loading rule-set entries.
const SKELETON_DETAIL_ROWS: usize = 4;
/// Height of one static detail skeleton placeholder.
const SKELETON_DETAIL_HEIGHT: f32 = 52.;
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

    /// Handles Up/Down/Enter/Space/Escape on the Rules page.
    pub(super) fn handle_rules_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> KeyHandled {
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

    fn run_rules_effect(&mut self, effect: RulesEffect, cx: &mut Context<Self>) {
        match effect {
            RulesEffect::None => {}
            RulesEffect::LoadTree { generation } => {
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
                        cx.notify();
                    });
                })
                .detach();
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
                        let next = this.rules.apply_mutation_finished(generation, result);
                        this.run_rules_effect(next, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// Renders the Rules page: header and responsive two-pane (or stacked) tree + detail layout.
    pub(super) fn render_rules(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let arrangement = frame.layout.class.chart_arrangement();
        let busy = matches!(self.rules.tree_state(), LoadState::Loading)
            || self.rules.mutating() == MutationActivity::Busy;

        let header = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(space.s)
            .child(
                div()
                    .ramp(ui, Ramp::Title)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(Msg::NavRules)),
            )
            .child(if busy {
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
                .on_click(cx.listener(|this, _, _, cx| this.reload_rules_tree(cx)))
            });

        let tree_pane = self.render_rules_tree_pane(ui, arrangement, cx);
        let detail_pane = self.render_rules_detail_pane(ui, arrangement, cx);

        let panes = match arrangement {
            ChartArrangement::SideBySide => div()
                .flex()
                .flex_row()
                .items_start()
                .gap(space.s)
                .child(tree_pane)
                .child(detail_pane),
            ChartArrangement::Stacked => div()
                .flex()
                .flex_col()
                .gap(space.s)
                .child(tree_pane)
                .child(detail_pane),
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
            .child(panes);

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
                    .px(space.xs)
                    .ramp(ui, Ramp::BodyStrong)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(Msg::RulesTreeTitle)),
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
                                    .child(node.key.clone()),
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
            LoadState::Ready(detail) => self.render_detail_entries(ui, detail).into_any_element(),
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
        let display_version = detail.map_or(node.version, |loaded| loaded.version);
        let parent_version = detail.and_then(|loaded| loaded.parent_version);
        let toggle_label = match node.self_enablement {
            RuleSetEnablement::Enabled => self.formatter.text(Msg::RulesActionDisable),
            RuleSetEnablement::Disabled => self.formatter.text(Msg::RulesActionEnable),
        };
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
                                    .child(display_key.to_owned()),
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

    fn render_detail_entries(&self, ui: &Widgets<'_>, detail: &RuleSetDetail) -> Div {
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
                .map(|(index, entry)| self.render_entry_row(ui, index, entry)),
        )
    }

    fn render_entry_row(
        &self,
        ui: &Widgets<'_>,
        index: usize,
        entry: &RuleEntryDetail,
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
                    .child(self.formatter.entry_condition_summary(&entry.summary)),
            )
            .children(provenance_line)
    }

    fn render_truncated_notice(&self, ui: &Widgets<'_>, msg: Msg, count: usize) -> Div {
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

    fn render_rules_error_block<L>(
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
