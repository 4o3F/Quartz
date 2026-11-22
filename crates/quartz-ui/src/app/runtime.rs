//! GPUI start-up shared by every window: the embedded theme, icon assets, appearance and the run
//! loop that returns the first error of its windows.

use std::{cell::Cell, rc::Rc};

use gpui_kit::{
    App, Bounds, Pixels, TitlebarOptions, Window, WindowBounds, WindowOptions,
    base::LayoutDirection,
    component::{Theme, ThemeMode, ThemeSet, TitleBar},
    px, size,
};
use quartz_i18n::Language;
use serde::Deserialize;
use snafu::{ResultExt as _, Snafu};

use super::{
    icons::AppAssets,
    system_theme::SystemTheme,
    theme_mapping::ComponentMapping,
    tokens::{LoadPaletteError, QuartzSection, Tokens},
};
use crate::layout::{MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH};

const THEME_JSON: &str = include_str!("../../themes/fluent.json");

/// The embedded theme could not be read.
#[derive(Debug, Snafu)]
pub enum LoadThemeError {
    /// The component themes are not valid gpui-component theme JSON.
    #[snafu(display("could not decode the embedded Fluent component themes"))]
    DecodeComponents { source: serde_json::Error },
    /// A Quartz token is missing or invalid.
    #[snafu(display("could not read the Quartz theme tokens"))]
    LoadTokens { source: LoadPaletteError },
}

/// A GPUI platform call failed; GPUI reports its errors as `anyhow` values.
#[derive(Debug, Snafu)]
#[snafu(
    display("the GPUI platform layer reported an error"),
    visibility(pub(crate))
)]
pub struct GpuiError {
    #[snafu(source(from(anyhow::Error, GpuiError::capture)))]
    source: anyhow::Error,
}
impl GpuiError {
    fn capture(source: anyhow::Error) -> anyhow::Error {
        // Window errors can contain COM error information; return owned diagnostics after GPUI exits.
        anyhow::anyhow!("{source:#}")
    }
}

#[derive(Debug, Deserialize)]
struct ComponentThemes {
    #[serde(flatten)]
    themes: ThemeSet,
    quartz: QuartzSection,
}

/// The embedded theme: the gpui-component light and dark themes and the Quartz tokens.
#[derive(Debug)]
pub(crate) struct ThemeBundle {
    pub(super) components: ThemeSet,
    pub(super) mapping: ComponentMapping,
    #[cfg(feature = "preview")]
    pub(super) preview: super::system_theme::PreviewTheme,
    pub(super) tokens: Rc<Tokens>,
}

impl ThemeBundle {
    /// Reads the theme compiled into the executable (11 section 9.1).
    pub(crate) fn embedded() -> Result<Self, LoadThemeError> {
        let file: ComponentThemes =
            serde_json::from_str(THEME_JSON).context(DecodeComponentsSnafu)?;
        let mapping = file.quartz.component_mapping.clone();
        let tokens = Tokens::from_section(file.quartz).context(LoadTokensSnafu)?;
        Ok(Self {
            components: file.themes,
            mapping,
            tokens: Rc::new(tokens),
            #[cfg(feature = "preview")]
            preview: super::system_theme::PreviewTheme::System,
        })
    }

    /// The Quartz tokens, shared with the windows.
    pub(crate) fn tokens(&self) -> Rc<Tokens> {
        Rc::clone(&self.tokens)
    }

    /// Installs the component themes as gpui-component's light and dark themes.
    pub(super) fn install(&self, cx: &mut App) {
        for theme in &self.components.themes {
            let theme = Rc::new(theme.clone());
            if theme.mode.is_dark() {
                Theme::global_mut(cx).dark_theme = theme;
            } else {
                Theme::global_mut(cx).light_theme = theme;
            }
        }
    }
}

