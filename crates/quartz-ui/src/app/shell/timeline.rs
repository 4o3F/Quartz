//! The timeline: category main track, per-application swimlanes, axis, hover and selection
//! (07 section 4, 11 sections 3.4 and 5.3). Blocks are positioned by fractions of the plot width
//! (see [`crate::timeline_scale`]), so the layout never needs the plot's pixel size and nothing
//! redraws while idle.

use gpui_kit::{
    ClickEvent, Context, Div, ElementId, Hsla, InteractiveElement as _, IntoElement,
    MouseMoveEvent, ParentElement as _, Pixels, Stateful, StatefulInteractiveElement as _,
    Styled as _, anchored, assets::IconName, div, pattern_slash, point,
    prelude::FluentBuilder as _, px, relative,
};

use super::{AppShell, HoverTarget, LaneMode, RangeChoice};
use crate::{
    app::{
        metrics::{Density, Metrics, Ramp},
        widgets::{ButtonContent, RampExt as _, Segment as SegmentedItem, Widgets},
    },
    format::{Arg, Msg, SourceText},
    model::{
        CategoryId, ClockAnomaly, DetailsSubject, Lane, MainBlock, Segment, StateBlock, StateKind,
        TimeSpan,
    },
    timeline_scale::{View, ZoomStep, fitting_label},
};

/// Width of the label column left of the plot.
const LABEL_WIDTH: f32 = 140.;
/// Narrowest plot width assumed when choosing block labels.
const MIN_PLOT_WIDTH: f32 = 200.;
/// Width of a tick label, centered on its tick.
const AXIS_LABEL_WIDTH: f32 = 40.;
/// Narrowest painted block, so one-pixel blocks stay visible.
const MIN_BLOCK_WIDTH: f32 = 2.;
/// Horizontal padding of block text; `timeline_scale` reserves twice this when fitting labels.
const BLOCK_TEXT_PADDING: f32 = 6.;
/// Width of the hover tooltip.
const TOOLTIP_WIDTH: f32 = 260.;
/// Offset of the tooltip from the pointer.
const TOOLTIP_OFFSET: (f32, f32) = (12., 16.);
/// Height of a category chip.
const CHIP_HEIGHT: f32 = 20.;

/// Lane-mode choices in display order with their element ids and labels.
const LANE_MODES: [(LaneMode, &str, Msg); 2] = [
    (LaneMode::Swimlanes, "lanes-apps", Msg::TimelineLanesApps),
    (
        LaneMode::CategoryOnly,
        "lanes-category",
        Msg::TimelineLanesCategory,
    ),
];

/// Whether the pointer is over a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BlockHover {
    Idle,
    Hovered,
}

/// Estimated plot width, used only to decide which labels fit inside blocks: the page width
/// without the page margins, the card padding, the label column and the plot padding.
fn estimated_plot_width(main_width: Pixels, metrics: &Metrics) -> f32 {
    let space = &metrics.space;
    let page = f32::from(main_width).min(f32::from(metrics.layout.max_content_width));
    let chrome = 2. * f32::from(space.xxl) + 2. * f32::from(space.l) + f32::from(space.m);
    (page - chrome - LABEL_WIDTH).max(MIN_PLOT_WIDTH)
}

impl AppShell {
    fn view(&self) -> View {
        match self.range {
            RangeChoice::Activity => View::activity(self.day.activity_range()),
            RangeChoice::FullDay => View::full_day(),
            RangeChoice::Zoomed(span) => {
                View::new(span).expect("zooming never yields an empty span")
            }
        }
    }

    fn zoom(&mut self, step: ZoomStep, cx: &mut Context<Self>) {
        self.range = RangeChoice::Zoomed(self.view().zoomed(step));
        self.hover = None;
        cx.notify();
    }

    fn toggle_full_day(&mut self, cx: &mut Context<Self>) {
        self.range = match self.range {
            RangeChoice::FullDay => RangeChoice::Activity,
            RangeChoice::Activity | RangeChoice::Zoomed(_) => RangeChoice::FullDay,
        };
        self.hover = None;
        cx.notify();
    }

