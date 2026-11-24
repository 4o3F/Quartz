//! Live sidebar selection, cancellation and rendering.
use super::{AppShell, OpenDetails, PanelContent};
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, RampExt as _, SkeletonTone, Widgets},
    },
    format::Msg,
    layout::PanelPlacement,
    link::{DetailData, DetailGeneration, DetailRequest, DetailTarget, LoadDataError},
    model::{
        CategoryColor, DetailsSubject,
        live_details::{CategoryDisplay, LiveDetails},
    },
};
use gpui_kit::{
    Context, Div, InteractiveElement as _, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px,
};

const TITLE_BAR_HEIGHT: f32 = 4.;
const WEEK_HEIGHT: f32 = 88.;
const SKELETON_ROWS: usize = 6;
const SKELETON_ROW_HEIGHT: f32 = 28.;
#[derive(Debug)]
pub(super) enum DetailPhase {
    Loading,
    Ready(LiveDetails),
    Failed(LoadDataError),
}
#[derive(Debug)]
pub(super) struct LivePanel {
    pub request: DetailRequest,
    pub heading: Option<String>,
    pub exe: Option<String>,
    pub color: Option<CategoryColor>,
    pub phase: DetailPhase,
}
impl AppShell {
    pub(super) fn cancel_details(&mut self, cx: &mut Context<Self>) {
        if self.details.take().is_some() {
            self.send_detail_intent(None, cx);
        }
    }

