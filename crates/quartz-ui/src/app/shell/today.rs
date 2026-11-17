//! Today page: date bar, status card, timeline, hourly chart, rankings, pending away (07 section 3).

use std::time::Duration;

use gpui_kit::{
    Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px,
};

use super::AppShell;
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, RampExt as _, Widgets},
    },
    format::{DurationParts, Formatter, Msg},
    model::{DaySummary, Share},
};

/// Width of the date picker button, wide enough for the longest date of either language.
const DATE_PICKER_WIDTH: f32 = 200.;

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

/// One cell of the status card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatCell {
    value: StatValue,
    label: Msg,
    /// Shown after the value of a cell that opens a list.
    trailing: Option<IconName>,
}

/// The status card cells in display order (07 section 3).
fn stat_cells(summary: &DaySummary) -> [StatCell; 6] {
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
        main_width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let space = &ui.metrics.space;
        let content = div()
            .flex()
            .flex_col()
            .gap(space.s)
            .w_full()
            .max_w(ui.metrics.layout.max_content_width)
            .mx_auto()
            .px(space.xxl)
            .pt(space.xxxl)
            .pb(space.xxl)
            .child(self.render_date_bar(ui))
            .child(self.render_status(ui))
            .child(self.render_timeline(ui, main_width, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(space.s)
                    .child(self.render_hourly(ui))
                    .child(self.render_ranking(ui, cx)),
            )
            .children(self.render_pending(ui));
        div()
            .id("today-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(content)
    }

    // The date buttons are inert until day navigation exists.
    fn render_date_bar(&self, ui: &Widgets<'_>) -> impl IntoElement {
        let density = Density::Standard;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui.metrics.space.s)
            .child(ui.button(
                "date-prev",
                density,
                ButtonContent::Icon(IconName::ChevronLeft),
            ))
            .child(
                ui.button(
                    "date-pick",
                    density,
                    ButtonContent::Icon(IconName::Calendar),
                )
                .w(px(DATE_PICKER_WIDTH))
                .justify_start()
                .child(div().flex_1().child(self.formatter.date(self.day.date())))
                .child(ui.icon(
                    IconName::ChevronDown,
                    density,
                    ui.palette.text_secondary,
                )),
            )
            .child(ui.button(
                "date-next",
                density,
                ButtonContent::Icon(IconName::ChevronRight),
            ))
            .child(ui.button(
                "date-today",
                density,
                ButtonContent::Label(self.formatter.text(Msg::TodayButton)),
            ))
    }

    fn render_status(&self, ui: &Widgets<'_>) -> impl IntoElement {
        let divider = ui.palette.stroke_divider;
        let cells = stat_cells(self.day.summary())
            .into_iter()
            .enumerate()
            .map(|(index, cell)| {
                let cell = self.stat_cell(ui, cell);
                if index == 0 {
                    cell.pl(px(0.))
                } else {
                    cell.border_l_1().border_color(divider)
                }
            });
        ui.card()
            .flex()
            .flex_col()
            .gap(ui.metrics.space.s)
            .px(ui.metrics.space.l)
            .py(ui.metrics.space.m)
            .child(div().flex().flex_row().children(cells))
            .child(self.render_share_bar(ui))
    }

    /// One status cell; the caller sets every cell but the first off with a divider.
    fn stat_cell(&self, ui: &Widgets<'_>, cell: StatCell) -> Div {
        let palette = ui.palette;
        let value = match cell.value {
            StatValue::Duration {
                duration,
                number,
                unit,
            } => duration_value(ui, self.formatter, duration, number, unit),
            StatValue::Percent(share) => text_value(ui, self.formatter.percent(share)),
            StatValue::SwitchesPerHour(rate) => text_value(ui, self.formatter.switches(rate)),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .px(ui.metrics.space.l)
            .child(value.when_some(cell.trailing, |value, icon| {
                value.child(ui.icon(icon, Density::Compact, palette.text_secondary))
            }))
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(self.formatter.text(cell.label)),
            )
    }
}

/// Number and unit pairs in different sizes, such as `7` `小时` `5` `分`.
fn duration_value(
    ui: &Widgets<'_>,
    formatter: Formatter,
    duration: Duration,
    number: Ramp,
    unit: Ramp,
) -> Div {
    let (hours, minutes) = (
        formatter.text(Msg::UnitHour),
        formatter.text(Msg::UnitMinute),
    );
    let pairs = match DurationParts::from_duration(duration) {
        DurationParts::Minutes(count) => vec![(count, minutes)],
        DurationParts::Hours(count) => vec![(count, hours)],
        DurationParts::HoursMinutes {
            hours: hour_count,
            minutes: minute_count,
        } => vec![(hour_count, hours), (minute_count, minutes)],
    };
    let row = div()
        .flex()
        .flex_row()
        .items_baseline()
        .gap(ui.metrics.space.xxs);
    row.children(pairs.into_iter().flat_map(|(count, label)| {
        [
            div()
                .ramp(ui, number)
                .text_color(ui.palette.text_primary)
                .child(count.to_string()),
            div()
                .ramp(ui, unit)
                .text_color(ui.palette.text_secondary)
                .mr(ui.metrics.space.xxs)
                .child(label),
        ]
    }))
}

/// A status value set as one subtitle string.
fn text_value(ui: &Widgets<'_>, text: String) -> Div {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .ramp(ui, Ramp::Subtitle)
        .text_color(ui.palette.text_primary)
        .child(text)
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
}
