//! Main and startup windows share the approved failure screen.

use crate::app::{AppShell, Page, RangeChoice, ShellInit};
use crate::model::Day;
use crate::{
    app::failure_screen::{FailureScreen, RecoveryActivity},
    app::{
        Appearance, CallGpuiError, CallGpuiSnafu, LoadThemeError, ThemeBundle, Widgets, run_gpui,
        window_options,
    },
    failure::{Failure, FailureAction, FailureKind, Severity},
    format::{Formatter, Msg},
    link::{ConnectionState, Link, LinkBridge, LinkEvent},
    text_scale::{LiveTextScale, ReadTextScaleError, TextScaleChange, TextScaleSource},
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Div, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Size, StatefulInteractiveElement as _, Styled as _,
    Window, component::ActiveTheme as _, div, px, size,
};
use quartz_i18n::{Language, ParseLanguageError};
use quartz_protocol::{ControlOp, CoreError, StateSnapshot};
use snafu::{ResultExt as _, Snafu};
use std::{cell::RefCell, rc::Rc};

/// Initial size of the main shell window.
const MAIN_WINDOW_SIZE: Size<f32> = Size {
    width: 1280.,
    height: 800.,
};

/// Selects an immediate window, a startup failure, or hidden GPUI prewarming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOptions {
    Open {
        language: &'static str,
    },
    CoreError {
        language: &'static str,
        error: CoreError,
    },
    Prewarm {
        language: &'static str,
    },
}
/// Failures that prevent creating any UI window.
#[derive(Debug, Snafu)]
pub enum RunUiError {
    #[snafu(display("could not acquire UI instance"))]
    AcquireInstance {
        source: quartz_pipe::AcquireUiInstanceError,
    },
    #[snafu(display("could not select the interface language"))]
    SelectLanguage { source: ParseLanguageError },
    #[snafu(display("could not load the embedded theme"))]
    LoadTheme { source: LoadThemeError },
    #[snafu(display("could not read the text size"))]
    ReadTextScale { source: ReadTextScaleError },
    #[snafu(display("could not open UI window"))]
    OpenWindow { source: CallGpuiError },
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
    shell: Option<Entity<AppShell>>,
    screen: Entity<FailureScreen>,
    activity: RecoveryActivity,
    query_generations: crate::app::QueryGenerations,
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
            ConnectionState::Connected { .. } => {
                if let Some(shell) = &self.shell {
                    return div().size_full().child(shell.clone()).into_any_element();
                }
                self.content(&ui)
                    .child(self.formatter.text(Msg::LinkConnecting))
                    .into_any_element()
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
            .into_any_element()
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
        match event.window_action() {
            crate::link::WindowAction::Quit => {
                Self::quit(cx);
                return;
            }
            crate::link::WindowAction::Activate => {
                // GPUI restores minimized Windows windows before requesting foreground activation.
                window.activate_window();
                cx.activate(true);
                self.acknowledge_frame(window, cx);
                cx.notify();
                return;
            }
            crate::link::WindowAction::Observe => {}
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
                if let Some(shell) = &self.shell {
                    shell.update(cx, |shell, cx| {
                        shell.state_changed(snapshot.clone(), self.formatter, cx)
                    });
                } else if let Some(link) = self.link.clone() {
                    let init = ShellInit {
                        day: Day::empty(snapshot.logical_date),
                        formatter: self.formatter,
                        tokens: Rc::new(
                            crate::app::system_theme::SystemTheme::tokens(cx)
                                .as_ref()
                                .clone(),
                        ),
                        page: Page::Today,
                        details: None,
                        range: RangeChoice::FullDay,
                        text_scale: self.text_scale,
                    };
                    let shell = cx.new(|cx| {
                        let mut shell = AppShell::new(init, window, cx);
                        shell.connect(link, snapshot.clone(), cx);
                        shell.resume_queries(self.query_generations);
                        shell
                    });
                    self.shell = Some(shell);
                }
                self.snapshot = Some(snapshot);
            }
            LinkEvent::DetailLoading { generation } => {
                if let Some(shell) = &self.shell {
                    shell.update(cx, |shell, cx| shell.detail_loading(generation, cx));
                }
            }
            LinkEvent::DetailProgress {
                generation,
                progress,
            } => {
                if let Some(shell) = &self.shell {
                    shell.update(cx, |shell, cx| {
                        shell.detail_progress(generation, progress, cx)
                    });
                }
            }
            LinkEvent::DetailLoaded { generation, result } => {
                if let Some(shell) = &self.shell {
                    shell.update(cx, |shell, cx| shell.detail_loaded(generation, result, cx));
                }
            }
            LinkEvent::Day(changed) => {
                if let Some(shell) = &self.shell {
                    shell.update(cx, |shell, cx| shell.day_changed(changed.date, cx));
                }
            }
            LinkEvent::DayLoading { generation } => {
                if let Some(shell) = &self.shell {
                    shell.update(cx, |shell, cx| shell.day_loading(generation, cx));
                }
            }
            LinkEvent::DayLoaded { generation, result } => {
                if let Some(shell) = &self.shell {
                    shell.update(cx, |shell, cx| shell.day_loaded(generation, result, cx));
                }
            }
            LinkEvent::Control(ControlOp::UiShutdown) | LinkEvent::Disconnected => Self::quit(cx),
            LinkEvent::Control(_) => tracing::warn!("received a non-UI control operation"),
        }
        cx.notify();
    }
}
impl RunOptions {
    fn instance_policy(&self) -> crate::instance::InstancePolicy {
        match self {
            Self::CoreError { .. } => crate::instance::InstancePolicy::CoreError,
            Self::Open { .. } | Self::Prewarm { .. } => crate::instance::InstancePolicy::Required,
        }
    }

