//! Main and startup windows share the approved failure screen.

use crate::{
    app::failure_screen::{FailureScreen, RecoveryActivity},
    app::{
        Appearance, GpuiError, GpuiSnafu, LoadThemeError, Ramp, RampExt as _, ThemeBundle, Widgets,
        run_gpui, window_options,
    },
    failure::{Failure, FailureAction, FailureKind, Severity},
    format::{Arg, Formatter, Msg},
    link::{ConnectionState, Link, LinkBridge, LinkEvent},
    text_scale::{LiveTextScale, ReadTextScaleError, TextScaleChange, TextScaleSource},
};
use gpui_kit::{
    AppContext as _, Bounds, Context, Div, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Size, StatefulInteractiveElement as _, Styled as _, Task, Window,
    component::ActiveTheme as _, div, px, size,
};
use quartz_i18n::{Language, ParseLanguageError};
use quartz_protocol::{AppIdentity, ControlOp, CoreError, StateSnapshot};
use snafu::{ResultExt as _, Snafu};

/// Size of the existing placeholder window.
const MAIN_WINDOW_SIZE: Size<f32> = Size {
    width: 960.,
    height: 640.,
};

/// Selects a visible launch, startup failure, or the M3-E prewarm placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOptions {
    Open {
        language: &'static str,
    },
    CoreError {
        language: &'static str,
        error: CoreError,
    },
    Prewarm,
}
/// Failures that prevent creating any UI window.
#[derive(Debug, Snafu)]
pub enum RunUiError {
    #[snafu(display("could not select the interface language"))]
    SelectLanguage { source: ParseLanguageError },
    #[snafu(display("could not load the embedded theme"))]
    LoadTheme { source: LoadThemeError },
    #[snafu(display("could not read the text size"))]
    ReadTextScale { source: ReadTextScaleError },
    #[snafu(display("could not open UI window"))]
    OpenWindow { source: GpuiError },
}

#[derive(Debug, Clone, Copy)]
enum LinkAction {
    Retry,
    StartCore,
}

