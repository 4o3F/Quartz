//! The timeline: category main track, per-application swimlanes, axis, hover and selection
//! (07 section 4, 11 sections 3.4 and 5.3). The plot is built during prepaint of the frame from
//! the bounds the layout gave it, so blocks, ticks and labels are placed in logical pixels on
//! the physical pixel grid of that frame (see [`crate::timeline_scale`]); nothing lags a resize
//! and nothing is drawn empty first.

use gpui_kit::{
    AnyElement, ClickEvent, Context, Div, ElementId, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseMoveEvent, ParentElement as _, Pixels, Point, Stateful,
    StatefulInteractiveElement as _, Styled as _, anchored, assets::IconName, div, pattern_slash,
    point, prelude::FluentBuilder as _, px,
};

use super::{AppShell, Frame, HoverTarget, LaneMode, LayerCx, RangeChoice};
use crate::{
    app::{
        measure::TextMeasure,
        metrics::{Density, Ramp},
        widgets::{ButtonContent, RampExt as _, Segment as SegmentedItem, Widgets},
    },
    format::{Arg, Msg, SourceText},
    layout::{ColumnRange, LABEL_COLUMN_MAX_WIDTH},
    model::{
        CategoryId, ClockAnomaly, DetailsSubject, Lane, MainBlock, Segment, StateBlock, StateKind,
        TimeOfDay, TimeSpan,
    },
    timeline_scale::{
        BuildPlotGridError, Interval, PixelPlacement, PlotGrid, RowTiles, ScaleFactor, Tick,
        TickBudget, View, ZoomStep,
    },
};

/// The label column left of the plot: as wide as its widest label (11 section 5.6), within this
/// range; longer application names are ellipsized.
const LABEL_COLUMN_RANGE: ColumnRange = ColumnRange::new(72., LABEL_COLUMN_MAX_WIDTH);
/// Widest text an axis tick label shows, for measuring; digits are equally wide (11 section 4.1).
const TICK_LABEL_SAMPLE: &str = "00:00";
/// Horizontal padding of block text; the label has to fit between the paddings.
const BLOCK_TEXT_PADDING: f32 = 6.;
/// Narrowest the hover tooltip gets (11 section 5.6).
const TOOLTIP_MIN_WIDTH: f32 = 200.;
/// Widest the hover tooltip gets before its text wraps (11 section 5.6).
const TOOLTIP_MAX_WIDTH: f32 = 320.;
/// Offset of the tooltip from the pointer.
const TOOLTIP_OFFSET: Point<Pixels> = point(px(12.), px(16.));
/// Smallest height of a category chip; it grows with the text.
const CHIP_HEIGHT: f32 = 20.;

/// One lane-mode option with its element id and localized label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LaneModeOption {
    mode: LaneMode,
    id: &'static str,
    label: Msg,
}

/// Lane-mode choices in display order with their element ids and labels.
const LANE_MODES: [LaneModeOption; 2] = [
    LaneModeOption {
        mode: LaneMode::Swimlanes,
        id: "lanes-apps",
        label: Msg::TimelineLanesApps,
    },
    LaneModeOption {
        mode: LaneMode::CategoryOnly,
        id: "lanes-category",
        label: Msg::TimelineLanesCategory,
    },
];

/// Whether the pointer is over a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BlockHover {
    Idle,
    Hovered,
}

/// What the plot derives from its bounds in the frame it is laid out: the pixel grid and the
/// ticks that fit.
#[derive(Debug, Clone, PartialEq)]
struct PlotGeometry {
    width: f32,
    grid: PlotGrid,
    tick_label_width: f32,
    ticks: Vec<Tick>,
}

