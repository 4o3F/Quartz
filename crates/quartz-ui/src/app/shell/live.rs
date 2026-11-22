//! Live Today state and intents. All I/O runs through the pipe thread.
use super::{AppShell, Frame, RangeChoice};
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, RampExt as _, Widgets},
    },
    format::{Formatter, Msg},
    link::{DayFailure, DayGeneration, DayLoad, DayRequest, DayView, Link},
    model::{Day, DaySource, TimeOfDay},
};
use gpui_kit::{
    AnyElement, Canvas, Context, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Task, assets::IconName, canvas, div, px,
};
use quartz_protocol::{StateSnapshot, StopReason};

// Dimensions from the approved Today data states mockup.
const EMPTY_CARD_MIN_HEIGHT: f32 = 300.;
const EMPTY_TILE_SIZE: f32 = 48.;
const EMPTY_ICON_SIZE: f32 = 24.;
const RECORDING_PILL_HEIGHT: f32 = 24.;
const RECORDING_DOT_SIZE: f32 = 6.;
const PROGRESS_RING_SIZE: f32 = 14.;
const PROGRESS_RING_GAP: f32 = 6.;
const CRITICAL_BORDER_OPACITY: f32 = 0.28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoadPhase {
    Loading,
    Ready,
    Refreshing,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Indicator {
    Hidden,
    Visible,
}
#[derive(Debug)]
pub(super) struct LivePage {
    pub(super) link: Link,
    pub snapshot: StateSnapshot,
    pub generation: DayGeneration,
    pub phase: LoadPhase,
    pub failure: Option<DayFailure>,
    request: Option<DayRequest>,
    indicator: Indicator,
    timer: Option<Task<()>>,
}
impl AppShell {
    pub(super) fn clock_label(&self, time: TimeOfDay) -> String {
        let Some(local) = self.day.local_clock(time) else {
            return time.to_string();
        };
        self.format_local_clock(local)
    }
    pub(super) fn clock_utc(&self, utc: i64) -> String {
        let local = self
            .day
            .local_clock_at(utc)
            .expect("loaded details have core time zone context");
        if local.second() == 0 && local.millisecond() == 0 {
            return self.format_local_clock(local);
        }
        let seconds = if local.millisecond() == 0 {
            format!("{:02}", local.second())
        } else {
            format!("{:02}.{:03}", local.second(), local.millisecond())
        };
        match self
            .live
            .as_ref()
            .and_then(|live| live.snapshot.locale)
            .map(|locale| locale.hour_cycle)
        {
            Some(quartz_protocol::HourCycle::H12) => {
                let hour = local.hour();
                self.formatter.text_with(
                    Msg::DetailsClockTwelve,
                    &[
                        crate::format::Arg::Hours
                            .with(&if hour % 12 == 0 { 12 } else { hour % 12 }.to_string()),
                        crate::format::Arg::Minutes.with(&format!("{:02}", local.minute())),
                        crate::format::Arg::Value.with(&seconds),
                        crate::format::Arg::Name.with(&self.formatter.text(if hour < 12 {
                            Msg::ClockMorning
                        } else {
                            Msg::ClockAfternoon
                        })),
                    ],
                )
            }
            Some(quartz_protocol::HourCycle::H24) | None => {
                format!("{:02}:{:02}:{seconds}", local.hour(), local.minute())
            }
        }
    }
    fn format_local_clock(&self, local: jiff::civil::Time) -> String {
        match self
            .live
            .as_ref()
            .and_then(|live| live.snapshot.locale)
            .map(|locale| locale.hour_cycle)
        {
            Some(quartz_protocol::HourCycle::H12) => {
                let hour = local.hour();
                self.formatter.text_with(
                    Msg::ClockTwelve,
                    &[
                        crate::format::Arg::Hours
                            .with(&if hour % 12 == 0 { 12 } else { hour % 12 }.to_string()),
                        crate::format::Arg::Minutes.with(&format!("{:02}", local.minute())),
                        crate::format::Arg::Name.with(&self.formatter.text(if hour < 12 {
                            Msg::ClockMorning
                        } else {
                            Msg::ClockAfternoon
                        })),
                    ],
                )
            }
            Some(quartz_protocol::HourCycle::H24) | None => local.strftime("%H:%M").to_string(),
        }
    }
    pub(crate) fn connect(&mut self, link: Link, snapshot: StateSnapshot, cx: &mut Context<Self>) {
        self.day = Day::empty(snapshot.logical_date);
        self.range = RangeChoice::FullDay;
        self.live = Some(LivePage {
            link,
            snapshot,
            generation: DayGeneration::default(),
            phase: LoadPhase::Loading,
            failure: None,
            request: None,
            indicator: Indicator::Hidden,
            timer: None,
        });
        cx.notify();
    }
    pub(crate) fn state_changed(
        &mut self,
        snapshot: StateSnapshot,
        formatter: Formatter,
        cx: &mut Context<Self>,
    ) {
        self.formatter = formatter;
        let follow = self.live.as_ref().is_some_and(|live| {
            (self.day.date() == live.snapshot.logical_date
                && snapshot.logical_date != live.snapshot.logical_date)
                || self.day.date() > snapshot.logical_date
        });
        if follow {
            self.cancel_details(cx);
        }
        if let Some(live) = &mut self.live {
            live.snapshot = snapshot;
            if follow {
                self.calendar = None;
                self.hover = None;
                self.pan = None;
                self.range = RangeChoice::FullDay;
                self.day = Day::empty(live.snapshot.logical_date);
                live.request = None;
                live.generation = live.generation.next();
                live.phase = LoadPhase::Loading;
                live.failure = None;
                live.timer = None;
                live.indicator = Indicator::Hidden;
            }
        }
        cx.notify();
    }
    pub(crate) fn day_loading(&mut self, generation: DayGeneration, cx: &mut Context<Self>) {
        let Some(live) = &mut self.live else {
            return;
        };
        if !live.generation.accepts(generation) {
            return;
        }
        live.failure = None;
        if matches!(self.day.source(), DaySource::Live(_)) {
            live.phase = LoadPhase::Refreshing;
            let delay = cx
                .background_executor()
                .timer(crate::link::REFRESH_INDICATOR_DELAY);
            live.timer = Some(cx.spawn(async move |this, cx| {
                delay.await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(live) = &mut this.live
                        && live.generation == generation
                        && live.phase == LoadPhase::Refreshing
                    {
                        live.indicator = Indicator::Visible;
                        cx.notify();
                    }
                });
            }));
        } else {
            live.phase = LoadPhase::Loading;
        }
        cx.notify();
    }
    pub(crate) fn day_loaded(
        &mut self,
        generation: DayGeneration,
        result: Result<crate::link::DayData, DayFailure>,
        cx: &mut Context<Self>,
    ) {
        let Some(live) = &mut self.live else {
            return;
        };
        if !live.generation.accepts(generation) {
            return;
        }
        live.timer = None;
        live.indicator = Indicator::Hidden;
        match result.and_then(|data| Day::from_protocol(data).map_err(|_| DayFailure::InvalidData))
        {
            Ok(day) => {
                if matches!(self.day.source(), DaySource::Loading) {
                    self.range = RangeChoice::Activity;
                }
                self.day = day;
                self.hover = None;
                live.phase = LoadPhase::Ready;
                live.failure = None;
            }
            Err(error) => {
                live.failure = Some(error);
                live.phase = LoadPhase::Ready;
            }
        }
        cx.notify();
    }
    fn load_request(&mut self, request: DayRequest, mode: DayLoad, cx: &mut Context<Self>) {
        let Some(live) = &mut self.live else {
            return;
        };
        live.generation = live.generation.next();
        live.request = Some(request);
        let generation = live.generation;
        let link = live.link.clone();
        self.day_loading(generation, cx);
        cx.spawn(async move |this, cx| {
            if let Err(source) = link.load_day(request, generation, mode).await {
                let _ = this.update(cx, |this, cx| {
                    this.day_loaded(generation, Err(DayFailure::Request { source }), cx)
                });
            }
        })
        .detach();
    }
    pub(super) fn observe_plot(&mut self, logical: f32, scale: f32, cx: &mut Context<Self>) {
        self.plot_width = Some(logical);
        let Some(live) = &self.live else {
            return;
        };
        let Ok(physical_width) = DayRequest::physical_width(logical, scale) else {
            return;
        };
        let view = match self.day.source() {
            DaySource::Live(_) if self.range != RangeChoice::FullDay => DayView::Range(
                self.day
                    .utc_range(self.view().span())
                    .expect("live day has UTC bounds"),
            ),
            DaySource::Fixture | DaySource::Loading | DaySource::Live(_) => DayView::WholeDay,
        };
        let request = DayRequest {
            date: self.day.date(),
            view,
            physical_width,
        };
        if live.request == Some(request) {
            return;
        }
        self.load_request(request, DayLoad::Settled, cx);
    }
    pub(super) fn measure_plot(&self, cx: &Context<Self>) -> Canvas<()> {
        let entity = cx.entity();
        canvas(
            move |bounds, window, app| {
                let width = f32::from(bounds.size.width);
                let scale = window.scale_factor();
                window.defer(app, move |_, app| {
                    entity.update(app, |shell, cx| shell.observe_plot(width, scale, cx))
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }
    pub(super) fn retry_day(&mut self, cx: &mut Context<Self>) {
        if let Some(request) = self.live.as_ref().and_then(|live| live.request) {
            self.load_request(request, DayLoad::Immediate, cx);
        }
    }
    pub(super) fn select_date(&mut self, date: quartz_protocol::LocalDate, cx: &mut Context<Self>) {
        let Some(live) = &mut self.live else {
            return;
        };
        if date > live.snapshot.logical_date || date == self.day.date() {
            return;
        }
        self.calendar = None;
        self.pan = None;
        let previous = live.request;
        live.generation = live.generation.next();
        live.failure = None;
        live.phase = LoadPhase::Loading;
        live.request = None;
        live.timer = None;
        live.indicator = Indicator::Hidden;
        self.day = Day::empty(date);
        self.hover = None;
        self.cancel_details(cx);
        self.range = RangeChoice::FullDay;
        if let Some(mut request) = previous {
            request.date = date;
            request.view = DayView::WholeDay;
            self.load_request(request, DayLoad::Immediate, cx);
        }
        cx.notify();
    }
    pub(super) fn stop_bar(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> Option<AnyElement> {
        let live = self.live.as_ref()?;
        if live.snapshot.recording {
            return None;
        }
        let reason = live.snapshot.stop_reason?;
        let message = match reason {
            StopReason::Starting => Msg::StopStarting,
            StopReason::PersistenceFailed => Msg::StopPersistence,
            StopReason::SessionUnavailable => Msg::StopSession,
            StopReason::CaptureGap => Msg::StopGap,
            StopReason::Locked => Msg::StopLocked,
            StopReason::Disconnected => Msg::StopDisconnected,
            StopReason::Suspended => Msg::StopSuspended,
            StopReason::Paused => Msg::StopPaused,
            StopReason::NoForeground => Msg::StopForeground,
            StopReason::Ended => Msg::StopEnded,
        };
        let critical = reason == StopReason::PersistenceFailed;
        let mut bar = ui
            .card()
            .flex()
            .items_center()
            .gap(ui.metrics.space.m)
            .p(ui.metrics.space.l)
            .bg(if critical {
                ui.palette.status_critical_bg
            } else {
                ui.palette.fill_card
            })
            .border_color(if critical {
                ui.palette.status_critical.opacity(CRITICAL_BORDER_OPACITY)
            } else {
                ui.palette.stroke_card
            })
            .child(ui.icon(
                if critical {
                    IconName::CircleX
                } else {
                    IconName::Clock
                },
                Density::Compact,
                if critical {
                    ui.palette.status_critical
                } else {
                    ui.palette.text_secondary
                },
            ))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap(ui.metrics.space.s)
                    .child(
                        div()
                            .ramp(ui, Ramp::BodyStrong)
                            .child(self.formatter.text(Msg::StopTitle)),
                    )
                    .child(
                        div()
                            .ramp(ui, Ramp::Body)
                            .text_color(ui.palette.text_secondary)
                            .child(self.formatter.text(message)),
                    ),
            );
        if reason == StopReason::Paused {
            bar = bar.child(
                ui.button(
                    "resume-recording",
                    Density::Compact,
                    ButtonContent::Label(self.formatter.text(Msg::Resume)),
                )
                .on_click(cx.listener(|this, _, _, cx| this.resume(cx))),
            );
        }
        Some(bar.into_any_element())
    }
    fn resume(&self, cx: &mut Context<Self>) {
        let Some(live) = &self.live else {
            return;
        };
        let link = live.link.clone();
        cx.spawn(async move |this, cx| {
            if let Err(source) = link
                .request(quartz_protocol::TrackingResumeRequest {})
                .await
            {
                let _ = this.update(cx, |this, cx| {
                    if let Some(live) = &mut this.live {
                        live.failure = Some(source.into());
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }
    pub(super) fn live_notice(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let live = self.live.as_ref()?;
        if let Some(error) = &live.failure {
            return Some(
                ui.card()
                    .flex()
                    .flex_wrap()
                    .gap(ui.metrics.space.m)
                    .p(ui.metrics.space.l)
                    .child(ui.icon(
                        IconName::TriangleAlert,
                        Density::Standard,
                        ui.palette.status_caution,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .ramp(ui, Ramp::BodyStrong)
                                    .child(self.formatter.text(Msg::DayFailedTitle)),
                            )
                            .child(div().ramp(ui, Ramp::Body).child(self.formatter.text(
                                if error.is_busy() {
                                    Msg::DayBusy
                                } else {
                                    Msg::DayFailedMessage
                                },
                            )))
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .text_color(ui.palette.text_tertiary)
                                    .font(ui.mono_font())
                                    .child(error.code().to_string()),
                            ),
                    )
                    .child(
                        ui.button(
                            "retry-day",
                            Density::Standard,
                            ButtonContent::Label(self.formatter.text(Msg::RetryDay)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.retry_day(cx))),
                    )
                    .into_any_element(),
            );
        }
        if live.phase == LoadPhase::Loading {
            return Some(self.skeleton(ui, frame, cx));
        }
        if self.day.summary().active.is_zero() && self.day.pending_ranges().is_empty() {
            let today = self.day.date() == live.snapshot.logical_date;
            let mut inner = ui
                .card()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(ui.metrics.space.m)
                .p(ui.metrics.space.xxl)
                .min_h(px(EMPTY_CARD_MIN_HEIGHT))
                .child(
                    div()
                        .size(px(EMPTY_TILE_SIZE))
                        .rounded(ui.metrics.radius.overlay)
                        .bg(ui.palette.fill_subtle_hover)
                        .border_1()
                        .border_color(ui.palette.stroke_card)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            ui.icon(
                                if today {
                                    IconName::Clock
                                } else {
                                    IconName::Calendar
                                },
                                Density::Standard,
                                ui.palette.text_secondary,
                            )
                            .size(px(EMPTY_ICON_SIZE)),
                        ),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Subtitle)
                        .child(self.formatter.text(if today {
                            Msg::EmptyTodayTitle
                        } else {
                            Msg::EmptyPastTitle
                        })),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Body)
                        .text_color(ui.palette.text_secondary)
                        .child(self.formatter.text(if today {
                            Msg::EmptyTodayMessage
                        } else {
                            Msg::EmptyPastMessage
                        })),
                );
            if today && live.snapshot.recording {
                inner = inner.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui.metrics.space.s)
                        .h(px(RECORDING_PILL_HEIGHT))
                        .px(ui.metrics.space.m)
                        .rounded(ui.metrics.radius.control)
                        .bg(ui.palette.fill_subtle_hover)
                        .ramp(ui, Ramp::Caption)
                        .text_color(ui.palette.text_secondary)
                        .child(
                            div()
                                .size(px(RECORDING_DOT_SIZE))
                                .rounded_full()
                                .bg(ui.palette.status_success),
                        )
                        .child(self.formatter.text(Msg::LinkRecording)),
                );
            }
            if !today {
                inner = inner.child(
                    ui.button(
                        "empty-go-today",
                        Density::Standard,
                        ButtonContent::Label(self.formatter.text(Msg::GoToday)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(live) = &this.live {
                            this.select_date(live.snapshot.logical_date, cx);
                        }
                    })),
                );
            }
            return Some(inner.into_any_element());
        }
        None
    }
    fn skeleton(&self, ui: &Widgets<'_>, frame: &Frame<'_>, cx: &Context<Self>) -> AnyElement {
        use super::stat_grid::StatGrid;
        use crate::layout::ChartArrangement;
        const VALUE_SLOT_HEIGHT: f32 = 36.;
        const VALUE_HEIGHT: f32 = 22.;
        const VALUE_WIDTH: f32 = 72.;
        const ACTIVE_WIDTH: f32 = 104.;
        const TIMELINE_HEIGHT: f32 = 228.;
        const CHART_HEIGHT: f32 = 192.;
        const LANE_COUNT: usize = 4;
        // Static bars and their heights come from the approved loading state.
        const BAR_HEIGHTS: [f32; 11] = [28., 86., 72., 60., 34., 52., 82., 48., 66., 26., 36.];
        let fill = ui.palette.text_tertiary.opacity(0.18);
        let labels = [
            Msg::StatActive,
            Msg::StatFocus,
            Msg::StatDistraction,
            Msg::StatUnclassified,
            Msg::StatLongestFocus,
            Msg::StatSwitches,
        ];
        let widest = labels
            .iter()
            .map(|label| {
                frame
                    .measure
                    .width(&self.formatter.text(*label), Ramp::Caption)
            })
            .fold(ACTIVE_WIDTH, f32::max);
        let cells = labels.map(|label| {
            div()
                .flex()
                .flex_col()
                .gap(ui.metrics.space.xs)
                .child(
                    div()
                        .h(px(VALUE_SLOT_HEIGHT))
                        .flex()
                        .items_end()
                        .pb(px(4.))
                        .child(
                            div()
                                .h(px(VALUE_HEIGHT))
                                .w(px(if label == Msg::StatActive {
                                    ACTIVE_WIDTH
                                } else {
                                    VALUE_WIDTH
                                }))
                                .rounded(ui.metrics.radius.control)
                                .bg(fill),
                        ),
                )
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .text_color(ui.palette.text_secondary)
                        .child(self.formatter.text(label)),
                )
                .into_any_element()
        });
        let status = ui
            .card()
            .px(ui.metrics.space.l)
            .py(ui.metrics.space.m)
            .flex()
            .flex_col()
            .gap(ui.metrics.space.s)
            .child(StatGrid::new(
                cells,
                widest,
                px(VALUE_SLOT_HEIGHT) + ui.line_height(Ramp::Caption) + ui.metrics.space.xs,
                ui.metrics.space.m,
                ui.metrics.space.m,
            ))
            .child(
                div()
                    .h(px(8.))
                    .w_full()
                    .rounded_full()
                    .bg(ui.palette.fill_subtle_hover),
            )
            .child(
                div()
                    .h(px(12.))
                    .w(px(240.))
                    .rounded(ui.metrics.radius.control)
                    .bg(ui.palette.fill_subtle_hover),
            );
        let label_width = self.label_column_width(ui, frame.measure);
        let timeline = ui
            .card()
            .h(px(TIMELINE_HEIGHT))
            .p(ui.metrics.space.l)
            .flex()
            .flex_col()
            .gap(ui.metrics.space.m)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui.metrics.space.s)
                    .child(
                        div()
                            .flex_1()
                            .ramp(ui, Ramp::Subtitle)
                            .child(self.formatter.text(Msg::TimelineTitle)),
                    )
                    .child(ui.disabled_button(
                        "loading-zoom-out",
                        Density::Compact,
                        ButtonContent::Icon(IconName::Minus),
                    ))
                    .child(ui.disabled_button(
                        "loading-zoom-in",
                        Density::Compact,
                        ButtonContent::Icon(IconName::Plus),
                    ))
                    .child(ui.disabled_button(
                        "loading-full-day",
                        Density::Compact,
                        ButtonContent::Label(self.formatter.text(Msg::TimelineFullDay)),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .gap(ui.metrics.space.s)
                    .child(
                        div()
                            .w(px(label_width))
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .gap(ui.metrics.space.m)
                            .child(
                                div()
                                    .ramp(ui, Ramp::Caption)
                                    .child(self.formatter.text(Msg::TimelineMainTrack)),
                            )
                            .children((0..LANE_COUNT).map(|_| {
                                div()
                                    .h(px(12.))
                                    .w_full()
                                    .rounded(ui.metrics.radius.control)
                                    .bg(ui.palette.fill_subtle_hover)
                            })),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(ui.metrics.space.s)
                            .child(
                                div()
                                    .h(px(28.))
                                    .w_full()
                                    .rounded(ui.metrics.radius.control)
                                    .bg(fill),
                            )
                            .children((0..LANE_COUNT).map(|_| {
                                div()
                                    .h(px(18.))
                                    .w_full()
                                    .rounded(ui.metrics.radius.control)
                                    .bg(ui.palette.fill_subtle_hover)
                            }))
                            .child(self.measure_plot(cx)),
                    ),
            );
        let hourly = ui
            .card()
            .flex_1()
            .min_w(px(0.))
            .h(px(CHART_HEIGHT))
            .p(ui.metrics.space.l)
            .flex()
            .flex_col()
            .gap(ui.metrics.space.s)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .child(self.formatter.text(Msg::HourlyTitle)),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .gap(ui.metrics.space.s)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .justify_between()
                            .ramp(ui, Ramp::Caption)
                            .text_color(ui.palette.text_tertiary)
                            .child(self.formatter.whole_minutes(60))
                            .child(self.formatter.whole_minutes(30))
                            .child(self.formatter.whole_minutes(0)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .items_end()
                            .justify_between()
                            .gap(ui.metrics.space.s)
                            .children(BAR_HEIGHTS.map(|height| {
                                div().flex_1().max_w(px(30.)).h(px(height)).bg(fill)
                            })),
                    ),
            );
        let ranking = ui
            .card()
            .flex_1()
            .min_w(px(0.))
            .h(px(CHART_HEIGHT))
            .p(ui.metrics.space.l)
            .flex()
            .flex_col()
            .gap(ui.metrics.space.m)
            .child(
                div()
                    .ramp(ui, Ramp::Subtitle)
                    .child(self.formatter.text(Msg::RankingTitle)),
            )
            .children((0..5).map(|_| {
                div()
                    .flex()
                    .items_center()
                    .gap(ui.metrics.space.m)
                    .child(
                        div()
                            .size(px(16.))
                            .rounded(ui.metrics.radius.control)
                            .bg(ui.palette.fill_subtle_hover),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(12.))
                            .rounded(ui.metrics.radius.control)
                            .bg(fill),
                    )
                    .child(
                        div()
                            .w(px(56.))
                            .h(px(12.))
                            .rounded(ui.metrics.radius.control)
                            .bg(ui.palette.fill_subtle_hover),
                    )
            }));
        let charts = match frame.layout.class.chart_arrangement() {
            ChartArrangement::SideBySide => div().flex().flex_row(),
            ChartArrangement::Stacked => div().flex().flex_col(),
        };
        div()
            .flex()
            .flex_col()
            .gap(ui.metrics.space.s)
            .child(status)
            .child(timeline)
            .child(charts.gap(ui.metrics.space.s).child(hourly).child(ranking))
            .into_any_element()
    }
    pub(super) fn refresh_indicator(&self, ui: &Widgets<'_>) -> Option<AnyElement> {
        (self.live.as_ref()?.indicator == Indicator::Visible).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(PROGRESS_RING_GAP))
                .ml_auto()
                .ramp(ui, Ramp::Caption)
                .text_color(ui.palette.text_tertiary)
                .child({
                    use gpui_kit::component::{Icon, Sizable as _, spinner::Spinner};
                    Spinner::new()
                        .icon(Icon::default().path(crate::app::icons::AppAssets::progress_path()))
                        .color(ui.palette.text_tertiary)
                        .with_size(px(PROGRESS_RING_SIZE))
                })
                .child(self.formatter.text(Msg::DayUpdating))
                .into_any_element()
        })
    }
}

impl AppShell {
    pub(super) fn annotate(
        &self,
        action: crate::link::AwayAction,
        range: crate::model::PendingAwaySpan,
        cx: &mut Context<Self>,
    ) {
        let Some(live) = &self.live else {
            return;
        };
        let uuid = match windows::core::GUID::new().map(|id| format!("{id:?}")) {
            Ok(text) => match text.parse() {
                Ok(uuid) => quartz_protocol::AnnotationId(uuid),
                Err(error) => {
                    tracing::warn!(%error, "could not parse annotation identity");
                    return;
                }
            },
            Err(error) => {
                tracing::warn!(%error, "could not create annotation identity");
                return;
            }
        };
        let Some(range) = self.day.utc_range(range.span) else {
            return;
        };
        let request = action.request(range, uuid);
        let link = live.link.clone();
        cx.spawn(async move |this, cx| {
            if let Err(source) = link.request(request).await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(live) = &mut this.live {
                        live.failure = Some(source.into());
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }
}
