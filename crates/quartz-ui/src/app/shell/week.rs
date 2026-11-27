//! Week page: navigation header with total active time, 7-day stacked category columns, and
//! per-day focus & distraction share lines (07 section 5).

use std::time::Duration;

use gpui_kit::{
    AnyElement, Bounds, Canvas, Context, Div, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, PathBuilder, Pixels, Point, StatefulInteractiveElement as _, Styled as _,
    Window, assets::IconName, canvas, div, point, px,
};
use jiff::civil::Weekday;
use quartz_protocol::LocalDate;

use super::{
    AppShell, Frame, Page,
    charts::{
        AXIS_COLUMN_RANGE, ChartGridline, StackedSegment, render_gridline, render_legend_item,
        render_stacked_column,
    },
};
use crate::{
    app::{
        metrics::{Density, Ramp},
        tokens::ContrastMode,
        widgets::{ButtonContent, RampExt as _, SkeletonTone, Widgets},
    },
    format::{Arg, Msg},
    layout::WindowClass,
    link::{LoadDataError, RequestCoreError},
    model::{
        BuiltinCategory, PageVisibility, ProgressUpdate, Share, WEEK_DAY_COUNT, WeekData, WeekDay,
        WeekDayKind, WeekEffect, WeekLoadState, weekday_from_protocol,
    },
};

/// Plot height of the 7-day stacked category column chart in logical pixels.
const WEEK_COLUMNS_HEIGHT: f32 = 160.;
/// Plot height of the 7-day focus & distraction share line chart in logical pixels.
const WEEK_SHARES_HEIGHT: f32 = 128.;
/// Stroke width of the focus and distraction share lines in logical pixels.
const SHARE_LINE_STROKE_WIDTH: f32 = 2.;
/// Diameter of each day point marker on the share line chart in logical pixels.
const SHARE_POINT_DIAMETER: f32 = 8.;
/// Half of [`SHARE_POINT_DIAMETER`] for centering markers on plotted coordinates.
const SHARE_POINT_RADIUS: f32 = SHARE_POINT_DIAMETER / 2.;
/// Outline thickness of hollow distraction point markers in logical pixels.
const SHARE_POINT_BORDER_WIDTH: f32 = 2.;
/// Dash stroke length for the distraction series in high contrast mode (logical pixels).
const SHARE_DASH_LENGTH: f32 = 6.;
/// Gap length between dashes in high contrast mode (logical pixels).
const SHARE_DASH_GAP: f32 = 4.;
/// Minimum width of the week date-range display pill in logical pixels.
const WEEK_RANGE_MIN_WIDTH: f32 = 148.;
/// Minimum height of the empty-week card, aligned with Today's empty state.
const EMPTY_CARD_MIN_HEIGHT: f32 = 260.;
/// Icon tile size in the empty-week state card.
const EMPTY_TILE_SIZE: f32 = 48.;
/// Icon glyph size inside the empty-week state tile.
const EMPTY_ICON_SIZE: f32 = 24.;
/// Width of the total-active skeleton placeholder in the header.
const SKELETON_TOTAL_WIDTH: f32 = 120.;
/// Height of the total-active skeleton placeholder in the header.
const SKELETON_TOTAL_HEIGHT: f32 = 20.;
/// Width of the legend skeleton strip under the stacked columns skeleton.
const SKELETON_LEGEND_WIDTH: f32 = 240.;
/// Height of the legend skeleton strip under the stacked columns skeleton.
const SKELETON_LEGEND_HEIGHT: f32 = 12.;
/// Static placeholder bar heights for the 7-day loading skeleton (last day left low/empty).
const SKELETON_BAR_HEIGHTS: [f32; WEEK_DAY_COUNT] = [104., 88., 60., 122., 84., 52., 20.];
/// Maximum width of one skeleton column bar in logical pixels.
const SKELETON_BAR_MAX_WIDTH: f32 = 52.;
/// Horizontal legend line swatch width in logical pixels.
const LEGEND_LINE_WIDTH: f32 = 16.;
/// Horizontal legend line swatch thickness in logical pixels.
const LEGEND_LINE_HEIGHT: f32 = 2.;
/// Y-axis percentage ticks for the share line chart from top to bottom.
const SHARE_AXIS_TICKS: [(f32, u32); 3] = [(0.0, 100), (0.5, 50), (1.0, 0)];
/// Number of days in a week as `f32` for slot-center calculations.
const WEEK_DAY_COUNT_F32: f32 = 7.;