    fn set_lanes(&mut self, lanes: LaneMode, cx: &mut Context<Self>) {
        self.lanes = lanes;
        self.hover = None;
        cx.notify();
    }

    fn visible_lanes(&self) -> &[Lane] {
        match self.lanes {
            LaneMode::Swimlanes => self.day.lanes(),
            LaneMode::CategoryOnly => &[],
        }
    }

    pub(super) fn render_timeline(
        &self,
        ui: &Widgets<'_>,
        main_width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = self.view();
        let plot_width = estimated_plot_width(main_width, ui.metrics);
        ui.card()
            .flex()
            .flex_col()
            .gap(ui.metrics.space.s)
            .p(ui.metrics.space.l)
            .child(self.timeline_header(ui, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .child(self.label_column(ui))
                    .child(self.plot(ui, view, plot_width, cx)),
            )
    }

    fn timeline_header(&self, ui: &Widgets<'_>, cx: &mut Context<Self>) -> Div {
        let full_day = self.range == RangeChoice::FullDay;
        let density = Density::Compact;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui.metrics.space.s)
            .child(
                div()
                    .flex_1()
                    .ramp(ui, Ramp::Subtitle)
                    .child(self.formatter.text(Msg::TimelineTitle)),
            )
            .children(self.day.activity_range().map(|range| {
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(ui.palette.text_secondary)
                    .mr(ui.metrics.space.s)
                    .child(range.to_string())
            }))
            .child(ui.segmented(
                "timeline-lanes",
                LANE_MODES.map(|(mode, id, label)| SegmentedItem {
                    id,
                    label: self.formatter.text(label),
                    value: mode,
                }),
                self.lanes,
                |mode| cx.listener(move |this, _: &ClickEvent, _, cx| this.set_lanes(mode, cx)),
            ))
            .child(
                ui.button("zoom-out", density, ButtonContent::Icon(IconName::Minus))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom(ZoomStep::Out, cx))),
            )
            .child(
                ui.button("zoom-in", density, ButtonContent::Icon(IconName::Plus))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom(ZoomStep::In, cx))),
            )
            .child(
                ui.button(
                    "zoom-full",
                    density,
                    ButtonContent::Label(self.formatter.text(if full_day {
                        Msg::TimelineFitActivity
                    } else {
                        Msg::TimelineFullDay
                    })),
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_full_day(cx))),
            )
    }

    /// The label column; its rows mirror the plot rows exactly.
    fn label_column(&self, ui: &Widgets<'_>) -> Div {
        let palette = ui.palette;
        let sizes = &ui.metrics.timeline;
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(LABEL_WIDTH))
            .py(ui.metrics.space.xs)
            .child(
                div()
                    .h(sizes.main_track_height)
                    .flex()
                    .items_center()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(self.formatter.text(Msg::TimelineMainTrack)),
            )
            .children(self.visible_lanes().iter().map(|lane| {
                let app = self.day.app(lane.app);
                div()
                    .h(sizes.lane_height)
                    .mt(sizes.lane_gap)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui.metrics.space.s)
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(ui.icon(app.glyph.into(), Density::Compact, palette.text_secondary))
                    .child(app.short_name.clone())
            }))
            .child(div().h(sizes.axis_height))
    }

    fn plot(&self, ui: &Widgets<'_>, view: View, plot_width: f32, cx: &mut Context<Self>) -> Div {
        let rows = div()
            .flex()
            .flex_col()
            .py(ui.metrics.space.xs)
            .child(self.main_track(ui, view, plot_width, cx))
            .children(
                self.visible_lanes()
                    .iter()
                    .map(|lane| self.lane_row(ui, view, lane, cx)),
            )
            .child(self.axis(ui, view));
        ui.plot_area()
            .relative()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .children(self.gridlines(ui, view))
            .child(rows)
            .children(self.selection_overlay(ui, view))
    }

    fn gridlines(&self, ui: &Widgets<'_>, view: View) -> Vec<Div> {
        view.ticks()
            .into_iter()
            .map(|tick| {
                div()
                    .absolute()
                    .top(px(0.))
                    .bottom(px(0.))
                    .left(relative(tick.fraction))
                    .w(ui.metrics.layout.divider)
                    .bg(ui.palette.stroke_divider)
            })
            .collect()
    }

    fn axis(&self, ui: &Widgets<'_>, view: View) -> Div {
        let labels = view
            .ticks()
            .into_iter()
            .filter(|tick| tick.has_room_for_label())
            .map(|tick| {
                div()
                    .absolute()
                    .top(ui.metrics.space.xs)
                    .left(relative(tick.fraction))
                    .ml(px(-AXIS_LABEL_WIDTH / 2.))
                    .w(px(AXIS_LABEL_WIDTH))
                    .flex()
                    .justify_center()
                    .ramp(ui, Ramp::Caption)
                    .text_color(ui.palette.text_tertiary)
                    .child(tick.time.to_string())
            });
        div()
            .relative()
            .h(ui.metrics.timeline.axis_height)
            .children(labels)
    }

    fn selection_overlay(&self, ui: &Widgets<'_>, view: View) -> Option<Div> {
        let interval = self.details.as_ref()?.content.interval.as_ref()?;
        let placement = view.place(interval.span)?;
        let accent = ui.palette.accent_fill;
        Some(
            div()
                .absolute()
                .top(px(0.))
                .bottom(px(0.))
                .left(relative(placement.left))
                .w(relative(placement.width))
                .bg(accent.opacity(ui.metrics.opacity.selection))
                .border_l_1()
                .border_r_1()
                .border_color(accent),
        )
    }

    fn block_hover(&self, target: HoverTarget) -> BlockHover {
        if self.hover.is_some_and(|hover| hover.target == target) {
            BlockHover::Hovered
        } else {
            BlockHover::Idle
        }
    }

    fn with_hover<E>(&self, element: E, target: HoverTarget, cx: &mut Context<Self>) -> E
    where
        E: gpui_kit::StatefulInteractiveElement,
    {
        element
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                this.set_hover(target, event.position, cx);
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.clear_hover(target, cx);
                }
            }))
    }

    fn main_track(
        &self,
        ui: &Widgets<'_>,
        view: View,
        plot_width: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let blocks = self.day.main_track().iter().filter_map(|block| {
            let span = block.span();
            let placement = view.place(span)?;
            let target = HoverTarget::MainBlock(span.start());
            let pixels = placement.width * plot_width;
            let body = block_body(ui, self.block_hover(target));
            let body = match block {
                MainBlock::Active { category, .. } => {
                    self.active_block(ui, body, *category, span, pixels)
                }
                MainBlock::State(state) => self.state_block(ui, body, state, pixels),
            };
            let tile = tile(
                ("main-block", span.start().seconds()),
                placement.left,
                placement.width,
            )
            .child(body);
            Some(self.with_hover(tile, target, cx))
        });
        div()
            .relative()
            .h(ui.metrics.timeline.main_track_height)
            .children(blocks.collect::<Vec<_>>())
    }

    fn active_block(
        &self,
        ui: &Widgets<'_>,
        body: Div,
        category: CategoryId,
        span: TimeSpan,
        pixels: f32,
    ) -> Div {
        let category = self.day.category(category);
        let color = ui.palette.category(category.color);
        let name = self.formatter.category_name(&category.name);
        let full = self.formatter.text_with(
            Msg::TimelineBlockLabel,
            &[
                (Arg::Name, &name),
                (Arg::Duration, &self.formatter.duration(span.duration())),
            ],
        );
        block_text(
            ui,
            body.bg(color),
            fitting_label(pixels, &name, &full),
            ui.palette.text_on(color),
        )
    }

    /// A state block styled per 11 section 3.4.
    fn state_block(&self, ui: &Widgets<'_>, body: Div, state: &StateBlock, pixels: f32) -> Div {
        let palette = ui.palette;
        let metrics = ui.metrics;
        let body = match state.kind {
            StateKind::Idle { preceding, .. } => {
                let color = preceding.map_or(palette.status_neutral, |id| {
                    palette.category(self.day.category(id).color)
                });
                body.bg(color.opacity(metrics.opacity.idle))
                    .child(div().absolute().size_full().bg(pattern_slash(
                        palette.stroke_control_strong,
                        metrics.timeline.hatch_width,
                        metrics.timeline.hatch_spacing,
                    )))
            }
            StateKind::Locked | StateKind::Sleep | StateKind::CoreStopped => {
                body.bg(palette.fill_subtle_hover)
            }
            kind @ (StateKind::NoForeground | StateKind::Excluded | StateKind::CollectionGap) => {
                let name = self.formatter.state_name(kind);
                let full = self.formatter.text_with(
                    Msg::TimelineStateLabel,
                    &[
                        (Arg::State, &name),
                        (
                            Arg::Duration,
                            &self.formatter.duration(state.span.duration()),
                        ),
                    ],
                );
                block_text(
                    ui,
                    body.bg(palette
                        .status_neutral
                        .opacity(metrics.opacity.neutral_state)),
                    fitting_label(pixels, &name, &full),
                    palette.text_primary,
                )
            }
        };
        body.when(state.clock == ClockAnomaly::Detected, |body| {
            body.child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .right(px(0.))
                    .h(metrics.timeline.clock_mark_height)
                    .bg(palette.status_caution),
            )
        })
    }

    fn lane_row(&self, ui: &Widgets<'_>, view: View, lane: &Lane, cx: &mut Context<Self>) -> Div {
        let blocks = self.day.lane_segments(lane).filter_map(|segment| {
            let placement = view.place(segment.span)?;
            let id = segment.id;
            let target = HoverTarget::Segment(id);
            let color = ui
                .palette
                .category(self.day.category(segment.category).color);
            let tile = tile(("segment", id.get()), placement.left, placement.width)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(DetailsSubject::Segment(id), cx);
                }))
                .child(block_body(ui, self.block_hover(target)).bg(color));
            Some(self.with_hover(tile, target, cx))
        });
        div()
            .relative()
            .mt(ui.metrics.timeline.lane_gap)
            .h(ui.metrics.timeline.lane_height)
            .children(blocks.collect::<Vec<_>>())
    }

    pub(super) fn render_tooltip(&self, ui: &Widgets<'_>) -> Option<impl IntoElement> {
        let hover = self.hover?;
        let body = match hover.target {
            HoverTarget::Segment(id) => self.segment_tooltip(
                ui,
                self.day
                    .segment(id)
                    .expect("hover targets are segments of the day shown"),
            ),
            HoverTarget::MainBlock(start) => self.block_tooltip(
                ui,
                self.day
                    .main_block_at(start)
                    .expect("hover targets are main blocks of the day shown"),
            ),
        };
        let (offset_x, offset_y) = TOOLTIP_OFFSET;
        Some(
            anchored()
                .position(hover.position)
                .offset(point(px(offset_x), px(offset_y)))
                .snap_to_window_with_margin(ui.metrics.space.s)
                .child(
                    ui.flyout()
                        .w(px(TOOLTIP_WIDTH))
                        .p(ui.metrics.space.m)
                        .flex()
                        .flex_col()
                        .gap(ui.metrics.space.xs)
                        .text_color(ui.palette.text_primary)
                        .child(body),
                ),
        )
    }

    fn segment_tooltip(&self, ui: &Widgets<'_>, segment: &Segment) -> Div {
        let palette = ui.palette;
        let app = self.day.app(segment.app);
        let category = self.day.category(segment.category);
        let rule = self.formatter.classification(
            &self.day,
            &segment.source,
            SourceText::WithCategory(&category.name),
        );
        div()
            .flex()
            .flex_col()
            .gap(ui.metrics.space.xs)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui.metrics.space.s)
                    .ramp(ui, Ramp::BodyStrong)
                    .child(ui.icon(app.glyph.into(), Density::Compact, palette.text_primary))
                    .child(app.name.clone()),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(segment.title.clone()),
            )
            .children(segment.domain.clone().map(|domain| {
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_tertiary)
                    .child(domain)
            }))
            .child(self.span_row(ui, segment.span))
            .child(self.category_chip(ui, segment.category))
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(rule),
            )
    }

    fn block_tooltip(&self, ui: &Widgets<'_>, block: &MainBlock) -> Div {
        let head = match block {
            MainBlock::Active { category, .. } => self.category_chip(ui, *category),
            MainBlock::State(state) => div()
                .ramp(ui, Ramp::BodyStrong)
                .child(self.formatter.state_name(state.kind)),
        };
        div()
            .flex()
            .flex_col()
            .gap(ui.metrics.space.xs)
            .child(head)
            .child(self.span_row(ui, block.span()))
    }

    fn span_row(&self, ui: &Widgets<'_>, span: TimeSpan) -> Div {
        div()
            .flex()
            .flex_row()
            .justify_between()
            .ramp(ui, Ramp::Caption)
            .text_color(ui.palette.text_primary)
            .child(span.to_string())
            .child(self.formatter.duration(span.duration()))
    }

    pub(super) fn category_chip(&self, ui: &Widgets<'_>, id: CategoryId) -> Div {
        let category = self.day.category(id);
        div()
            .flex()
            .flex_row()
            .items_center()
            .self_start()
            .gap(ui.metrics.space.s)
            .px(ui.metrics.space.s)
            .h(px(CHIP_HEIGHT))
            .rounded(ui.metrics.radius.control)
            .bg(ui.palette.fill_control_hover)
            .ramp(ui, Ramp::Caption)
            .child(ui.swatch(ui.palette.category(category.color)))
            .child(self.formatter.category_name(&category.name))
    }
}

