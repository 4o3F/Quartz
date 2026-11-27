//! Live Today state and intents. All I/O runs through the pipe thread.
use super::{AppShell, Frame, RangeChoice};
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, InfoBarStyle, RampExt as _, SkeletonTone, Widgets},
    },
    format::{Formatter, Msg},
    link::{DayGeneration, DayLoad, DayRequest, DayView, Link, LoadDataError},
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
    pub failure: Option<LoadDataError>,
    request: Option<DayRequest>,
    pub(super) actions: ActionWrites,
    indicator: Indicator,
    timer: Option<Task<()>>,
    load_task: Option<Task<()>>,
    resume_task: Option<Task<()>>,
}
impl LivePage {
    fn finish_annotation(
        &mut self,
        day: &Day,
        range: crate::link::UtcRange,
        action: crate::link::AwayAction,
        result: Result<(), crate::link::RequestCoreError>,
    ) {
        let succeeded = result.is_ok();
        self.actions.finish(range, action, result);
        if succeeded {
            self.actions.reconcile(day);
        }
    }
    fn clear_resume_failure(&mut self) {
        if self
            .actions
            .failure
            .as_ref()
            .is_some_and(|failure| matches!(failure.action, MutationAction::Resume))
        {
            self.actions.failure = None;
        }
    }
    fn begin_resume(&mut self) {
        self.resume_task = None;
        self.clear_resume_failure();
    }
    fn observe_recording(&mut self) {
        if self.snapshot.recording {
            self.resume_task = None;
            self.clear_resume_failure();
        }
    }
    fn change_date(&mut self) {
        self.actions.failure = None;
        self.load_task = None;
        self.resume_task = None;
    }
    fn finish_resume(&mut self, result: Result<(), crate::link::RequestCoreError>) {
        match result {
            Ok(()) => self.clear_resume_failure(),
            Err(_) if self.snapshot.recording => self.clear_resume_failure(),
            Err(source) => {
                self.actions.failure = Some(ActionFailure {
                    action: MutationAction::Resume,
                    error: source.into(),
                })
            }
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritePhase {
    Writing,
    Submitted,
}
#[derive(Debug)]
struct SpanWrite {
    range: crate::link::UtcRange,
    phase: WritePhase,
}
#[derive(Debug, Clone, Copy)]
enum MutationAction {
    Away(crate::link::AwayAction),
    Resume,
}
impl MutationAction {
    fn message(self, formatter: Formatter) -> String {
        match self {
            Self::Resume => formatter.text(Msg::ResumeFailed),
            Self::Away(action) => formatter.text_with(
                Msg::AwayFailed,
                &[crate::format::Arg::Name.with(&formatter.text(match action {
                    crate::link::AwayAction::Rest => Msg::PendingRest,
                    crate::link::AwayAction::Meeting => Msg::PendingMeeting,
                    crate::link::AwayAction::Reading => Msg::PendingReading,
                    crate::link::AwayAction::Ignore => Msg::PendingDismiss,
                }))],
            ),
        }
    }
}
#[derive(Debug)]
struct ActionFailure {
    action: MutationAction,
    error: LoadDataError,
}
#[derive(Debug, Default)]
pub(super) struct ActionWrites {
    spans: Vec<SpanWrite>,
    failure: Option<ActionFailure>,
}
impl ActionWrites {
    pub(super) fn contains(&self, range: crate::link::UtcRange) -> bool {
        self.spans.iter().any(|write| write.range == range)
    }
    fn begin(&mut self, range: crate::link::UtcRange) -> Result<(), crate::link::RequestCoreError> {
        if self.contains(range) {
            return Err(crate::link::RequestCoreError::Busy);
        }
        self.spans.push(SpanWrite {
            range,
            phase: WritePhase::Writing,
        });
        self.failure = None;
        Ok(())
    }
    fn finish(
        &mut self,
        range: crate::link::UtcRange,
        action: crate::link::AwayAction,
        result: Result<(), crate::link::RequestCoreError>,
    ) {
        match result {
            Ok(()) => {
                if let Some(write) = self.spans.iter_mut().find(|write| write.range == range) {
                    write.phase = WritePhase::Submitted;
                }
            }
            Err(source) => {
                self.spans.retain(|write| write.range != range);
                self.failure = Some(ActionFailure {
                    action: MutationAction::Away(action),
                    error: source.into(),
                });
            }
        }
    }
    fn reconcile(&mut self, day: &Day) {
        self.spans.retain(|write| {
            write.phase == WritePhase::Writing
                || day
                    .pending_ranges()
                    .iter()
                    .any(|pending| day.utc_range(pending.span) == Some(write.range))
        });
    }
}
/// Maps a core [`StopReason`] to its localized status message key.
pub(super) const fn stop_reason_msg(reason: StopReason) -> Msg {
    match reason {
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
    }
}

impl AppShell {
    pub(super) fn day_snapshot(&self) -> Option<&StateSnapshot> {
        #[cfg(feature = "preview")]
        if let Some(preview) = &self.preview {
            return Some(&preview.snapshot);
        }
        self.live.as_ref().map(|live| &live.snapshot)
    }
    fn load_phase(&self) -> Option<LoadPhase> {
        #[cfg(feature = "preview")]
        if let Some(preview) = &self.preview {
            return Some(preview.phase);
        }
        self.live.as_ref().map(|live| live.phase)
    }
    fn day_failure(&self) -> Option<&LoadDataError> {
        #[cfg(feature = "preview")]
        if let Some(preview) = &self.preview {
            return preview.failure.as_ref();
        }
        self.live.as_ref().and_then(|live| live.failure.as_ref())
    }
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
            actions: ActionWrites::default(),
            indicator: Indicator::Hidden,
            timer: None,
            load_task: None,
            resume_task: None,
        });
        if self.page == super::Page::Rules {
            self.open_rules_page(cx);
        }
        if self.page == super::Page::Week {
            self.open_week_page(cx);
        }
        if self.page == super::Page::Settings {
            self.open_settings_page(cx);
        }
        cx.notify();
    }
    pub(crate) fn state_changed(
        &mut self,
        snapshot: StateSnapshot,
        formatter: Formatter,
        cx: &mut Context<Self>,
    ) {
        if self.formatter != formatter {
            self.settings_selects_sync = super::InputSyncState::NeedsSync;
        }
        self.formatter = formatter;
        let previous_today = self.current_logical_today();
        let new_today = snapshot.logical_date;
        let first_weekday = snapshot
            .locale
            .map(|locale| crate::model::weekday_from_protocol(locale.first_day_of_week));
        let follow = self.live.as_ref().is_some_and(|live| {
            (self.day.date() == live.snapshot.logical_date
                && snapshot.logical_date != live.snapshot.logical_date)
                || self.day.date() > snapshot.logical_date
        });
        let recovered = self
            .live
            .as_ref()
            .is_some_and(|live| !live.snapshot.recording && snapshot.recording);
        if follow {
            self.cancel_details(cx);
        }
        let recording = snapshot.recording;
        let stop_reason = snapshot.stop_reason;
        if let Some(live) = &mut self.live {
            live.snapshot = snapshot;
            live.observe_recording();
            if follow {
                live.change_date();
                self.calendar = None;
                self.hover = None;
                self.pan = None;
                self.range_select = None;
                self.segment_menu = None;
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
        self.settings.sync_recording_state(recording, stop_reason);
        if previous_today != new_today {
            self.settings.on_logical_date_changed(new_today);
            self.settings_calendar = None;
        }
        if follow {
            self.on_live_day_context_changed(None, cx);
            self.on_rules_mutated(cx);
        }
        self.on_week_snapshot_changed(previous_today, new_today, first_weekday, cx);
        if recovered && !follow {
            self.retry_day(cx);
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
        result: Result<crate::link::DayData, LoadDataError>,
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
        let loaded_context = match result
            .and_then(|data| Day::from_protocol(data).map_err(|_| LoadDataError::InvalidData))
        {
            Ok(day) => {
                if matches!(self.day.source(), DaySource::Loading) {
                    self.range = RangeChoice::Activity;
                }
                live.actions.reconcile(&day);
                let context = day.live_time_context();
                self.day = day;
                self.hover = None;
                live.phase = LoadPhase::Ready;
                live.failure = None;
                context
            }
            Err(error) => {
                live.failure = Some(error);
                live.phase = LoadPhase::Ready;
                None
            }
        };
        if let Some(context) = loaded_context {
            self.on_live_day_context_changed(Some(context), cx);
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
        let task = cx.spawn(async move |this, cx| {
            if let Err(source) = link.load_day(request, generation, mode).await {
                let _ = this.update(cx, |this, cx| {
                    this.day_loaded(generation, Err(LoadDataError::Request { source }), cx)
                });
            }
        });
        if let Some(live) = &mut self.live {
            live.load_task = Some(task);
        }
    }
    pub(super) fn observe_plot(
        &mut self,
        origin_x: f32,
        logical: f32,
        scale: f32,
        cx: &mut Context<Self>,
    ) {
        self.plot_origin_x = Some(origin_x);
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
        let mode = if live.request.is_none() {
            DayLoad::Immediate
        } else {
            DayLoad::Settled
        };
        self.load_request(request, mode, cx);
    }
    pub(super) fn measure_plot(&self, cx: &Context<Self>) -> Canvas<()> {
        let entity = cx.entity();
        canvas(
            move |bounds, window, app| {
                let origin_x = f32::from(bounds.origin.x);
                #[cfg(feature = "preview")]
                let origin_y = f32::from(bounds.origin.y);
                let width = f32::from(bounds.size.width);
                let scale = window.scale_factor();
                window.defer(app, move |_, app| {
                    entity.update(app, |shell, cx| {
                        shell.observe_plot(origin_x, width, scale, cx);
                        #[cfg(feature = "preview")]
                        shell.sync_preview_plot_overlays(origin_x, origin_y, width, cx);
                    })
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
        self.range_select = None;
        self.segment_menu = None;
        let previous = live.request;
        live.generation = live.generation.next();
        live.change_date();
        live.failure = None;
        live.phase = LoadPhase::Loading;
        live.request = None;
        live.timer = None;
        live.indicator = Indicator::Hidden;
        self.day = Day::empty(date);
        self.hover = None;
        self.cancel_details(cx);
        self.on_live_day_context_changed(None, cx);
        self.range = RangeChoice::FullDay;
        if let Some(mut request) = previous {
            request.date = date;
            request.view = DayView::WholeDay;
            self.load_request(request, DayLoad::Immediate, cx);
        }
        cx.notify();
    }
    pub(super) fn stop_bar(&self, ui: &Widgets<'_>, cx: &Context<Self>) -> Option<AnyElement> {
        let snapshot = self.day_snapshot()?;
        if snapshot.recording {
            return None;
        }
        let reason = snapshot.stop_reason?;
        let message = stop_reason_msg(reason);
        let critical = reason == StopReason::PersistenceFailed;
        let colors = ui.info_bar_colors(if critical {
            InfoBarStyle::Critical
        } else {
            InfoBarStyle::Neutral
        });
        let mut bar = ui
            .card()
            .flex()
            .items_center()
            .gap(ui.metrics.space.m)
            .p(ui.metrics.space.l)
            .bg(colors.background)
            .border_color(colors.border)
            .text_color(colors.foreground)
            .child(ui.icon(
                if critical {
                    IconName::CircleX
                } else {
                    IconName::Clock
                },
                Density::Compact,
                colors.icon,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
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
                            .text_color(colors.secondary)
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
    fn resume(&mut self, cx: &mut Context<Self>) {
        let Some(live) = &mut self.live else {
            return;
        };
        live.begin_resume();
        let link = live.link.clone();
        let date = self.day.date();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let result = link
                .request(quartz_protocol::TrackingResumeRequest {})
                .await
                .map(|_| ());
            let _ = this.update(cx, |this, cx| {
                if this.day.date() != date {
                    return;
                }
                if let Some(live) = &mut this.live {
                    live.finish_resume(result);
                }
                cx.notify();
            });
        });
        if let Some(live) = &mut self.live {
            live.resume_task = Some(task);
        }
    }
    pub(super) fn action_notice(&self, ui: &Widgets<'_>) -> Option<AnyElement> {
        let failure = self.live.as_ref()?.actions.failure.as_ref()?;
        Some(
            ui.card()
                .p(ui.metrics.space.l)
                .flex()
                .flex_col()
                .gap(ui.metrics.space.xs)
                .ramp(ui, Ramp::Body)
                .child(failure.action.message(self.formatter))
                .child(
                    div()
                        .ramp(ui, Ramp::Caption)
                        .font(ui.mono_font())
                        .text_color(ui.palette.text_secondary)
                        .child(failure.error.code().to_string()),
                )
                .into_any_element(),
        )
    }
    pub(super) fn live_notice(
        &self,
        ui: &Widgets<'_>,
        frame: &Frame<'_>,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(error) = self.day_failure() {
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
        if self.load_phase() == Some(LoadPhase::Loading) {
            return Some(self.skeleton(ui, frame, cx));
        }
        let snapshot = self.day_snapshot()?;
        if self.day.summary().active.is_zero() && self.day.pending_ranges().is_empty() {
            let today = self.day.date() == snapshot.logical_date;
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
            if today && snapshot.recording {
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
                            ui.skeleton_block(SkeletonTone::Strong)
                                .h(px(VALUE_HEIGHT))
                                .w(px(if label == Msg::StatActive {
                                    ACTIVE_WIDTH
                                } else {
                                    VALUE_WIDTH
                                }))
                                .rounded(ui.metrics.radius.control),
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
                ui.skeleton_block(SkeletonTone::Subtle)
                    .h(px(8.))
                    .w_full()
                    .rounded_full(),
            )
            .child(
                ui.skeleton_block(SkeletonTone::Subtle)
                    .h(px(12.))
                    .w(px(240.))
                    .rounded(ui.metrics.radius.control),
            );
        let label_width = self.label_column_width(ui, frame.measure);
        let timeline = ui
            .card()
            .min_h(px(TIMELINE_HEIGHT))
            .p(ui.metrics.space.l)
            .flex()
            .flex_col()
            .gap(ui.metrics.space.m)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .min_w(px(0.))
                    .items_center()
                    .gap(ui.metrics.space.s)
                    .child(
                        div()
                            .flex_auto()
                            .min_w(px(0.))
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
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .ramp(ui, Ramp::Caption)
                                    .child(self.formatter.text(Msg::TimelineMainTrack)),
                            )
                            .children((0..LANE_COUNT).map(|_| {
                                ui.skeleton_block(SkeletonTone::Subtle)
                                    .h(px(12.))
                                    .w_full()
                                    .rounded(ui.metrics.radius.control)
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
                                ui.skeleton_block(SkeletonTone::Strong)
                                    .h(px(28.))
                                    .w_full()
                                    .rounded(ui.metrics.radius.control),
                            )
                            .children((0..LANE_COUNT).map(|_| {
                                ui.skeleton_block(SkeletonTone::Subtle)
                                    .h(px(18.))
                                    .w_full()
                                    .rounded(ui.metrics.radius.control)
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
                                ui.skeleton_block(SkeletonTone::Strong)
                                    .flex_1()
                                    .max_w(px(30.))
                                    .h(px(height))
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
                        ui.skeleton_block(SkeletonTone::Subtle)
                            .size(px(16.))
                            .rounded(ui.metrics.radius.control),
                    )
                    .child(
                        ui.skeleton_block(SkeletonTone::Strong)
                            .flex_1()
                            .h(px(12.))
                            .rounded(ui.metrics.radius.control),
                    )
                    .child(
                        ui.skeleton_block(SkeletonTone::Subtle)
                            .w(px(56.))
                            .h(px(12.))
                            .rounded(ui.metrics.radius.control),
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
        &mut self,
        action: crate::link::AwayAction,
        range: crate::model::PendingAwaySpan,
        cx: &mut Context<Self>,
    ) {
        let Some(range) = self.day.utc_range(range.span) else {
            return;
        };
        let Some(live) = &mut self.live else {
            return;
        };
        if live.actions.contains(range) {
            return;
        }
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
        if live.actions.begin(range).is_err() {
            return;
        }
        cx.notify();
        let date = self.day.date();
        let request = action.request(range, uuid);
        let link = live.link.clone();
        cx.spawn(async move |this, cx| {
            let result = link.request(request).await.map(|_| ());
            let _ = this.update(cx, |this, cx| {
                let Some(live) = &mut this.live else {
                    return;
                };
                if this.day.date() != date {
                    live.actions.spans.retain(|write| write.range != range);
                    return;
                }
                let succeeded = result.is_ok();
                live.finish_annotation(&this.day, range, action, result);
                if succeeded {
                    this.on_annotations_day_changed(date, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::{AwayAction, RequestCoreError, UtcRange};

    fn page() -> LivePage {
        LivePage {
            link: Link::closed_for_test(),
            snapshot: StateSnapshot {
                logical_date: "1970-01-01".parse().expect("date"),
                foreground: None,
                recording: false,
                stop_reason: Some(StopReason::Paused),
                timed_policies: Default::default(),
                browser_streams: Vec::new(),
                ui_resident: quartz_protocol::ResidentEnabled::Disabled,
                ui_language: quartz_protocol::UiLanguage::English,
                locale: None,
            },
            generation: DayGeneration::default(),
            phase: LoadPhase::Ready,
            failure: None,
            request: None,
            actions: ActionWrites::default(),
            indicator: Indicator::Hidden,
            timer: None,
            load_task: None,
            resume_task: None,
        }
    }
    #[test]
    fn resume_error_is_cleared_by_retry_success_and_recording_recovery() {
        let mut page = page();
        page.finish_resume(Err(RequestCoreError::Timeout));
        assert!(page.actions.failure.is_some());
        page.begin_resume();
        assert!(
            page.actions.failure.is_none(),
            "retry clears the previous Resume notice"
        );
        page.finish_resume(Err(RequestCoreError::Timeout));
        page.finish_resume(Ok(()));
        assert!(
            page.actions.failure.is_none(),
            "successful Resume clears the notice"
        );
        page.finish_resume(Err(RequestCoreError::Timeout));
        page.snapshot.recording = true;
        page.observe_recording();
        assert!(
            page.actions.failure.is_none(),
            "tray recovery clears Resume errors"
        );
    }
    #[test]
    fn date_change_clears_action_errors_and_late_success_reconciles_pending_spans() {
        let mut page = page();
        let range = UtcRange {
            start: 0,
            end: 900_000,
        };
        page.actions.begin(range).expect("submit");
        page.actions
            .finish(range, AwayAction::Rest, Err(RequestCoreError::Timeout));
        page.change_date();
        assert!(
            page.actions.failure.is_none(),
            "navigation and midnight follow clear notices"
        );
        page.actions.begin(range).expect("submit again");
        let updated_day = Day::empty(page.snapshot.logical_date);
        page.actions.reconcile(&updated_day);
        assert!(
            page.actions.contains(range),
            "the write is still in flight during day_loaded"
        );
        page.finish_annotation(&updated_day, range, AwayAction::Rest, Ok(()));
        assert!(
            !page.actions.contains(range),
            "late success observes the already refreshed day"
        );
    }

    #[test]
    fn away_submissions_guard_each_span_until_success_is_reflected_and_allow_failed_retries() {
        let first = UtcRange {
            start: 0,
            end: 900_000,
        };
        let second = UtcRange {
            start: 900_000,
            end: 1_800_000,
        };
        let mut writes = ActionWrites::default();
        writes.begin(first).expect("first write");
        assert!(matches!(writes.begin(first), Err(RequestCoreError::Busy)));
        writes
            .begin(second)
            .expect("another span remains independent");
        writes.finish(first, AwayAction::Rest, Ok(()));
        assert!(
            writes.begin(first).is_err(),
            "success still waits for refreshed data"
        );
        writes.finish(second, AwayAction::Meeting, Err(RequestCoreError::Timeout));
        assert!(!writes.contains(second));
        let failure = writes.failure.as_ref().expect("action failure");
        assert!(matches!(
            failure.action,
            MutationAction::Away(AwayAction::Meeting)
        ));
        assert_eq!(failure.error.code().as_str(), "timeout");
        writes.begin(second).expect("retry failed action");
        writes.reconcile(&Day::empty("1970-01-01".parse().expect("date")));
        assert!(!writes.contains(first));
        assert!(
            writes.contains(second),
            "a refresh cannot release a write still in flight"
        );
    }

    #[test]
    fn annotation_failure_preserves_ready_day_content_and_uses_its_own_notice() {
        let snapshot = StateSnapshot {
            logical_date: "1970-01-01".parse().expect("date"),
            foreground: None,
            recording: true,
            stop_reason: None,
            timed_policies: Default::default(),
            browser_streams: Vec::new(),
            ui_resident: quartz_protocol::ResidentEnabled::Disabled,
            ui_language: quartz_protocol::UiLanguage::English,
            locale: None,
        };
        let mut live = LivePage {
            link: Link::closed_for_test(),
            snapshot,
            generation: DayGeneration::default(),
            phase: LoadPhase::Ready,
            failure: None,
            request: None,
            actions: ActionWrites::default(),
            indicator: Indicator::Hidden,
            timer: None,
            load_task: None,
            resume_task: None,
        };
        let range = UtcRange {
            start: 0,
            end: 900_000,
        };
        live.actions.begin(range).expect("submit");
        live.finish_annotation(
            &Day::empty(live.snapshot.logical_date),
            range,
            AwayAction::Rest,
            Err(RequestCoreError::Timeout),
        );
        assert_eq!(live.phase, LoadPhase::Ready);
        assert!(
            live.failure.is_none(),
            "day-load notice must not replace the loaded content"
        );
        assert!(live.actions.failure.is_some());
        assert!(
            !live.actions.contains(range),
            "failed actions can be retried"
        );
    }

    #[test]
    fn mutation_failures_name_the_action_instead_of_a_day_load_failure() {
        let formatter = Formatter::new(quartz_i18n::Language::English);
        assert_eq!(
            MutationAction::Resume.message(formatter),
            "Couldn’t resume recording. Try again."
        );
        assert!(
            MutationAction::Away(AwayAction::Rest)
                .message(formatter)
                .contains("Break")
        );
        let mut writes = ActionWrites::default();
        let range = UtcRange {
            start: 0,
            end: 900_000,
        };
        writes.begin(range).expect("write");
        writes.finish(range, AwayAction::Rest, Err(RequestCoreError::Timeout));
        assert!(writes.failure.is_some());
    }
}
