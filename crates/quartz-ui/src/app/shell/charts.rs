//! Category share bar and hourly stacked bars (07 section 10).

use std::time::Duration;

use gpui_kit::{Div, IntoElement, ParentElement as _, Styled as _, div, px, relative};

use super::AppShell;
use crate::{
    app::{
        metrics::Ramp,
        widgets::{RampExt as _, Widgets},
    },
    format::{Arg, Msg},
    model::{HourlyBar, SECONDS_PER_MINUTE, Share},
};

/// Height of the category share bar on the status card.
const SHARE_BAR_HEIGHT: f32 = 8.;
/// Height of the hourly chart plot.
const CHART_HEIGHT: f32 = 120.;
/// The time a full-height hourly bar stands for.
const CHART_SCALE: Duration = Duration::from_hours(1);
/// Width of one hourly bar.
const BAR_WIDTH: f32 = 24.;
/// Width of the value axis labels left of the plot.
const AXIS_LABEL_WIDTH: f32 = 36.;

/// One labeled gridline of the hourly chart, as a fraction of the plot height from the top.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Gridline {
    value: Duration,
    from_top: f32,
}

/// Gridlines at the full scale, half of it and zero.
fn gridlines() -> [Gridline; 3] {
    [CHART_SCALE, CHART_SCALE / 2, Duration::ZERO].map(|value| Gridline {
        value,
        from_top: 1. - Share::of(value, CHART_SCALE).fraction(),
    })
}

impl AppShell {
    /// 100% stacked category bar with legend; sits on the status card.
    pub(super) fn render_share_bar(&self, ui: &Widgets<'_>) -> impl IntoElement {
        let palette = ui.palette;
        let space = &ui.metrics.space;
        let active = self.day.summary().active;
        let mut bar = div()
            .flex()
            .flex_row()
            .gap(ui.metrics.timeline.block_gap)
            .h(px(SHARE_BAR_HEIGHT))
            .w_full()
            .rounded(ui.metrics.radius.control)
            .overflow_hidden();
        let mut legend = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_x(space.l)
            .gap_y(space.xs);
        for entry in self.day.category_totals() {
            let category = self.day.category(entry.category);
            let color = palette.category(category.color);
            bar = bar.child(
                div()
                    .h_full()
                    .w(relative(Share::of(entry.total, active).fraction()))
                    .bg(color),
            );
            legend = legend.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.s)
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(ui.swatch(color))
                    .child(self.formatter.text_with(
                        Msg::LegendItem,
                        &[
                            (Arg::Name, &self.formatter.category_name(&category.name)),
                            (Arg::Duration, &self.formatter.duration(entry.total)),
                        ],
                    )),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .child(bar)
            .child(legend)
    }

    pub(super) fn render_hourly(&self, ui: &Widgets<'_>) -> impl IntoElement {
        let bars = self.day.hourly();
        let plot =
            ui.plot_area()
                .flex()
                .flex_col()
                .p(ui.metrics.space.s)
                .child(
                    div()
                        .relative()
                        .h(px(CHART_HEIGHT))
                        .children(
                            gridlines()
                                .into_iter()
                                .flat_map(|line| self.gridline(ui, line)),
                        )
                        .child(
                            div()
                                .absolute()
                                .left(px(AXIS_LABEL_WIDTH))
                                .right(px(0.))
                                .top(px(0.))
                                .h_full()
                                .flex()
                                .child(
                                    div().flex().flex_row().flex_1().h_full().children(
                                        bars.iter().map(|bar| self.hourly_column(ui, bar)),
                                    ),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .pl(px(AXIS_LABEL_WIDTH))
                        .pt(ui.metrics.space.xs)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_1()
                                .children(bars.iter().map(|bar| {
                                    div()
                                        .flex_1()
                                        .flex()
                                        .justify_center()
                                        .ramp(ui, Ramp::Caption)
                                        .text_color(ui.palette.text_tertiary)
                                        .child(bar.hour.to_string())
                                })),
                        ),
                );
        ui.card()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .gap(ui.metrics.space.s)
            .p(ui.metrics.space.l)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .child(self.formatter.text(Msg::HourlyTitle)),
            )
            .child(plot)
    }

    /// A horizontal gridline with its value label centered on it.
    fn gridline(&self, ui: &Widgets<'_>, line: Gridline) -> [Div; 2] {
        let top = px(line.from_top * CHART_HEIGHT);
        let label_height = ui.metrics.ramp(Ramp::Caption).line_height;
        let minutes = line.value.as_secs() / u64::from(SECONDS_PER_MINUTE);
        [
            div()
                .absolute()
                .left(px(AXIS_LABEL_WIDTH))
                .right(px(0.))
                .top(top)
                .h(ui.metrics.layout.divider)
                .bg(ui.palette.stroke_divider),
            div()
                .absolute()
                .left(px(0.))
                .w(px(AXIS_LABEL_WIDTH))
                .ramp(ui, Ramp::Caption)
                .text_color(ui.palette.text_tertiary)
                .top(top - label_height / 2.)
                .child(self.formatter.whole_minutes(minutes)),
        ]
    }

    /// One hour's stacked bar, categories bottom to top in category order.
    fn hourly_column(&self, ui: &Widgets<'_>, bar: &HourlyBar) -> Div {
        let stack = div()
            .flex()
            .flex_col_reverse()
            .gap(ui.metrics.timeline.block_gap)
            .w(px(BAR_WIDTH))
            .rounded_t(ui.metrics.timeline.block_radius)
            .overflow_hidden()
            .children(bar.parts.iter().map(|part| {
                let color = ui.palette.category(self.day.category(part.category).color);
                div()
                    .w_full()
                    .h(px(
                        Share::of(part.total, CHART_SCALE).fraction() * CHART_HEIGHT
                    ))
                    .flex_shrink_0()
                    .bg(color)
            }));
        div()
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .flex_1()
            .h_full()
            .child(stack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gridlines_run_from_the_full_scale_at_the_top_to_zero_at_the_bottom() {
        let lines = gridlines();
        let tops: Vec<f32> = lines.iter().map(|line| line.from_top).collect();
        assert_eq!(tops, [0., 0.5, 1.]);
        assert_eq!(lines.first().map(|line| line.value), Some(CHART_SCALE));
    }
}
