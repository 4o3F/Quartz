//! Today page: date bar, status card, timeline, hourly chart, rankings, pending away (07 section 3).

use std::time::Duration;

use gpui_kit::{
    Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px,
};

use super::{AppShell, Frame, stat_grid::StatGrid};
use crate::{
    app::{
        measure::TextMeasure,
        metrics::{Density, Ramp},
        widgets::{ButtonContent, RampExt as _, Widgets},
    },
    format::{DurationParts, Formatter, Msg},
    layout::ChartArrangement,
    model::{DaySummary, Share},
};

/// Number of cells in the status card (07 section 3).
pub(super) const STAT_CELL_COUNT: usize = 6;
/// Narrowest the date picker button gets; it grows with the formatted date (11 section 5.6).
const DATE_PICKER_MIN_WIDTH: f32 = 120.;
/// Widest the date picker button gets; a longer date ends with an ellipsis.
const DATE_PICKER_MAX_WIDTH: f32 = 240.;

/// What one cell of the status card shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatValue {
    /// Numbers in `number` type with their units in `unit` type.
    Duration {
        duration: Duration,
        number: Ramp,
        unit: Ramp,
    },
    Percent(Share),
    SwitchesPerHour(u64),
}

/// Which text of a status value a segment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentRole {
    Number,
    Unit,
}

/// One run of text of a status value with its type step.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatSegment {
    text: String,
    ramp: Ramp,
    role: SegmentRole,
}

impl StatValue {
    /// The text runs of the value, such as `7` `小时` `5` `分`.
    fn segments(self, formatter: Formatter) -> Vec<StatSegment> {
        let whole = |text: String, ramp| {
            vec![StatSegment {
                text,
                ramp,
                role: SegmentRole::Number,
            }]
        };
        match self {
            Self::Percent(share) => whole(formatter.percent(share), Ramp::Subtitle),
            Self::SwitchesPerHour(rate) => whole(formatter.switches(rate), Ramp::Subtitle),
            Self::Duration {
                duration,
                number,
                unit,
            } => {
                struct DurationRun {
                    count: u64,
                    label: String,
                }

                let hours = formatter.text(Msg::UnitHour);
                let minutes = formatter.text(Msg::UnitMinute);
                let runs = match DurationParts::from_duration(duration) {
                    DurationParts::Minutes(count) => vec![DurationRun {
                        count,
                        label: minutes,
                    }],
                    DurationParts::Hours(count) => vec![DurationRun {
                        count,
                        label: hours,
                    }],
                    DurationParts::HoursMinutes {
                        hours: hour_count,
                        minutes: minute_count,
                    } => vec![
                        DurationRun {
                            count: hour_count,
                            label: hours,
                        },
                        DurationRun {
                            count: minute_count,
                            label: minutes,
                        },
                    ],
                };
                runs.into_iter()
                    .flat_map(|run| {
                        [
                            StatSegment {
                                text: run.count.to_string(),
                                ramp: number,
                                role: SegmentRole::Number,
                            },
                            StatSegment {
                                text: run.label,
                                ramp: unit,
                                role: SegmentRole::Unit,
                            },
                        ]
                    })
                    .collect()
            }
        }
    }
}

/// One cell of the status card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatCell {
    value: StatValue,
    label: Msg,
    /// Shown after the value of a cell that opens a list.
    trailing: Option<IconName>,
}

/// The status card cells in display order (07 section 3).
fn stat_cells(summary: &DaySummary) -> [StatCell; STAT_CELL_COUNT] {
    let cell = |value, label| StatCell {
        value,
        label,
        trailing: None,
    };
    let duration = |duration, number, unit| StatValue::Duration {
        duration,
        number,
        unit,
    };
    [
        cell(
            duration(summary.active, Ramp::Title, Ramp::BodyLarge),
            Msg::StatActive,
        ),
        cell(StatValue::Percent(summary.focus), Msg::StatFocus),
        cell(
            StatValue::Percent(summary.distraction),
            Msg::StatDistraction,
        ),
        StatCell {
            trailing: Some(IconName::ChevronRight),
            ..cell(
                duration(summary.unclassified, Ramp::Subtitle, Ramp::Body),
                Msg::StatUnclassified,
            )
        },
        cell(
            duration(summary.longest_focus_streak, Ramp::Subtitle, Ramp::Body),
            Msg::StatLongestFocus,
        ),
        cell(
            StatValue::SwitchesPerHour(summary.switches_per_hour()),
            Msg::StatSwitches,
        ),
    ]
}