/// Stroke pattern for a share line series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineDash {
    Solid,
    Dashed,
}

/// Visual fill style for day point markers on the share line chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointStyle {
    Solid,
    Outlined,
}

/// Styling bundle for one share line series (focus or distraction).
#[derive(Debug, Clone, Copy)]
struct ShareSeriesStyle {
    color: Hsla,
    dash: LineDash,
    point: PointStyle,
}

impl AppShell {
    /// Effective first weekday from the core snapshot locale, matching the calendar popover
    /// without inventing a fallback when regional settings are unavailable.
    pub(super) fn effective_first_weekday(&self) -> Option<Weekday> {
        self.day_snapshot()
            .and_then(|snapshot| snapshot.locale)
            .map(|locale| weekday_from_protocol(locale.first_day_of_week))
    }

    /// Loads the Week page when it opens (and ensures the shared category catalog is loaded for
    /// hierarchy ordering and category metadata).
    pub(super) fn open_week_page(&mut self, cx: &mut Context<Self>) {
        let categories_effect = self.rules.ensure_categories_loaded();
        self.run_rules_effect(categories_effect, cx);
        let Some(first_weekday) = self.effective_first_weekday() else {
            return;
        };
        let today = self.current_logical_today();
        let effect = self.week.open(today, first_weekday);
        self.run_week_effect(effect, cx);
    }

    /// Reacts to a `day` topic invalidation for `date`.
    pub(super) fn on_week_day_changed(&mut self, date: LocalDate, cx: &mut Context<Self>) {
        let visibility = self.week_visibility();
        let today = self.current_logical_today();
        let effect = self.week.on_day_changed(date, today, visibility);
        self.run_week_effect(effect, cx);
    }

    /// Reacts to rule or category mutations that may alter weekly category aggregates.
    pub(super) fn on_week_rules_mutated(&mut self, cx: &mut Context<Self>) {
        let visibility = self.week_visibility();
        let today = self.current_logical_today();
        let effect = self.week.on_rules_mutated(today, visibility);
        self.run_week_effect(effect, cx);
    }

    /// Reacts to `StateSnapshot` changes in logical date or regional first weekday.
    pub(super) fn on_week_snapshot_changed(
        &mut self,
        previous_today: LocalDate,
        new_today: LocalDate,
        first_weekday: Option<Weekday>,
        cx: &mut Context<Self>,
    ) {
        let visibility = self.week_visibility();
        let effect =
            self.week
                .on_snapshot_changed(previous_today, new_today, first_weekday, visibility);
        self.run_week_effect(effect, cx);
    }

    fn week_visibility(&self) -> PageVisibility {
        if self.page == Page::Week {
            PageVisibility::Visible
        } else {
            PageVisibility::Hidden
        }
    }

    fn select_previous_week(&mut self, cx: &mut Context<Self>) {
        let today = self.current_logical_today();
        let effect = self.week.previous_week(today);
        self.run_week_effect(effect, cx);
        cx.notify();
    }

    fn select_next_week(&mut self, cx: &mut Context<Self>) {
        let today = self.current_logical_today();
        let effect = self.week.next_week(today);
        self.run_week_effect(effect, cx);
        cx.notify();
    }

    fn select_this_week(&mut self, cx: &mut Context<Self>) {
        let today = self.current_logical_today();
        let effect = self.week.go_this_week(today);
        self.run_week_effect(effect, cx);
        cx.notify();
    }

    fn retry_week(&mut self, cx: &mut Context<Self>) {
        let today = self.current_logical_today();
        let effect = self.week.reload(today);
        self.run_week_effect(effect, cx);
        cx.notify();
    }

