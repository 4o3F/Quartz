//! Details sidebar (07 section 8): subject header, selected interval, 7-day bars, common titles,
//! explanation tree and actions.

use gpui_kit::{
    ClickEvent, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, assets::IconName, div, px,
    relative,
};

use super::AppShell;
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, RampExt as _, Widgets},
    },
    format::{Arg, Msg, SourceText},
    model::{BarDay, Details, Explanation, IntervalCard, Share},
};

/// Height of the 7-day chart.
const WEEK_CHART_HEIGHT: f32 = 88.;
/// Width of one day's bar.
const WEEK_BAR_WIDTH: f32 = 36.;
/// Rows of the common titles list.
const TITLE_ROWS: usize = 4;
/// Height of one common title row.
const TITLE_ROW_HEIGHT: f32 = 28.;
/// Width of a title's proportion bar.
const TITLE_BAR_WIDTH: f32 = 56.;
/// Height of a title's proportion bar.
const TITLE_BAR_HEIGHT: f32 = 4.;
/// Width of a title's duration column.
const TITLE_VALUE_WIDTH: f32 = 72.;

impl AppShell {
    pub(super) fn render_details(
        &self,
        ui: &Widgets<'_>,
        details: &Details,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = ui.palette;
        let space = &ui.metrics.space;
        let color = palette.category(self.day.category(details.category).color);
        let body = div()
            .id("details-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(space.l)
            .px(space.l)
            .pb(space.l)
            .children(
                details
                    .interval
                    .as_ref()
                    .map(|interval| self.interval_card(ui, interval)),
            )
            .child(self.section(ui, Msg::DetailsWeek, self.week_chart(ui, details, color)))
            .child(self.section(ui, Msg::DetailsTitles, self.titles(ui, details, color)))
            .child(self.section(
                ui,
                Msg::DetailsWhy,
                self.explanation(ui, details.explanation.as_ref()),
            ));
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(ui.metrics.layout.panel_width)
            .h_full()
            .bg(palette.fill_layer)
            .border_l_1()
            .border_color(palette.stroke_divider)
            .child(self.details_header(ui, details, cx))
            .child(body)
            .child(self.details_actions(ui))
    }

    fn details_header(&self, ui: &Widgets<'_>, details: &Details, cx: &mut Context<Self>) -> Div {
        let palette = ui.palette;
        let space = &ui.metrics.space;
        let app = self.day.app(details.app);
        let close_size = ui.metrics.density(Density::Compact).control_height;
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(space.s)
            .p(space.l)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(space.s)
                    .child(div().pt(space.xxs).child(ui.icon(
                        app.glyph.into(),
                        Density::Compact,
                        palette.text_primary,
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().ramp(ui, Ramp::BodyStrong).child(app.name.clone()))
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .text_color(palette.text_secondary)
                                    .child(app.exe.clone()),
                            ),
                    ),
            )
            .child(
                div()
                    .id("details-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(close_size)
                    .rounded(ui.metrics.radius.control)
                    .cursor_pointer()
                    .hover(ui.subtle_hover())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.close_details(cx);
                    }))
                    .child(ui.icon(IconName::Close, Density::Compact, palette.text_primary)),
            )
    }

    // The actions are inert until classification and privacy rules can be edited.
    // Compact density; the row wraps instead of truncating labels when the
    // translated actions are wider than the panel (English needs two rows).
    fn details_actions(&self, ui: &Widgets<'_>) -> Div {
        let density = Density::Compact;
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(ui.metrics.space.s)
            .p(ui.metrics.space.l)
            .child(
                ui.button(
                    "action-classify",
                    density,
                    ButtonContent::Label(self.formatter.text(Msg::DetailsClassify)),
                )
                .flex_none(),
            )
            .child(
                ui.button(
                    "action-privacy",
                    density,
                    ButtonContent::IconLabel(
                        IconName::Shield,
                        self.formatter.text(Msg::DetailsPrivacy),
                    ),
                )
                .flex_none(),
            )
            .child(
                ui.button(
                    "action-delete",
                    density,
                    ButtonContent::Icon(IconName::Trash),
                )
                .flex_none()
                .text_color(ui.palette.status_critical),
            )
    }

    fn section(&self, ui: &Widgets<'_>, title: Msg, content: Div) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(ui.metrics.space.s)
            .child(
                div()
                    .ramp(ui, Ramp::BodyStrong)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(title)),
            )
            .child(content)
    }

    fn interval_card(&self, ui: &Widgets<'_>, interval: &IntervalCard) -> Div {
        let palette = ui.palette;
        let space = &ui.metrics.space;
        let rule = self
            .formatter
            .classification(&self.day, &interval.source, SourceText::Short);
        ui.card()
            .flex()
            .flex_col()
            .gap(space.s)
            .p(space.m)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_baseline()
                    .child(
                        div()
                            .ramp(ui, Ramp::BodyStrong)
                            .child(interval.span.to_string()),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.duration(interval.span.duration())),
                    ),
            )
            .child(div().ramp(ui, Ramp::Caption).child(interval.title.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.s)
                    .child(self.category_chip(ui, interval.category))
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(rule),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.xs)
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(ui.icon(
                        IconName::ChevronRight,
                        Density::Compact,
                        palette.text_secondary,
                    ))
                    .child(self.formatter.text_with(
                        Msg::DetailsMerged,
                        &[(Arg::Count, &interval.merged.to_string())],
                    )),
            )
    }

    fn week_chart(&self, ui: &Widgets<'_>, details: &Details, color: Hsla) -> Div {
        let palette = ui.palette;
        let longest = details
            .week
            .iter()
            .map(|bar| bar.total)
            .max()
            .unwrap_or_default();
        let bars = div()
            .flex()
            .flex_row()
            .items_end()
            .h(px(WEEK_CHART_HEIGHT))
            .children(details.week.iter().map(|day| {
                let height = Share::of(day.total, longest).fraction() * WEEK_CHART_HEIGHT;
                div()
                    .flex()
                    .flex_1()
                    .justify_center()
                    .items_end()
                    .h_full()
                    .child(
                        div()
                            .w(px(WEEK_BAR_WIDTH))
                            .h(px(height))
                            .rounded_t(ui.metrics.timeline.block_radius)
                            .bg(color),
                    )
            }));
        let labels =
            div()
                .flex()
                .flex_row()
                .pt(ui.metrics.space.xs)
                .children(details.week.iter().map(|day| {
                    let (name, text_color) = match day.day {
                        BarDay::Earlier(weekday) => {
                            (self.formatter.weekday(weekday), palette.text_secondary)
                        }
                        BarDay::Today => {
                            (self.formatter.text(Msg::DetailsToday), palette.text_primary)
                        }
                    };
                    div()
                        .flex_1()
                        .flex()
                        .justify_center()
                        .ramp(ui, Ramp::Caption)
                        .text_color(text_color)
                        .child(name)
                }));
        div()
            .flex()
            .flex_col()
            .child(ui.plot_area().p(ui.metrics.space.s).child(bars))
            .child(labels)
    }

    fn titles(&self, ui: &Widgets<'_>, details: &Details, color: Hsla) -> Div {
        let palette = ui.palette;
        let bar_radius = ui.metrics.timeline.block_radius;
        let longest = details
            .titles
            .iter()
            .map(|row| row.total)
            .max()
            .unwrap_or_default();
        div().flex().flex_col().gap(ui.metrics.space.xs).children(
            details.titles.iter().take(TITLE_ROWS).map(|row| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui.metrics.space.m)
                    .h(px(TITLE_ROW_HEIGHT))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .ramp(ui, Ramp::Caption)
                            .child(row.title.clone()),
                    )
                    .child(
                        div()
                            .w(px(TITLE_BAR_WIDTH))
                            .h(px(TITLE_BAR_HEIGHT))
                            .flex_shrink_0()
                            .rounded(bar_radius)
                            .bg(palette.fill_subtle_hover)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(Share::of(row.total, longest).fraction()))
                                    .rounded(bar_radius)
                                    .bg(color),
                            ),
                    )
                    .child(
                        div()
                            .w(px(TITLE_VALUE_WIDTH))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .ramp(ui, Ramp::Caption)
                            .text_color(palette.text_secondary)
                            .child(self.formatter.duration(row.total)),
                    )
            }),
        )
    }

    /// The explain tree: rule, rule set, the matched condition with its actual value.
    fn explanation(&self, ui: &Widgets<'_>, explanation: Option<&Explanation>) -> Div {
        let palette = ui.palette;
        let space = &ui.metrics.space;
        let Some(why) = explanation else {
            return div()
                .ramp(ui, Ramp::Caption)
                .text_color(palette.text_secondary)
                .child(self.formatter.text(Msg::DetailsNoRule));
        };
        // Lines under a check icon start where the text beside the icon starts.
        let text_indent = ui.metrics.density(Density::Compact).icon_size + space.s;
        let check = |color| ui.icon(IconName::Check, Density::Compact, color);
        div()
            .flex()
            .flex_col()
            .gap(space.xs)
            .ramp(ui, Ramp::Caption)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.s)
                    .child(check(palette.text_secondary))
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.formatter.text_with(
                                Msg::DetailsRulePriority,
                                &[
                                    (Arg::Rule, &why.rule.name),
                                    (Arg::Priority, &why.rule.priority.to_string()),
                                ],
                            )),
                    )
                    .child(
                        div()
                            .px(space.s)
                            .rounded(ui.metrics.radius.control)
                            .bg(palette.status_success_bg)
                            .text_color(palette.status_success)
                            .child(self.formatter.text(Msg::DetailsEffective)),
                    ),
            )
            .child(
                div()
                    .pl(text_indent)
                    .text_color(palette.text_secondary)
                    .child(
                        self.formatter
                            .text_with(Msg::DetailsRuleSet, &[(Arg::Name, &why.rule.rule_set)]),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space.xxs)
                    .ml(text_indent)
                    .pl(space.s)
                    .border_l_1()
                    .border_color(palette.stroke_divider)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(space.s)
                            .child(check(palette.status_success))
                            .child(
                                div()
                                    .font(ui.mono_font())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.formatter.rule_condition(&why.rule.condition)),
                            ),
                    )
                    .child(
                        div()
                            .pl(text_indent)
                            .text_color(palette.text_secondary)
                            .child(
                                self.formatter
                                    .text_with(Msg::DetailsActual, &[(Arg::Value, &why.actual)]),
                            ),
                    ),
            )
            .child(
                div()
                    .text_color(palette.text_secondary)
                    .child(self.formatter.text(Msg::DetailsNoShadowed)),
            )
    }
}
