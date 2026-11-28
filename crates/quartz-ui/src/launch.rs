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
    prewarm::{
        LastWindowAction, PrewarmAction, PrewarmGate, SessionLifecycle, ShowWindowDecision,
        WindowCloseDecision, WindowPresence,
    },
    text_scale::{LiveTextScale, ReadTextScaleError, TextScaleChange, TextScaleSource},
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Div, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Size, StatefulInteractiveElement as _, Styled as _,
    Window, component::ActiveTheme as _, div, px, size,
};
use quartz_i18n::{Language, ParseLanguageError};
use quartz_protocol::{ControlOp, CoreError, StateSnapshot};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle};
use snafu::{ResultExt as _, Snafu};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{
        IsWindow, IsWindowVisible, SHOW_WINDOW_CMD, SW_HIDE, SW_SHOW, ShowWindow,
    },
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeVisibility {
    Hidden,
    Visible,
}
impl NativeVisibility {
    const fn command(self) -> SHOW_WINDOW_CMD {
        match self {
            Self::Hidden => SW_HIDE,
            Self::Visible => SW_SHOW,
        }
    }
    const fn from_visible(visible: bool) -> Self {
        if visible { Self::Visible } else { Self::Hidden }
    }
}

#[derive(Debug, Snafu)]
enum WindowVisibilityError {
    #[snafu(display("GPUI did not expose a native window handle"))]
    GetWindowHandle { source: HandleError },
    #[snafu(display("the window handle is not a Win32 handle"))]
    RequireWin32Handle,
    #[snafu(display("the Win32 window handle is no longer valid"))]
    InvalidWindowHandle,
    #[snafu(display("Win32 ShowWindow left visibility {actual:?} instead of {expected:?}"))]
    UnexpectedVisibility {
        expected: NativeVisibility,
        actual: NativeVisibility,
    },
}

fn native_hwnd(window: &Window) -> Result<HWND, WindowVisibilityError> {
    let handle = HasWindowHandle::window_handle(window).context(GetWindowHandleSnafu)?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return RequireWin32HandleSnafu.fail();
    };
    Ok(HWND(std::ptr::with_exposed_provenance_mut(
        handle.hwnd.get().cast_unsigned(),
    )))
}

fn set_native_visibility(
    hwnd: HWND,
    visibility: NativeVisibility,
) -> Result<(), WindowVisibilityError> {
    // SAFETY: `IsWindow` safely validates any HWND value without dereferencing it.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return InvalidWindowHandleSnafu.fail();
    }
    // SAFETY: `hwnd` was just validated as a live Win32 window owned by this thread.
    // `ShowWindow`'s return value reports previous visibility rather than success or failure.
    let _ = unsafe { ShowWindow(hwnd, visibility.command()) };
    // SAFETY: `hwnd` is a live Win32 window owned by this thread.
    let actual = NativeVisibility::from_visible(unsafe { IsWindowVisible(hwnd) }.as_bool());
    if actual != visibility {
        return UnexpectedVisibilitySnafu {
            expected: visibility,
            actual,
        }
        .fail();
    }
    Ok(())
}

fn hide_native_window(window: &Window) -> Result<(), WindowVisibilityError> {
    set_native_visibility(native_hwnd(window)?, NativeVisibility::Hidden)
}