impl AppShell {
    pub(super) fn render_today(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let arrangement = frame.layout.class.chart_arrangement();
        let charts = div().flex().gap(space.s).map(|row| match arrangement {
            ChartArrangement::SideBySide => row.flex_row(),
            ChartArrangement::Stacked => row.flex_col(),
        });
        let content = div()
            .flex()
            .flex_col()
            .gap(space.s)
            .w_full()
            .max_w(ui.metrics.layout.max_content_width)
            .mx_auto()
            .px(
                if frame.layout.class == crate::layout::WindowClass::Compact {
                    space.l
                } else {
                    space.xxl
                },
            )
            .pt(
                if frame.layout.class == crate::layout::WindowClass::Compact {
                    space.l
                } else {
                    space.xxxl
                },
            )
            .pb(space.xxl)
            .child(self.render_date_bar(ui, cx))
            .children(self.stop_bar(ui, cx));
        let content = match self.live_notice(ui, frame, cx) {
            Some(notice) => content.child(notice),
            None => content
                .child(self.render_status(ui, frame.measure))
                .child(self.render_timeline(ui, frame, cx))
                .child(
                    charts
                        .child(self.render_hourly(ui, frame, cx))
                        .child(self.render_ranking(ui, frame, cx)),
                )
                .children(self.render_pending(ui, cx)),
        };
        div()
            .id("today-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(content)
    }

    fn render_date_bar(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> impl IntoElement {
        let density = Density::Standard;
        let on_today = self
            .day_snapshot()
            .is_none_or(|snapshot| self.day.date() >= snapshot.logical_date);
        div()
            .relative()
            .flex()
            .items_center()
            .gap(ui.metrics.space.s)
            .child(
                ui.button(
                    "date-prev",
                    density,
                    ButtonContent::Icon(IconName::ChevronLeft),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Ok(date) = this.day.date().yesterday() {
                        this.select_date(date, cx);
                    }
                })),
            )
            .child(
                (if self
                    .live
                    .as_ref()
                    .is_some_and(|live| live.snapshot.locale.is_none())
                {
                    ui.disabled_button(
                        "date-pick",
                        density,
                        ButtonContent::IconLabel(
                            IconName::Calendar,
                            self.formatter.date(self.day.date()),
                        ),
                    )
                } else {
                    ui.button(
                        "date-pick",
                        density,
                        ButtonContent::IconLabel(
                            IconName::Calendar,
                            self.formatter.date(self.day.date()),
                        ),
                    )
                })
                .min_w(px(DATE_PICKER_MIN_WIDTH))
                .max_w(px(DATE_PICKER_MAX_WIDTH))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_calendar(cx))),
            )
            .child(
                (if on_today {
                    ui.disabled_button(
                        "date-next",
                        density,
                        ButtonContent::Icon(IconName::ChevronRight),
                    )
                } else {
                    ui.button(
                        "date-next",
                        density,
                        ButtonContent::Icon(IconName::ChevronRight),
                    )
                })
                .text_color(if on_today {
                    ui.palette.text_disabled
                } else {
                    ui.palette.text_primary
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(live) = &this.live
                        && this.day.date() < live.snapshot.logical_date
                        && let Ok(date) = this.day.date().tomorrow()
                    {
                        this.select_date(date, cx);
                    }
                })),
            )
            .child(
                (if on_today {
                    ui.disabled_button(
                        "date-today",
                        density,
                        ButtonContent::Label(self.formatter.text(Msg::TodayButton)),
                    )
                } else {
                    ui.button(
                        "date-today",
                        density,
                        ButtonContent::Label(self.formatter.text(Msg::TodayButton)),
                    )
                })
                .text_color(if on_today {
                    ui.palette.text_disabled
                } else {
                    ui.palette.text_primary
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(live) = &this.live {
                        this.select_date(live.snapshot.logical_date, cx);
                    }
                })),
            )
            .children(
                self.live
                    .as_ref()
                    .filter(|live| live.snapshot.locale.is_none())
                    .map(|_| {
                        div()
                            .ramp(ui, Ramp::Caption)
                            .text_color(ui.palette.text_tertiary)
                            .child(self.formatter.text(Msg::LocaleUnavailable))
                    }),
            )
            .children(self.refresh_indicator(ui))
            .children(self.render_calendar(ui, cx))
    }

    /// The cells in equal columns: the largest of 6, 3, 2 or 1 columns in which the widest cell
    /// still fits the width the card gives the grid (07 section 2.1).
    fn render_status(&self, ui: &Widgets<'_>, measure: &TextMeasure<'_>) -> impl IntoElement {
        let views: [StatCellView; STAT_CELL_COUNT] = {
            let mut cells = stat_cells(self.day.summary());
            if let Some(cell) = cells.last_mut() {
                cell.value = StatValue::SwitchesPerHour(self.day.switches_per_hour());
            }
            cells.map(|cell| self.stat_cell(ui, measure, cell))
        };
        let widest = views
            .iter()
            .fold(0., |widest, view| f32::max(widest, view.width));
        let height = views
            .iter()
            .fold(px(0.), |height, view| height.max(view.height));
        let cells = views.map(|view| view.element.into_any_element());
        ui.card()
            .flex()
            .flex_col()
            .gap(ui.metrics.space.s)
            .px(ui.metrics.space.l)
            .py(ui.metrics.space.m)
            .child(StatGrid::new(
                cells,
                widest,
                height,
                ui.metrics.space.l,
                ui.metrics.space.s,
            ))
            .child(self.render_share_bar(ui))
    }

    /// One status cell with its content width and height.
    fn stat_cell(
        &self,
        ui: &Widgets<'_>,
        measure: &TextMeasure<'_>,
        cell: StatCell,
    ) -> StatCellView {
        let palette = ui.palette;
        let segments = cell.value.segments(self.formatter);
        let gap = f32::from(ui.metrics.space.xxs);
        let value_width: f32 = segments
            .iter()
            .map(|segment| measure.width(&segment.text, segment.ramp) + gap)
            .sum();
        let trailing_width = cell.trailing.map_or(0., |_| {
            f32::from(ui.metrics.density(Density::Compact).icon_size)
        });
        let label = self.formatter.text(cell.label);
        let width = (value_width + trailing_width).max(measure.width(&label, Ramp::Caption));
        // Baseline alignment of mixed sizes can add a few pixels above the tallest line.
        let value_height = segments.iter().fold(px(0.), |height, segment| {
            height.max(ui.line_height(segment.ramp))
        }) + ui.metrics.space.xs;
        let height = value_height + ui.line_height(Ramp::Caption);
        let value = div()
            .flex()
            .flex_row()
            .min_w(px(0.))
            .overflow_hidden()
            .items_baseline()
            .gap(ui.metrics.space.xxs)
            .children(segments.into_iter().map(|segment| {
                let color = match segment.role {
                    SegmentRole::Number => palette.text_primary,
                    SegmentRole::Unit => palette.text_secondary,
                };
                div()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .ramp(ui, segment.ramp)
                    .tabular_figures()
                    .text_color(color)
                    .child(segment.text)
            }));
        let element = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .child(value.when_some(cell.trailing, |value, icon| {
                value.child(ui.icon(icon, Density::Compact, palette.text_secondary))
            }))
            .child(
                div()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(label),
            );
        StatCellView {
            element,
            width,
            height,
        }
    }
}