/// Which appearance the windows use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Appearance {
    /// Follow the Windows light or dark setting, also while running.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    fn apply(self, cx: &mut App) {
        match self {
            Self::System => Theme::sync_system_appearance(None, cx),
            Self::Light => Theme::change(ThemeMode::Light, None, cx),
            Self::Dark => Theme::change(ThemeMode::Dark, None, cx),
        }
    }

    /// Makes `window` follow later system appearance changes when the appearance is `System`.
    pub(crate) fn observe(self, window: &mut Window, cx: &mut App) {
        if self == Self::System {
            window
                .observe_window_appearance(|window, cx| {
                    SystemTheme::refresh(cx);
                    Theme::sync_system_appearance(Some(window), cx);
                    SystemTheme::apply_components(cx);
                })
                .detach();
        }
        SystemTheme::observe_window(window, cx);
    }
}

/// Options of a window with the custom Quartz title bar and the minimum window size of 07
/// section 2.3.
pub(crate) fn window_options(title: String, bounds: Bounds<Pixels>, cx: &App) -> WindowOptions {
    WindowOptions {
        window_background: SystemTheme::window_appearance(cx),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..TitleBar::title_bar_options()
        }),
        ..TitleBar::window_options()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowClosePolicy {
    Quit,
    Managed,
}
impl WindowClosePolicy {
    fn quit_mode(self) -> gpui_kit::QuitMode {
        match self {
            Self::Quit => gpui_kit::QuitMode::LastWindowClosed,
            Self::Managed => gpui_kit::QuitMode::Explicit,
        }
    }
}

/// Starts GPUI in `language` and `appearance` with `theme`, lets `open` open the windows and runs
/// until the selected window lifecycle quits the application.
///
/// # Errors
/// Returns the error of `open`, which also quits the application.
pub(crate) fn run_gpui<E: 'static>(
    language: Language,
    appearance: Appearance,
    theme: ThemeBundle,
    close_policy: WindowClosePolicy,
    open: impl FnOnce(&mut App) -> Result<(), E> + 'static,
) -> Result<(), E> {
    language.set_global();
    gpui_kit::component::set_locale(language.code());
    let failure: Rc<Cell<Option<E>>> = Rc::new(Cell::new(None));
    let report = Rc::clone(&failure);
    gpui_kit::application()
        .with_assets(AppAssets)
        .with_quit_mode(close_policy.quit_mode())
        .run(move |cx| {
            gpui_kit::init(cx);
            LayoutDirection::from_locale(language.code()).set_global(cx);
            #[cfg(feature = "preview")]
            cx.set_global(theme.preview);
            SystemTheme::install(theme, cx);
            appearance.apply(cx);
            SystemTheme::apply_components(cx);
            if let Err(error) = open(cx) {
                report.set(Some(error));
                cx.quit();
            }
        });
    failure.take().map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Debug, Snafu)]
    #[snafu(display("native window failure"))]
    struct OpenNativeWindowError {
        drops: Arc<AtomicUsize>,
    }
    impl Drop for OpenNativeWindowError {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn returned_gpui_error_releases_native_source_before_application_teardown() {
        let drops = Arc::new(AtomicUsize::new(0));
        let source = anyhow::Error::new(OpenNativeWindowError {
            drops: Arc::clone(&drops),
        })
        .context("creating GPUI window");
        let result = Err::<(), _>(source).context(GpuiSnafu);
        let error = result.expect_err("window error");
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            format!("{:#}", error.source),
            "creating GPUI window: native window failure"
        );
        drop(error);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn managed_lifecycle_disables_gpui_automatic_quit_on_last_window_close() {
        assert_eq!(
            WindowClosePolicy::Managed.quit_mode(),
            gpui_kit::QuitMode::Explicit
        );
        assert_eq!(
            WindowClosePolicy::Quit.quit_mode(),
            gpui_kit::QuitMode::LastWindowClosed
        );
    }

    #[test]
    fn embedded_theme_loads() {
        let theme = ThemeBundle::embedded().expect("embedded theme");
        assert_eq!(theme.components.themes.len(), 2);
    }
}