/// Hit area of one timeline block, placed by plot fractions. The painted body inside leaves a
/// background gap on its right (11 section 3.3); blocks have square corners so touching blocks
/// never leave notches, and the minimum width keeps one-pixel blocks solid.
fn tile(id: impl Into<ElementId>, left: f32, width: f32) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .absolute()
        .top(px(0.))
        .h_full()
        .left(relative(left))
        .w(relative(width))
        .min_w(px(MIN_BLOCK_WIDTH))
}

/// Painted body of a block; the hover outline is an overlay so it never eats into narrow blocks.
fn block_body(ui: &Widgets<'_>, hover: BlockHover) -> Div {
    div()
        .relative()
        .flex_1()
        .h_full()
        .mr(ui.metrics.timeline.block_gap)
        .overflow_hidden()
        .when(hover == BlockHover::Hovered, |body| {
            body.child(
                div()
                    .absolute()
                    .size_full()
                    .border_1()
                    .border_color(ui.palette.text_primary),
            )
        })
}

/// Adds `label`, when there is one, as block text in `color`.
fn block_text(ui: &Widgets<'_>, body: Div, label: Option<String>, color: Hsla) -> Div {
    body.when_some(label, |body, text| {
        body.child(
            div()
                .px(px(BLOCK_TEXT_PADDING))
                .h_full()
                .flex()
                .items_center()
                .whitespace_nowrap()
                .ramp(ui, Ramp::Caption)
                .text_color(color)
                .child(text),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ThemeBundle;

    #[test]
    fn plot_width_estimate_follows_the_page_width_up_to_the_content_limit() {
        let tokens = ThemeBundle::embedded().expect("embedded theme").tokens();
        let metrics = tokens.metrics();
        // 1200 px content minus 2 x 24 page margin, 2 x 16 card padding, 12 plot padding and
        // the 140 px label column.
        assert!((estimated_plot_width(px(1600.), metrics) - 968.).abs() < f32::EPSILON);
        assert!((estimated_plot_width(px(992.), metrics) - 760.).abs() < f32::EPSILON);
        assert!((estimated_plot_width(px(100.), metrics) - MIN_PLOT_WIDTH).abs() < f32::EPSILON);
    }
}
