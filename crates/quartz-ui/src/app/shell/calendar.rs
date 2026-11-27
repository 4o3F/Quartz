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
pub(super) enum MonthDirection {
    Previous,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CalendarTarget {
    TodayDate,
    SettingsDeleteStart,
    SettingsDeleteEnd,
}

impl CalendarTarget {
    const fn flyout_id(self) -> &'static str {
        match self {
            Self::TodayDate => "date-calendar",
            Self::SettingsDeleteStart => "settings-delete-start-calendar",
            Self::SettingsDeleteEnd => "settings-delete-end-calendar",
        }
    }

    const fn prev_id(self) -> &'static str {
        match self {
            Self::TodayDate => "calendar-prev",
            Self::SettingsDeleteStart => "settings-delete-start-calendar-prev",
            Self::SettingsDeleteEnd => "settings-delete-end-calendar-prev",
        }
    }

    const fn next_id(self) -> &'static str {
        match self {
            Self::TodayDate => "calendar-next",
            Self::SettingsDeleteStart => "settings-delete-start-calendar-next",
            Self::SettingsDeleteEnd => "settings-delete-end-calendar-next",
        }
    }

    const fn cell_prefix(self) -> &'static str {
        match self {
            Self::TodayDate => "calendar-cell",
            Self::SettingsDeleteStart => "settings-delete-start-cell",
            Self::SettingsDeleteEnd => "settings-delete-end-cell",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CalendarPopover {
    pub(super) month: CalendarMonth,
    pub(super) selected: Option<Date>,
    pub(super) today: Date,
    pub(super) max_selectable: Date,
    pub(super) first_weekday: Weekday,
    pub(super) left: f32,
    pub(super) target: CalendarTarget,
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

    pub(super) fn step(self, direction: MonthDirection, max_selectable: Date) -> Option<Self> {
        let span = match direction {
            MonthDirection::Previous => (-1).months(),
            MonthDirection::Next => 1.months(),
        };
        let date = self.first.checked_add(span).ok()?;
        (date <= max_selectable.first_of_month()).then(|| Self::new(date))
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

    fn move_calendar_target(
        &mut self,
        target: CalendarTarget,
        direction: MonthDirection,
        cx: &mut Context<Self>,
    ) {
        match target {
            CalendarTarget::TodayDate => {
                let Some(month) = self.calendar else {
                    return;
                };
                let Some(today) = self.day_snapshot().map(|snapshot| snapshot.logical_date) else {
                    return;
                };
                if let Some(next) = month.step(direction, today) {
                    self.calendar = Some(next);
                    cx.notify();
                }
            }
            CalendarTarget::SettingsDeleteStart | CalendarTarget::SettingsDeleteEnd => {
                self.move_settings_calendar(direction, cx);
            }
        }
    }

    fn select_calendar_date(&mut self, target: CalendarTarget, date: Date, cx: &mut Context<Self>) {
        match target {
            CalendarTarget::TodayDate => {
                self.select_date(date, cx);
                self.calendar = None;
                cx.notify();
            }
            CalendarTarget::SettingsDeleteStart => {
                self.select_settings_delete_date(super::settings::DeleteDateField::Start, date, cx);
            }
            CalendarTarget::SettingsDeleteEnd => {
                self.select_settings_delete_date(super::settings::DeleteDateField::End, date, cx);
            }
        }
    }

    fn dismiss_calendar(&mut self, target: CalendarTarget, cx: &mut Context<Self>) {
        match target {
            CalendarTarget::TodayDate => {
                self.calendar = None;
                cx.notify();
            }
            CalendarTarget::SettingsDeleteStart | CalendarTarget::SettingsDeleteEnd => {
                self.settings_calendar = None;
                cx.notify();
            }
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
        let first_weekday = crate::model::weekday_from_protocol(locale.first_day_of_week);
        let today = snapshot.logical_date;
        Some(self.render_calendar_popover(
            ui,
            CalendarPopover {
                month,
                selected: Some(self.day.date()),
                today,
                max_selectable: today,
                first_weekday,
                left: FLYOUT_LEFT,
                target: CalendarTarget::TodayDate,
            },
            cx,
        ))
    }

    pub(super) fn render_calendar_popover(
        &self,
        ui: &Widgets<'_>,
        popover: CalendarPopover,
        cx: &Context<Self>,
    ) -> AnyElement {
        let month = popover.month;
        let first = popover.first_weekday;
        let target = popover.target;
        let next = if month.first >= popover.max_selectable.first_of_month() {
            ui.disabled_button(
                target.next_id(),
                Density::Compact,
                ButtonContent::Icon(IconName::ChevronRight),
            )
        } else {
            ui.button(
                target.next_id(),
                Density::Compact,
                ButtonContent::Icon(IconName::ChevronRight),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.move_calendar_target(target, MonthDirection::Next, cx);
            }))
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
                    target.prev_id(),
                    Density::Compact,
                    ButtonContent::Icon(IconName::ChevronLeft),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.move_calendar_target(target, MonthDirection::Previous, cx);
                })),
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
        let cell_prefix = target.cell_prefix();
        let cells = month
            .cells(first)
            .into_iter()
            .enumerate()
            .map(|(index, date)| {
                let mut cell = div()
                    .id((cell_prefix, index))
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
                    if popover.selected == Some(date) {
                        circle = circle
                            .bg(ui.palette.accent_fill)
                            .text_color(ui.palette.text_on_accent);
                    } else if date == popover.today {
                        circle = circle.border_color(ui.palette.accent_fill);
                        circle.style().border_widths.top = Some(px(TODAY_RING_WIDTH).into());
                        circle.style().border_widths.right = Some(px(TODAY_RING_WIDTH).into());
                        circle.style().border_widths.bottom = Some(px(TODAY_RING_WIDTH).into());
                        circle.style().border_widths.left = Some(px(TODAY_RING_WIDTH).into());
                    }
                    if date > popover.max_selectable {
                        circle = circle.text_color(ui.palette.text_disabled);
                    } else {
                        cell =
                            cell.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_calendar_date(target, date, cx);
                                }));
                    }
                    cell = cell.child(circle);
                }
                cell
            });
        deferred(
            ui.flyout()
                .id(target.flyout_id())
                .absolute()
                .top(ui.metrics.density(Density::Standard).control_height + ui.metrics.space.xs)
                .left(px(popover.left))
                .w(px(FLYOUT_WIDTH))
                .rounded(ui.metrics.radius.overlay)
                .shadow(ui.shadows(&ui.metrics.shadow.overlay))
                .on_mouse_down_out(cx.listener(
                    move |this, event: &gpui_kit::MouseDownEvent, _, cx| {
                        if event.button == MouseButton::Left {
                            this.dismiss_calendar(target, cx);
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
        .into_any_element()
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