    fn run_week_effect(&mut self, effect: WeekEffect, cx: &mut Context<Self>) {
        let WeekEffect::Query {
            request,
            week_start,
            today,
            generation,
        } = effect
        else {
            return;
        };
        let Some(link) = self.live.as_ref().map(|live| live.link.clone()) else {
            let follow_up = self.week.apply_loaded(
                generation,
                week_start,
                today,
                Err(LoadDataError::from(RequestCoreError::Disconnected)),
                self.rules.categories(),
            );
            debug_assert_eq!(follow_up, WeekEffect::None);
            return;
        };
        let (progress_tx, mut progress_rx) = tokio::sync::watch::channel(None);
        cx.spawn(async move |this, cx| {
            while progress_rx.changed().await.is_ok() {
                let Some(progress) = *progress_rx.borrow_and_update() else {
                    continue;
                };
                let keep_listening = this
                    .update(cx, |this, cx| {
                        if this.week.apply_progress(generation, progress) == ProgressUpdate::Applied
                        {
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep_listening {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = link.query_range(request, progress_tx).await;
            let _ = this.update(cx, |this, cx| {
                let follow_up = this.week.apply_loaded(
                    generation,
                    week_start,
                    today,
                    result,
                    this.rules.categories(),
                );
                this.run_week_effect(follow_up, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Renders the Week page inside the main scroll area.
    pub(super) fn render_week(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let compact = frame.layout.class == WindowClass::Compact;
        let today = self.current_logical_today();
        let has_locale = self.effective_first_weekday().is_some();

        let body: AnyElement = if !has_locale {
            self.render_week_locale_unavailable(ui)
        } else {
            match self.week.state() {
                WeekLoadState::Loading => self.render_week_loading(ui),
                WeekLoadState::Failed(error) => self.render_week_failure(ui, error, cx),
                WeekLoadState::Ready(data) if data.is_empty() => {
                    self.render_week_empty(ui, today, cx)
                }
                WeekLoadState::Ready(data) => self.render_week_charts(ui, frame, data, today, cx),
            }
        };

        let content = div()
            .flex()
            .flex_col()
            .gap(space.s)
            .w_full()
            .max_w(ui.metrics.layout.max_content_width)
            .mx_auto()
            .px(if compact { space.l } else { space.xxl })
            .pt(if compact { space.l } else { space.xxxl })
            .pb(space.xxl)
            .child(self.render_week_header(ui, today, has_locale, cx))
            .child(body);

        div()
            .id("week-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(content)
    }

    fn render_week_header(
        &self,
        ui: &Widgets<'_>,
        today: LocalDate,
        has_locale: bool,
        cx: &Context<Self>,
    ) -> Div {
        let density = Density::Standard;
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let on_current_week = !has_locale || self.week.is_current_week(today);

        let range_text = match (self.week.week_start(), self.week.week_end_inclusive()) {
            (Some(start), Some(end)) => self.formatter.week_range(start, end),
            _ => self.formatter.text(Msg::NavWeek),
        };

        let prev_btn = if has_locale {
            ui.button(
                "week-prev",
                density,
                ButtonContent::Icon(IconName::ChevronLeft),
            )
            .on_click(cx.listener(|this, _, _, cx| this.select_previous_week(cx)))
        } else {
            ui.disabled_button(
                "week-prev",
                density,
                ButtonContent::Icon(IconName::ChevronLeft),
            )
        };

        let next_btn = if on_current_week {
            ui.disabled_button(
                "week-next",
                density,
                ButtonContent::Icon(IconName::ChevronRight),
            )
        } else {
            ui.button(
                "week-next",
                density,
                ButtonContent::Icon(IconName::ChevronRight),
            )
            .on_click(cx.listener(|this, _, _, cx| this.select_next_week(cx)))
        };

        let this_week_btn = if on_current_week {
            ui.disabled_button(
                "week-current",
                density,
                ButtonContent::Label(self.formatter.text(Msg::WeekThisWeek)),
            )
        } else {
            ui.button(
                "week-current",
                density,
                ButtonContent::Label(self.formatter.text(Msg::WeekThisWeek)),
            )
            .on_click(cx.listener(|this, _, _, cx| this.select_this_week(cx)))
        };

        let range_pill = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(space.s)
            .min_h(ui.metrics.density(density).control_height)
            .min_w(px(WEEK_RANGE_MIN_WIDTH))
            .px(space.m)
            .rounded(ui.metrics.radius.control)
            .bg(palette.fill_card)
            .border_1()
            .border_color(palette.stroke_card)
            .child(ui.icon(IconName::CalendarRange, density, palette.text_secondary))
            .child(
                div()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .ramp(ui, Ramp::BodyStrong)
                    .tabular_figures()
                    .text_color(palette.text_primary)
                    .child(range_text),
            );

        let total_summary: Option<AnyElement> = if !has_locale {
            None
        } else {
            match self.week.state() {
                WeekLoadState::Loading => Some(
                    ui.skeleton_block(SkeletonTone::Strong)
                        .w(px(SKELETON_TOTAL_WIDTH))
                        .h(px(SKELETON_TOTAL_HEIGHT))
                        .rounded(ui.metrics.radius.control)
                        .into_any_element(),
                ),
                WeekLoadState::Ready(data) => Some(
                    div()
                        .ramp(ui, Ramp::BodyStrong)
                        .tabular_figures()
                        .text_color(palette.text_secondary)
                        .child(self.formatter.text_with(
                            Msg::WeekTotal,
                            &[Arg::Duration.with(&self.formatter.duration(data.total_active))],
                        ))
                        .into_any_element(),
                ),
                WeekLoadState::Failed(_) => None,
            }
        };

        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(space.s)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(space.s)
                    .child(prev_btn)
                    .child(range_pill)
                    .child(next_btn)
                    .child(this_week_btn),
            )
            .children(total_summary)
    }

    fn render_week_charts(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        data: &WeekData,
        today: LocalDate,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let space = &ui.metrics.space;
        let truncation_notice = data.truncated.then(|| {
            div()
                .ramp(ui, Ramp::Caption)
                .text_color(ui.palette.text_secondary)
                .child(self.formatter.text(Msg::WeekTruncated))
        });

        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .children(truncation_notice)
            .child(self.render_week_columns_card(ui, frame, data, today))
            .child(self.render_week_shares_card(ui, frame, data, today, cx))
            .into_any_element()
    }

    fn render_week_columns_card(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        data: &WeekData,
        today: LocalDate,
    ) -> Div {
        let space = &ui.metrics.space;
        let scale = data.chart_scale;
        let gridlines = [
            ChartGridline {
                from_top: 0.0,
                label: self.formatter.duration(scale),
            },
            ChartGridline {
                from_top: 0.5,
                label: self.formatter.duration(scale / 2),
            },
            ChartGridline {
                from_top: 1.0,
                label: self.formatter.duration(Duration::ZERO),
            },
        ];
        let axis_width = AXIS_COLUMN_RANGE.select(frame.measure.widths(
            gridlines.iter().map(|line| line.label.as_str()),
            Ramp::Caption,
        ));

        let columns =
            div()
                .flex()
                .flex_row()
                .flex_1()
                .h_full()
                .children(data.days.iter().enumerate().map(|(index, day)| {
                    let accessible_label = self.week_day_accessible_label(day, data);
                    let segments = day.segments.iter().map(|segment| StackedSegment {
                        color: segment.color,
                        fraction: Share::of(segment.duration.min(scale), scale).fraction(),
                    });
                    render_stacked_column(
                        ui,
                        ("week-col", index),
                        accessible_label,
                        segments,
                        WEEK_COLUMNS_HEIGHT,
                        space.xs,
                    )
                }));

        let plot = ui
            .plot_area()
            .flex()
            .flex_col()
            .p(space.s)
            .child(
                div()
                    .relative()
                    .h(px(WEEK_COLUMNS_HEIGHT))
                    .children(gridlines.iter().flat_map(|line| {
                        render_gridline(ui, line, WEEK_COLUMNS_HEIGHT, axis_width)
                    }))
                    .child(
                        div()
                            .absolute()
                            .left(px(axis_width))
                            .right(px(0.))
                            .top(px(0.))
                            .h_full()
                            .flex()
                            .child(columns),
                    ),
            )
            .child(self.render_week_day_axis(ui, &data.days, today, axis_width));

        let legend = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_x(space.l)
            .gap_y(space.xs)
            .children(data.categories.iter().map(|category| {
                render_legend_item(
                    ui,
                    category.color,
                    self.formatter.text_with(
                        Msg::LegendItem,
                        &[
                            Arg::Name.with(&self.formatter.category_name(&category.name)),
                            Arg::Duration.with(&self.formatter.duration(category.total)),
                        ],
                    ),
                )
            }));

        ui.card()
            .flex()
            .flex_col()
            .w_full()
            .gap(space.s)
            .p(space.l)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(Msg::WeekColumnsTitle)),
            )
            .child(plot)
            .child(legend)
    }

    fn render_week_shares_card(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        data: &WeekData,
        today: LocalDate,
        cx: &mut Context<Self>,
    ) -> Div {
        let space = &ui.metrics.space;
        let palette = ui.palette;
        let (focus_style, distraction_style) = share_series_styles(ui);

        let gridlines: Vec<ChartGridline> = SHARE_AXIS_TICKS
            .into_iter()
            .map(|(from_top, pct)| ChartGridline {
                from_top,
                label: self
                    .formatter
                    .text_with(Msg::Percent, &[Arg::Value.with(&pct.to_string())]),
            })
            .collect();
        let axis_width = AXIS_COLUMN_RANGE.select(frame.measure.widths(
            gridlines.iter().map(|line| line.label.as_str()),
            Ramp::Caption,
        ));

        let mut focus_points: [Option<f32>; WEEK_DAY_COUNT] = [None; WEEK_DAY_COUNT];
        let mut distraction_points: [Option<f32>; WEEK_DAY_COUNT] = [None; WEEK_DAY_COUNT];
        for (slot, day) in focus_points.iter_mut().zip(data.days.iter()) {
            *slot = day.focus_share.map(Share::fraction);
        }
        for (slot, day) in distraction_points.iter_mut().zip(data.days.iter()) {
            *slot = day.distraction_share.map(Share::fraction);
        }

        let lines_canvas = share_lines_canvas(
            focus_points,
            focus_style,
            distraction_points,
            distraction_style,
        );
        let points_layer = self.bounds_layer(cx, move |layer| {
            let width = f32::from(layer.bounds.size.width);
            let height = f32::from(layer.bounds.size.height);
            layer.with_ui(|ui, _, _| {
                render_share_points(
                    ui,
                    width,
                    height,
                    focus_points,
                    focus_style,
                    distraction_points,
                    distraction_style,
                )
            })
        });

        let plot =
            ui.plot_area()
                .flex()
                .flex_col()
                .p(space.s)
                .child(
                    div()
                        .relative()
                        .h(px(WEEK_SHARES_HEIGHT))
                        .children(gridlines.iter().flat_map(|line| {
                            render_gridline(ui, line, WEEK_SHARES_HEIGHT, axis_width)
                        }))
                        .child(
                            div()
                                .absolute()
                                .left(px(axis_width))
                                .right(px(0.))
                                .top(px(0.))
                                .h_full()
                                .child(lines_canvas)
                                .child(points_layer),
                        ),
                )
                .child(self.render_week_day_axis(ui, &data.days, today, axis_width));

        let legend = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(space.l)
            .child(share_legend_item(
                ui,
                focus_style,
                self.formatter.text(Msg::StatFocus),
            ))
            .child(share_legend_item(
                ui,
                distraction_style,
                self.formatter.text(Msg::StatDistraction),
            ));

        ui.card()
            .flex()
            .flex_col()
            .w_full()
            .gap(space.s)
            .p(space.l)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_baseline()
                    .justify_between()
                    .gap(space.s)
                    .child(
                        div()
                            .ramp(ui, Ramp::Subtitle)
                            .text_color(palette.text_primary)
                            .child(self.formatter.text(Msg::WeekSharesTitle)),
                    )
                    .child(legend),
            )
            .child(plot)
    }

    fn render_week_day_axis(
        &self,
        ui: &Widgets<'_>,
        days: &[WeekDay],
        today: LocalDate,
        axis_width: f32,
    ) -> Div {
        let palette = ui.palette;
        let space = &ui.metrics.space;
        div()
            .flex()
            .flex_row()
            .ml(px(axis_width))
            .mt(space.xs)
            .children(days.iter().map(|day| {
                let primary_color = if day.date == today {
                    palette.text_primary
                } else if day.kind == WeekDayKind::Future {
                    palette.text_tertiary
                } else {
                    palette.text_secondary
                };
                let date_color = if day.kind == WeekDayKind::Future {
                    palette.text_tertiary
                } else {
                    palette.text_secondary
                };
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .px(space.xxs)
                    .child(ui.axis_label(self.formatter.weekday(day.date.weekday()), primary_color))
                    .child(ui.axis_label(self.formatter.month_day(day.date), date_color))
            }))
    }

    fn week_day_accessible_label(&self, day: &WeekDay, data: &WeekData) -> Option<String> {
        if day.kind == WeekDayKind::Future || day.active.is_zero() {
            return None;
        }
        let parts: Vec<String> = day
            .segments
            .iter()
            .filter_map(|segment| {
                let category = data.categories.get(segment.category)?;
                Some(self.formatter.text_with(
                    Msg::LegendItem,
                    &[
                        Arg::Name.with(&self.formatter.category_name(&category.name)),
                        Arg::Duration.with(&self.formatter.duration(segment.duration)),
                    ],
                ))
            })
            .collect();
        if parts.is_empty() {
            return None;
        }
        Some(format!(
            "{} · {}",
            self.formatter.date(day.date),
            parts.join("; ")
        ))
    }

    fn render_week_loading(&self, ui: &Widgets<'_>) -> AnyElement {
        let space = &ui.metrics.space;
        let progress_bar =
            super::progress::query_progress_view(ui, self.formatter, self.week.progress());

        let columns_skeleton = ui
            .card()
            .flex()
            .flex_col()
            .w_full()
            .gap(space.s)
            .p(space.l)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(Msg::WeekColumnsTitle)),
            )
            .child(
                ui.plot_area()
                    .flex()
                    .flex_row()
                    .items_end()
                    .justify_between()
                    .gap(space.m)
                    .h(px(WEEK_COLUMNS_HEIGHT))
                    .p(space.m)
                    .children(SKELETON_BAR_HEIGHTS.into_iter().map(|height| {
                        div()
                            .flex()
                            .flex_1()
                            .justify_center()
                            .items_end()
                            .h_full()
                            .child(
                                ui.skeleton_block(SkeletonTone::Strong)
                                    .w_full()
                                    .max_w(px(SKELETON_BAR_MAX_WIDTH))
                                    .h(px(height))
                                    .rounded_t(ui.metrics.timeline.block_radius),
                            )
                    })),
            )
            .child(
                ui.skeleton_block(SkeletonTone::Subtle)
                    .w(px(SKELETON_LEGEND_WIDTH))
                    .h(px(SKELETON_LEGEND_HEIGHT))
                    .rounded(ui.metrics.radius.control),
            );

        let shares_skeleton = ui
            .card()
            .flex()
            .flex_col()
            .w_full()
            .gap(space.s)
            .p(space.l)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(Msg::WeekSharesTitle)),
            )
            .child(
                ui.plot_area()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .h(px(WEEK_SHARES_HEIGHT))
                    .p(space.m)
                    .children((0..SHARE_AXIS_TICKS.len()).map(|_| {
                        ui.skeleton_block(SkeletonTone::Subtle)
                            .w_full()
                            .h(ui.metrics.layout.divider)
                    })),
            );

        div()
            .flex()
            .flex_col()
            .gap(space.s)
            .children(progress_bar)
            .child(columns_skeleton)
            .child(shares_skeleton)
            .into_any_element()
    }

    fn render_week_empty(
        &self,
        ui: &Widgets<'_>,
        today: LocalDate,
        cx: &Context<Self>,
    ) -> AnyElement {
        let is_current = self.week.is_current_week(today);
        let mut card = ui
            .card()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(ui.metrics.space.m)
            .p(ui.metrics.space.xxl)
            .min_h(px(EMPTY_CARD_MIN_HEIGHT))
            .child(
                div()
                    .size(px(EMPTY_TILE_SIZE))
                    .rounded(ui.metrics.radius.overlay)
                    .bg(ui.palette.fill_subtle_hover)
                    .border_1()
                    .border_color(ui.palette.stroke_card)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        ui.icon(
                            if is_current {
                                IconName::Clock
                            } else {
                                IconName::Calendar
                            },
                            Density::Standard,
                            ui.palette.text_secondary,
                        )
                        .size(px(EMPTY_ICON_SIZE)),
                    ),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(if is_current {
                        Msg::WeekEmptyCurrentTitle
                    } else {
                        Msg::WeekEmptyPastTitle
                    })),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(if is_current {
                        Msg::WeekEmptyCurrentMessage
                    } else {
                        Msg::WeekEmptyPastMessage
                    })),
            );
        if !is_current {
            card = card.child(
                ui.button(
                    "week-empty-go-current",
                    Density::Standard,
                    ButtonContent::Label(self.formatter.text(Msg::WeekGoThisWeek)),
                )
                .on_click(cx.listener(|this, _, _, cx| this.select_this_week(cx))),
            );
        }
        card.into_any_element()
    }

    fn render_week_failure(
        &self,
        ui: &Widgets<'_>,
        error: &LoadDataError,
        cx: &Context<Self>,
    ) -> AnyElement {
        ui.card()
            .flex()
            .flex_wrap()
            .gap(ui.metrics.space.m)
            .p(ui.metrics.space.l)
            .child(ui.icon(
                IconName::TriangleAlert,
                Density::Standard,
                ui.palette.status_caution,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .ramp(ui, Ramp::BodyStrong)
                            .child(self.formatter.text(Msg::WeekFailedTitle)),
                    )
                    .child(div().ramp(ui, Ramp::Body).child(self.formatter.text(
                        if error.is_busy() {
                            Msg::DayBusy
                        } else {
                            Msg::WeekFailedMessage
                        },
                    )))
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(ui.palette.text_tertiary)
                            .font(ui.mono_font())
                            .child(error.code().to_string()),
                    ),
            )
            .child(
                ui.button(
                    "retry-week",
                    Density::Standard,
                    ButtonContent::Label(self.formatter.text(Msg::RetryDay)),
                )
                .on_click(cx.listener(|this, _, _, cx| this.retry_week(cx))),
            )
            .into_any_element()
    }

