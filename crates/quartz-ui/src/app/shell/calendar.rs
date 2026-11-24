//! A small CalendarDatePicker flyout: the component calendar does not expose day-cell styling.
use super::AppShell;
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, RampExt as _, Widgets},
    },
    format::{Arg, Msg},
};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, assets::IconName, deferred,
    div, px,
};
use jiff::{
    ToSpan as _,
    civil::{Date, Weekday},
};

/// Approved today-states.html CalendarDatePicker dimensions, in logical pixels.
const FLYOUT_WIDTH: f32 = 300.;
const HEADER_HEIGHT: f32 = 40.;
const WEEKDAY_HEIGHT: f32 = 28.;
const DAY_ROW_HEIGHT: f32 = 38.;
const DAY_CIRCLE_SIZE: f32 = 34.;
const TODAY_RING_WIDTH: f32 = 1.5;
/// Seven weekdays in the Gregorian calendar used by jiff.
const DAYS_PER_WEEK: i8 = 7;
/// Align the flyout below the date picker after the previous-day button (today-states.html).
const FLYOUT_LEFT: f32 = 40.;
#[derive(Debug, Clone, Copy)]
enum MonthDirection {
    Previous,
    Next,
}
#[derive(Debug, Clone, Copy)]
pub(super) struct CalendarMonth {
    first: Date,
}
impl CalendarMonth {
    pub(super) fn new(selected: Date) -> Self {
        Self {
            first: selected.first_of_month(),
        }
    }
    fn cells(self, start: Weekday) -> Vec<Option<Date>> {
        let leading = (self.first.weekday().to_monday_zero_offset()
            - start.to_monday_zero_offset())
        .rem_euclid(DAYS_PER_WEEK);
        let used = leading + self.first.days_in_month();
        let cells = (i16::from(used) + i16::from(DAYS_PER_WEEK) - 1) / i16::from(DAYS_PER_WEEK)
            * i16::from(DAYS_PER_WEEK);
        (0..cells)
            .map(|index| {
                let day = index - i16::from(leading) + 1;
                if day < 1 || day > i16::from(self.first.days_in_month()) {
                    None
                } else {
                    Some(
                        Date::new(
                            self.first.year(),
                            self.first.month(),
                            i8::try_from(day).expect("day within month"),
                        )
                        .expect("valid month day"),
                    )
                }
            })
            .collect()
    }
}
impl AppShell {
    pub(super) fn toggle_calendar(&mut self, cx: &mut Context<Self>) {
        if self
            .day_snapshot()
            .and_then(|snapshot| snapshot.locale)
            .is_none()
        {
            return;
        }
        self.calendar = match self.calendar {
            None => Some(CalendarMonth::new(self.day.date())),
            Some(_) => None,
        };
        self.hover = None;
        cx.notify();
    }
    fn move_calendar(&mut self, direction: MonthDirection, cx: &mut Context<Self>) {
        let Some(month) = self.calendar else {
            return;
        };
        let span = match direction {
            MonthDirection::Previous => (-1).months(),
            MonthDirection::Next => 1.months(),
        };
        if let Ok(date) = month.first.checked_add(span)
            && self
                .day_snapshot()
                .is_some_and(|snapshot| date <= snapshot.logical_date.first_of_month())
        {
            self.calendar = Some(CalendarMonth::new(date));
            cx.notify();
        }
    }
    pub(super) fn render_calendar(
        &self,
        ui: &Widgets<'_>,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let month = self.calendar?;
        let snapshot = self.day_snapshot()?;
        let locale = snapshot.locale?;
        let first = match locale.first_day_of_week {
            quartz_protocol::Weekday::Monday => Weekday::Monday,
            quartz_protocol::Weekday::Tuesday => Weekday::Tuesday,
            quartz_protocol::Weekday::Wednesday => Weekday::Wednesday,
            quartz_protocol::Weekday::Thursday => Weekday::Thursday,
            quartz_protocol::Weekday::Friday => Weekday::Friday,
            quartz_protocol::Weekday::Saturday => Weekday::Saturday,
            quartz_protocol::Weekday::Sunday => Weekday::Sunday,
        };
        let today = snapshot.logical_date;
        let selected = self.day.date();
        let next = if month.first >= today.first_of_month() {
            ui.disabled_button(
                "calendar-next",
                Density::Compact,
                ButtonContent::Icon(IconName::ChevronRight),
            )
        } else {
            ui.button(
                "calendar-next",
                Density::Compact,
                ButtonContent::Icon(IconName::ChevronRight),
            )
            .on_click(cx.listener(|this, _, _, cx| this.move_calendar(MonthDirection::Next, cx)))
        };
        let month_text = self.formatter.text_with(
            Msg::CalendarMonth,
            &[
                Arg::Month.with(
                    &self
                        .formatter
                        .text(Msg::month(month.first.month()).expect("month label")),
                ),
                Arg::Value.with(&month.first.year().to_string()),
            ],
        );
        let header = div()
            .h(px(HEADER_HEIGHT))
            .flex()
            .items_center()
            .gap(ui.metrics.space.xs)
            .px(ui.metrics.space.m)
            .border_b_1()
            .border_color(ui.palette.stroke_divider)
            .child(
                div()
                    .flex_1()
                    .ramp(ui, Ramp::BodyStrong)
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(month_text),
            )
            .child(
                ui.button(
                    "calendar-prev",
                    Density::Compact,
                    ButtonContent::Icon(IconName::ChevronLeft),
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.move_calendar(MonthDirection::Previous, cx)),
                ),
            )
            .child(next);
        let weekdays = div()
            .grid()
            .grid_cols(u16::try_from(DAYS_PER_WEEK).expect("positive weekday count"))
            .children((0..DAYS_PER_WEEK).map(|offset| {
                div()
                    .h(px(WEEKDAY_HEIGHT))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .flex()
                    .justify_center()
                    .items_center()
                    .ramp(ui, Ramp::Caption)
                    .text_color(ui.palette.text_secondary)
                    .child(
                        self.formatter
                            .weekday(first.wrapping_add(i64::from(offset))),
                    )
            }));
        let cells = month
            .cells(first)
            .into_iter()
            .enumerate()
            .map(|(index, date)| {
                let mut cell = div()
                    .id(("calendar-cell", index))
                    .h(px(DAY_ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center();
                if let Some(date) = date {
                    let mut circle = div()
                        .size(px(DAY_CIRCLE_SIZE))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .ramp(ui, Ramp::Body)
                        .child(date.day().to_string());
                    if date == selected {
                        circle = circle
                            .bg(ui.palette.accent_fill)
                            .text_color(ui.palette.text_on_accent);
                    } else if date == today {
                        circle = circle.border_color(ui.palette.accent_fill);
                        circle.style().border_widths.top = Some(px(TODAY_RING_WIDTH).into());
                        circle.style().border_widths.right = Some(px(TODAY_RING_WIDTH).into());
                        circle.style().border_widths.bottom = Some(px(TODAY_RING_WIDTH).into());
                        circle.style().border_widths.left = Some(px(TODAY_RING_WIDTH).into());
                    }
                    if date > today {
                        circle = circle.text_color(ui.palette.text_disabled);
                    } else {
                        cell =
                            cell.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_date(date, cx);
                                    this.calendar = None;
                                    cx.notify();
                                }));
                    }
                    cell = cell.child(circle);
                }
                cell
            });
        Some(
            deferred(
                ui.flyout()
                    .id("date-calendar")
                    .absolute()
                    .top(ui.metrics.density(Density::Standard).control_height + ui.metrics.space.xs)
                    .left(px(FLYOUT_LEFT))
                    .w(px(FLYOUT_WIDTH))
                    .rounded(ui.metrics.radius.overlay)
                    .shadow(ui.shadows(&ui.metrics.shadow.overlay))
                    .on_mouse_down_out(cx.listener(
                        |this, event: &gpui_kit::MouseDownEvent, _, cx| {
                            if event.button == MouseButton::Left {
                                this.calendar = None;
                                cx.notify();
                            }
                        },
                    ))
                    .child(header)
                    .child(
                        div().p(ui.metrics.space.m).child(weekdays).child(
                            div()
                                .grid()
                                .grid_cols(
                                    u16::try_from(DAYS_PER_WEEK).expect("positive weekday count"),
                                )
                                .gap_y(ui.metrics.space.xxs)
                                .children(cells),
                        ),
                    ),
            )
            .into_any_element(),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_placement_uses_named_dimensions_and_one_week_length() {
        let source = include_str!("calendar.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("implementation");
        assert!(!source.contains(".grid_cols(7)"));
        assert!(!source.contains(".left(px(40.))"));
        assert!(!source.contains(".gap_y(px(2.))"));
    }

    #[test]
    fn calendar_cells_remain_in_complete_seven_day_rows_for_every_week_start() {
        for year in [2024, 2026] {
            for month in 1..=12 {
                let month = CalendarMonth::new(Date::new(year, month, 1).expect("month"));
                for offset in 0..DAYS_PER_WEEK {
                    let first = Weekday::Monday.wrapping_add(i64::from(offset));
                    let cells = month.cells(first);
                    assert_eq!(
                        cells.len() % usize::try_from(DAYS_PER_WEEK).expect("week length"),
                        0
                    );
                    assert_eq!(
                        cells.iter().flatten().count(),
                        usize::try_from(month.first.days_in_month()).expect("days")
                    );
                    for date in cells.iter().flatten() {
                        assert_eq!(date.month(), month.first.month());
                    }
                }
            }
        }
    }

    #[test]
    fn calendar_grid_respects_monday_and_sunday_first_without_future_selection_data() {
        let month = CalendarMonth::new(Date::new(2026, 11, 21).expect("date"));
        let sunday = month.cells(Weekday::Sunday);
        let monday = month.cells(Weekday::Monday);
        assert_eq!(
            sunday.first().copied().flatten(),
            Some(Date::new(2026, 11, 1).expect("date"))
        );
        assert!(monday.iter().take(6).all(Option::is_none));
        assert_eq!(sunday.iter().flatten().count(), 30);
        assert_eq!(monday.iter().flatten().count(), 30);
    }
}
