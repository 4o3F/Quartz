//! The timeline: category main track, per-application swimlanes, axis, hover and selection
//! (07 section 4, 11 sections 3.4 and 5.3). Blocks are positioned by fractions of the plot width,
//! so the layout never needs the plot's pixel size and nothing redraws while idle.

use gpui_kit::{
    Context, Div, ElementId, Hsla, InteractiveElement as _, IntoElement, MouseMoveEvent,
    ParentElement as _, Stateful, StatefulInteractiveElement as _, Styled as _, anchored,
    assets::IconName, div, pattern_slash, point, prelude::FluentBuilder as _, px, relative,
};

use super::{AppShell, HoverTarget, LaneMode, RangeChoice};
use crate::app::{
    model::{
        ClassificationSource, DetailsSubject, MainBlock, Segment, SegmentId, StateBlock, StateKind,
        TimeOfDay, TimeSpan,
    },
    tokens::{
        Palette,
        metrics::{AXIS_HEIGHT, BLOCK_RADIUS, LANE_GAP, LANE_HEIGHT, MAIN_TRACK_HEIGHT},
    },
    widgets::{Density, RADIUS_CONTROL, Ramp, RampExt as _, icon, space},
};

const LABEL_WIDTH: f32 = 140.;
const PLOT_PADDING: f32 = 4.;
const MARGIN_MINUTES: u32 = 15;
const MIN_ZOOM_MINUTES: f32 = 5.;
/// Horizontal padding a block label needs on top of its text.
const LABEL_PADDING: f32 = 12.;
const TOOLTIP_WIDTH: f32 = 260.;

/// Visible window as fractions of the plot.
#[derive(Debug, Clone, Copy)]
struct View {
    start: f32,
    end: f32,
}

impl View {
    fn from_span(span: TimeSpan) -> Self {
        Self {
            start: span.start.minutes_f32(),
            end: span.end.minutes_f32().max(span.start.minutes_f32() + 1.),
        }
    }

    fn length(self) -> f32 {
        self.end - self.start
    }

    /// Left and width fractions of `span`, clipped to the view; `None` when outside.
    fn place(self, span: TimeSpan) -> Option<(f32, f32)> {
        let start = span.start.minutes_f32().max(self.start);
        let end = span.end.minutes_f32().min(self.end);
        (end > start).then(|| {
            (
                (start - self.start) / self.length(),
                (end - start) / self.length(),
            )
        })
    }
}

impl AppShell {
    fn view(&self) -> View {
        match self.range {
            RangeChoice::Activity => {
                let range = self.day.activity_range();
                View::from_span(TimeSpan::new(
                    range.start.earlier_by_minutes(MARGIN_MINUTES),
                    range.end.later_by_minutes(MARGIN_MINUTES),
                ))
            }
            RangeChoice::FullDay => {
                View::from_span(TimeSpan::new(TimeOfDay::START, TimeOfDay::END))
            }
            RangeChoice::Zoomed {
                start_minute,
                end_minute,
            } => View {
                start: start_minute,
                end: end_minute,
            },
        }
    }

    fn zoom(&mut self, factor: f32, cx: &mut Context<Self>) {
        let view = self.view();
        let center = (view.start + view.end) / 2.;
        let half = (view.length() * factor / 2.).clamp(MIN_ZOOM_MINUTES / 2., 12. * 60.);
        let start = (center - half).max(0.);
        let end = (center + half).min(24. * 60.);
        self.range = RangeChoice::Zoomed {
            start_minute: start,
            end_minute: end,
        };
        self.hover = None;
        cx.notify();
    }

