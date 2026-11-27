//! Category share bar and hourly stacked bars (07 section 10).

use std::time::Duration;

use gpui_kit::{
    AnyElement, Context, Div, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Role, Stateful, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _, px, relative,
};

use super::{AppShell, Frame, LayerCx};
use crate::{
    app::{
        metrics::Ramp,
        widgets::{RampExt as _, Widgets},
    },
    format::{Arg, Msg},
    layout::{ChartArrangement, ColumnRange},
    model::{CategoryColor, HourlyBar, SECONDS_PER_MINUTE, Share},
};

/// Height of the category share bar on the status card.
const SHARE_BAR_HEIGHT: f32 = 8.;
/// Height of the hourly chart plot.
const CHART_HEIGHT: f32 = 120.;
/// The time a full-height hourly bar stands for.
const CHART_SCALE: Duration = Duration::from_hours(1);
/// Width of the value axis label column: as wide as its widest label (11 section 5.6), but at
/// least this much so the plot does not jump between languages.
pub(super) const AXIS_COLUMN_RANGE: ColumnRange = ColumnRange::new(24., 72.);
/// Space between the hour labels of the bar axis.
const HOUR_LABEL_GAP: f32 = 4.;
/// Hours per label the axis tries, finest first; each divides 24 so labels stay evenly spread.
const HOUR_STRIDES: [u8; 7] = [1, 2, 3, 4, 6, 8, 12];
/// The stride used when even the coarsest tried one does not fit: one label at noon-sized spacing.
const COARSEST_HOUR_STRIDE: u8 = 12;

/// Which hours of the bar axis carry a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HourStride(u8);

impl HourStride {
    /// The finest stride whose labels `label_width` wide fit in bar slots `slot_width` wide.
    fn fit(slot_width: f32, label_width: f32) -> Self {
        let needed = label_width + HOUR_LABEL_GAP;
        let stride = HOUR_STRIDES
            .into_iter()
            .find(|stride| slot_width * f32::from(*stride) >= needed)
            .unwrap_or(COARSEST_HOUR_STRIDE);
        Self(stride)
    }

    /// Whether the bar at `index` is labeled, including repeated hours at a clock change.
    fn labels(self, index: usize) -> bool {
        index.is_multiple_of(usize::from(self.0))
    }
}

/// A tick's nonoverlapping text region, clipped to the plot at both ends.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TickLabelBounds {
    left: f32,
    width: f32,
}

impl TickLabelBounds {
    fn centered(center: f32, spacing: f32, plot_width: f32) -> Self {
        let left = (center - spacing / 2.).max(0.);
        let right = (center + spacing / 2.).min(plot_width);
        let extent = (right - left).max(0.);
        let padding = (HOUR_LABEL_GAP / 2.).min(extent / 2.);
        Self {
            left: left + padding,
            width: extent - padding * 2.,
        }
    }
}

/// One labeled horizontal gridline of a chart plot, as a fraction of the plot height from the top.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ChartGridline {
    pub(super) from_top: f32,
    pub(super) label: String,
}

/// One stacked category segment in a vertical column chart, sized as a fraction of plot height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StackedSegment {
    pub(super) color: CategoryColor,
    pub(super) fraction: f32,
}

/// Renders one horizontal gridline and its left-aligned y-axis tick label.
pub(super) fn render_gridline(
    ui: &Widgets<'_>,
    line: &ChartGridline,
    chart_height: f32,
    axis_width: f32,
) -> [Div; 2] {
    let top = px(line.from_top * chart_height);
    let label_height = ui.line_height(Ramp::Caption);
    [
        div()
            .absolute()
            .left(px(axis_width))
            .right(px(0.))
            .top(top)
            .h(ui.metrics.layout.divider)
            .bg(ui.palette.stroke_divider),
        div()
            .absolute()
            .left(px(0.))
            .w(px(axis_width))
            .ramp(ui, Ramp::Caption)
            .tabular_figures()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_color(ui.palette.text_tertiary)
            .top(top - label_height / 2.)
            .child(line.label.clone()),
    ]
}

