//! The approved error card and connecting state, shared by startup and main windows.

use super::{
    Ramp, RampExt as _, Widgets, icons::AppAssets, metrics::Density, tokens::ContrastMode,
    widgets::ButtonContent,
};
use crate::{
    failure::{
        CopyPlacement, DetailLine, DetailsExpansion, Failure, FailureAction, FailurePresentation,
        Severity,
    },
    format::{Formatter, Msg},
    text_scale::TextScale,
};
use gpui_kit::{
    ClipboardItem, Context, Div, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Icon, Sizable as _, scroll::ScrollableElement as _, spinner::Spinner,
    },
    div, px,
};

#[derive(Debug, Clone, Copy)]
enum ProgressSize {
    Inline,
    Standalone,
}

/// A running recovery action disables both recovery buttons until the result arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RecoveryActivity {
    #[default]
    Idle,
    StartingCore,
}

#[derive(Debug)]
pub(crate) struct FailureScreen {
    failure: Option<Failure>,
    formatter: Formatter,
    expansion: DetailsExpansion,
    activity: RecoveryActivity,
    text_scale: TextScale,
}
impl EventEmitter<FailureAction> for FailureScreen {}
impl FailureScreen {
    pub(crate) fn new(
        failure: Option<Failure>,
        formatter: Formatter,
        text_scale: TextScale,
    ) -> Self {
        let expansion = failure
            .as_ref()
            .map_or(DetailsExpansion::Collapsed, |failure| {
                failure.kind().details_expansion()
            });
        Self {
            failure,
            formatter,
            expansion,
            activity: RecoveryActivity::Idle,
            text_scale,
        }
    }
    pub(crate) fn set_failure(&mut self, failure: Option<Failure>, cx: &mut Context<Self>) {
        self.expansion = failure
            .as_ref()
            .map_or(DetailsExpansion::Collapsed, |failure| {
                failure.kind().details_expansion()
            });
        self.failure = failure;
        self.activity = RecoveryActivity::Idle;
        cx.notify();
    }
    pub(crate) fn set_activity(&mut self, activity: RecoveryActivity, cx: &mut Context<Self>) {
        self.activity = activity;
        cx.notify();
    }
    pub(crate) fn set_format(
        &mut self,
        formatter: Formatter,
        text_scale: TextScale,
        cx: &mut Context<Self>,
    ) {
        if self.formatter != formatter || self.text_scale != text_scale {
            self.formatter = formatter;
            self.text_scale = text_scale;
            cx.notify();
        }
    }
    fn copy_details(&self, cx: &mut Context<Self>) {
        if let Some(failure) = &self.failure {
            cx.write_to_clipboard(ClipboardItem::new_string(
                FailurePresentation::new(failure, self.formatter).copy_text(),
            ));
        }
    }
    fn emit_action(&self, action: FailureAction, cx: &mut Context<Self>) {
        if action == FailureAction::CopyDetails {
            self.copy_details(cx);
        } else if self.activity == RecoveryActivity::Idle || action == FailureAction::Exit {
            cx.emit(action);
        }
    }
    fn ring(ui: &Widgets<'_>, size: ProgressSize, color: gpui_kit::Hsla) -> impl IntoElement {
        let diameter = match size {
            ProgressSize::Inline => ui.metrics.density(Density::Compact).icon_size,
            ProgressSize::Standalone => ui.metrics.failure.glyph_size,
        };
        let track = match size {
            ProgressSize::Inline if ui.palette.contrast == ContrastMode::Standard => {
                color.opacity(ui.metrics.failure.busy_track_opacity)
            }
            ProgressSize::Inline => color,
            ProgressSize::Standalone => ui.palette.stroke_control_strong,
        };
        div()
            .relative()
            .size(diameter)
            .flex_shrink_0()
            .child(
                Icon::default()
                    .path(AppAssets::progress_track_path())
                    .size(diameter)
                    .text_color(track)
                    .absolute()
                    .inset_0(),
            )
            .child(
                div().absolute().inset_0().child(
                    Spinner::new()
                        .icon(Icon::default().path(AppAssets::progress_path()))
                        .color(color)
                        .with_size(diameter),
                ),
            )
    }
    fn card(
        &self,
        failure: &Failure,
        ui: &Widgets<'_>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let presentation = FailurePresentation::new(failure, self.formatter);
        let space = &ui.metrics.space;
        let geometry = &ui.metrics.failure;
        let palette = ui.palette;
        let foreground = match failure.severity() {
            Severity::Recoverable => palette.status_caution,
            Severity::Unrecoverable => palette.status_critical,
        };
        let background = match failure.severity() {
            Severity::Recoverable => palette.status_caution_bg,
            Severity::Unrecoverable => palette.status_critical_bg,
        };
        // High contrast uses opaque system colors rather than tinted surfaces.
        let status_text = if palette.contrast == ContrastMode::High {
            palette.text_on_accent
        } else {
            foreground
        };
        let status_border = if palette.contrast == ContrastMode::High {
            palette.text_primary
        } else {
            foreground.opacity(geometry.status_border_opacity)
        };
        let badge = div()
            .flex()
            .items_center()
            .gap(geometry.badge_dot)
            .px(space.s)
            .min_h(ui.height_for_text(
                ui.metrics.density(Density::Standard).icon_size,
                Ramp::Caption,
            ))
            .rounded(ui.metrics.radius.control)
            .bg(background)
            .text_color(status_text)
            .ramp(ui, Ramp::Caption)
            .child(
                div()
                    .size(geometry.badge_dot)
                    .rounded_full()
                    .bg(status_text),
            )
            .child(self.formatter.text(match failure.severity() {
                Severity::Recoverable => Msg::FailureRecoverable,
                Severity::Unrecoverable => Msg::FailureUnrecoverable,
            }));
        let header = div()
            .flex()
            .items_start()
            .gap(space.l)
            .px(space.xxl)
            .pt(space.xxl)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
                    .mt(space.xxs)
                    .size(geometry.tile_size)
                    .rounded(ui.metrics.radius.overlay)
                    .bg(background)
                    .border(ui.metrics.layout.divider)
                    .border_color(status_border)
                    .child(
                        Icon::default()
                            .path(AppAssets::status_path(failure.severity()))
                            .size(geometry.glyph_size)
                            .text_color(status_text),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(space.xs)
                    .child(div().flex().child(badge))
                    .child(
                        div()
                            .ramp(ui, Ramp::Subtitle)
                            .whitespace_normal()
                            .child(presentation.title),
                    ),
            );
        let mut actions = div().flex().items_center().flex_wrap().gap(space.s);
        for (index, &action) in presentation.actions.iter().enumerate() {
            let primary = index == 0;
            let busy = self.activity == RecoveryActivity::StartingCore;
            let label = if busy && primary {
                self.formatter.text(Msg::FailureStarting)
            } else {
                action.label(self.formatter)
            };
            let content = if action == FailureAction::CopyDetails {
                ButtonContent::IconLabel(IconName::Copy, label)
            } else {
                ButtonContent::Label(label)
            };
            let color = if primary {
                palette.text_on_accent
            } else {
                palette.text_primary
            };
            let mut button = if primary {
                ui.accent_button(action.element_id(), Density::Standard, content)
            } else {
                ui.button(action.element_id(), Density::Standard, content)
            }
            .px(space.l);
            if busy && action != FailureAction::Exit {
                button = button.cursor_default();
                if primary {
                    // Keep the ring before the label by using a separate content row.
                    actions = actions.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(space.s)
                            .px(space.l)
                            .min_h(ui.metrics.density(Density::Standard).control_height)
                            .rounded(ui.metrics.radius.control)
                            .bg(if palette.contrast == ContrastMode::Standard {
                                palette.accent_fill.opacity(geometry.busy_opacity)
                            } else {
                                palette.accent_fill
                            })
                            .text_color(color)
                            .ramp(ui, Ramp::Body)
                            .child(Self::ring(ui, ProgressSize::Inline, color))
                            .child(self.formatter.text(Msg::FailureStarting)),
                    );
                    continue;
                }
                button = button
                    .bg(palette.fill_control_disabled)
                    .text_color(palette.text_disabled)
                    .hover(move |style| style.bg(palette.fill_control_disabled))
                    .active(move |style| style.bg(palette.fill_control_disabled));
            } else {
                button = button
                    .on_click(cx.listener(move |this, _, _, cx| this.emit_action(action, cx)));
            }
            actions = actions.child(button);
        }
        let main = div()
            .flex()
            .flex_col()
            .gap(space.l)
            .pt(space.s)
            .pr(space.xxl)
            .pb(space.xxl)
            .pl(space.xxl + geometry.tile_size + space.l)
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(palette.text_secondary)
                    .whitespace_normal()
                    .child(presentation.message),
            )
            .child(actions);
        let mut footer_header = div()
            .flex()
            .items_center()
            .gap(space.s)
            .px(space.xxl)
            .py(space.xs)
            .min_h(ui.metrics.density(Density::Standard).row_height)
            .child(
                div()
                    .id("toggle-error-details")
                    .flex()
                    .flex_1()
                    .min_w(px(0.))
                    .items_center()
                    .gap(space.s)
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.expansion.toggle();
                        cx.notify();
                    }))
                    .child(ui.icon(
                        if self.expansion == DetailsExpansion::Expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        Density::Compact,
                        palette.text_secondary,
                    ))
                    .child(
                        div()
                            .ramp(ui, Ramp::Body)
                            .text_color(if self.expansion == DetailsExpansion::Expanded {
                                palette.text_primary
                            } else {
                                palette.text_secondary
                            })
                            .child(self.formatter.text(Msg::FailureDetails)),
                    ),
            );
        if failure.kind().copy_placement() == CopyPlacement::DetailsFooter {
            footer_header = footer_header.child(
                ui.button(
                    "copy-error-footer",
                    Density::Compact,
                    ButtonContent::IconLabel(
                        IconName::Copy,
                        self.formatter.text(Msg::FailureCopyDetails),
                    ),
                )
                .bg(palette.transparent)
                .border_color(palette.transparent)
                .text_color(palette.text_secondary)
                .px(space.s)
                .on_click(cx.listener(|this, _, _, cx| this.copy_details(cx))),
            );
        }
        let mut footer = div()
            .bg(palette.fill_card_secondary)
            .border_t(ui.metrics.layout.divider)
            .border_color(palette.stroke_divider)
            .child(footer_header);
        if self.expansion == DetailsExpansion::Expanded {
            footer = footer.child(self.details(presentation.details, ui, window, cx));
        }
        ui.card()
            .w(geometry.card_width)
            .max_w_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .m_auto()
            .overflow_hidden()
            .child(header)
            .child(main)
            .child(footer)
    }
    fn details(
        &self,
        lines: Vec<DetailLine>,
        ui: &Widgets<'_>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(space.m)
            .px(space.xxl)
            .pt(space.m)
            .pb(space.l)
            .border_t(ui.metrics.layout.divider)
            .border_color(ui.palette.stroke_divider);
        let mut values = div().flex().flex_col().gap(space.xs);
        // The widest actual localized label defines the shared key column.
        let measure =
            super::measure::TextMeasure::new(window, *ui, window.text_style().font_family);
        let key_width = lines
            .iter()
            .filter_map(|line| match line {
                DetailLine::Value { label, .. } => Some(label),
                DetailLine::Command(_) | DetailLine::Text(_) => None,
            })
            .map(|label| px(measure.width(label, Ramp::Caption)))
            .fold(px(0.), gpui_kit::Pixels::max);
        for (index, line) in lines.into_iter().enumerate() {
            match line {
                DetailLine::Value { label, value } => {
                    values = values.child(
                        div()
                            .flex()
                            .items_start()
                            .gap(space.l)
                            .ramp(ui, Ramp::Caption)
                            .child(
                                div()
                                    .w(key_width)
                                    .flex_shrink_0()
                                    .text_color(ui.palette.text_secondary)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .font(ui.mono_font())
                                    .whitespace_normal()
                                    .child(value),
                            ),
                    );
                }
                DetailLine::Text(text) => {
                    body = body.child(values);
                    values = div().flex().flex_col().gap(space.xs);
                    body = body.child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(ui.palette.text_secondary)
                            .whitespace_normal()
                            .child(text),
                    );
                }
                DetailLine::Command(command) => {
                    body = body.child(values);
                    values = div().flex().flex_col().gap(space.xs);
                    let copied = command.clone();
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space.xs)
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .text_color(ui.palette.text_secondary)
                                    .child(self.formatter.text(Msg::FailureRestoreCommand)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(space.s)
                                    .p(space.s)
                                    .pl(space.m)
                                    .bg(ui.palette.bg_base)
                                    .border(ui.metrics.layout.divider)
                                    .border_color(ui.palette.stroke_control)
                                    .rounded(ui.metrics.radius.control)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .ramp(ui, Ramp::Caption)
                                            .line_height(
                                                ui.metrics.failure.command_line_height
                                                    * ui.text_scale.for_gpui(),
                                            )
                                            .font(ui.mono_font())
                                            .whitespace_normal()
                                            .child(command),
                                    )
                                    .child(
                                        ui.button(
                                            ("copy-restore-command", index),
                                            Density::Compact,
                                            ButtonContent::IconLabel(
                                                IconName::Copy,
                                                self.formatter.text(Msg::FailureCopyCommand),
                                            ),
                                        )
                                        .flex_shrink_0()
                                        .px(space.s)
                                        .ramp(ui, Ramp::Caption)
                                        .on_click(
                                            cx.listener(move |_, _, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copied.clone(),
                                                ))
                                            }),
                                        ),
                                    ),
                            ),
                    );
                }
            }
        }
        body.child(values)
    }
}
impl Render for FailureScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = super::system_theme::SystemTheme::tokens(cx);
        let ui = Widgets::new(&tokens, cx.theme().mode, self.text_scale);
        let content = match &self.failure {
            Some(failure) => self.card(failure, &ui, window, cx),
            None => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(ui.metrics.space.m)
                .m_auto()
                .flex_shrink_0()
                .child(Self::ring(
                    &ui,
                    ProgressSize::Standalone,
                    ui.palette.accent_fill,
                ))
                .child(
                    div()
                        .ramp(&ui, Ramp::Body)
                        .text_color(ui.palette.text_secondary)
                        .child(self.formatter.text(Msg::LinkConnecting)),
                ),
        };
        div()
            .id("error-stage")
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.))
            .p(ui.metrics.space.xxl)
            .child(content)
            .overflow_y_scrollbar()
    }
}
