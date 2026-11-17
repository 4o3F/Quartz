//! Rankings with tabs (07 section 3).

use gpui_kit::{
    ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px, relative,
};

use super::AppShell;
use crate::app::{
    model::{DetailsSubject, RankingKind},
    tokens::Palette,
    widgets::{RADIUS_CONTROL, Ramp, RampExt as _, icon, space},
};

const ROW_COUNT: usize = 5;
const ROW_HEIGHT: f32 = 32.;
const BAR_HEIGHT: f32 = 6.;
const VALUE_WIDTH: f32 = 84.;
const NAME_WIDTH: f32 = 150.;

impl AppShell {
    pub(super) fn render_ranking(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.day.ranking(self.ranking);
        let max = rows
            .first()
            .map_or(1., |row| row.total.as_secs_f32().max(1.));
        let mut list = div().flex().flex_col();
        for (index, row) in rows.into_iter().take(ROW_COUNT).enumerate() {
            let selected =
                self.details == Some(row.subject) && matches!(row.subject, DetailsSubject::App(_));
            let dominant = row
                .parts
                .iter()
                .max_by_key(|(_, total)| *total)
                .map_or(crate::app::model::CategoryId(0), |(id, _)| *id);
            let accent = p.category(self.day.category(dominant).color);
            let hover = p.fill_subtle_hover;
            let subject = row.subject;
            let mut bar = div()
                .flex()
                .flex_row()
                .h_full()
                .w(relative(row.total.as_secs_f32() / max))
                .rounded(px(BAR_HEIGHT / 2.))
                .overflow_hidden();
            for (category, total) in &row.parts {
                let color = p.category(self.day.category(*category).color);
                bar = bar.child(
                    div()
                        .h_full()
                        .w(relative(
                            total.as_secs_f32() / row.total.as_secs_f32().max(1.),
                        ))
                        .bg(color),
                );
            }
            list = list.child(
                div()
                    .id(("rank", index))
                    .relative()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::M))
                    .h(px(ROW_HEIGHT))
                    .px(px(space::S))
                    .rounded(px(RADIUS_CONTROL))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(p.fill_subtle_hover))
                    .when(!selected, |row| row.hover(move |style| style.bg(hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select(subject, cx);
                    }))
                    .child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .top(px((ROW_HEIGHT - 16.) / 2.))
                            .w(px(3.))
                            .h(px(16.))
                            .rounded_full()
                            .bg(accent),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(space::S))
                            .w(px(NAME_WIDTH))
                            .flex_shrink_0()
                            .ml(px(space::XS))
                            .child(icon(row.glyph.icon_name(), 16., p.text_secondary))
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
                            .rounded(px(BAR_HEIGHT / 2.))
                            .bg(p.fill_subtle_hover)
                            .child(bar),
                    )
                    .child(
                        div()
                            .w(px(VALUE_WIDTH))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .ramp(Ramp::Caption)
                            .text_color(p.text_secondary)
                            .child(self.fmt.duration(row.total)),
                    ),
            );
        }
        let tabs = p.segmented(
            "ranking-tabs",
            vec![
                (self.fmt.text("today.ranking.apps"), RankingKind::Apps),
                (self.fmt.text("today.ranking.domains"), RankingKind::Domains),
                (
                    self.fmt.text("today.ranking.documents"),
                    RankingKind::Documents,
                ),
                (self.fmt.text("today.ranking.mixed"), RankingKind::Mixed),
            ],
            self.ranking,
            |kind| {
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.ranking = kind;
                    cx.notify();
                })
            },
        );
        p.card()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .gap(px(space::S))
            .p(px(space::L))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .ramp(Ramp::Subtitle)
                            .child(self.fmt.text("today.ranking.title")),
                    )
                    .child(tabs),
            )
            .child(list)
    }
}