/// Renders one stacked category column (bottom to top) inside a chart plot.
pub(super) fn render_stacked_column(
    ui: &Widgets<'_>,
    id: impl Into<ElementId>,
    accessible_label: Option<String>,
    segments: impl IntoIterator<Item = StackedSegment>,
    chart_height: f32,
    padding_x: Pixels,
) -> Stateful<Div> {
    let stack = div()
        .flex()
        .flex_col_reverse()
        .gap(ui.metrics.timeline.block_gap)
        .w_full()
        .max_h(px(chart_height))
        .rounded_t(ui.metrics.timeline.block_radius)
        .overflow_hidden()
        .children(segments.into_iter().map(|segment| {
            ui.category_mark(
                div()
                    .w_full()
                    .h(px(segment.fraction.clamp(0., 1.) * chart_height))
                    .min_h(px(0.))
                    .flex_shrink_1(),
                segment.color,
            )
        }));
    div()
        .id(id)
        .when_some(accessible_label, |column, label| {
            column.role(Role::Meter).aria_label(label)
        })
        .flex()
        .flex_col()
        .justify_end()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .px(padding_x)
        .child(stack)
}

/// Renders one category legend item with its theme-aware swatch and text.
pub(super) fn render_legend_item(ui: &Widgets<'_>, color: CategoryColor, text: String) -> Div {
    let palette = ui.palette;
    let swatch_color = palette.category(color);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(ui.metrics.space.s)
        .ramp(ui, Ramp::Caption)
        .text_color(palette.text_secondary)
        .child(ui.category_mark(ui.swatch(swatch_color), color))
        .child(text)
}

/// One labeled gridline of the hourly chart, as a fraction of the plot height from the top.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Gridline {
    value: Duration,
    from_top: f32,
}