    fn render_week_locale_unavailable(&self, ui: &Widgets<'_>) -> AnyElement {
        ui.card()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(ui.metrics.space.m)
            .p(ui.metrics.space.xxl)
            .min_h(px(EMPTY_CARD_MIN_HEIGHT))
            .child(
                div()
                    .size(px(EMPTY_TILE_SIZE))
                    .rounded(ui.metrics.radius.overlay)
                    .bg(ui.palette.fill_subtle_hover)
                    .border_1()
                    .border_color(ui.palette.stroke_card)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        ui.icon(
                            IconName::CalendarRange,
                            Density::Standard,
                            ui.palette.text_secondary,
                        )
                        .size(px(EMPTY_ICON_SIZE)),
                    ),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .text_color(ui.palette.text_primary)
                    .child(self.formatter.text(Msg::WeekLocaleUnavailableTitle)),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Body)
                    .text_color(ui.palette.text_secondary)
                    .child(self.formatter.text(Msg::WeekLocaleUnavailableMessage)),
            )
            .into_any_element()
    }
}

fn share_series_styles(ui: &Widgets<'_>) -> (ShareSeriesStyle, ShareSeriesStyle) {
    let palette = ui.palette;
    if palette.contrast == ContrastMode::High {
        (
            ShareSeriesStyle {
                color: palette.accent_fill,
                dash: LineDash::Solid,
                point: PointStyle::Solid,
            },
            ShareSeriesStyle {
                color: palette.text_primary,
                dash: LineDash::Dashed,
                point: PointStyle::Outlined,
            },
        )
    } else {
        (
            ShareSeriesStyle {
                color: palette.category(BuiltinCategory::Development.default_color()),
                dash: LineDash::Solid,
                point: PointStyle::Solid,
            },
            ShareSeriesStyle {
                color: palette.category(BuiltinCategory::Entertainment.default_color()),
                dash: LineDash::Solid,
                point: PointStyle::Outlined,
            },
        )
    }
}

