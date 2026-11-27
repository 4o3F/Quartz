//! Rankings with tabs (07 section 3).

use std::time::Duration;

use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px, relative,
};

use super::{AppShell, Frame, Page};
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, PROPORTION_BAR_MIN_WIDTH, RampExt as _, Segment, Widgets},
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
/// Neutral icon tile size for the empty Documents ranking state.
const EMPTY_DOCUMENTS_TILE_SIZE: f32 = 40.;
/// Icon glyph size inside the empty Documents tile.
const EMPTY_DOCUMENTS_ICON_SIZE: f32 = 20.;

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

/// Distinguishes element ids between the Ranking card and the Websites card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RankRowScope {
    Ranking,
    Websites,
}

impl RankRowScope {
    const fn id_prefix(self) -> &'static str {
        match self {
            Self::Ranking => "rank",
            Self::Websites => "website-rank",
        }
    }
}

/// The parts of a ranking row that depend on the other rows or on measuring.
#[derive(Debug, Clone, PartialEq)]
struct RankCell {
    /// Card that owns this row, so GPUI element ids do not collide across cards.
    scope: RankRowScope,
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
                            scope: RankRowScope::Ranking,
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
                        || matches!(tab.kind, RankingKind::Apps | RankingKind::Documents)
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
        let truncated_notice = (self.day.ranking_truncated() && !rows.is_empty())
            .then(|| self.render_truncated_notice(ui, Msg::RankingTruncated, rows.len()));
        let content = if self.ranking == RankingKind::Documents && rows.is_empty() {
            self.render_empty_documents(ui, cx)
        } else {
            list
        };
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
                    .min_w(px(0.))
                    .items_center()
                    .gap(ui.metrics.space.s)
                    .child(
                        div()
                            .flex_auto()
                            .min_w(px(0.))
                            .ramp(ui, Ramp::Subtitle)
                            .child(self.formatter.text(Msg::RankingTitle)),
                    )
                    .child(tabs),
            )
            .children(truncated_notice)
            .child(content)
    }

    pub(super) fn render_websites(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let rows = self.day.websites();
        if rows.is_empty() {
            return None;
        }
        let longest = rows.iter().map(|row| row.total).max().unwrap_or_default();
        let durations: Vec<String> = rows
            .iter()
            .map(|row| self.formatter.duration(row.total))
            .collect();
        let value_width = VALUE_RANGE.select(
            frame
                .measure
                .widths(durations.iter().map(String::as_str), Ramp::Caption),
        );
        let list =
            div()
                .flex()
                .flex_col()
                .children(rows.iter().zip(durations).enumerate().map(
                    |(index, (row, duration))| {
                        let cell = RankCell {
                            scope: RankRowScope::Websites,
                            index,
                            duration,
                            value_width,
                        };
                        self.rank_row(ui, row, cell, RankBar::of(row, longest), cx)
                    },
                ));
        let truncated_notice = self
            .day
            .ranking_truncated()
            .then(|| self.render_truncated_notice(ui, Msg::RankingTruncated, rows.len()));
        Some(
            ui.card()
                .flex()
                .flex_col()
                .w_full()
                .gap(ui.metrics.space.s)
                .p(ui.metrics.space.l)
                .child(
                    div()
                        .min_w(px(0.))
                        .ramp(ui, Ramp::Subtitle)
                        .child(self.formatter.text(Msg::WebsitesTitle)),
                )
                .children(truncated_notice)
                .child(list),
        )
    }

    fn render_empty_documents(&self, ui: &Widgets<'_>, cx: &mut Context<Self>) -> Div {
        let palette = ui.palette;
        let space = &ui.metrics.space;
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(space.s)
            .px(space.m)
            .py(space.l)
            .child(
                div()
                    .size(px(EMPTY_DOCUMENTS_TILE_SIZE))
                    .rounded(ui.metrics.radius.overlay)
                    .bg(palette.fill_subtle_hover)
                    .border_1()
                    .border_color(palette.stroke_card)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        ui.icon(
                            IconName::FileText,
                            Density::Standard,
                            palette.text_secondary,
                        )
                        .size(px(EMPTY_DOCUMENTS_ICON_SIZE)),
                    ),
            )
            .child(
                div()
                    .ramp(ui, Ramp::BodyStrong)
                    .text_color(palette.text_primary)
                    .whitespace_normal()
                    .child(self.formatter.text(Msg::RankingDocumentsEmptyTitle)),
            )
            .child(
                div()
                    .ramp(ui, Ramp::Caption)
                    .text_color(palette.text_secondary)
                    .whitespace_normal()
                    .child(self.formatter.text(Msg::RankingDocumentsEmptyMessage)),
            )
            .child(
                ui.button(
                    "ranking-documents-open-rules",
                    Density::Compact,
                    ButtonContent::Label(self.formatter.text(Msg::RankingDocumentsEmptyAction)),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.set_page(Page::Rules, cx);
                })),
            )
    }

    fn document_pill(ui: &Widgets<'_>, text: String) -> Div {
        div()
            .px(ui.metrics.space.xs)
            .rounded(ui.metrics.radius.control)
            .bg(ui.palette.fill_control)
            .border_1()
            .border_color(ui.palette.stroke_control)
            .ramp(ui, Ramp::Caption)
            .text_color(ui.palette.text_secondary)
            .max_w_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(text)
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
        let name = self.formatter.rank_label(&row.label);
        let has_meta =
            row.meta.project.is_some() || row.meta.profile.is_some() || row.meta.app_name.is_some();
        let name_column = if has_meta {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .gap(space.xxs)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .gap(space.xs)
                        .min_w(px(0.))
                        .child(
                            div()
                                .min_w(px(0.))
                                .max_w_full()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(name),
                        )
                        .children(
                            row.meta
                                .project
                                .as_ref()
                                .map(|project| Self::document_pill(ui, project.clone())),
                        )
                        .children(
                            row.meta
                                .profile
                                .as_ref()
                                .map(|profile| Self::document_pill(ui, profile.clone())),
                        ),
                )
                .children(row.meta.app_name.as_ref().map(|app_name| {
                    div()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .ramp(ui, Ramp::Caption)
                        .text_color(palette.text_secondary)
                        .child(app_name.clone())
                }))
        } else {
            div()
                .min_w(px(0.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(name)
        };
        div()
            .id((cell.scope.id_prefix(), cell.index))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(space.m)
            .min_h(row_height)
            .px(space.s)
            .when(has_meta, |row| row.py(space.xxs))
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
                    .child(name_column),
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