fn show_native_window(hwnd: HWND) -> Result<(), WindowVisibilityError> {
    set_native_visibility(hwnd, NativeVisibility::Visible)
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
    fn apply_snapshot(
        &mut self,
        snapshot: StateSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        self.title = self.formatter.text(Msg::AppName);
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
    fn prepare_hide(&mut self, cx: &mut Context<Self>) {
        if let Some(shell) = &self.shell {
            shell.update(cx, |shell, _| shell.prepare_hide());
        }
    }
    fn reopen(
        &mut self,
        connection: ConnectionState,
        formatter: Formatter,
        snapshot: Option<StateSnapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_connection(connection, cx);
        let had_shell = self.shell.is_some();
        if let Some(snapshot) = snapshot {
            self.apply_snapshot(snapshot, window, cx);
        } else {
            self.formatter = formatter;
            self.title = self.formatter.text(Msg::AppName);
            let language = formatter.language();
            language.set_global();
            gpui_kit::component::set_locale(language.code());
        }
        if had_shell && let Some(shell) = &self.shell {
            shell.update(cx, |shell, cx| shell.on_shown(cx));
        }
        if self.link.is_some() {
            self.acknowledge_frame(window, cx);
        }
        cx.notify();
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
            LinkEvent::State(snapshot) => self.apply_snapshot(snapshot, window, cx),
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
                let session = Rc::new_cyclic(|self_ref| {
                    RefCell::new(UiSession {
                        self_ref: self_ref.clone(),
                        deferred_error: Rc::clone(&report),
                        formatter,
                        connection,
                        link,
                        snapshot: None,
                        window: None,
                        gate: PrewarmGate::Hidden,
                        policy,
                        lifecycle: SessionLifecycle::Running,
                        query_generations: Default::default(),
                    })
                });
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
struct UiSession {
    self_ref: Weak<RefCell<Self>>,
    deferred_error: Rc<RefCell<Option<RunUiError>>>,
    formatter: Formatter,
    connection: ConnectionState,
    link: Option<Link>,
    snapshot: Option<StateSnapshot>,
    window: Option<UiWindow>,
    gate: PrewarmGate,
    policy: crate::instance::InstancePolicy,
    lifecycle: SessionLifecycle,
    query_generations: crate::app::QueryGenerations,
}
impl UiSession {
    fn effective_resident(&self) -> quartz_protocol::ResidentEnabled {
        if self.policy == crate::instance::InstancePolicy::CoreError {
            quartz_protocol::ResidentEnabled::Disabled
        } else {
            self.snapshot
                .as_ref()
                .map_or(quartz_protocol::ResidentEnabled::Enabled, |state| {
                    state.ui_resident
                })
        }
    }
    fn close_decision(&self) -> WindowCloseDecision {
        WindowCloseDecision::decide(self.lifecycle, self.effective_resident(), &self.connection)
    }
    fn finish_hide(&mut self, cx: &mut gpui_kit::App) -> WindowCloseDecision {
        if let Some(window) = &self.window {
            window.view.update(cx, |view, cx| view.prepare_hide(cx));
            if let Some(shell) = &window.view.read(cx).shell {
                self.query_generations = shell.read(cx).query_generations();
            }
        }
        self.gate = PrewarmGate::Hidden;
        let Some(link) = &self.link else {
            tracing::warn!("resident UI has no pipe handle; destroying window and exiting");
            self.lifecycle = SessionLifecycle::Stopping;
            return WindowCloseDecision::Destroy;
        };
        if let Err(error) = link.hidden() {
            tracing::warn!(%error, "could not queue UI hidden notification; destroying window and exiting");
            self.lifecycle = SessionLifecycle::Stopping;
            return WindowCloseDecision::Destroy;
        }
        WindowCloseDecision::Hide
    }
    fn destroy_existing_window(&mut self, cx: &mut gpui_kit::App) {
        if let Some(window) = self.window.take() {
            if let Some(shell) = &window.view.read(cx).shell {
                self.query_generations = shell.read(cx).query_generations();
            }
            if let Err(error) = window
                .handle
                .update(cx, |_, window, _| window.remove_window())
            {
                tracing::warn!(%error, "could not destroy previous UI window");
            }
        }
    }
    fn shutdown_and_quit(&mut self, cx: &mut gpui_kit::App) {
        self.lifecycle = SessionLifecycle::Stopping;
        self.destroy_existing_window(cx);
        cx.quit();
    }
    fn create_window(&mut self, cx: &mut gpui_kit::App) -> Result<(), RunUiError> {
        let handle = WindowLaunch {
            formatter: self.formatter,
            connection: self.connection.clone(),
            link: self.link.clone(),
            snapshot: self.snapshot.clone(),
            query_generations: self.query_generations,
        }
        .open(self.self_ref.clone(), cx)?;
        self.window = Some(handle);
        self.gate = PrewarmGate::Opened;
        Ok(())
    }
    fn is_running_opened(&self) -> bool {
        self.lifecycle == SessionLifecycle::Running && self.gate == PrewarmGate::Opened
    }
    fn recover_show_failure(&mut self, cx: &mut gpui_kit::App) {
        if !self.is_running_opened() {
            return;
        }
        self.destroy_existing_window(cx);
        if let Err(error) = self.create_window(cx) {
            tracing::error!(%error, "could not recreate UI window after show failure");
            *self.deferred_error.borrow_mut() = Some(error);
            self.lifecycle = SessionLifecycle::Stopping;
            cx.quit();
        }
    }
    fn reuse_window(&mut self, cx: &mut gpui_kit::App) -> Result<(), RunUiError> {
        let Some(existing) = &self.window else {
            return self.create_window(cx);
        };
        let handle = existing.handle;
        let view = existing.view.clone();
        let connection = self.connection.clone();
        let formatter = self.formatter;
        let snapshot = self.snapshot.clone();
        let prepared = handle.update(cx, |_, window, cx| {
            let hwnd = native_hwnd(window);
            if hwnd.is_ok() {
                view.update(cx, |view, cx| {
                    view.reopen(connection, formatter, snapshot, window, cx);
                });
            }
            hwnd
        });
        let hwnd = match prepared {
            Ok(Ok(hwnd)) => hwnd,
            Ok(Err(error)) => {
                tracing::warn!(%error, "could not read native handle for hidden UI window; recreating window");
                self.destroy_existing_window(cx);
                return self.create_window(cx);
            }
            Err(error) => {
                tracing::warn!(%error, "hidden UI window is no longer available; opening a new window");
                self.window = None;
                return self.create_window(cx);
            }
        };
        self.gate = PrewarmGate::Opened;
        let session = self.self_ref.clone();
        cx.spawn(async move |cx| {
            // Run `ShowWindow(SW_SHOW)` outside GPUI's `AppCell` borrow because `WM_SHOWWINDOW`
            // synchronously invokes `draw_window` on Windows.
            let should_show = session
                .upgrade()
                .is_some_and(|session| session.borrow().is_running_opened());
            if !should_show {
                return;
            }
            if let Err(error) = show_native_window(hwnd) {
                tracing::warn!(%error, "could not show hidden resident UI window; recreating window");
                cx.update(|cx| {
                    if let Some(session) = session.upgrade() {
                        session.borrow_mut().recover_show_failure(cx);
                    }
                });
                return;
            }
            cx.update(|cx| {
                let still_open = session
                    .upgrade()
                    .is_some_and(|session| session.borrow().is_running_opened());
                if !still_open {
                    return;
                }
                if let Err(error) = handle.update(cx, |_, window, cx| {
                    // GPUI restores minimized Windows windows before requesting foreground activation.
                    window.activate_window();
                    cx.activate(true);
                }) {
                    tracing::warn!(%error, "could not activate reused UI window");
                }
            });
        })
        .detach();
        Ok(())
    }
    fn open(&mut self, cx: &mut gpui_kit::App) -> Result<(), RunUiError> {
        match ShowWindowDecision::decide(WindowPresence::from_option(self.window.as_ref())) {
            ShowWindowDecision::CreateWindow => self.create_window(cx),
            ShowWindowDecision::ReuseWindow => self.reuse_window(cx),
        }
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
        let resident = self.effective_resident();
        match LastWindowAction::decide(resident, &self.connection) {
            LastWindowAction::Quit => {
                self.lifecycle = SessionLifecycle::Stopping;
                cx.quit();
            }
            LastWindowAction::HideAndNotify => {
                self.gate = PrewarmGate::Hidden;
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
        match self.gate {
            PrewarmGate::Hidden => match self.gate.receive(&event) {
                PrewarmAction::Wait => {}
                PrewarmAction::Open => self.open(cx)?,
                PrewarmAction::Quit => self.shutdown_and_quit(cx),
            },
            PrewarmGate::Opened => {
                let action = event.window_action();
                if let Some(window) = &self.window {
                    let view = window.view.clone();
                    if let Err(error) = window.handle.update(cx, |_, window, cx| {
                        view.update(cx, |view, cx| view.receive(event, window, cx))
                    }) {
                        tracing::debug!(%error, "UI window closed before event delivery");
                        self.last_window_closed(cx);
                        match action {
                            crate::link::WindowAction::Quit => self.shutdown_and_quit(cx),
                            crate::link::WindowAction::Activate
                                if self.lifecycle == SessionLifecycle::Running =>
                            {
                                self.open(cx)?
                            }
                            crate::link::WindowAction::Activate
                            | crate::link::WindowAction::Observe => {}
                        }
                    }
                } else {
                    self.gate = PrewarmGate::Hidden;
                    match self.gate.receive(&event) {
                        PrewarmAction::Wait => {}
                        PrewarmAction::Open => self.open(cx)?,
                        PrewarmAction::Quit => self.shutdown_and_quit(cx),
                    }
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
    fn open(
        self,
        session: Weak<RefCell<UiSession>>,
        cx: &mut gpui_kit::App,
    ) -> Result<UiWindow, RunUiError> {
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
                window.on_window_should_close(cx, move |window, cx| {
                    let Some(session) = session.upgrade() else {
                        return true;
                    };
                    let decision = {
                        let session = session.borrow();
                        session.close_decision()
                    };
                    let outcome = match decision {
                        WindowCloseDecision::Destroy => WindowCloseDecision::Destroy,
                        WindowCloseDecision::Hide => match hide_native_window(window) {
                            Ok(()) => session.borrow_mut().finish_hide(cx),
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "could not hide resident UI window; falling back to destroying it"
                                );
                                WindowCloseDecision::Destroy
                            }
                        },
                    };
                    matches!(outcome, WindowCloseDecision::Destroy)
                });
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
