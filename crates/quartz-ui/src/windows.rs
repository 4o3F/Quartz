//! Windows blank-window lifecycle for M0.

use std::{cell::RefCell, rc::Rc};

use crate::core_error::CoreErrorPresentation;
use gpui_kit::base::LayoutDirection;
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeSet, TitleBar, label::Label};
use gpui_kit::{
    AppContext as _, Bounds, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, StatefulInteractiveElement as _, Styled as _, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, px, size,
};
use quartz_protocol::CoreError;
use serde::Deserialize;
use snafu::{ResultExt, Snafu};

gpui_kit::assets::icon_assets!(
    CaptionAssets,
    [WindowMinimize, WindowMaximize, WindowRestore, WindowClose]
);

/// Selects a visible launch or the M3 prewarm placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunOptions {
    /// Opens one window using the language negotiated by the process entry point.
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
#[non_exhaustive]
pub enum RunUiError {
    /// The embedded Fluent theme could not be decoded.
    #[snafu(display("could not decode embedded Fluent theme"))]
    DecodeTheme { source: serde_json::Error },
    /// GPUI could not create the application window.
    #[snafu(display("could not open UI window"))]
    OpenWindow { source: anyhow::Error },
}

#[derive(Debug, Deserialize)]
struct FluentTheme {
    #[serde(flatten)]
    component: ThemeSet,
    quartz: WindowTokens,
}

#[derive(Debug, Deserialize)]
struct WindowTokens {
    #[serde(rename = "title_bar.height")]
    title_bar_height: f32,
    #[serde(rename = "space.m")]
    title_padding: f32,
    #[serde(rename = "type.caption")]
    caption: CaptionTokens,
}

#[derive(Debug, Deserialize)]
struct CaptionTokens {
    font_family: String,
    font_size: f32,
    line_height: f32,
}

#[derive(Debug)]
struct BlankWindow {
    tokens: WindowTokens,
    error: Option<CoreErrorPresentation>,
}

impl Render for BlankWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut content = div()
            .id("core-error-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(px(16.))
            .p(px(32.))
            .w_full();
        if let Some(error) = &self.error {
            content = content
                .child(
                    div()
                        .text_size(px(24.))
                        .line_height(px(32.))
                        .child(error.title.clone()),
                )
                .child(
                    div()
                        .text_size(px(15.))
                        .line_height(px(24.))
                        .whitespace_normal()
                        .child(error.message.clone()),
                );
            for text in &error.recovery {
                content = content.child(
                    div()
                        .text_size(px(13.))
                        .line_height(px(20.))
                        .whitespace_normal()
                        .child(text.clone()),
                );
            }
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                TitleBar::new()
                    .h(px(self.tokens.title_bar_height))
                    .pl(px(self.tokens.title_padding))
                    .border_b_0()
                    .bg(theme.background)
                    .child(
                        div()
                            .text_size(px(self.tokens.caption.font_size))
                            .line_height(px(self.tokens.caption.line_height))
                            .font_family(self.tokens.caption.font_family.clone())
                            .text_color(theme.foreground)
                            .child(Label::new(quartz_i18n::translate("app.name").into_owned())),
                    ),
            )
            .child(content)
    }
}

impl RunOptions {
    /// Runs the UI on the main thread and exits when its last window closes.
    ///
    /// # Errors
    /// Returns theme decoding or GPUI window creation failures.
    pub fn run(self) -> Result<(), RunUiError> {
        let options = self;
        let (language, error) = match options {
            RunOptions::Open { language } => (language, None),
            RunOptions::CoreError { language, error } => {
                (language, Some(CoreErrorPresentation::new(&error, language)))
            }
            // TODO(M3): Initialize D3D without a visible window and implement prewarm TTL and handshake.
            RunOptions::Prewarm => return Ok(()),
        };
        quartz_i18n::set_locale(language);
        gpui_kit::component::set_locale(language);
        let themes: FluentTheme = serde_json::from_str(include_str!("../themes/fluent.json"))
            .context(DecodeThemeSnafu)?;
        let outcome = Rc::new(RefCell::new(Ok(())));
        let launch_outcome = Rc::clone(&outcome);
        gpui_kit::application()
            .with_assets(CaptionAssets)
            .run(move |cx| {
                gpui_kit::init(cx);
                LayoutDirection::from_locale(language).set_global(cx);
                for theme in themes.component.themes {
                    if theme.mode.is_dark() {
                        Theme::global_mut(cx).dark_theme = Rc::new(theme);
                    } else {
                        Theme::global_mut(cx).light_theme = Rc::new(theme);
                    }
                }
                Theme::sync_system_appearance(None, cx);
                cx.on_window_closed(|cx, _| {
                    if cx.windows().is_empty() {
                        cx.quit();
                    }
                })
                .detach();
                let options = WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        if error.is_some() {
                            size(px(900.), px(620.))
                        } else {
                            size(px(960.), px(640.))
                        },
                        cx,
                    ))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(quartz_i18n::translate("app.name").into_owned().into()),
                        ..TitleBar::title_bar_options()
                    }),
                    ..TitleBar::window_options()
                };
                if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
                    window
                        .observe_window_appearance(|window, cx| {
                            Theme::sync_system_appearance(Some(window), cx);
                        })
                        .detach();
                    cx.new(|_| BlankWindow {
                        tokens: themes.quartz,
                        error,
                    })
                })
                .context(OpenWindowSnafu)
                {
                    *launch_outcome.borrow_mut() = Err(error);
                    cx.quit();
                }
            });
        outcome.replace(Ok(()))
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
    fn window_failure_preserves_upstream_cause() {
        use std::{error::Error as _, io};

        let source = anyhow::Error::new(io::Error::from(io::ErrorKind::PermissionDenied));
        let error = Err::<(), _>(source)
            .context(OpenWindowSnafu)
            .expect_err("window failure must reach the caller");
        let source = error.source().expect("upstream window error");
        assert_eq!(
            source
                .downcast_ref::<io::Error>()
                .expect("original I/O cause")
                .kind(),
            io::ErrorKind::PermissionDenied,
        );
    }
}
