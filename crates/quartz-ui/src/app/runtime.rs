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

use super::tokens::{LoadPaletteError, Tokens};
use crate::layout::{MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH};

const THEME_JSON: &str = include_str!("../../themes/fluent.json");

gpui_kit::assets::icon_assets!(
    pub(crate) AppAssets,
    [
        WindowMinimize,
        WindowMaximize,
        WindowRestore,
        WindowClose,
        Menu,
        Calendar,
        CalendarRange,
        Tag,
        Settings,
        ChevronLeft,
        ChevronRight,
        ChevronDown,
        Minus,
        Plus,
        Close,
        Clock,
        Check,
        Shield,
        Trash,
        Code,
        Globe,
        MessageCircle,
        Send,
        SquareTerminal,
        FileText
    ]
);

/// The embedded theme could not be read.
#[derive(Debug, Snafu)]
#[non_exhaustive]
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
    source: anyhow::Error,
}

#[derive(Debug, Deserialize)]
struct ComponentThemes {
    #[serde(flatten)]
    themes: ThemeSet,
}

/// The embedded theme: the gpui-component light and dark themes and the Quartz tokens.
#[derive(Debug)]
pub(crate) struct ThemeBundle {
    components: ThemeSet,
    tokens: Rc<Tokens>,
}

impl ThemeBundle {
    /// Reads the theme compiled into the executable (11 section 9.1).
    pub(crate) fn embedded() -> Result<Self, LoadThemeError> {
        let components: ComponentThemes =
            serde_json::from_str(THEME_JSON).context(DecodeComponentsSnafu)?;
        let tokens = Tokens::from_theme_json(THEME_JSON).context(LoadTokensSnafu)?;
        Ok(Self {
            components: components.themes,
            tokens: Rc::new(tokens),
        })
    }

    /// The Quartz tokens, shared with the windows.
    pub(crate) fn tokens(&self) -> Rc<Tokens> {
        Rc::clone(&self.tokens)
    }

    /// Installs the component themes as gpui-component's light and dark themes.
    fn install(self, cx: &mut App) {
        for theme in self.components.themes {
            let theme = Rc::new(theme);
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
    pub(crate) fn observe(self, window: &mut Window) {
        if self == Self::System {
            window
                .observe_window_appearance(|window, cx| {
                    Theme::sync_system_appearance(Some(window), cx);
                })
                .detach();
        }
    }
}

/// Options of a window with the custom Quartz title bar and the minimum window size of 07
/// section 2.3.
pub(crate) fn window_options(title: String, bounds: Bounds<Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..TitleBar::title_bar_options()
        }),
        ..TitleBar::window_options()
    }
}

/// Starts GPUI in `language` and `appearance` with `theme`, lets `open` open the windows and runs
/// until the last window closes.
///
/// # Errors
/// Returns the error of `open`, which also quits the application.
pub(crate) fn run_gpui<E: 'static>(
    language: Language,
    appearance: Appearance,
    theme: ThemeBundle,
    open: impl FnOnce(&mut App) -> Result<(), E> + 'static,
) -> Result<(), E> {
    language.set_global();
    gpui_kit::component::set_locale(language.code());
    let failure: Rc<Cell<Option<E>>> = Rc::new(Cell::new(None));
    let report = Rc::clone(&failure);
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            LayoutDirection::from_locale(language.code()).set_global(cx);
            theme.install(cx);
            appearance.apply(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
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

    #[test]
    fn embedded_theme_loads() {
        let theme = ThemeBundle::embedded().expect("embedded theme");
        assert_eq!(theme.components.themes.len(), 2);
    }
}