    pub(super) fn select_live_details(&mut self, subject: DetailsSubject, cx: &mut Context<Self>) {
        let Some(request) = self.day.detail_request(subject) else {
            return;
        };
        let app = match subject {
            DetailsSubject::App(app) => Some(self.day.app(app)),
            DetailsSubject::Segment(id) => self
                .day
                .segment(id)
                .map(|segment| self.day.app(segment.app)),
            DetailsSubject::MainBlock(_) => None,
        };
        let color = match subject {
            DetailsSubject::Segment(id) => self
                .day
                .segment(id)
                .map(|segment| self.day.category(segment.category).color),
            _ => None,
        };
        self.details = Some(OpenDetails {
            subject,
            content: PanelContent::Live(LivePanel {
                request,
                heading: app.map(|app| app.name.clone()),
                exe: app.map(|app| app.exe.clone()).filter(|exe| !exe.is_empty()),
                color,
                phase: DetailPhase::Loading,
            }),
        });
        self.send_detail_intent(Some(request), cx);
    }
    pub(super) fn send_detail_intent(
        &mut self,
        request: Option<DetailRequest>,
        cx: &mut Context<Self>,
    ) {
        self.detail_generation = self.detail_generation.next();
        let generation = self.detail_generation;
        let Some(live) = &self.live else {
            return;
        };
        let link = live.link.clone();
        cx.spawn(async move |this, cx| {
            if let Err(source) = link.select_details(request, generation).await {
                let _ = this.update(cx, |this, cx| {
                    this.detail_loaded(generation, Err(source.into()), cx)
                });
            }
        })
        .detach();
    }
    pub(crate) fn detail_loading(&mut self, generation: DetailGeneration, cx: &mut Context<Self>) {
        if !self.detail_generation.accepts(generation) {
            return;
        }
        if let Some(OpenDetails {
            content: PanelContent::Live(panel),
            ..
        }) = &mut self.details
        {
            if matches!(panel.phase, DetailPhase::Failed(_)) {
                panel.phase = DetailPhase::Loading;
            }
            cx.notify();
        }
    }
    pub(crate) fn detail_loaded(
        &mut self,
        generation: DetailGeneration,
        result: Result<DetailData, LoadDataError>,
        cx: &mut Context<Self>,
    ) {
        if !self.detail_generation.accepts(generation) {
            return;
        }
        let Some(OpenDetails {
            content: PanelContent::Live(panel),
            ..
        }) = &mut self.details
        else {
            return;
        };
        panel.phase = match result.and_then(|data| {
            if data.request != panel.request {
                return Err(LoadDataError::InvalidData);
            }
            LiveDetails::from_protocol(data).map_err(|_| LoadDataError::InvalidData)
        }) {
            Ok(details) => DetailPhase::Ready(details),
            Err(error) => DetailPhase::Failed(error),
        };
        cx.notify();
    }
    fn retry_details(&mut self, cx: &mut Context<Self>) {
        let Some(OpenDetails {
            content: PanelContent::Live(panel),
            ..
        }) = &mut self.details
        else {
            return;
        };
        panel.phase = DetailPhase::Loading;
        let request = panel.request;
        self.send_detail_intent(Some(request), cx);
        cx.notify();
    }
    pub(super) fn render_live_details(
        &self,
        ui: &Widgets<'_>,
        measure: &crate::app::measure::TextMeasure<'_>,
        panel: &LivePanel,
        placement: PanelPlacement,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let body =
            match &panel.phase {
                DetailPhase::Loading => div().flex().flex_col().gap(ui.metrics.space.m).children(
                    (0..SKELETON_ROWS).map(|_| {
                        ui.skeleton_block(SkeletonTone::Subtle)
                            .h(px(SKELETON_ROW_HEIGHT))
                            .w_full()
                            .rounded(ui.metrics.radius.control)
                    }),
                ),
                DetailPhase::Failed(error) => div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.))
                    .gap(ui.metrics.space.s)
                    .child(
                        div()
                            .ramp(ui, Ramp::BodyStrong)
                            .child(self.formatter.text(Msg::DetailsFailed)),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Body)
                            .text_color(ui.palette.text_secondary)
                            .child(self.formatter.text(if error.is_busy() {
                                Msg::DayBusy
                            } else {
                                Msg::DetailsFailedMessage
                            })),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Caption)
                            .font(ui.mono_font())
                            .child(error.code().to_string()),
                    )
                    .child(
                        ui.button(
                            "retry-details",
                            Density::Compact,
                            ButtonContent::Label(self.formatter.text(Msg::RetryDay)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.retry_details(cx))),
                    ),
                DetailPhase::Ready(data) => self.live_detail_body(ui, measure, panel, data),
            };
        div()
            .id("details-panel")
            .flex()
            .flex_col()
            .min_w(px(0.))
            .flex_shrink_0()
            .w(ui.metrics.layout.panel_width)
            .h_full()
            .bg(ui.palette.fill_layer)
            .border_l_1()
            .border_color(ui.palette.stroke_divider)
            .when(placement == PanelPlacement::Overlay, |panel| {
                panel.shadow(ui.shadows(&ui.metrics.shadow.overlay))
            })
            .track_focus(&self.panel_focus)
            .on_key_down(cx.listener(|this, event, window, cx| {
                this.dismiss_overlay_on_escape(event, window, cx)
            }))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(ui.metrics.space.s)
                    .p(ui.metrics.space.l)
                    .child(ui.icon(
                        if panel.heading.is_some() {
                            IconName::SquareTerminal
                        } else {
                            IconName::Clock
                        },
                        Density::Compact,
                        ui.palette.text_primary,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .whitespace_normal()
                                    .ramp(ui, Ramp::BodyStrong)
                                    .child(panel.heading.clone().unwrap_or_else(|| {
                                        self.formatter.text(Msg::DetailsIntervals)
                                    })),
                            )
                            .children(panel.exe.as_ref().map(|exe| {
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .text_color(ui.palette.text_secondary)
                                    .whitespace_normal()
                                    .child(exe.clone())
                            })),
                    )
                    .child(
                        ui.button(
                            "details-close",
                            Density::Compact,
                            ButtonContent::Icon(IconName::Close),
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.close_details(window, cx)),
                        ),
                    ),
            )
            .child(
                div()
                    .id("details-scroll")
                    .track_scroll(&self.details_scroll)
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .min_w(px(0.))
                    .px(ui.metrics.space.l)
                    .pb(ui.metrics.space.l)
                    .child(body),
            )
            .child(self.details_actions(ui))
    }
    fn title_duration(ui: &Widgets<'_>, duration: String, width: f32) -> Div {
        div()
            .w(px(width))
            .flex_shrink_0()
            .flex()
            .justify_end()
            .ramp(ui, Ramp::Caption)
            .tabular_figures()
            .text_color(ui.palette.text_secondary)
            .child(
                div()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(duration),
            )
    }
    fn detail_mark(ui: &Widgets<'_>, body: Div, category: Option<CategoryColor>) -> Div {
        match category {
            Some(category) => ui.category_mark(body, category),
            None => body.bg(ui.palette.accent_fill),
        }
    }
    fn detail_category(&self, ui: &Widgets<'_>, category: &CategoryDisplay) -> Div {
        div()
            .flex()
            .min_w(px(0.))
            .max_w_full()
            .items_center()
            .gap(ui.metrics.space.xs)
            .ramp(ui, Ramp::Caption)
            .child(ui.category_mark(
                ui.swatch(ui.palette.category(category.color)),
                category.color,
            ))
            .child(
                div()
                    .min_w(px(0.))
                    .whitespace_normal()
                    .child(self.formatter.category_name(&category.name)),
            )
    }
    fn live_detail_body(
        &self,
        ui: &Widgets<'_>,
        measure: &crate::app::measure::TextMeasure<'_>,
        panel: &LivePanel,
        data: &LiveDetails,
    ) -> Div {
        let mut body = div().flex().flex_col().gap(ui.metrics.space.l);
        if matches!(panel.request.target, DetailTarget::Segment { .. }) {
            let rows = div().flex().flex_col().gap(ui.metrics.space.s).children(
                data.intervals.iter().map(|interval| {
                    ui.card()
                        .p(ui.metrics.space.m)
                        .flex()
                        .flex_col()
                        .min_w(px(0.))
                        .gap(ui.metrics.space.xs)
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .min_w(px(0.))
                                .gap(ui.metrics.space.s)
                                .justify_between()
                                .ramp(ui, Ramp::BodyStrong)
                                .child(format!(
                                    "{} – {}",
                                    self.clock_utc(interval.range.start),
                                    self.clock_utc(interval.range.end)
                                ))
                                .child(self.formatter.detail_duration(interval.duration)),
                        )
                        .children(interval.app.as_ref().map(|app| {
                            div()
                                .min_w(px(0.))
                                .w_full()
                                .whitespace_normal()
                                .ramp(ui, Ramp::Caption)
                                .child(app.clone())
                        }))
                        .children(interval.title.as_ref().map(|title| {
                            div()
                                .min_w(px(0.))
                                .w_full()
                                .whitespace_normal()
                                .ramp(ui, Ramp::Caption)
                                .child(title.clone())
                        }))
                        .child(self.detail_category(ui, &interval.category))
                        .child(
                            div()
                                .ramp(ui, Ramp::Caption)
                                .text_color(ui.palette.text_secondary)
                                .min_w(px(0.))
                                .w_full()
                                .whitespace_normal()
                                .child(self.formatter.detail_source(&interval.source)),
                        )
                }),
            );
            body = body.child(self.section(
                ui,
                Msg::DetailsIntervals,
                if data.intervals.is_empty() {
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(ui.palette.text_secondary)
                        .child(self.formatter.text(Msg::DetailsEmpty))
                } else {
                    rows
                },
            ));
        }
        if let Some(entity) = &data.entity {
            let longest = entity
                .daily
                .iter()
                .map(|day| day.duration)
                .max()
                .unwrap_or_default();
            let bars =
                div()
                    .flex()
                    .gap(ui.metrics.space.s)
                    .h(px(WEEK_HEIGHT))
                    .items_end()
                    .children(entity.daily.iter().map(|day| {
                        div().flex_1().h(px(WEEK_HEIGHT)).flex().items_end().child(
                            Self::detail_mark(
                                ui,
                                div()
                                    .w_full()
                                    .h(px(WEEK_HEIGHT
                                        * crate::model::Share::of(day.duration, longest)
                                            .fraction()))
                                    .rounded_t(ui.metrics.timeline.block_radius),
                                panel.color,
                            ),
                        )
                    }));
            let labels = div()
                .flex()
                .px(ui.metrics.space.s)
                .children(entity.daily.iter().map(|day| {
                    let label = if self
                        .live
                        .as_ref()
                        .is_some_and(|live| day.date == live.snapshot.logical_date)
                    {
                        self.formatter.text(Msg::DetailsToday)
                    } else {
                        self.formatter.weekday(day.date.weekday())
                    };
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .px(ui.metrics.space.xxs)
                        .child(ui.axis_label(label, ui.palette.text_secondary))
                }));
            body = body.child(
                self.section(
                    ui,
                    Msg::DetailsWeek,
                    div()
                        .child(ui.plot_area().p(ui.metrics.space.s).child(bars))
                        .child(labels),
                ),
            );
            let title_max = entity
                .titles
                .iter()
                .map(|row| row.duration)
                .max()
                .unwrap_or_default();
            let durations: Vec<_> = entity
                .titles
                .iter()
                .map(|row| self.formatter.duration(row.duration))
                .collect();
            let value_width = super::details::TITLE_VALUE_RANGE
                .select(measure.widths(durations.iter().map(String::as_str), Ramp::Caption));
            let titles = div().flex().flex_col().gap(ui.metrics.space.xs).children(
                entity.titles.iter().zip(durations).map(|(row, duration)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(ui.metrics.space.m)
                        .min_h(ui.height_for_text(
                            ui.metrics.density(Density::Compact).row_height,
                            Ramp::Caption,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .ramp(ui, Ramp::Caption)
                                .child(row.title.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(crate::app::widgets::PROPORTION_BAR_MIN_WIDTH))
                                .h(
                                    if ui.palette.contrast == crate::app::tokens::ContrastMode::High
                                    {
                                        ui.metrics.space.s
                                    } else {
                                        px(TITLE_BAR_HEIGHT)
                                    },
                                )
                                .rounded(ui.metrics.timeline.block_radius)
                                .bg(ui.palette.fill_subtle_hover)
                                .child(Self::detail_mark(
                                    ui,
                                    div()
                                        .h_full()
                                        .w(gpui_kit::relative(
                                            crate::model::Share::of(row.duration, title_max)
                                                .fraction(),
                                        ))
                                        .rounded(ui.metrics.timeline.block_radius),
                                    panel.color,
                                )),
                        )
                        .child(Self::title_duration(ui, duration, value_width))
                }),
            );
            body = body.child(self.section(
                ui,
                Msg::DetailsTitles,
                if entity.titles.is_empty() {
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(ui.palette.text_secondary)
                        .child(self.formatter.text(Msg::DetailsNoTitles))
                } else {
                    titles
                },
            ));
            let rules = div()
                .flex()
                .flex_col()
                .min_w(px(0.))
                .gap(ui.metrics.space.s)
                .children(entity.rules.iter().map(|rule| {
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.))
                        .gap(ui.metrics.space.xs)
                        .child(
                            div()
                                .ramp(ui, Ramp::Caption)
                                .min_w(px(0.))
                                .w_full()
                                .whitespace_normal()
                                .child(self.formatter.rule_label(&rule.label)),
                        )
                        .child(
                            div()
                                .ramp(ui, Ramp::Caption)
                                .font(ui.mono_font())
                                .text_color(ui.palette.text_secondary)
                                .min_w(px(0.))
                                .w_full()
                                .whitespace_normal()
                                .child(rule.identity.as_str().to_owned()),
                        )
                        .child(self.detail_category(ui, &rule.category))
                }));
            body = body.child(self.section(
                ui,
                Msg::DetailsRules,
                if entity.rules.is_empty() {
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(ui.palette.text_secondary)
                        .child(self.formatter.text(Msg::DetailsNoRule))
                } else {
                    rules
                },
            ));
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{
            runtime::ThemeBundle,
            tokens::{Palette, SystemColors},
        },
        model::PaletteSlot,
        text_scale::TextScale,
    };
    #[test]
    fn detail_category_marks_use_patterns_and_title_durations_use_tabular_figures() {
        let theme = ThemeBundle::embedded().expect("theme");
        let colors: SystemColors =
            serde_json::from_str(include_str!("../../../themes/preview-high-contrast.json"))
                .expect("colors");
        let palette = Palette::high_contrast(&colors);
        let ui = Widgets {
            palette: &palette,
            metrics: theme.tokens.metrics(),
            text_scale: TextScale::UNSCALED,
        };
        for slot in PaletteSlot::ALL {
            let mut mark = AppShell::detail_mark(&ui, div(), Some(CategoryColor::Palette(slot)));
            assert_eq!(
                mark.style().background,
                Some(colors.window.into()),
                "category marks must use the opaque patterned surface"
            );
            let mut swatch = ui.category_mark(
                ui.swatch(palette.category(CategoryColor::Palette(slot))),
                CategoryColor::Palette(slot),
            );
            assert_eq!(swatch.style().background, Some(colors.window.into()));
        }
        let mut duration = AppShell::title_duration(&ui, "1h 5m".into(), 60.);
        assert_eq!(
            duration.style().text_style().font_features,
            Some(Widgets::tabular_figures())
        );
    }
}
