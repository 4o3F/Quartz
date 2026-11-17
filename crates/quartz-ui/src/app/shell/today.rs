//! Today page: date bar, status card, timeline, hourly chart, rankings, pending away (07 section 3).

use std::time::Duration;

use gpui_kit::{
    Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px,
};

use super::AppShell;
use crate::app::{
    format::Formatter,
    model::Share,
    tokens::{Palette, metrics::MAX_CONTENT_WIDTH},
    widgets::{Density, Ramp, RampExt as _, icon, space},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Divider {
    Hidden,
    Visible,
}

impl AppShell {
    pub(super) fn render_today(
        &self,
        p: &Palette,
        main_width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let content = div()
            .flex()
            .flex_col()
            .gap(px(space::S))
            .w_full()
            .max_w(px(MAX_CONTENT_WIDTH))
            .mx_auto()
            .px(px(space::XXL))
            .pt(px(space::XXXL))
            .pb(px(space::XXL))
            .child(self.render_date_bar(p))
            .child(self.render_status(p))
            .child(self.render_timeline(p, main_width, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(space::S))
                    .child(self.render_hourly(p))
                    .child(self.render_ranking(p, cx)),
            )
            .children(self.render_pending(p));
        div()
            .id("today-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(content)
    }

    fn render_date_bar(&self, p: &Palette) -> impl IntoElement {
        let date = self.fmt.date(self.day.date);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(space::S))
            .child(p.button(
                "date-prev",
                Density::Standard,
                None,
                Some(IconName::ChevronLeft),
            ))
            .child(
                p.button(
                    "date-pick",
                    Density::Standard,
                    None,
                    Some(IconName::Calendar),
                )
                .w(px(200.))
                .justify_start()
                .child(div().flex_1().child(date))
                .child(icon(IconName::ChevronDown, 16., p.text_secondary)),
            )
            .child(p.button(
                "date-next",
                Density::Standard,
                None,
                Some(IconName::ChevronRight),
            ))
            .child(p.button(
                "date-today",
                Density::Standard,
                Some(self.fmt.text("today.today")),
                None,
            ))
    }

    fn render_status(&self, p: &Palette) -> impl IntoElement {
        let summary = self.day.summary();
        let fmt = self.fmt;
        let cells = [
            self.stat_cell(
                p,
                fmt.duration_value(p, summary.active, Ramp::Title, Ramp::BodyLarge),
                "today.stat.active",
                Divider::Hidden,
            ),
            self.stat_cell(
                p,
                fmt.percent_value(p, summary.focus),
                "today.stat.focus",
                Divider::Visible,
            ),
            self.stat_cell(
                p,
                fmt.percent_value(p, summary.distraction),
                "today.stat.distraction",
                Divider::Visible,
            ),
            self.stat_cell(
                p,
                (fmt)
                    .duration_value(p, summary.unclassified, Ramp::Subtitle, Ramp::Body)
                    .child(icon(IconName::ChevronRight, 14., p.text_secondary)),
                "today.stat.unclassified",
                Divider::Visible,
            ),
            self.stat_cell(
                p,
                fmt.duration_value(p, summary.longest_focus_streak, Ramp::Subtitle, Ramp::Body),
                "today.stat.longest_focus",
                Divider::Visible,
            ),
            self.stat_cell(
                p,
                fmt.switch_value(p, summary.switches_per_hour),
                "today.stat.switches",
                Divider::Visible,
            ),
        ];
        p.card()
            .flex()
            .flex_col()
            .gap(px(space::S))
            .px(px(space::L))
            .py(px(space::M))
            .child(div().flex().flex_row().children(cells))
            .child(self.render_share_bar(p))
    }

    fn stat_cell(&self, p: &Palette, value: Div, label_key: &str, divider: Divider) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .px(px(space::L))
            .when(divider == Divider::Hidden, |cell| cell.pl(px(0.)))
            .when(divider == Divider::Visible, |cell| {
                cell.border_l_1().border_color(p.stroke_divider)
            })
            .child(value)
            .child(
                div()
                    .ramp(Ramp::Caption)
                    .text_color(p.text_secondary)
                    .child(self.fmt.text(label_key)),
            )
    }
}

impl Formatter {
    /// Number and unit pairs in different sizes, such as `7` `小时` `5` `分`.
    fn duration_value(self, p: &Palette, duration: Duration, big: Ramp, small: Ramp) -> Div {
        let parts = crate::app::format::DurationParts::from_duration(duration);
        let pairs = parts
            .hours
            .map(|hours| (hours, self.hour_unit()))
            .into_iter()
            .chain(parts.minutes.map(|minutes| (minutes, self.minute_unit())));
        pairs.fold(
            div().flex().flex_row().items_baseline().gap(px(space::XXS)),
            |row, (value, unit)| {
                row.child(
                    div()
                        .ramp(big)
                        .text_color(p.text_primary)
                        .child(value.to_string()),
                )
                .child(
                    div()
                        .ramp(small)
                        .text_color(p.text_secondary)
                        .mr(px(space::XXS))
                        .child(unit),
                )
            },
        )
    }
    fn percent_value(self, p: &Palette, share: Share) -> Div {
        div()
            .flex()
            .flex_row()
            .items_baseline()
            .ramp(Ramp::Subtitle)
            .text_color(p.text_primary)
            .child(self.percent(share))
    }
    fn switch_value(self, p: &Palette, per_hour: f32) -> Div {
        let rounded = per_hour.round().max(0.).to_string();
        div()
            .flex()
            .flex_row()
            .items_baseline()
            .ramp(Ramp::Subtitle)
            .text_color(p.text_primary)
            .child(self.text_with("today.stat.switch_count", &[("count", &rounded)]))
    }
}