#[derive(Debug)]
pub(crate) struct LaunchWindow {
    title: String,
    formatter: Formatter,
    text_scale: LiveTextScale,
    connection: ConnectionState,
    link: Option<Link>,
    snapshot: Option<StateSnapshot>,
    screen: Entity<FailureScreen>,
    activity: RecoveryActivity,
    _events: Option<Task<()>>,
}
impl Render for LaunchWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = crate::app::system_theme::SystemTheme::tokens(cx);
        let ui = Widgets::new(&tokens, cx.theme().mode, self.text_scale.current());
        self.screen.update(cx, |screen, cx| {
            screen.set_format(self.formatter, self.text_scale.current(), cx)
        });
        let content = match &self.connection {
            ConnectionState::Failed(_) | ConnectionState::Connecting => div()
                .flex_1()
                .min_h(px(0.))
                .child(self.screen.clone())
                .into_any_element(),
            ConnectionState::Connected { core_version, .. } => {
                let mut content =
                    self.content(&ui)
                        .child(
                            div()
                                .ramp(&ui, Ramp::Title)
                                .child(self.formatter.text(Msg::LinkConnected)),
                        )
                        .child(div().ramp(&ui, Ramp::Body).child(self.formatter.text_with(
                            Msg::LinkCoreVersion,
                            &[Arg::Version.with(&core_version.0)],
                        )));
                if let Some(snapshot) = &self.snapshot {
                    let recording = self.formatter.text(if snapshot.recording {
                        Msg::LinkRecording
                    } else {
                        Msg::LinkStopped
                    });
                    let foreground = match snapshot
                        .foreground
                        .as_ref()
                        .and_then(|foreground| foreground.app.as_ref())
                    {
                        Some(
                            AppIdentity::Exe(app)
                            | AppIdentity::Aumid(app)
                            | AppIdentity::Unresolved(app),
                        ) => self
                            .formatter
                            .text_with(Msg::LinkForeground, &[Arg::App.with(app)]),
                        None => self.formatter.text(Msg::LinkNoForeground),
                    };
                    content = content
                        .child(div().ramp(&ui, Ramp::BodyStrong).child(recording))
                        .child(
                            div()
                                .ramp(&ui, Ramp::Body)
                                .whitespace_normal()
                                .child(foreground),
                        );
                }
                content.into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(ui.palette.window_background)
            .text_color(ui.palette.text_primary)
            .child(ui.title_bar(self.title.clone(), window))
            .child(content)
    }
}
impl LaunchWindow {
    fn content(&self, ui: &Widgets<'_>) -> gpui_kit::Stateful<Div> {
        div()
            .id("launch-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(ui.metrics.space.l)
            .p(ui.metrics.space.xxxl)
            .w_full()
    }
    fn act(&mut self, action: FailureAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            FailureAction::CopyDetails => {} // The screen owns clipboard operations.
            FailureAction::Exit => Self::quit(cx),
            FailureAction::RetryConnection => self.run_action(LinkAction::Retry, window, cx),
            FailureAction::StartCore | FailureAction::RetryStartCore => {
                self.run_action(LinkAction::StartCore, window, cx)
            }
        }
    }
    fn run_action(&mut self, action: LinkAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(link) = self.link.clone() else {
            self.set_connection(
                ConnectionState::Failed(Failure::new(FailureKind::InterfaceUnavailable)),
                cx,
            );
            cx.notify();
            return;
        };
        match action {
            LinkAction::Retry => {
                self.connection = ConnectionState::Connecting;
                self.screen
                    .update(cx, |screen, cx| screen.set_failure(None, cx));
            }
            LinkAction::StartCore => {
                self.activity = RecoveryActivity::StartingCore;
                self.screen.update(cx, |screen, cx| {
                    screen.set_activity(RecoveryActivity::StartingCore, cx)
                });
            }
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = match action {
                LinkAction::Retry => link
                    .retry()
                    .await
                    .map_err(|error| Failure::from_request(&error)),
                LinkAction::StartCore => link
                    .start_core()
                    .await
                    .map_err(|error| Failure::from_start(&error)),
            };
            if let Err(failure) = result
                && this
                    .update_in(cx, |this, _, cx| {
                        this.set_connection(ConnectionState::Failed(failure), cx);
                        cx.notify();
                    })
                    .is_err()
            {
                tracing::debug!("UI closed during failure action");
            }
        })
        .detach();
    }
    fn acknowledge_frame(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(link) = self.link.clone() else {
            return;
        };
        cx.on_next_frame(window, move |_, _, cx| {
            cx.spawn(async move |_, _| {
                if let Err(error) = link.first_frame().await {
                    tracing::warn!(%error, "could not queue UI first frame");
                }
            })
            .detach();
        });
    }
    fn quit(cx: &mut Context<Self>) {
        cx.defer(|cx| {
            for handle in cx.windows() {
                if let Err(error) = handle.update(cx, |_, window, _| window.remove_window()) {
                    tracing::warn!(%error, "could not close UI window");
                }
            }
            cx.quit();
        });
    }
    fn set_connection(&mut self, state: ConnectionState, cx: &mut Context<Self>) {
        let failure = match &state {
            ConnectionState::Failed(failure) => Some(failure.clone()),
            ConnectionState::Connecting | ConnectionState::Connected { .. } => None,
        };
        self.screen
            .update(cx, |screen, cx| screen.set_failure(failure, cx));
        self.activity = RecoveryActivity::Idle;
        self.snapshot = None;
        self.connection = state;
    }
    fn receive(&mut self, event: LinkEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.exits_ui() {
            Self::quit(cx);
            return;
        }
        match event {
            LinkEvent::Connection(state) => {
                if !(matches!(state, ConnectionState::Connecting)
                    && self.activity == RecoveryActivity::StartingCore)
                {
                    self.set_connection(state, cx);
                }
            }
            LinkEvent::State(snapshot) => {
                let language = match snapshot.ui_language {
                    quartz_protocol::UiLanguage::English => Language::English,
                    quartz_protocol::UiLanguage::SimplifiedChinese => Language::SimplifiedChinese,
                };
                #[cfg(debug_assertions)]
                let language = if self.formatter.language() == Language::PseudoEnglish {
                    Language::PseudoEnglish
                } else {
                    language
                };
                self.formatter = Formatter::new(language);
                language.set_global();
                gpui_kit::component::set_locale(language.code());
                self.snapshot = Some(snapshot);
            }
            LinkEvent::Control(ControlOp::UiShow | ControlOp::UiActivate) => {
                window.activate_window();
                cx.activate(true);
                self.acknowledge_frame(window, cx);
            }
            LinkEvent::Day(changed) => {
                tracing::debug!(date = %changed.date, "UI day data changed");
                // TODO(M3-D1): Invalidate and re-request the displayed day's snapshots.
            }
            LinkEvent::Control(ControlOp::UiShutdown) | LinkEvent::Disconnected => Self::quit(cx),
            LinkEvent::Control(_) => tracing::warn!("received a non-UI control operation"),
        }
        cx.notify();
    }
}
impl RunOptions {
    /// Runs one visible window until the last window closes or its established pipe disconnects.
    ///
    /// # Errors
    /// Returns language, theme, text-scale or GPUI window failures.
    pub fn run(self) -> Result<(), RunUiError> {
        let (code, failure) = match self {
            Self::Open { language } => (language, None),
            Self::CoreError { language, error } => (language, Some(Failure::from_startup(error))),
            // TODO(M3-E): Initialize D3D without a visible window and implement prewarming.
            Self::Prewarm => return Ok(()),
        };
        let language: Language = code.parse().context(SelectLanguageSnafu)?;
        let formatter = Formatter::new(language);
        let bridge = match &failure {
            None => Some(LinkBridge::start()),
            Some(failure) if failure.severity() == Severity::Recoverable => {
                Some(LinkBridge::for_failure())
            }
            Some(_) => None,
        };
        let mut connection = match failure {
            Some(failure) => ConnectionState::Failed(failure),
            None => ConnectionState::Connecting,
        };
        let bridge = match bridge {
            Some(Ok(bridge)) => Some(bridge),
            Some(Err(error)) => {
                tracing::warn!(%error, "could not start UI pipe thread");
                connection =
                    ConnectionState::Failed(Failure::new(FailureKind::InterfaceUnavailable));
                None
            }
            None => None,
        };
        let theme = ThemeBundle::embedded().context(LoadThemeSnafu)?;
        let appearance = Appearance::System;
        run_gpui(language, appearance, theme, move |cx| {
            let text_scale =
                LiveTextScale::read(TextScaleSource::System).context(ReadTextScaleSnafu)?;
            let title = formatter.text(Msg::AppName);
            let bounds = Bounds::centered(
                None,
                size(px(MAIN_WINDOW_SIZE.width), px(MAIN_WINDOW_SIZE.height)),
                cx,
            );
            gpui_kit::open_window(
                window_options(title.clone(), bounds, cx),
                cx,
                move |window, cx| {
                    appearance.observe(window, cx);
                    cx.new(|cx| {
                        cx.observe_window_activation(window, |this: &mut LaunchWindow, _, cx| {
                            crate::app::system_theme::SystemTheme::refresh(cx);
                            if this.text_scale.refresh() == TextScaleChange::Changed {
                                cx.notify();
                            }
                        })
                        .detach();
                        let failure = match &connection {
                            ConnectionState::Failed(failure) => Some(failure.clone()),
                            ConnectionState::Connecting | ConnectionState::Connected { .. } => None,
                        };
                        let screen = cx
                            .new(|_| FailureScreen::new(failure, formatter, text_scale.current()));
                        cx.subscribe_in(
                            &screen,
                            window,
                            |this: &mut LaunchWindow, _, action, window, cx| {
                                this.act(*action, window, cx)
                            },
                        )
                        .detach();
                        let mut view = LaunchWindow {
                            title,
                            formatter,
                            text_scale,
                            connection,
                            link: bridge.as_ref().map(|bridge| bridge.link.clone()),
                            snapshot: None,
                            screen,
                            activity: RecoveryActivity::Idle,
                            _events: None,
                        };
                        if let Some(bridge) = bridge {
                            let mut events = bridge.events;
                            view._events = Some(cx.spawn_in(window, async move |this, cx| {
                                while let Some(event) = events.recv().await {
                                    if this
                                        .update_in(cx, |this, window, cx| {
                                            this.receive(event, window, cx)
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }));
                            view.acknowledge_frame(window, cx);
                        }
                        view
                    })
                },
            )
            .context(GpuiSnafu)
            .context(OpenWindowSnafu)
            .map(|_| ())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prewarm_exits_without_starting_a_window_loop() {
        RunOptions::Prewarm
            .run()
            .expect("prewarm placeholder has no window");
    }
    #[test]
    fn an_unknown_language_is_reported_before_any_window_opens() {
        assert!(matches!(
            RunOptions::Open { language: "fr" }.run(),
            Err(RunUiError::SelectLanguage { .. })
        ));
    }
}