impl PlotGeometry {
    /// The geometry of a plot `width` wide showing `view`.
    fn new(
        width: f32,
        scale: ScaleFactor,
        block_gap: f32,
        tick_label_width: f32,
        view: View,
    ) -> Result<Self, BuildPlotGridError> {
        Ok(Self {
            width,
            grid: PlotGrid::new(width, scale, block_gap)?,
            tick_label_width,
            ticks: view.ticks(TickBudget::fit(width, tick_label_width)),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PanGesture {
    start_x: f32,
    view: View,
}

impl AppShell {
    pub(super) fn view(&self) -> View {
        match self.range {
            RangeChoice::Activity => {
                if matches!(self.day.source(), super::DaySource::Fixture) {
                    return View::activity(self.day.activity_range());
                }
                let full = self.day.full_span();
                let span = self
                    .day
                    .activity_range()
                    .map(|span| {
                        let margin = crate::timeline_scale::ACTIVITY_MARGIN_MINUTES
                            * crate::model::SECONDS_PER_MINUTE
                            * crate::model::MILLIS_PER_SECOND;
                        TimeSpan::new(
                            TimeOfDay::from_elapsed_millis(
                                span.start().millis().saturating_sub(margin),
                            ),
                            TimeOfDay::from_elapsed_millis(
                                span.end()
                                    .millis()
                                    .saturating_add(margin)
                                    .min(full.end().millis()),
                            ),
                        )
                    })
                    .unwrap_or(full);
                View::new(span).expect("activity view is nonempty")
            }
            RangeChoice::FullDay => View::new(self.day.full_span()).expect("nonempty logical day"),
            RangeChoice::Zoomed(view) => view,
        }
    }

    fn move_pan(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        use num_traits::ToPrimitive as _;
        let Some(pan) = self.pan else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.pan = None;
            return;
        }
        let Some(width) = self.plot_width.filter(|width| *width > 0.) else {
            return;
        };
        let delta = f64::from(pan.start_x - f32::from(event.position.x))
            * pan.view.span().duration().as_secs_f64()
            * f64::from(crate::model::MILLIS_PER_SECOND)
            / f64::from(width);
        if let Some(delta) = delta.round().to_i64() {
            let view = pan.view.panned(delta, self.day.full_span());
            if view != self.view() {
                self.range = RangeChoice::Zoomed(view);
                self.hover = None;
                cx.notify();
            }
        }
    }

    fn zoom(&mut self, step: ZoomStep, cx: &mut Context<Self>) {
        self.range = RangeChoice::Zoomed(self.view().zoomed_within(step, self.day.full_span()));
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

    /// The height of the main track row.
    fn track_height(ui: &Widgets<'_>) -> Pixels {
        ui.height_for_text(ui.metrics.timeline.main_track_height, Ramp::Caption)
    }

    /// The height of one lane row.
    fn lane_height(ui: &Widgets<'_>) -> Pixels {
        ui.height_for_text(ui.metrics.timeline.lane_height, Ramp::Caption)
    }

    /// The height of the axis row: the token or, when text has grown, the caption line and the
    /// space above it.
    fn axis_height(ui: &Widgets<'_>) -> Pixels {
        ui.metrics
            .timeline
            .axis_height
            .max(ui.line_height(Ramp::Caption) + ui.metrics.space.xs)
    }

    /// The height of the plot box: the rows between the vertical plot padding.
    fn plot_height(&self, ui: &Widgets<'_>) -> Pixels {
        let sizes = &ui.metrics.timeline;
        let lane_row = sizes.lane_gap + Self::lane_height(ui);
        let lane_rows = std::iter::repeat_n(lane_row, self.visible_lanes().len())
            .fold(px(0.), |total, row| total + row);
        2. * ui.metrics.space.xs + Self::track_height(ui) + lane_rows + Self::axis_height(ui)
    }

    pub(super) fn render_timeline(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label_width = self.label_column_width(ui, frame.measure);
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
                    .gap(ui.metrics.space.s)
                    .child(self.label_column(ui, label_width))
                    .child(self.plot(ui, cx)),
            )
    }

    /// The width of the label column: the widest of the main track label and the lane labels
    /// (icon, gap and name).
    pub(super) fn label_column_width(&self, ui: &Widgets<'_>, measure: &TextMeasure<'_>) -> f32 {
        let main = measure.width(&self.formatter.text(Msg::TimelineMainTrack), Ramp::Caption);
        let icon = f32::from(ui.metrics.density(Density::Compact).icon_size)
            + f32::from(ui.metrics.space.s);
        let lanes = self
            .visible_lanes()
            .iter()
            .map(|lane| icon + measure.width(&self.day.app(lane.app).short_name, Ramp::Caption));
        LABEL_COLUMN_RANGE.select(std::iter::once(main).chain(lanes))
    }

    fn timeline_header(&self, ui: &Widgets<'_>, cx: &mut Context<Self>) -> Div {
        let full_day = self.range == RangeChoice::FullDay;
        let density = Density::Compact;
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .min_w(px(0.))
            .items_center()
            .gap(ui.metrics.space.s)
            .child(
                div()
                    .flex_auto()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .ramp(ui, Ramp::Subtitle)
                    .child(self.formatter.text(Msg::TimelineTitle)),
            )
            .child(
                // The span the plot shows, which is not the activity range after a zoom, in
                // the full-day view, or with the activity margins (07 section 4.1).
                div()
                    .flex_shrink_0()
                    .max_w_full()
                    .whitespace_nowrap()
                    .ramp(ui, Ramp::Caption)
                    .text_color(ui.palette.text_secondary)
                    .mr(ui.metrics.space.s)
                    .child(format!(
                        "{} – {}",
                        self.clock_label(self.view().span().start()),
                        self.clock_label(self.view().span().end())
                    )),
            )
            .child(ui.segmented(
                "timeline-lanes",
                LANE_MODES.map(|option| SegmentedItem {
                    id: option.id,
                    label: self.formatter.text(option.label),
                    value: option.mode,
                }),
                self.lanes,
                |mode| cx.listener(move |this, _: &ClickEvent, _, cx| this.set_lanes(mode, cx)),
            ))
            .child(
                ui.button("zoom-out", density, ButtonContent::Icon(IconName::Minus))
                    .flex_shrink_0()
                    .on_click(cx.listener(|this, _, _, cx| this.zoom(ZoomStep::Out, cx))),
            )
            .child(
                ui.button("zoom-in", density, ButtonContent::Icon(IconName::Plus))
                    .flex_shrink_0()
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
                .max_w_full()
                .flex_shrink_0()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_full_day(cx))),
            )
    }

    /// The label column; its rows mirror the plot rows exactly.
    fn label_column(&self, ui: &Widgets<'_>, width: f32) -> Div {
        let palette = ui.palette;
        let sizes = &ui.metrics.timeline;
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(width))
            .py(ui.metrics.space.xs)
            .child(
                div()
                    .h(Self::track_height(ui))
                    .flex()
                    .items_center()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(self.formatter.text(Msg::TimelineMainTrack)),
            )
            .children(self.visible_lanes().iter().map(|lane| {
                let app = self.day.app(lane.app);
                div()
                    .h(Self::lane_height(ui))
                    .mt(sizes.lane_gap)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui.metrics.space.s)
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(div().flex_shrink_0().child(ui.icon(
                        app.glyph.into(),
                        Density::Compact,
                        palette.text_secondary,
                    )))
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(app.short_name.clone()),
                    )
            }))
            .child(div().h(Self::axis_height(ui)))
    }

    /// The plot box. Its content is built from the bounds the layout gives the box in this
    /// frame, so nothing lags a resize, a navigation change or a text size change.
    fn plot(&self, ui: &Widgets<'_>, cx: &mut Context<Self>) -> Div {
        let line_height = ui.line_height(Ramp::Body);
        ui.plot_area()
            .relative()
            .flex_1()
            .min_w(px(0.))
            .h(self.plot_height(ui))
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, _| {
                    this.pan = Some(PanGesture {
                        start_x: f32::from(event.position.x),
                        view: this.view(),
                    });
                }),
            )
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| this.move_pan(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.pan = None),
            )
            .on_scroll(
                cx.listener(move |this, event: &gpui_kit::ScrollEvent, _, cx| {
                    let delta = event.delta.pixel_delta(line_height);
                    if delta.y != px(0.) {
                        this.zoom(
                            if delta.y > px(0.) {
                                ZoomStep::In
                            } else {
                                ZoomStep::Out
                            },
                            cx,
                        );
                        cx.stop_propagation();
                    }
                }),
            )
            .child(self.bounds_layer(cx, |layer| layer.shell.plot_content(layer)))
            .child(self.measure_plot(cx))
    }

    fn plot_content(&self, layer: &LayerCx<'_>) -> AnyElement {
        layer.with_ui(|ui, measure, scale| {
            let view = self.view();
            let Ok(geometry) = PlotGeometry::new(
                f32::from(layer.bounds.size.width),
                scale,
                f32::from(ui.metrics.timeline.block_gap),
                measure.width(TICK_LABEL_SAMPLE, Ramp::Caption),
                view,
            ) else {
                // Narrower than one marker: nothing can be drawn inside (07 section 4.3).
                return div().size_full().into_any_element();
            };
            let rows = div()
                .flex()
                .flex_col()
                .py(ui.metrics.space.xs)
                .child(self.main_track(ui, measure, &geometry, layer))
                .children(
                    self.visible_lanes()
                        .iter()
                        .map(|lane| self.lane_row(ui, view, &geometry, lane, layer)),
                )
                .child(self.axis(ui, &geometry));
            div()
                .relative()
                .size_full()
                .children(self.gridlines(ui, &geometry))
                .child(rows)
                .children(self.selection_overlay(ui, view, &geometry))
                .into_any_element()
        })
    }

    fn gridlines(&self, ui: &Widgets<'_>, geometry: &PlotGeometry) -> Vec<Div> {
        geometry
            .ticks
            .iter()
            .map(|tick| {
                div()
                    .absolute()
                    .top(px(0.))
                    .bottom(px(0.))
                    .left(px(geometry.grid.snap(tick.fraction)))
                    .w(ui.metrics.layout.divider)
                    .bg(ui.palette.stroke_divider)
            })
            .collect()
    }

    fn axis(&self, ui: &Widgets<'_>, geometry: &PlotGeometry) -> Div {
        let label_width = geometry.tick_label_width;
        let labels = geometry
            .ticks
            .iter()
            .filter(|tick| tick.has_room_for_label(geometry.width, label_width))
            .map(|tick| {
                div()
                    .absolute()
                    .top(ui.metrics.space.xs)
                    .left(px(geometry.grid.snap(tick.fraction) - label_width / 2.))
                    .w(px(label_width))
                    .flex()
                    .justify_center()
                    .ramp(ui, Ramp::Caption)
                    .tabular_figures()
                    .whitespace_nowrap()
                    .text_color(ui.palette.text_tertiary)
                    .child(self.clock_label(tick.time))
            });
        div().relative().h(Self::axis_height(ui)).children(labels)
    }

    fn selection_overlay(
        &self,
        ui: &Widgets<'_>,
        view: View,
        geometry: &PlotGeometry,
    ) -> Option<Div> {
        let selected = self.details.as_ref()?.span(&self.day)?;
        let span = geometry.grid.span(view.place(selected)?);
        let accent = ui.palette.accent_fill;
        Some(
            div()
                .absolute()
                .top(px(0.))
                .bottom(px(0.))
                .left(px(span.left))
                .w(px(span.width))
                .bg(ui.state_color(accent, ui.metrics.opacity.selection))
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

    fn with_hover<E>(&self, element: E, target: HoverTarget, layer: &LayerCx<'_>) -> E
    where
        E: gpui_kit::StatefulInteractiveElement,
    {
        element
            .on_mouse_move(layer.listen(move |this, event: &MouseMoveEvent, _, cx| {
                this.set_hover(target, event.position, cx);
            }))
            .on_hover(layer.listen(move |this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.clear_hover(target, cx);
                }
            }))
    }

    fn main_track(
        &self,
        ui: &Widgets<'_>,
        measure: &TextMeasure<'_>,
        geometry: &PlotGeometry,
        layer: &LayerCx<'_>,
    ) -> Div {
        let view = self.view();
        let mut intervals = Vec::new();
        for (index, block) in self.day.main_track().iter().enumerate() {
            let span = block.span();
            let Some(placement) = view.place(span) else {
                continue;
            };
            let pixels = geometry.grid.place(placement);
            let target = HoverTarget::MainBlock(span.start());
            let body = block_body(ui, self.block_hover(target), pixels);
            let body = match block {
                MainBlock::Active { category, .. } => {
                    self.active_block(ui, measure, body, *category, span, pixels)
                }
                MainBlock::State(state) => self.state_block(ui, measure, body, state, pixels),
            };
            let tile = tile(("main-block", index), pixels)
                .child(body)
                .on_mouse_down(
                    MouseButton::Left,
                    layer.listen(|_, _, _, cx| cx.stop_propagation()),
                )
                .on_click(layer.listen(move |this, _, window, cx| {
                    this.select(DetailsSubject::MainBlock(span), window, cx)
                }));
            intervals.push(Interval {
                placement: pixels,
                duration: span.duration(),
                item: self.with_hover(tile, target, layer),
            });
        }
        div()
            .relative()
            .h(Self::track_height(ui))
            .children(RowTiles::arrange(intervals).into_paint_order())
    }

    fn active_block(
        &self,
        ui: &Widgets<'_>,
        measure: &TextMeasure<'_>,
        body: Div,
        category: CategoryId,
        span: TimeSpan,
        block: PixelPlacement,
    ) -> Div {
        let category = self.day.category(category);
        let color = ui.palette.category(category.color);
        let name = self.formatter.category_name(&category.name);
        let full = self.formatter.text_with(
            Msg::TimelineBlockLabel,
            &[
                Arg::Name.with(&name),
                Arg::Duration.with(&self.formatter.duration(span.duration())),
            ],
        );
        block_text(
            ui,
            ui.category_mark(body, category.color),
            measure.fit_block_label(block, BLOCK_TEXT_PADDING, &name, &full),
            if ui.palette.contrast == crate::app::tokens::ContrastMode::High {
                ui.palette.text_primary
            } else {
                ui.palette.text_on(color)
            },
        )
    }

    /// A state block styled per 11 section 3.4.
    fn state_block(
        &self,
        ui: &Widgets<'_>,
        measure: &TextMeasure<'_>,
        body: Div,
        state: &StateBlock,
        block: PixelPlacement,
    ) -> Div {
        let palette = ui.palette;
        let metrics = ui.metrics;
        let body = match state.kind {
            StateKind::Idle { preceding, .. } => {
                let color = preceding.map_or(palette.status_neutral, |id| {
                    palette.category(self.day.category(id).color)
                });
                body.bg(ui.state_color(color, metrics.opacity.idle)).child(
                    div().absolute().size_full().bg(pattern_slash(
                        palette.stroke_control_strong,
                        metrics.timeline.hatch_width,
                        metrics.timeline.hatch_spacing,
                    )),
                )
            }
            StateKind::Locked | StateKind::Sleep | StateKind::CoreStopped => {
                body.bg(palette.fill_subtle_hover)
            }
            kind @ (StateKind::NoForeground | StateKind::Excluded | StateKind::CollectionGap) => {
                let name = self.formatter.state_name(kind);
                let full = self.formatter.text_with(
                    Msg::TimelineStateLabel,
                    &[
                        Arg::State.with(&name),
                        Arg::Duration.with(&self.formatter.duration(state.span.duration())),
                    ],
                );
                block_text(
                    ui,
                    body.bg(
                        if palette.contrast == crate::app::tokens::ContrastMode::High {
                            palette.status_neutral
                        } else {
                            ui.state_color(palette.status_neutral, metrics.opacity.neutral_state)
                        },
                    ),
                    measure.fit_block_label(block, BLOCK_TEXT_PADDING, &name, &full),
                    palette.text_primary,
                )
            }
        };
        let body = body.when(
            palette.contrast == crate::app::tokens::ContrastMode::High,
            |body| {
                let foreground = palette.text_primary;
                body.child(
                    gpui_kit::canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            window.paint_quad(gpui_kit::outline(
                                bounds,
                                foreground,
                                Default::default(),
                            ))
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
            },
        );
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

    fn lane_row(
        &self,
        ui: &Widgets<'_>,
        view: View,
        geometry: &PlotGeometry,
        lane: &Lane,
        layer: &LayerCx<'_>,
    ) -> Div {
        let mut intervals = Vec::new();
        for segment in self.day.lane_segments(lane) {
            let Some(placement) = view.place(segment.span) else {
                continue;
            };
            let pixels = geometry.grid.place(placement);
            let id = segment.id;
            let target = HoverTarget::Segment(id);
            let tile = tile(("segment", id.get()), pixels)
                .cursor_pointer()
                .on_click(layer.listen(move |this, _: &ClickEvent, window, cx| {
                    this.select(DetailsSubject::Segment(id), window, cx);
                }))
                .child({
                    let category = self.day.category(segment.category).color;
                    block_text(
                        ui,
                        ui.category_mark(
                            block_body(ui, self.block_hover(target), pixels),
                            category,
                        ),
                        layer.with_ui(|_, measure, _| {
                            measure.fit_block_label(
                                pixels,
                                BLOCK_TEXT_PADDING,
                                &segment.title,
                                &segment.title,
                            )
                        }),
                        ui.palette.text_on(ui.palette.category(category)),
                    )
                });
            intervals.push(Interval {
                placement: pixels,
                duration: segment.duration(),
                item: self.with_hover(tile, target, layer),
            });
        }
        div()
            .relative()
            .mt(ui.metrics.timeline.lane_gap)
            .h(Self::lane_height(ui))
            .children(RowTiles::arrange(intervals).into_paint_order())
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
        Some(
            anchored()
                .position(hover.position)
                .offset(TOOLTIP_OFFSET)
                .snap_to_window_with_margin(ui.metrics.space.s)
                .child(
                    ui.flyout()
                        .min_w(px(TOOLTIP_MIN_WIDTH))
                        .max_w(px(TOOLTIP_MAX_WIDTH))
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
            .child(format!(
                "{} – {}",
                self.clock_label(span.start()),
                self.clock_label(span.end())
            ))
            .child(self.formatter.duration(span.duration()))
    }

    pub(super) fn category_chip(&self, ui: &Widgets<'_>, id: CategoryId) -> Div {
        let category = self.day.category(id);
        div()
            .flex()
            .flex_row()
            .items_center()
            .self_start()
            .min_w(px(0.))
            .max_w_full()
            .gap(ui.metrics.space.s)
            .px(ui.metrics.space.s)
            .min_h(px(CHIP_HEIGHT))
            .rounded(ui.metrics.radius.control)
            .bg(ui.palette.fill_control_hover)
            .ramp(ui, Ramp::Caption)
            .child(ui.category_mark(
                ui.swatch(ui.palette.category(category.color)),
                category.color,
            ))
            .child(
                div()
                    .min_w(px(0.))
                    .whitespace_normal()
                    .child(self.formatter.category_name(&category.name)),
            )
    }
}

/// Hit area of one timeline block or marker, placed in pixels of the plot. It blocks the mouse
/// from the tiles beneath it, so one click reaches one handler, but lets the wheel through. The painted body inside leaves a
/// background gap on its right (11 section 3.3); blocks have square corners so touching blocks
/// never leave notches.
fn tile(id: impl Into<ElementId>, block: PixelPlacement) -> Stateful<Div> {
    div()
        .id(id)
        .block_mouse_except_scroll()
        .flex()
        .absolute()
        .top(px(0.))
        .h_full()
        .left(px(block.left()))
        .w(px(block.width()))
}

/// Painted body of a block; the hover outline is an overlay so it never eats into narrow blocks.
fn block_body(ui: &Widgets<'_>, hover: BlockHover, block: PixelPlacement) -> Div {
    div()
        .relative()
        .flex_1()
        .h_full()
        .mr(px(block.gap()))
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
                .when(
                    ui.palette.contrast == crate::app::tokens::ContrastMode::High,
                    |label| {
                        label
                            .bg(ui.palette.bg_base)
                            .mt(ui.metrics.layout.divider)
                            .ml(ui.metrics.layout.divider)
                            .h(AppShell::track_height(ui) - 2. * ui.metrics.layout.divider)
                    },
                )
                .flex()
                .items_center()
                .whitespace_nowrap()
                .ramp(ui, Ramp::Caption)
                .text_color(color)
                .child(text),
        )
    })
}