fn share_legend_item(ui: &Widgets<'_>, style: ShareSeriesStyle, label: String) -> Div {
    let dot = match style.point {
        PointStyle::Solid => div()
            .size(px(SHARE_POINT_DIAMETER))
            .rounded_full()
            .bg(style.color),
        PointStyle::Outlined => div()
            .size(px(SHARE_POINT_DIAMETER))
            .rounded_full()
            .bg(ui.palette.bg_base)
            .border(px(SHARE_POINT_BORDER_WIDTH))
            .border_color(style.color),
    };
    let swatch = div()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .w(px(LEGEND_LINE_WIDTH))
        .h(px(SHARE_POINT_DIAMETER))
        .child(
            div()
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .h(px(LEGEND_LINE_HEIGHT))
                .bg(style.color),
        )
        .child(dot);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(ui.metrics.space.s)
        .ramp(ui, Ramp::Caption)
        .text_color(ui.palette.text_secondary)
        .child(swatch)
        .child(label)
}

fn share_lines_canvas(
    focus: [Option<f32>; WEEK_DAY_COUNT],
    focus_style: ShareSeriesStyle,
    distraction: [Option<f32>; WEEK_DAY_COUNT],
    distraction_style: ShareSeriesStyle,
) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            paint_share_series(window, bounds, focus, focus_style);
            paint_share_series(window, bounds, distraction, distraction_style);
        },
    )
    .absolute()
    .size_full()
}