    pub(super) fn render_timeline(
        &self,
        p: &Palette,
        main_width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = self.view();
        // Estimated plot width, used only to decide which labels fit inside blocks.
        let plot_width =
            (main_width.min(1200.) - 2. * space::XXL - 2. * space::L - LABEL_WIDTH - space::M)
                .max(200.);
        let lanes = match self.lanes {
            LaneMode::Swimlanes => self.day.lanes(),
            LaneMode::CategoryOnly => Vec::new(),
        };
        let range = self.day.activity_range();
        let full_day = self.range == RangeChoice::FullDay;

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(space::S))
            .child(
                div()
                    .flex_1()
                    .ramp(Ramp::Subtitle)
                    .child(self.fmt.text("timeline.title")),
            )
            .child(
                div()
                    .ramp(Ramp::Caption)
                    .text_color(p.text_secondary)
                    .mr(px(space::S))
                    .child(range.to_string()),
            )
            .child(p.segmented(
                "timeline-lanes",
                vec![
                    (self.fmt.text("timeline.lanes.apps"), LaneMode::Swimlanes),
                    (
                        self.fmt.text("timeline.lanes.category"),
                        LaneMode::CategoryOnly,
                    ),
                ],
                self.lanes,
                |mode| {
                    cx.listener(move |this, _: &gpui_kit::ClickEvent, _, cx| {
                        this.lanes = mode;
                        this.hover = None;
                        cx.notify();
                    })
                },
            ))
            .child(
                p.button("zoom-out", Density::Compact, None, Some(IconName::Minus))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom(2., cx))),
            )
            .child(
                p.button("zoom-in", Density::Compact, None, Some(IconName::Plus))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom(0.5, cx))),
            )
            .child(
                p.button(
                    "zoom-full",
                    Density::Compact,
                    Some(self.fmt.text(if full_day {
                        "timeline.fit_activity"
                    } else {
                        "timeline.full_day"
                    })),
                    None,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.range = if full_day {
                        RangeChoice::Activity
                    } else {
                        RangeChoice::FullDay
                    };
                    this.hover = None;
                    cx.notify();
                })),
            );

        // Left label column; rows mirror the plot rows exactly.
        let mut labels = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(LABEL_WIDTH))
            .py(px(PLOT_PADDING))
            .child(
                div()
                    .h(px(MAIN_TRACK_HEIGHT))
                    .flex()
                    .items_center()
                    .ramp(Ramp::Caption)
                    .text_color(p.text_secondary)
                    .child(self.fmt.text("timeline.main_track")),
            );
        for lane in &lanes {
            let app = self.day.app(lane.app);
            labels = labels.child(
                div()
                    .h(px(LANE_HEIGHT))
                    .mt(px(LANE_GAP))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::S))
                    .ramp(Ramp::Caption)
                    .text_color(p.text_secondary)
                    .child(icon(app.glyph.icon_name(), 14., p.text_secondary))
                    .child(app.short_name.clone()),
            );
        }
        labels = labels.child(div().h(px(AXIS_HEIGHT)));

        let mut rows = div()
            .flex()
            .flex_col()
            .py(px(PLOT_PADDING))
            .child(self.main_track(p, view, plot_width, cx));
        for lane in &lanes {
            rows = rows.child(self.lane_row(p, view, &lane.segments, cx));
        }
        rows = rows.child(self.axis(p, view));

        let mut plot = (p)
            .plot_area()
            .relative()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .children(self.gridlines(p, view))
            .child(rows);
        if let Some(selection) = self.selection_overlay(p, view) {
            plot = plot.child(selection);
        }

        p.card()
            .flex()
            .flex_col()
            .gap(px(space::S))
            .p(px(space::L))
            .child(header)
            .child(div().flex().flex_row().child(labels).child(plot))
    }

    fn gridlines(&self, p: &Palette, view: View) -> Vec<Div> {
        view.ticks()
            .into_iter()
            .map(|minute| {
                div()
                    .absolute()
                    .top(px(0.))
                    .bottom(px(0.))
                    .left(relative((minute - view.start) / view.length()))
                    .w(px(1.))
                    .bg(p.stroke_divider)
            })
            .collect()
    }

    fn axis(&self, p: &Palette, view: View) -> Div {
        let mut axis = div().relative().h(px(AXIS_HEIGHT));
        for minute in view.ticks() {
            let fraction = (minute - view.start) / view.length();
            if !(0.02..=0.98).contains(&fraction) {
                continue;
            }
            let time = TimeOfDay::from_minutes_f32(minute);
            axis = axis.child(
                div()
                    .absolute()
                    .top(px(space::XS))
                    .left(relative((minute - view.start) / view.length()))
                    .ml(px(-20.))
                    .w(px(40.))
                    .flex()
                    .justify_center()
                    .ramp(Ramp::Caption)
                    .text_color(p.text_tertiary)
                    .child(time.to_string()),
            );
        }
        axis
    }

    fn selection_overlay(&self, p: &Palette, view: View) -> Option<Div> {
        let DetailsSubject::Segment(id) = self.details? else {
            return None;
        };
        let segment = self.day.segment(id)?;
        let (left, width) = view.place(segment.span)?;
        Some(
            div()
                .absolute()
                .top(px(0.))
                .bottom(px(0.))
                .left(relative(left))
                .w(relative(width))
                .bg(p.accent_fill.opacity(0.2))
                .border_l_1()
                .border_r_1()
                .border_color(p.accent_fill),
        )
    }

    fn hover_state(&self, target: HoverTarget) -> bool {
        self.hover.is_some_and(|hover| hover.target == target)
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

    fn main_track(&self, p: &Palette, view: View, plot_width: f32, cx: &mut Context<Self>) -> Div {
        let mut row = div().relative().h(px(MAIN_TRACK_HEIGHT));
        for (index, block) in self.day.main_track().iter().enumerate() {
            let Some((left, width)) = view.place(block.span()) else {
                continue;
            };
            let target = HoverTarget::MainBlock(index);
            let hovered = self.hover_state(target);
            let pixels = width * plot_width;
            let element = p.block_body(if hovered {
                BlockHover::Hovered
            } else {
                BlockHover::Idle
            });
            let element = match block {
                MainBlock::Active { span, category } => {
                    let category = self.day.category(*category);
                    let color = p.category(category.color);
                    let name = self.fmt.category_name(&category.name);
                    let label = block_label(
                        pixels,
                        &name,
                        &self.fmt.text_with(
                            "timeline.block_label",
                            &[
                                ("name", &name),
                                ("duration", &self.fmt.duration(span.duration())),
                            ],
                        ),
                    );
                    (element.bg(color)).block_text(label, p.text_on(color))
                }
                MainBlock::State(state) => self.state_block(p, element, state, pixels),
            };
            let tile = tile(("main-block", index), left, width).child(element);
            row = row.child(self.with_hover(tile, target, cx));
        }
        row
    }

    fn state_block(&self, p: &Palette, element: Div, state: &StateBlock, pixels: f32) -> Div {
        match state.kind {
            StateKind::Idle => {
                let color = state.category.map_or(p.status_neutral, |id| {
                    p.category(self.day.category(id).color)
                });
                element.bg(color.opacity(0.4)).child(
                    div()
                        .absolute()
                        .size_full()
                        .bg(pattern_slash(p.stroke_control_strong, 1., 4.)),
                )
            }
            kind if kind.is_quiet() => element.bg(p.fill_subtle_hover),
            kind => {
                let name = self.fmt.text(kind.label_key());
                let full = self.fmt.text_with(
                    "timeline.state_label",
                    &[
                        ("state", &name),
                        ("duration", &self.fmt.duration(state.span.duration())),
                    ],
                );
                (element.bg(p.status_neutral.opacity(0.3)))
                    .block_text(block_label(pixels, &name, &full), p.text_primary)
            }
        }
        .when(state.clock_anomaly, |element| {
            element.child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .right(px(0.))
                    .h(px(2.))
                    .bg(p.status_caution),
            )
        })
    }

    fn lane_row(
        &self,
        p: &Palette,
        view: View,
        segments: &[SegmentId],
        cx: &mut Context<Self>,
    ) -> Div {
        let mut row = div().relative().mt(px(LANE_GAP)).h(px(LANE_HEIGHT));
        for id in segments {
            let Some(segment) = self.day.segment(*id) else {
                continue;
            };
            let Some((left, width)) = view.place(segment.span) else {
                continue;
            };
            let target = HoverTarget::Segment(*id);
            let hovered = self.hover_state(target);
            let color = p.category(self.day.category(segment.category).color);
            let id = *id;
            let tile = tile(("segment", id.0 as usize), left, width)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(DetailsSubject::Segment(id), cx);
                }))
                .child(
                    p.block_body(if hovered {
                        BlockHover::Hovered
                    } else {
                        BlockHover::Idle
                    })
                    .bg(color),
                );
            row = row.child(self.with_hover(tile, target, cx));
        }
        row
    }

    pub(super) fn render_tooltip(&self, p: &Palette) -> Option<impl IntoElement> {
        let hover = self.hover?;
        let body = match hover.target {
            HoverTarget::Segment(id) => self.segment_tooltip(p, self.day.segment(id)?),
            HoverTarget::MainBlock(index) => {
                let track = self.day.main_track();
                self.block_tooltip(p, track.get(index)?)
            }
        };
        Some(
            anchored()
                .position(hover.position)
                .offset(point(px(12.), px(16.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    p.flyout()
                        .w(px(TOOLTIP_WIDTH))
                        .p(px(space::M))
                        .flex()
                        .flex_col()
                        .gap(px(space::XS))
                        .text_color(p.text_primary)
                        .child(body),
                ),
        )
    }

    fn segment_tooltip(&self, p: &Palette, segment: &Segment) -> Div {
        let app = self.day.app(segment.app);
        let category = self.day.category(segment.category);
        let rule = match &segment.source {
            ClassificationSource::Rule { condition } => self.fmt.text_with(
                "timeline.tooltip.rule",
                &[
                    ("condition", condition),
                    ("category", &self.fmt.category_name(&category.name)),
                ],
            ),
            ClassificationSource::Manual => self.fmt.text("timeline.tooltip.manual"),
            ClassificationSource::Unmatched => self.fmt.text("timeline.tooltip.unmatched"),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::S))
                    .ramp(Ramp::BodyStrong)
                    .child(icon(app.glyph.icon_name(), 16., p.text_primary))
                    .child(app.name.clone()),
            )
            .child(
                div()
                    .ramp(Ramp::Caption)
                    .text_color(p.text_secondary)
                    .child(segment.title.clone()),
            )
            .children(segment.domain.clone().map(|domain| {
                div()
                    .ramp(Ramp::Caption)
                    .text_color(p.text_tertiary)
                    .child(domain)
            }))
            .child(self.span_row(p, segment.span))
            .child(self.category_chip(p, segment.category))
            .child(
                div()
                    .ramp(Ramp::Caption)
                    .text_color(p.text_secondary)
                    .child(rule),
            )
    }

    fn block_tooltip(&self, p: &Palette, block: &MainBlock) -> Div {
        let head = match block {
            MainBlock::Active { category, .. } => self.category_chip(p, *category),
            MainBlock::State(state) => div()
                .ramp(Ramp::BodyStrong)
                .child(self.fmt.text(state.kind.label_key())),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(head)
            .child(self.span_row(p, block.span()))
    }

    fn span_row(&self, p: &Palette, span: TimeSpan) -> Div {
        div()
            .flex()
            .flex_row()
            .justify_between()
            .ramp(Ramp::Caption)
            .text_color(p.text_primary)
            .child(span.to_string())
            .child(self.fmt.duration(span.duration()))
    }

    pub(super) fn category_chip(&self, p: &Palette, id: crate::app::model::CategoryId) -> Div {
        let category = self.day.category(id);
        div()
            .flex()
            .flex_row()
            .items_center()
            .self_start()
            .gap(px(space::S))
            .px(px(space::S))
            .h(px(20.))
            .rounded(px(RADIUS_CONTROL))
            .bg(p.fill_control_hover)
            .ramp(Ramp::Caption)
            .child(
                div()
                    .size(px(8.))
                    .rounded(px(BLOCK_RADIUS))
                    .bg(p.category(category.color)),
            )
            .child(self.fmt.category_name(&category.name))
    }
}

