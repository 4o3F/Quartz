//! Rankings with tabs (07 section 3).

use std::time::Duration;

use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px, relative,
};

use super::{AppShell, Frame};
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{PROPORTION_BAR_MIN_WIDTH, RampExt as _, Segment, Widgets},
    },
    format::Msg,
    layout::{ChartArrangement, ColumnRange},
    model::{CategoryId, RankKey, RankRow, RankingKind, Share},
};

/// Rows shown per tab.
const RANKING_ROWS: usize = 5;
/// Height of the proportion bar.
const BAR_HEIGHT: f32 = 6.;
/// The duration column: as wide as its widest duration, within this range (11 section 5.6).
const VALUE_RANGE: ColumnRange = ColumnRange::new(32., 96.);

/// One category slice of a ranking proportion bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RankBarPart {
    category: CategoryId,
    share: Share,
}

/// The proportion bar of one row: its width relative to the longest row and its category parts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RankBar {
    width: Share,
    parts: Vec<RankBarPart>,
}

impl RankBar {
    fn of(row: &RankRow, longest: Duration) -> Self {
        Self {
            width: Share::of(row.total, longest),
            parts: row
                .parts
                .iter()
                .map(|part| RankBarPart {
                    category: part.category,
                    share: Share::of(part.total, row.total),
                })
                .collect(),
        }
    }
}

/// The parts of a ranking row that depend on the other rows or on measuring.
#[derive(Debug, Clone, PartialEq)]
struct RankCell {
    /// Position of the row, for its element id.
    index: usize,
    /// The formatted total.
    duration: String,
    /// The width of the duration column, shared by every row.
    value_width: f32,
}

/// One ranking tab in display order with its element id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RankingTab {
    kind: RankingKind,
    id: &'static str,
}

/// Ranking tabs in display order with their element ids.
const TABS: [RankingTab; 4] = [
    RankingTab {
        kind: RankingKind::Apps,
        id: "ranking-apps",
    },
    RankingTab {
        kind: RankingKind::Domains,
        id: "ranking-domains",
    },
    RankingTab {
        kind: RankingKind::Documents,
        id: "ranking-documents",
    },
    RankingTab {
        kind: RankingKind::Mixed,
        id: "ranking-mixed",
    },
];

impl AppShell {
    pub(super) fn render_ranking(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rows = self.day.ranking(self.ranking);
        let shown = || {
            rows.iter()
                .take(if !matches!(self.day.source(), super::DaySource::Fixture) {
                    rows.len()
                } else {
                    RANKING_ROWS
                })
        };
        let longest = rows.iter().map(|row| row.total).max().unwrap_or_default();
        let durations: Vec<String> = shown()
            .map(|row| self.formatter.duration(row.total))
            .collect();
        let value_width = VALUE_RANGE.select(
            frame
                .measure
                .widths(durations.iter().map(String::as_str), Ramp::Caption),
        );
        let list = div()
            .flex()
            .flex_col()
            .children(
                shown()
                    .zip(durations)
                    .enumerate()
                    .map(|(index, (row, duration))| {
                        let cell = RankCell {
                            index,
                            duration,
                            value_width,
                        };
                        self.rank_row(ui, row, cell, RankBar::of(row, longest), cx)
                    }),
            );
        let tabs = ui.segmented(
            "ranking-tabs",
            TABS.into_iter()
                .filter(|tab| {
                    matches!(self.day.source(), super::DaySource::Fixture)
                        || tab.kind == RankingKind::Apps
                })
                .map(|tab| Segment {
                    id: tab.id,
                    label: self.formatter.ranking_tab(tab.kind),
                    value: tab.kind,
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
            .map(|card| match frame.layout.class.chart_arrangement() {
                ChartArrangement::SideBySide => card.flex_1().min_w(px(0.)),
                ChartArrangement::Stacked => card.w_full(),
            })
            .gap(ui.metrics.space.s)
            .p(ui.metrics.space.l)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(ui.metrics.space.s)
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
        row: &RankRow,
        cell: RankCell,
        bar: RankBar,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let palette = ui.palette;
        let space = &ui.metrics.space;
        let row_height = ui.metrics.density(Density::Compact).row_height;
        let subject = row.key.details_subject();
        let selected = subject.is_some() && self.details_subject() == subject;
        let accent = row
            .dominant_category()
            .map_or(palette.text_tertiary, |category| {
                palette.category(self.day.category(category).color)
            });
        let icon = match &row.key {
            RankKey::App(app) => IconName::from(self.day.app(*app).glyph),
            RankKey::Domain(_) => IconName::Globe,
            RankKey::Document(_) => IconName::FileText,
        };
        div()
            .id(("rank", cell.index))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(space.m)
            .min_h(row_height)
            .px(space.s)
            .rounded(ui.metrics.radius.control)
            .when(selected, |row| row.bg(palette.fill_subtle_hover))
            .when(!selected, |row| row.hover(ui.subtle_hover()))
            .when_some(subject, |row, subject| {
                row.cursor_pointer().on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.select(subject, window, cx);
                    },
                ))
            })
            .child(ui.indicator_bar(accent, row_height))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w(px(0.))
                    .items_center()
                    .gap(space.s)
                    .ml(space.xs)
                    .child(div().flex_shrink_0().child(ui.icon(
                        icon,
                        Density::Compact,
                        palette.text_secondary,
                    )))
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
                    .min_w(px(PROPORTION_BAR_MIN_WIDTH))
                    .h(
                        if palette.contrast == crate::app::tokens::ContrastMode::High {
                            ui.metrics.space.s
                        } else {
                            px(BAR_HEIGHT)
                        },
                    )
                    .rounded_full()
                    .bg(palette.fill_subtle_hover)
                    .child(self.proportion_bar(ui, &bar)),
            )
            .child(
                div()
                    .w(px(cell.value_width))
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .ramp(ui, Ramp::Caption)
                    .tabular_figures()
                    .text_color(palette.text_secondary)
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(cell.duration),
                    ),
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
            .bg(ui.palette.text_tertiary)
            .children(bar.parts.iter().map(|part| {
                ui.category_mark(
                    div().h_full().w(relative(part.share.fraction())),
                    self.day.category(part.category).color,
                )
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixture::{BROWSER, EDITOR, PLAY, WORK, day, segment};

    #[test]
    fn bars_are_relative_to_the_longest_row_and_split_by_category() {
        #[derive(Debug, PartialEq, Eq)]
        struct CategoryPercent {
            category: CategoryId,
            percent: u32,
        }

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
        let parts: Vec<CategoryPercent> = bar
            .parts
            .iter()
            .map(|part| CategoryPercent {
                category: part.category,
                percent: part.share.percent(),
            })
            .collect();
        assert_eq!(
            parts,
            [
                CategoryPercent {
                    category: WORK,
                    percent: 33,
                },
                CategoryPercent {
                    category: PLAY,
                    percent: 67,
                },
            ]
        );
    }
}