fn paint_share_series(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    points: [Option<f32>; WEEK_DAY_COUNT],
    style: ShareSeriesStyle,
) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
        return;
    }

    let mut previous: Option<Point<Pixels>> = None;
    for (index, maybe_fraction) in points.into_iter().enumerate() {
        let Some(fraction) = maybe_fraction else {
            previous = None;
            continue;
        };
        let current = bounds.origin + share_point_offset(index, fraction, width, height);
        if let Some(start) = previous {
            paint_line_segment(window, start, current, style);
        }
        previous = Some(current);
    }
}

fn paint_line_segment(
    window: &mut Window,
    start: Point<Pixels>,
    end: Point<Pixels>,
    style: ShareSeriesStyle,
) {
    let mut path = PathBuilder::stroke(px(SHARE_LINE_STROKE_WIDTH));
    match style.dash {
        LineDash::Solid => {
            path.move_to(start);
            path.line_to(end);
        }
        LineDash::Dashed => {
            let x0 = f32::from(start.x);
            let y0 = f32::from(start.y);
            let dx = f32::from(end.x) - x0;
            let dy = f32::from(end.y) - y0;
            let distance = dx.hypot(dy);
            if distance <= f32::EPSILON {
                return;
            }
            let step = SHARE_DASH_LENGTH + SHARE_DASH_GAP;
            let mut offset = 0.;
            while offset < distance {
                let seg_end = (offset + SHARE_DASH_LENGTH).min(distance);
                let t0 = offset / distance;
                let t1 = seg_end / distance;
                path.move_to(point(px(x0 + dx * t0), px(y0 + dy * t0)));
                path.line_to(point(px(x0 + dx * t1), px(y0 + dy * t1)));
                offset += step;
            }
        }
    }
    match path.build() {
        Ok(built) => window.paint_path(built, style.color),
        Err(error) => tracing::warn!(%error, "could not draw week share line"),
    }
}