/// Hit area of one timeline block, placed by plot fractions. The painted body inside leaves a
/// 1 px background gap on its right (11 section 3.3); blocks have square corners so touching
/// blocks never leave notches, and the 2 px minimum keeps 1 px wide blocks solid.
fn tile(id: impl Into<ElementId>, left: f32, width: f32) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .absolute()
        .top(px(0.))
        .h_full()
        .left(relative(left))
        .w(relative(width))
        .min_w(px(2.))
}

/// Rough width of 12 px text: wide glyphs take 12 px, narrow ones 6.5 px.
fn estimate_text_width(text: &str) -> f32 {
    text.chars()
        .map(|c| if c.is_ascii() { 6.5 } else { 12. })
        .sum::<f32>()
        + LABEL_PADDING
}

/// Chooses the longest label that fits in `pixels`, or none.
fn block_label(pixels: f32, short: &str, full: &str) -> Option<String> {
    [full, short]
        .into_iter()
        .find(|text| estimate_text_width(text) <= pixels)
        .map(str::to_owned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockHover {
    Idle,
    Hovered,
}
impl Palette {
    /// Painted body of a block; the hover outline is an overlay so it never eats into narrow blocks.
    fn block_body(&self, hover: BlockHover) -> Div {
        div()
            .relative()
            .flex_1()
            .h_full()
            .mr(px(1.))
            .overflow_hidden()
            .when(hover == BlockHover::Hovered, |body| {
                body.child(
                    div()
                        .absolute()
                        .size_full()
                        .border_1()
                        .border_color(self.text_primary),
                )
            })
    }
}

impl StateKind {
    const fn label_key(self) -> &'static str {
        match self {
            StateKind::Idle => "timeline.state.idle",
            StateKind::Locked => "timeline.state.locked",
            StateKind::Sleep => "timeline.state.sleep",
            StateKind::CoreStopped => "timeline.state.core_stopped",
            StateKind::NoForeground => "timeline.state.no_foreground",
            StateKind::Excluded => "timeline.state.excluded",
            StateKind::CollectionGap => "timeline.state.gap",
        }
    }
}

trait BlockTextExt {
    fn block_text(self, label: Option<String>, color: Hsla) -> Self;
}
impl BlockTextExt for Div {
    fn block_text(self, label: Option<String>, color: Hsla) -> Div {
        self.when_some(label, |element, text| {
            element.child(
                div()
                    .px(px(space::S - 2.))
                    .h_full()
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .ramp(Ramp::Caption)
                    .text_color(color)
                    .child(text),
            )
        })
    }
}

impl View {
    fn tick_step_minutes(self) -> f32 {
        const STEPS: [f32; 9] = [1., 5., 10., 15., 30., 60., 120., 180., 360.];
        STEPS
            .into_iter()
            .find(|step| self.length() / step <= 14.)
            .unwrap_or(360.)
    }
    fn ticks(self) -> Vec<f32> {
        let step = self.tick_step_minutes();
        let first = (self.start / step).ceil();
        (0..)
            .map(|index| (first + index as f32) * step)
            .take_while(|minute| *minute <= self.end)
            .collect()
    }
}
