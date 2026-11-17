//! Details sidebar (07 section 8): subject header, selected interval, 7-day bars, common titles,
//! explanation tree and actions.

use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div, px, relative,
};

use super::AppShell;
use crate::app::{
    model::{ClassificationSource, Details, IntervalCard},
    tokens::{Palette, metrics::PANEL_WIDTH},
    widgets::{Density, RADIUS_CONTROL, Ramp, RampExt as _, icon, space},
};

const WEEK_CHART_HEIGHT: f32 = 88.;
const WEEK_BAR_WIDTH: f32 = 36.;
const MONO_FONT: &str = "Cascadia Mono";

impl AppShell {
    pub(super) fn render_details(
        &self,
        p: &Palette,
        details: &Details,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let color = p.category(self.day.category(details.category).color);
        let mut body = div()
            .id("details-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(px(space::L))
            .px(px(space::L))
            .pb(px(space::L));
        if let Some(interval) = &details.interval {
            body = body.child(self.interval_card(p, interval));
        }
        body = body
            .child(self.section(p, "details.week", self.week_chart(p, details, color)))
            .child(self.section(p, "details.titles", self.titles(p, details, color)))
            .child(self.section(p, "details.why", self.explanation(p, details)));

        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(PANEL_WIDTH))
            .h_full()
            .bg(p.fill_layer)
            .border_l_1()
            .border_color(p.stroke_divider)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(space::S))
                    .p(px(space::L))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(px(space::S))
                            .child(div().pt(px(space::XXS)).child(icon(
                                details.app.glyph.icon_name(),
                                16.,
                                p.text_primary,
                            )))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .ramp(Ramp::BodyStrong)
                                            .child(details.app.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .ramp(Ramp::Caption)
                                            .text_color(p.text_secondary)
                                            .child(details.app.exe.clone()),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("details-close")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .rounded(px(RADIUS_CONTROL))
                            .cursor_pointer()
                            .hover({
                                let hover = p.fill_subtle_hover;
                                move |style| style.bg(hover)
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.close_details(cx);
                            }))
                            .child(icon(IconName::Close, 14., p.text_primary)),
                    ),
            )
            .child(body)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(space::S))
                    .p(px(space::L))
                    .child(p.button(
                        "action-classify",
                        Density::Standard,
                        Some(self.fmt.text("details.action.classify")),
                        None,
                    ))
                    .child(p.button(
                        "action-privacy",
                        Density::Standard,
                        Some(self.fmt.text("details.action.privacy")),
                        Some(IconName::Shield),
                    ))
                    .child(
                        p.button(
                            "action-delete",
                            Density::Standard,
                            None,
                            Some(IconName::Trash),
                        )
                        .text_color(p.status_critical),
                    ),
            )
    }

    fn section(&self, p: &Palette, key: &str, content: Div) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(space::S))
            .child(
                div()
                    .ramp(Ramp::BodyStrong)
                    .text_color(p.text_primary)
                    .child(self.fmt.text(key)),
            )
            .child(content)
    }

    fn interval_card(&self, p: &Palette, interval: &IntervalCard) -> Div {
        let rule = match &interval.source {
            ClassificationSource::Rule { condition } => self
                .fmt
                .text_with("details.rule_line", &[("condition", condition)]),
            ClassificationSource::Manual => self.fmt.text("timeline.tooltip.manual"),
            ClassificationSource::Unmatched => self.fmt.text("timeline.tooltip.unmatched"),
        };
        p.card()
            .flex()
            .flex_col()
            .gap(px(space::S))
            .p(px(space::M))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_baseline()
                    .child(
                        div()
                            .ramp(Ramp::BodyStrong)
                            .child(interval.span.to_string()),
                    )
                    .child(
                        div()
                            .ramp(Ramp::Caption)
                            .text_color(p.text_secondary)
                            .child(self.fmt.duration(interval.span.duration())),
                    ),
            )
            .child(div().ramp(Ramp::Caption).child(interval.title.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::S))
                    .child(self.category_chip(p, interval.category))
                    .child(
                        div()
                            .ramp(Ramp::Caption)
                            .text_color(p.text_secondary)
                            .child(rule),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::XS))
                    .ramp(Ramp::Caption)
                    .text_color(p.text_secondary)
                    .child(icon(IconName::ChevronRight, 12., p.text_secondary))
                    .child(self.fmt.text_with(
                        "details.merged",
                        &[("count", &interval.merged_count.to_string())],
                    )),
            )
    }

    fn week_chart(&self, p: &Palette, details: &Details, color: gpui_kit::Hsla) -> Div {
        let max = details
            .week
            .iter()
            .map(|bar| bar.total.as_secs_f32())
            .fold(1., f32::max);
        let mut bars = div().flex().flex_row().items_end().h(px(WEEK_CHART_HEIGHT));
        let mut labels = div().flex().flex_row().pt(px(space::XS));
        for day in &details.week {
            let height = (day.total.as_secs_f32() / max * WEEK_CHART_HEIGHT).max(0.);
            bars = bars.child(
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
                            .rounded_t(px(2.))
                            .bg(color),
                    ),
            );
            let name = if day.is_today {
                self.fmt.text("details.today")
            } else {
                self.fmt.weekday(day.weekday)
            };
            labels = labels.child(
                div()
                    .flex_1()
                    .flex()
                    .justify_center()
                    .ramp(Ramp::Caption)
                    .text_color(if day.is_today {
                        p.text_primary
                    } else {
                        p.text_secondary
                    })
                    .child(name),
            );
        }
        div()
            .flex()
            .flex_col()
            .child(p.plot_area().p(px(space::S)).child(bars))
            .child(labels)
    }

    fn titles(&self, p: &Palette, details: &Details, color: gpui_kit::Hsla) -> Div {
        let max = details
            .titles
            .first()
            .map_or(1., |row| row.total.as_secs_f32().max(1.));
        let mut list = div().flex().flex_col().gap(px(space::XS));
        for row in &details.titles {
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::M))
                    .h(px(28.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .ramp(Ramp::Caption)
                            .child(row.title.clone()),
                    )
                    .child(
                        div()
                            .w(px(56.))
                            .h(px(4.))
                            .flex_shrink_0()
                            .rounded(px(2.))
                            .bg(p.fill_subtle_hover)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(row.total.as_secs_f32() / max))
                                    .rounded(px(2.))
                                    .bg(color),
                            ),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .ramp(Ramp::Caption)
                            .text_color(p.text_secondary)
                            .child(self.fmt.duration(row.total)),
                    ),
            );
        }
        list
    }

    fn explanation(&self, p: &Palette, details: &Details) -> Div {
        let Some(why) = &details.explanation else {
            return div()
                .ramp(Ramp::Caption)
                .text_color(p.text_secondary)
                .child(self.fmt.text("details.no_rule"));
        };
        let condition = format!("{} {} \"{}\"", why.field, why.operator, why.expected);
        div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .ramp(Ramp::Caption)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::S))
                    .child(icon(IconName::Check, 14., p.text_secondary))
                    .child(
                        div()
                            .ramp(Ramp::Caption)
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(self.fmt.text_with(
                                "details.rule_priority",
                                &[
                                    ("rule", &why.rule_name),
                                    ("priority", &why.priority.to_string()),
                                ],
                            )),
                    )
                    .child(
                        div()
                            .px(px(space::S))
                            .rounded(px(RADIUS_CONTROL))
                            .bg(p.status_success_bg)
                            .text_color(p.status_success)
                            .child(self.fmt.text("details.effective")),
                    ),
            )
            .child(
                div()
                    .pl(px(space::XXL - space::XXS))
                    .text_color(p.text_secondary)
                    .child(
                        self.fmt
                            .text_with("details.rule_set", &[("name", &why.rule_set)]),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::XXS))
                    .ml(px(space::XXL))
                    .pl(px(space::S))
                    .border_l_1()
                    .border_color(p.stroke_divider)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(space::S))
                            .child(icon(IconName::Check, 14., p.status_success))
                            .child(
                                div()
                                    .font_family(MONO_FONT)
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(condition),
                            ),
                    )
                    .child(
                        div()
                            .pl(px(space::XXL - space::XS))
                            .text_color(p.text_secondary)
                            .child(
                                self.fmt
                                    .text_with("details.actual", &[("value", &why.actual)]),
                            ),
                    ),
            )
            .child(
                div()
                    .text_color(p.text_secondary)
                    .child(self.fmt.text("details.no_shadowed")),
            )
    }
}
