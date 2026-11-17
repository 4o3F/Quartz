//! Rankings with tabs (07 section 3).

use std::time::Duration;

use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px, relative,
};

use super::AppShell;
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{RampExt as _, Segment, Widgets},
    },
    format::Msg,
    model::{CategoryId, RankKey, RankRow, RankingKind, Share},
};

/// Rows shown per tab.
const RANKING_ROWS: usize = 5;
/// Height of the proportion bar.
const BAR_HEIGHT: f32 = 6.;
/// Width of the duration column.
const VALUE_WIDTH: f32 = 84.;
/// Width of the name column.
const NAME_WIDTH: f32 = 150.;

/// The proportion bar of one row: its width relative to the longest row and its category parts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RankBar {
    width: Share,
    parts: Vec<(CategoryId, Share)>,
}

impl RankBar {
    fn of(row: &RankRow, longest: Duration) -> Self {
        Self {
            width: Share::of(row.total, longest),
            parts: row
                .parts
                .iter()
                .map(|part| (part.category, Share::of(part.total, row.total)))
                .collect(),
        }
    }
}

/// Ranking tabs in display order with their element ids.
const TABS: [(RankingKind, &str); 4] = [
    (RankingKind::Apps, "ranking-apps"),
    (RankingKind::Domains, "ranking-domains"),
    (RankingKind::Documents, "ranking-documents"),
    (RankingKind::Mixed, "ranking-mixed"),
];

impl AppShell {
    pub(super) fn render_ranking(
        &self,
        ui: &Widgets<'_>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rows = self.day.ranking(self.ranking);
        let longest = rows.iter().map(|row| row.total).max().unwrap_or_default();
        let list = div().flex().flex_col().children(
            rows.iter()
                .take(RANKING_ROWS)
                .enumerate()
                .map(|(index, row)| self.rank_row(ui, index, row, RankBar::of(row, longest), cx)),
        );
        let tabs = ui.segmented(
            "ranking-tabs",
            TABS.map(|(kind, id)| Segment {
                id,
                label: self.formatter.ranking_tab(kind),
                value: kind,
            }),
            self.ranking,
            |kind| {
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.ranking = kind;
                    cx.notify();
                })
            },
        );
        ui.card()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .gap(ui.metrics.space.s)
            .p(ui.metrics.space.l)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .ramp(ui, Ramp::Subtitle)
                            .child(self.formatter.text(Msg::RankingTitle)),
                    )
                    .child(tabs),
            )
            .child(list)
    }

    fn rank_row(
        &self,
        ui: &Widgets<'_>,
        index: usize,
        row: &RankRow,
        bar: RankBar,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let palette = ui.palette;
        let space = &ui.metrics.space;
        let row_height = ui.metrics.density(Density::Compact).row_height;
        let subject = row.key.details_subject();
        let selected = subject.is_some() && self.details_subject() == subject;
        let accent = palette.category(self.day.category(row.dominant_category()).color);
        let icon = match &row.key {
            RankKey::App(app) => IconName::from(self.day.app(*app).glyph),
            RankKey::Domain(_) => IconName::Globe,
            RankKey::Document(_) => IconName::FileText,
        };
        div()
            .id(("rank", index))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(space.m)
            .h(row_height)
            .px(space.s)
            .rounded(ui.metrics.radius.control)
            .when(selected, |row| row.bg(palette.fill_subtle_hover))
            .when(!selected, |row| row.hover(ui.subtle_hover()))
            .when_some(subject, |row, subject| {
                row.cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select(subject, cx);
                    }))
            })
            .child(ui.indicator_bar(accent, row_height))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(space.s)
                    .w(px(NAME_WIDTH))
                    .flex_shrink_0()
                    .ml(space.xs)
                    .child(ui.icon(icon, Density::Compact, palette.text_secondary))
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(row.name.clone()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .h(px(BAR_HEIGHT))
                    .rounded_full()
                    .bg(palette.fill_subtle_hover)
                    .child(self.proportion_bar(ui, &bar)),
            )
            .child(
                div()
                    .w(px(VALUE_WIDTH))
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .child(self.formatter.duration(row.total)),
            )
    }

    fn proportion_bar(&self, ui: &Widgets<'_>, bar: &RankBar) -> Div {
        div()
            .flex()
            .flex_row()
            .h_full()
            .w(relative(bar.width.fraction()))
            .rounded_full()
            .overflow_hidden()
            .children(bar.parts.iter().map(|(category, share)| {
                div()
                    .h_full()
                    .w(relative(share.fraction()))
                    .bg(ui.palette.category(self.day.category(*category).color))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixture::{BROWSER, EDITOR, PLAY, WORK, day, segment};

    #[test]
    fn bars_are_relative_to_the_longest_row_and_split_by_category() {
        let day = day(
            vec![
                segment(1, EDITOR, WORK, (9, 0), (10, 0)),
                segment(2, BROWSER, WORK, (10, 0), (10, 10)),
                segment(3, BROWSER, PLAY, (10, 10), (10, 30)),
            ],
            vec![],
        );
        let rows = day.ranking(RankingKind::Apps);
        let browser = rows.get(1).expect("browser row");
        let bar = RankBar::of(browser, Duration::from_secs(60 * 60));
        assert!((bar.width.fraction() - 0.5).abs() < f32::EPSILON);
        let parts: Vec<(CategoryId, u32)> = bar
            .parts
            .iter()
            .map(|(category, share)| (*category, share.percent()))
            .collect();
        assert_eq!(parts, [(WORK, 33), (PLAY, 67)]);
    }
}
