//! Launching the UI process window: the placeholder main window and the startup error window.

use quartz_i18n::{Language, ParseLanguageError};
use quartz_protocol::CoreError;
use snafu::{ResultExt as _, Snafu};

use std::rc::Rc;

use gpui_kit::{
    AppContext as _, Bounds, Context, Div, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Size, StatefulInteractiveElement as _, Styled as _, Window,
    component::ActiveTheme as _, div, px, size,
};

use crate::{
    app::{
        Appearance, GpuiError, GpuiSnafu, LoadThemeError, Ramp, RampExt as _, ThemeBundle, Tokens,
        Widgets, run_gpui, window_options,
    },
    core_error::{CoreErrorPresentation, RecoveryLine},
    format::{Formatter, Msg},
    text_scale::{LiveTextScale, ReadTextScaleError, TextScaleChange, TextScaleSource},
};

/// Size of the placeholder main window.
const MAIN_WINDOW_SIZE: Size<f32> = Size {
    width: 960.,
    height: 640.,
};
/// Size of the startup error window, sized for a message and a few backups.
const ERROR_WINDOW_SIZE: Size<f32> = Size {
    width: 900.,
    height: 620.,
};

/// Selects a visible launch or the M3 prewarm placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOptions {
    /// Opens one window in the language negotiated by the process entry point.
    Open { language: &'static str },
    /// Opens a localized startup error window without connecting to the core.
    CoreError {
        language: &'static str,
        error: CoreError,
    },
    /// Exits successfully without initializing GPUI until prewarming is implemented in M3.
    Prewarm,
}

/// Failures while loading the embedded theme or opening the UI window.
#[derive(Debug, Snafu)]
pub enum RunUiError {
    /// The entry point passed a language code without a translation catalog.
    #[snafu(display("could not select the interface language"))]
    SelectLanguage { source: ParseLanguageError },
    /// The embedded theme could not be read.
    #[snafu(display("could not load the embedded theme"))]
    LoadTheme { source: LoadThemeError },
    /// The Windows text size could not be read.
    #[snafu(display("could not read the text size"))]
    ReadTextScale { source: ReadTextScaleError },
    /// GPUI could not create the application window.
    #[snafu(display("could not open UI window"))]
    OpenWindow { source: GpuiError },
}

/// What the launch window shows.
#[derive(Debug)]
enum WindowContent {
    /// The main window until the main view is connected to the core.
    Placeholder,
    /// Why the core could not start and how to recover.
    CoreError(CoreErrorPresentation),
}

impl WindowContent {
    fn size(&self) -> Size<f32> {
        match self {
            Self::Placeholder => MAIN_WINDOW_SIZE,
            Self::CoreError(_) => ERROR_WINDOW_SIZE,
        }
    }
}

#[derive(Debug)]
struct LaunchWindow {
    tokens: Rc<Tokens>,
    title: String,
    content: WindowContent,
    text_scale: LiveTextScale,
}

impl Render for LaunchWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Rc::clone(&self.tokens);
        let ui = Widgets::new(&tokens, cx.theme().mode, self.text_scale.current());
        let space = &ui.metrics.space;
        let mut content = div()
            .id("launch-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(space.l)
            .p(space.xxxl)
            .w_full();
        if let WindowContent::CoreError(error) = &self.content {
            content = content
                .child(div().ramp(&ui, Ramp::Title).child(error.title.clone()))
                .child(
                    div()
                        .ramp(&ui, Ramp::Body)
                        .whitespace_normal()
                        .child(error.message.clone()),
                )
                .children(error.recovery.iter().map(|line| recovery_line(&ui, line)));
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(ui.palette.bg_base)
            .text_color(ui.palette.text_primary)
            .child(ui.title_bar(self.title.clone()))
            .child(content)
    }
}

/// One recovery line; restore commands are monospaced so they read as something to copy.
fn recovery_line(ui: &Widgets<'_>, line: &RecoveryLine) -> Div {
    let text = div().whitespace_normal();
    match line {
        RecoveryLine::BackupsHeading(text_value) => {
            text.ramp(ui, Ramp::BodyStrong).child(text_value.clone())
        }
        RecoveryLine::BackupPath(path) => {
            text.ramp(ui, Ramp::Body).child(path.display().to_string())
        }
        RecoveryLine::RestoreCommand(command) => text
            .ramp(ui, Ramp::Body)
            .font(ui.mono_font())
            .child(command.clone()),
        RecoveryLine::Warning(warning) => text.ramp(ui, Ramp::Body).child(warning.clone()),
    }
}

impl RunOptions {
    /// Runs the UI on the main thread and exits when its last window closes.
    ///
    /// # Errors
    /// Returns language, theme or window creation failures.
    pub fn run(self) -> Result<(), RunUiError> {
        let (code, error) = match self {
            Self::Open { language } => (language, None),
            Self::CoreError { language, error } => (language, Some(error)),
            // TODO(M3): Initialize D3D without a visible window and implement prewarm TTL and handshake.
            Self::Prewarm => return Ok(()),
        };
        let language: Language = code.parse().context(SelectLanguageSnafu)?;
        let formatter = Formatter::new(language);
        let content = match error {
            Some(error) => WindowContent::CoreError(CoreErrorPresentation::new(&error, formatter)),
            None => WindowContent::Placeholder,
        };
        let theme = ThemeBundle::embedded().context(LoadThemeSnafu)?;
        let tokens = theme.tokens();
        let appearance = Appearance::System;
        run_gpui(language, appearance, theme, move |cx| {
            // Read here, once GPUI has made this thread a single-threaded COM apartment.
            let text_scale =
                LiveTextScale::read(TextScaleSource::System).context(ReadTextScaleSnafu)?;
            let title = formatter.text(Msg::AppName);
            let window_size = content.size();
            let bounds = Bounds::centered(
                None,
                size(px(window_size.width), px(window_size.height)),
                cx,
            );
            gpui_kit::open_window(
                window_options(title.clone(), bounds),
                cx,
                move |window, cx| {
                    appearance.observe(window);
                    cx.new(|cx| {
                        cx.observe_window_activation(window, |this: &mut LaunchWindow, _, cx| {
                            if this.text_scale.refresh() == TextScaleChange::Changed {
                                cx.notify();
                            }
                        })
                        .detach();
                        LaunchWindow {
                            tokens,
                            title,
                            content,
                            text_scale,
                        }
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
            .expect("M0 prewarm must succeed without a window");
    }

    #[test]
    fn an_unknown_language_is_reported_before_any_window_opens() {
        let error = RunOptions::Open { language: "fr" }
            .run()
            .expect_err("no French catalog");
        assert!(matches!(error, RunUiError::SelectLanguage { .. }));
    }
}