fn share_point_offset(index: usize, fraction: f32, width: f32, height: f32) -> Point<Pixels> {
    let day = f32::from(u8::try_from(index).expect("week day index fits in u8"));
    let x = ((day + 0.5) / WEEK_DAY_COUNT_F32) * width;
    let y = (1. - fraction.clamp(0., 1.)) * height;
    point(px(x), px(y))
}

fn render_share_points(
    ui: &Widgets<'_>,
    width: f32,
    height: f32,
    focus: [Option<f32>; WEEK_DAY_COUNT],
    focus_style: ShareSeriesStyle,
    distraction: [Option<f32>; WEEK_DAY_COUNT],
    distraction_style: ShareSeriesStyle,
) -> AnyElement {
    if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
        return div().size_full().into_any_element();
    }
    let bg_base = ui.palette.bg_base;
    let build_markers = |series: [Option<f32>; WEEK_DAY_COUNT], style: ShareSeriesStyle| {
        series
            .into_iter()
            .enumerate()
            .filter_map(move |(index, maybe_fraction)| {
                let fraction = maybe_fraction?;
                let offset = share_point_offset(index, fraction, width, height);
                let marker = div()
                    .absolute()
                    .left(offset.x - px(SHARE_POINT_RADIUS))
                    .top(offset.y - px(SHARE_POINT_RADIUS))
                    .size(px(SHARE_POINT_DIAMETER))
                    .rounded_full();
                Some(match style.point {
                    PointStyle::Solid => marker.bg(style.color),
                    PointStyle::Outlined => marker
                        .bg(bg_base)
                        .border(px(SHARE_POINT_BORDER_WIDTH))
                        .border_color(style.color),
                })
            })
    };
    div()
        .relative()
        .size_full()
        .children(build_markers(focus, focus_style))
        .children(build_markers(distraction, distraction_style))
        .into_any_element()
}