/// Whole minutes in `value`.
fn minutes_of(value: Duration) -> u64 {
    value.as_secs() / u64::from(SECONDS_PER_MINUTE)
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
            bar = bar.child(
                ui.category_mark(
                    div()
                        .h_full()
                        .w(relative(Share::of(entry.total, active).fraction())),
                    category.color,
                ),
            );
            legend = legend.child(render_legend_item(
                ui,
                category.color,
                self.formatter.text_with(
                    Msg::LegendItem,
                    &[
                        Arg::Name.with(&self.formatter.category_name(&category.name)),
                        Arg::Duration.with(&self.formatter.duration(entry.total)),
                    ],
                ),
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .child(bar)
            .child(legend)
    }

    pub(super) fn render_hourly(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bars = self.day.hourly();
        let lines = gridlines();
        let chart_lines: Vec<ChartGridline> = lines
            .into_iter()
            .map(|line| ChartGridline {
                from_top: line.from_top,
                label: self.formatter.whole_minutes(minutes_of(line.value)),
            })
            .collect();
        let axis_width = AXIS_COLUMN_RANGE.select(frame.measure.widths(
            chart_lines.iter().map(|line| line.label.as_str()),
            Ramp::Caption,
        ));
        let plot = ui
            .plot_area()
            .flex()
            .flex_col()
            .p(ui.metrics.space.s)
            .child(
                div()
                    .relative()
                    .h(px(CHART_HEIGHT))
                    .children(
                        chart_lines
                            .iter()
                            .flat_map(|line| render_gridline(ui, line, CHART_HEIGHT, axis_width)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(axis_width))
                            .right(px(0.))
                            .top(px(0.))
                            .h_full()
                            .flex()
                            .child(
                                div().flex().flex_row().flex_1().h_full().children(
                                    bars.iter()
                                        .enumerate()
                                        .map(|(index, bar)| self.hourly_column(ui, index, bar)),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .relative()
                    .ml(px(axis_width))
                    .mt(ui.metrics.space.xs)
                    .h(ui.line_height(Ramp::Caption))
                    .child(self.bounds_layer(cx, |layer| layer.shell.hour_labels(layer))),
            );
        ui.card()
            .flex()
            .flex_col()
            .map(|card| match frame.layout.class.chart_arrangement() {
                ChartArrangement::SideBySide => card.flex_1().min_w(px(0.)),
                ChartArrangement::Stacked => card.w_full(),
            })
            .gap(ui.metrics.space.s)
            .p(ui.metrics.space.l)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .child(self.formatter.text(Msg::HourlyTitle)),
            )
            .child(plot)
    }

    /// The hour labels under the bars, thinned to the stride that fits the width the layout gave
    /// the row in this frame.
    fn hour_labels(&self, layer: &LayerCx<'_>) -> AnyElement {
        layer.with_ui(|ui, measure, _| {
            let bars = self.day.hourly();
            if bars.is_empty() {
                return div().size_full().into_any_element();
            }
            let slot = f32::from(layer.bounds.size.width)
                / f32::from(
                    u8::try_from(bars.len()).expect("a logical day has at most 25 hourly bars"),
                );
            let labels: Vec<String> = bars
                .iter()
                .map(|bar| self.formatter.hour_tick(bar.hour))
                .collect();
            let widest = measure
                .widths(labels.iter().map(String::as_str), Ramp::Caption)
                .fold(0., f32::max);
            let stride = HourStride::fit(slot, widest);
            let plot_width = f32::from(layer.bounds.size.width);
            div()
                .relative()
                .size_full()
                .overflow_hidden()
                .children(labels.into_iter().enumerate().filter_map(|(index, label)| {
                    if !stride.labels(index) {
                        return None;
                    }
                    let index = f32::from(
                        u8::try_from(index).expect("a logical day has at most 25 hourly bars"),
                    );
                    let bounds = TickLabelBounds::centered(
                        (index + 0.5) * slot,
                        f32::from(stride.0) * slot,
                        plot_width,
                    );
                    Some(
                        div()
                            .absolute()
                            .left(px(bounds.left))
                            .w(px(bounds.width))
                            .overflow_hidden()
                            .child(ui.axis_label(label, ui.palette.text_tertiary)),
                    )
                }))
                .into_any_element()
        })
    }

    /// One hour's stacked bar, categories bottom to top in category order.
    fn hourly_column(&self, ui: &Widgets<'_>, index: usize, bar: &HourlyBar) -> impl IntoElement {
        let label = self.formatter.hourly_bar(bar, &self.day);
        let scale = bar.chart_scale(CHART_SCALE);
        let segments = bar.visible_parts().map(|part| StackedSegment {
            color: self.day.category(part.category).color,
            fraction: part.chart_share(scale).fraction(),
        });
        render_stacked_column(
            ui,
            format!("hourly-{index}"),
            label,
            segments,
            CHART_HEIGHT,
            ui.metrics.space.xxs,
        )
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

    #[test]
    fn hour_labels_thin_out_as_the_bars_get_narrower() {
        let label = 14.;
        assert_eq!(HourStride::fit(30., label), HourStride(1));
        assert_eq!(
            HourStride::fit(13., label),
            HourStride(2),
            "13 px slots cannot hold 18 px labels"
        );
        assert_eq!(HourStride::fit(7., label), HourStride(3));
        assert_eq!(HourStride::fit(1., label), HourStride(COARSEST_HOUR_STRIDE));
        let every_third = HourStride(3);
        let labeled: Vec<usize> = (0..24).filter(|hour| every_third.labels(*hour)).collect();
        assert_eq!(labeled, [0, 3, 6, 9, 12, 15, 18, 21]);
    }

    #[test]
    fn hourly_label_regions_stay_inside_the_plot_and_never_overlap() {
        for bar_count in [23_u8, 24, 25] {
            for plot_width in [0., 1., 12., 120., 640., 2560.] {
                for label_width in [14., 32., 80.] {
                    let slot = plot_width / f32::from(bar_count);
                    let stride = HourStride::fit(slot, label_width);
                    let mut previous_right = 0.;
                    for index in 0..bar_count {
                        if !stride.labels(usize::from(index)) {
                            continue;
                        }
                        let bounds = TickLabelBounds::centered(
                            (f32::from(index) + 0.5) * slot,
                            f32::from(stride.0) * slot,
                            plot_width,
                        );
                        assert!(
                            bounds.left >= previous_right,
                            "{bounds:?} after {previous_right}"
                        );
                        assert!(bounds.width >= 0.);
                        assert!(bounds.left + bounds.width <= plot_width);
                        previous_right = bounds.left + bounds.width;
                    }
                }
            }
        }
    }
}