    /// Initializes GPUI, optionally waits without a window, and exits when its UI lifecycle ends.
    ///
    /// # Errors
    /// Returns mutex, language, theme, text-scale or GPUI window failures.
    pub fn run(self) -> Result<(), RunUiError> {
        let policy = self.instance_policy();
        let initial = if matches!(self, Self::Prewarm { .. }) {
            InitialWindow::Hidden
        } else {
            InitialWindow::Visible
        };
        let code = match &self {
            Self::Open { language }
            | Self::Prewarm { language }
            | Self::CoreError { language, .. } => *language,
        };
        let failure = match self {
            Self::CoreError { error, .. } => Some(Failure::from_startup(error)),
            Self::Open { .. } | Self::Prewarm { .. } => None,
        };
        let language: Language = code.parse().context(SelectLanguageSnafu)?;
        // Startup error windows must be shown even while a normal UI holds the session mutex.
        let _instance = if matches!(policy, crate::instance::InstancePolicy::CoreError) {
            None
        } else {
            let outcome = quartz_pipe::UiInstance::acquire().context(AcquireInstanceSnafu)?;
            if crate::instance::InstanceDecision::from(&outcome).forward(|| {
                quartz_pipe::CoreClient::connect().and_then(|mut client| client.open_ui())
            }) == crate::instance::InstanceDecision::ForwardAndExit
            {
                return Ok(());
            }
            Some(outcome)
        };
        let formatter = Formatter::new(language);
        let theme = ThemeBundle::embedded().context(LoadThemeSnafu)?;
        let appearance = Appearance::System;
        let deferred_error = Rc::new(RefCell::new(None));
        let report = Rc::clone(&deferred_error);
        let result = run_gpui(
            language,
            appearance,
            theme,
            crate::app::WindowClosePolicy::Managed,
            move |cx| {
                let bridge = match &failure {
                    None => Some(match initial {
                        InitialWindow::Visible => LinkBridge::start(),
                        InitialWindow::Hidden => LinkBridge::prewarm(),
                    }),
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
                        connection = ConnectionState::Failed(Failure::new(
                            FailureKind::InterfaceUnavailable,
                        ));
                        None
                    }
                    None => None,
                };

                let link = bridge.as_ref().map(|bridge| bridge.link.clone());
                let session = Rc::new(RefCell::new(UiSession {
                    formatter,
                    connection,
                    link,
                    snapshot: None,
                    window: None,
                    gate: crate::prewarm::PrewarmGate::Hidden,
                    policy,
                    lifecycle: SessionLifecycle::Running,
                    query_generations: Default::default(),
                }));
                let closed = Rc::clone(&session);
                cx.on_window_closed(move |cx, _| {
                    let closed = Rc::clone(&closed);
                    cx.defer(move |cx| {
                        if cx.windows().is_empty() {
                            closed.borrow_mut().last_window_closed(cx);
                        }
                    });
                })
                .detach();
                if initial == InitialWindow::Visible {
                    session.borrow_mut().open(cx)?;
                } else if bridge.is_none() {
                    tracing::warn!("hidden UI link unavailable; exiting");
                    cx.quit();
                    return Ok(());
                }
                if let Some(mut bridge) = bridge {
                    cx.spawn(async move |cx| {
                        while let Some(event) = bridge.events.recv().await {
                            cx.update(|cx| {
                                if let Err(error) = session.borrow_mut().receive(event, cx) {
                                    tracing::error!(%error, "could not reopen UI window");
                                    *report.borrow_mut() = Some(error);
                                    cx.quit();
                                }
                            });
                        }
                        cx.update(|cx| cx.quit());
                    })
                    .detach();
                }
                Ok(())
            },
        );
        result?;
        let error = deferred_error.borrow_mut().take();
        error.map_or(Ok(()), Err)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitialWindow {
    Visible,
    Hidden,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionLifecycle {
    Running,
    Stopping,
}
struct UiSession {
    formatter: Formatter,
    connection: ConnectionState,
    link: Option<Link>,
    snapshot: Option<StateSnapshot>,
    window: Option<UiWindow>,
    gate: crate::prewarm::PrewarmGate,
    policy: crate::instance::InstancePolicy,
    lifecycle: SessionLifecycle,
    query_generations: crate::app::QueryGenerations,
}
impl UiSession {
    fn open(&mut self, cx: &mut gpui_kit::App) -> Result<(), RunUiError> {
        let handle = WindowLaunch {
            formatter: self.formatter,
            connection: self.connection.clone(),
            link: self.link.clone(),
            snapshot: self.snapshot.clone(),
            query_generations: self.query_generations,
        }
        .open(cx)?;
        self.window = Some(handle);
        self.gate = crate::prewarm::PrewarmGate::Opened;
        Ok(())
    }
    fn last_window_closed(&mut self, cx: &mut gpui_kit::App) {
        if self.lifecycle == SessionLifecycle::Stopping {
            self.window = None;
            cx.quit();
            return;
        }
        let Some(window) = self.window.take() else {
            return;
        };
        if let Some(shell) = &window.view.read(cx).shell {
            self.query_generations = shell.read(cx).query_generations();
        }
        drop(window);
        let resident = if self.policy == crate::instance::InstancePolicy::CoreError {
            quartz_protocol::ResidentEnabled::Disabled
        } else {
            self.snapshot
                .as_ref()
                .map_or(quartz_protocol::ResidentEnabled::Enabled, |state| {
                    state.ui_resident
                })
        };
        match crate::prewarm::LastWindowAction::decide(resident, &self.connection) {
            crate::prewarm::LastWindowAction::Quit => {
                self.lifecycle = SessionLifecycle::Stopping;
                cx.quit();
            }
            crate::prewarm::LastWindowAction::HideAndNotify => {
                self.gate = crate::prewarm::PrewarmGate::Hidden;
                if let Some(link) = &self.link {
                    if let Err(error) = link.hidden() {
                        tracing::warn!(%error, "could not queue UI hidden notification");
                        self.lifecycle = SessionLifecycle::Stopping;
                        cx.quit();
                    }
                } else {
                    tracing::warn!("resident UI has no pipe handle; exiting");
                    cx.quit();
                }
            }
        }
    }
    fn receive(&mut self, event: LinkEvent, cx: &mut gpui_kit::App) -> Result<(), RunUiError> {
        match &event {
            LinkEvent::Connection(connection) => self.connection = connection.clone(),
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
                self.snapshot = Some(snapshot.clone());
            }
            LinkEvent::Disconnected | LinkEvent::Control(ControlOp::UiShutdown) => {
                self.lifecycle = SessionLifecycle::Stopping
            }
            LinkEvent::Day(_)
            | LinkEvent::DayLoading { .. }
            | LinkEvent::DayLoaded { .. }
            | LinkEvent::DetailLoading { .. }
            | LinkEvent::DetailProgress { .. }
            | LinkEvent::DetailLoaded { .. }
            | LinkEvent::Control(ControlOp::UiShow | ControlOp::UiActivate) => {}
        }
        let action = event.window_action();
        if let Some(window) = &self.window {
            let view = window.view.clone();
            if let Err(error) = window.handle.update(cx, |_, window, cx| {
                view.update(cx, |view, cx| view.receive(event, window, cx))
            }) {
                tracing::debug!(%error, "UI window closed before event delivery");
                self.last_window_closed(cx);
                match action {
                    crate::link::WindowAction::Quit => {
                        self.lifecycle = SessionLifecycle::Stopping;
                        cx.quit();
                    }
                    crate::link::WindowAction::Activate
                        if self.lifecycle == SessionLifecycle::Running =>
                    {
                        self.open(cx)?
                    }
                    crate::link::WindowAction::Activate | crate::link::WindowAction::Observe => {}
                }
            }
        } else {
            match self.gate.receive(&event) {
                crate::prewarm::PrewarmAction::Wait => {}
                crate::prewarm::PrewarmAction::Open => self.open(cx)?,
                crate::prewarm::PrewarmAction::Quit => {
                    self.lifecycle = SessionLifecycle::Stopping;
                    cx.quit();
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct UiWindow {
    handle: AnyWindowHandle,
    view: Entity<LaunchWindow>,
}

#[derive(Debug)]
struct WindowLaunch {
    formatter: Formatter,
    connection: ConnectionState,
    link: Option<Link>,
    snapshot: Option<StateSnapshot>,
    query_generations: crate::app::QueryGenerations,
}
impl WindowLaunch {
    fn open(self, cx: &mut gpui_kit::App) -> Result<UiWindow, RunUiError> {
        let Self {
            formatter,
            connection,
            link,
            snapshot,
            query_generations,
        } = self;
        let appearance = Appearance::System;
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
                    crate::app::system_theme::SystemTheme::observe_activation(window, cx);
                    cx.observe_window_activation(window, |this: &mut LaunchWindow, _, cx| {
                        if this.text_scale.refresh() == TextScaleChange::Changed {
                            cx.notify();
                        }
                    })
                    .detach();
                    let failure = match &connection {
                        ConnectionState::Failed(failure) => Some(failure.clone()),
                        ConnectionState::Connecting | ConnectionState::Connected { .. } => None,
                    };
                    let screen =
                        cx.new(|_| FailureScreen::new(failure, formatter, text_scale.current()));
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
                        link,
                        snapshot: None,
                        shell: None,
                        screen,
                        activity: RecoveryActivity::Idle,
                        query_generations,
                    };
                    if view.link.is_some() {
                        view.acknowledge_frame(window, cx);
                    }
                    if let Some(snapshot) = snapshot {
                        view.receive(LinkEvent::State(snapshot), window, cx);
                    }
                    view
                })
            },
        )
        .context(CallGpuiSnafu)
        .context(OpenWindowSnafu)
        .map(|opened| UiWindow {
            handle: opened.0,
            view: opened.1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn core_error_window_is_exempt_from_normal_ui_mutex() {
        use crate::instance::InstancePolicy;
        assert_eq!(
            RunOptions::Open { language: "en" }.instance_policy(),
            InstancePolicy::Required
        );
        assert_eq!(
            RunOptions::Prewarm { language: "en" }.instance_policy(),
            InstancePolicy::Required
        );
        assert_eq!(
            RunOptions::CoreError {
                language: "en",
                error: CoreError::new(quartz_protocol::CoreErrorCode::CoreStartFailed, vec![])
            }
            .instance_policy(),
            InstancePolicy::CoreError
        );
    }
    #[test]
    fn an_unknown_language_is_reported_before_any_window_opens() {
        assert!(matches!(
            RunOptions::Open { language: "fr" }.run(),
            Err(RunUiError::SelectLanguage { .. })
        ));
    }
}
