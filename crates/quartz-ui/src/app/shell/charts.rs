//! Category share bar and hourly stacked bars (07 section 10).

use std::time::Duration;

use gpui_kit::{IntoElement, ParentElement as _, Styled as _, div, px, relative};

use super::AppShell;
use crate::app::{
    tokens::Palette,
    widgets::{Ramp, RampExt as _, space},
};

const SHARE_BAR_HEIGHT: f32 = 8.;
const CHART_HEIGHT: f32 = 120.;
const CHART_MINUTES: u64 = 60;
const CHART_MAX: Duration = Duration::from_secs(CHART_MINUTES * 60);
const BAR_WIDTH: f32 = 24.;
const SEGMENT_GAP: f32 = 1.;
const AXIS_LABEL_WIDTH: f32 = 36.;

impl AppShell {
    /// 100% stacked category bar with legend; sits on the status card.
    pub(super) fn render_share_bar(&self, p: &Palette) -> impl IntoElement {
        let totals = self.day.category_totals();
        let active = self.day.active_total().as_secs_f32().max(1.);
        let mut bar = div()
            .flex()
            .flex_row()
            .gap(px(SEGMENT_GAP))
            .h(px(SHARE_BAR_HEIGHT))
            .w_full()
            .rounded(px(space::XS))
            .overflow_hidden();
        let mut legend = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_x(px(space::L))
            .gap_y(px(space::XS));
        for entry in &totals {
            let category = self.day.category(entry.category);
            let color = p.category(category.color);
            bar = bar.child(
                div()
                    .h_full()
                    .w(relative(entry.total.as_secs_f32() / active))
                    .bg(color),
            );
            legend = legend.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::S))
                    .ramp(Ramp::Caption)
                    .text_color(p.text_secondary)
                    .child(div().size(px(8.)).rounded(px(2.)).bg(color))
                    .child(self.fmt.text_with(
                        "today.legend_item",
                        &[
                            ("name", &self.fmt.category_name(&category.name)),
                            ("duration", &self.fmt.duration(entry.total)),
                        ],
                    )),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(space::S))
            .child(bar)
            .child(legend)
    }

    pub(super) fn render_hourly(&self, p: &Palette) -> impl IntoElement {
        let bars = self.day.hourly();
        let mut columns = div().flex().flex_row().flex_1().h_full();
        let mut hours = div().flex().flex_row().flex_1();
        for bar in &bars {
            let mut stack = div()
                .flex()
                .flex_col_reverse()
                .gap(px(SEGMENT_GAP))
                .w(px(BAR_WIDTH))
                .rounded_t(px(2.))
                .overflow_hidden();
            for (category, total) in &bar.parts {
                let color = p.category(self.day.category(*category).color);
                let height = total.as_secs_f32() / CHART_MAX.as_secs_f32() * CHART_HEIGHT;
                stack = stack.child(div().w_full().h(px(height)).flex_shrink_0().bg(color));
            }
            columns = columns.child(
                div()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .items_center()
                    .flex_1()
                    .h_full()
                    .child(stack),
            );
            hours = hours.child(
                div()
                    .flex_1()
                    .flex()
                    .justify_center()
                    .ramp(Ramp::Caption)
                    .text_color(p.text_tertiary)
                    .child(bar.hour.to_string()),
            );
        }
        let axis_label = |minutes: u64| {
            div()
                .absolute()
                .left(px(0.))
                .w(px(AXIS_LABEL_WIDTH))
                .ramp(Ramp::Caption)
                .text_color(p.text_tertiary)
                .child(
                    self.fmt
                        .text_with("duration.m", &[("minutes", &minutes.to_string())]),
                )
        };
        let grid = |fraction: f32| {
            div()
                .absolute()
                .left(px(AXIS_LABEL_WIDTH))
                .right(px(0.))
                .top(px(fraction * CHART_HEIGHT))
                .h(px(1.))
                .bg(p.stroke_divider)
        };
        let plot = (p)
            .plot_area()
            .flex()
            .flex_col()
            .p(px(space::S))
            .child(
                div()
                    .relative()
                    .h(px(CHART_HEIGHT))
                    .child(grid(0.))
                    .child(grid(0.5))
                    .child(grid(1.))
                    .child(axis_label(CHART_MINUTES).top(px(-8.)))
                    .child(axis_label(CHART_MINUTES / 2).top(px(CHART_HEIGHT / 2. - 8.)))
                    .child(axis_label(0).top(px(CHART_HEIGHT - 8.)))
                    .child(
                        div()
                            .absolute()
                            .left(px(AXIS_LABEL_WIDTH))
                            .right(px(0.))
                            .top(px(0.))
                            .h_full()
                            .flex()
                            .child(columns),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .pl(px(AXIS_LABEL_WIDTH))
                    .pt(px(space::XS))
                    .child(hours),
            );
        p.card()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .gap(px(space::S))
            .p(px(space::L))
            .child(
                div()
                    .ramp(Ramp::Subtitle)
                    .child(self.fmt.text("today.hourly.title")),
            )
            .child(plot)
    }
}