/// A status cell ready to place, with the room its content needs.
struct StatCellView {
    element: Div,
    width: f32,
    height: Pixels,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_unclassified_cell_opens_a_list() {
        let cells = stat_cells(&DaySummary::default());
        let opening: Vec<Msg> = cells
            .iter()
            .filter(|cell| cell.trailing.is_some())
            .map(|cell| cell.label)
            .collect();
        assert_eq!(opening, [Msg::StatUnclassified]);
        assert_eq!(cells.first().map(|cell| cell.label), Some(Msg::StatActive));
    }

    #[test]
    fn duration_values_alternate_numbers_and_units_and_text_values_are_one_run() {
        let formatter = Formatter::new(quartz_i18n::Language::English);
        let value = StatValue::Duration {
            duration: Duration::from_secs(7 * 3600 + 5 * 60),
            number: Ramp::Title,
            unit: Ramp::BodyLarge,
        };
        let roles: Vec<SegmentRole> = value
            .segments(formatter)
            .iter()
            .map(|segment| segment.role)
            .collect();
        assert_eq!(
            roles,
            [
                SegmentRole::Number,
                SegmentRole::Unit,
                SegmentRole::Number,
                SegmentRole::Unit
            ]
        );
        let percent = StatValue::Percent(Share::of(Duration::from_secs(1), Duration::from_secs(2)));
        assert_eq!(percent.segments(formatter).len(), 1);
    }
}
