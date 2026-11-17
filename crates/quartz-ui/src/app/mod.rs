//! The main application window: shell, Today page and the view models behind them.
//!
//! Phase 1 runs on mock data ([`model::Day::sample`]). The data layer later fills the same [`model`] types from
//! protocol messages; [`AppShell`] reads only these view models.

pub mod format;
mod mock;
pub mod model;
mod shell;
pub mod tokens;
mod widgets;

use std::{cell::RefCell, rc::Rc};

use gpui_kit::{
    AppContext as _, Bounds, TitlebarOptions, Window, WindowBounds, WindowOptions,
    base::LayoutDirection,
    component::{Theme, ThemeMode, ThemeSet, TitleBar},
    px, size,
};
use serde::Deserialize;
use snafu::{ResultExt, Snafu};

pub use shell::{AppShell, Page, ShellInit};

use self::{
    format::Formatter,
    model::{DetailsSubject, TimeOfDay},
    tokens::{LoadPaletteError, Tokens},
};

const THEME_JSON: &str = include_str!("../../themes/fluent.json");

gpui_kit::assets::icon_assets!(
    pub AppAssets,
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
        FileText,
        AppWindow
    ]
);

/// Which appearance a preview forces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    /// Follow the system setting.
    #[default]
    System,
    Light,
    Dark,
}

/// Which screen state a preview opens in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewState {
    #[default]
    Today,
    /// Today with the details sidebar open on a sample interval.
    TodayDetails,
    /// Today with the tooltip of a sample segment showing.
    TodayHover,
}

/// Moves a freshly created, still hidden window far off the visible desktop and shows it without
/// activation. The preview example supplies it, because the Win32 calls need dev-only dependencies.
pub type PlaceOffscreen = fn(&Window) -> Result<(), PlaceWindowError>;

/// Failures while placing the preview window off-screen.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum PlaceWindowError {
    /// The platform window handle is not available.
    #[snafu(display("could not get the native window handle"))]
    WindowHandle,
    /// A Win32 call failed.
    #[snafu(display("could not place the window off-screen"))]
    Platform { source: anyhow::Error },
}

/// Where the preview window is placed.
#[derive(Debug, Clone, Copy)]
pub enum PreviewPlacement {
    /// Centered and activated, like a normal launch.
    Visible,
    /// Created hidden and unfocused, moved off the desktop, then shown without activation.
    Offscreen(PlaceOffscreen),
}

/// Options of the mock-data preview window.
#[derive(Debug, Clone, Copy)]
pub struct PreviewOptions {
    pub language: &'static str,
    pub theme: ThemeChoice,
    pub state: PreviewState,
    pub placement: PreviewPlacement,
}

/// Failures while starting the preview window.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum RunPreviewError {
    /// The embedded theme could not be decoded for the component library.
    #[snafu(display("could not decode embedded Fluent theme"))]
    DecodeTheme { source: serde_json::Error },
    /// A Quartz token is missing from the theme.
    #[snafu(display("could not read Quartz theme tokens"))]
    LoadTokens { source: LoadPaletteError },
    /// The window could not be moved off-screen, so it was never shown.
    #[snafu(display("could not place the preview window off-screen"))]
    PlaceWindow { source: PlaceWindowError },
    /// GPUI could not create the window.
    #[snafu(display("could not open preview window"))]
    OpenWindow { source: anyhow::Error },
}

#[derive(Debug, Deserialize)]
struct ComponentThemes {
    #[serde(flatten)]
    component: ThemeSet,
}

impl PreviewOptions {
    /// Opens the main window with mock data and runs until it closes.
    ///
    /// # Errors
    /// Returns theme decoding or window creation failures.
    pub fn run(self) -> Result<(), RunPreviewError> {
        let options = self;
        quartz_i18n::set_locale(options.language);
        gpui_kit::component::set_locale(options.language);
        let themes: ComponentThemes = serde_json::from_str(THEME_JSON).context(DecodeThemeSnafu)?;
        let tokens = Tokens::from_theme_json(THEME_JSON).context(LoadTokensSnafu)?;
        let outcome = Rc::new(RefCell::new(Ok(())));
        let launch_outcome = Rc::clone(&outcome);
        gpui_kit::application()
            .with_assets(AppAssets)
            .run(move |cx| {
                gpui_kit::init(cx);
                LayoutDirection::from_locale(options.language).set_global(cx);
                for theme in themes.component.themes {
                    if theme.mode.is_dark() {
                        Theme::global_mut(cx).dark_theme = Rc::new(theme);
                    } else {
                        Theme::global_mut(cx).light_theme = Rc::new(theme);
                    }
                }
                cx.set_global(tokens);
                match options.theme {
                    ThemeChoice::System => Theme::sync_system_appearance(None, cx),
                    ThemeChoice::Light => Theme::change(ThemeMode::Light, None, cx),
                    ThemeChoice::Dark => Theme::change(ThemeMode::Dark, None, cx),
                }
                cx.on_window_closed(|cx, _| {
                    if cx.windows().is_empty() {
                        cx.quit();
                    }
                })
                .detach();
                let window_size = size(px(1280.), px(800.));
                // GPUI moves bounds outside every display back onto the primary one, so the window is
                // created on-screen but hidden and moved away before it is first shown.
                let bounds = Bounds::centered(None, window_size, cx);
                let visible = matches!(options.placement, PreviewPlacement::Visible);
                let window_options = WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: visible,
                    show: visible,
                    titlebar: Some(TitlebarOptions {
                        title: Some(quartz_i18n::translate("app.name").into_owned().into()),
                        ..TitleBar::title_bar_options()
                    }),
                    ..TitleBar::window_options()
                };
                let day = crate::app::model::Day::sample();
                let details = match options.state {
                    PreviewState::Today | PreviewState::TodayHover => None,
                    PreviewState::TodayDetails => day
                        .segments
                        .iter()
                        .find(|segment| segment.span.start == TimeOfDay::hm(16, 35))
                        .map(|segment| DetailsSubject::Segment(segment.id)),
                };
                let hover_segment = (options.state == PreviewState::TodayHover)
                    .then(|| {
                        day.segments
                            .iter()
                            .find(|segment| segment.span.start == TimeOfDay::hm(14, 50))
                            .map(|segment| segment.id)
                    })
                    .flatten();
                let init = ShellInit {
                    day,
                    formatter: Formatter::new(options.language),
                    page: Page::Today,
                    details,
                    hover_segment,
                };
                let follow_system = options.theme == ThemeChoice::System;
                let place_outcome = Rc::clone(&launch_outcome);
                let opened = gpui_kit::open_window(window_options, cx, move |window, cx| {
                    if let PreviewPlacement::Offscreen(place) = options.placement
                        && let Err(error) = place(window)
                    {
                        *place_outcome.borrow_mut() =
                            Err(RunPreviewError::PlaceWindow { source: error });
                        cx.quit();
                    }
                    if follow_system {
                        window
                            .observe_window_appearance(|window, cx| {
                                Theme::sync_system_appearance(Some(window), cx);
                            })
                            .detach();
                    }
                    cx.new(|_| AppShell::new(init))
                })
                .context(OpenWindowSnafu)
                .and_then(|(handle, _)| {
                    if matches!(options.placement, PreviewPlacement::Visible) {
                        return Ok(());
                    }
                    // The hidden window comes up with GPUI's default size and the DPI of wherever it
                    // was moved; restore the requested content size once GPUI has registered it.
                    handle
                        .update(cx, |_, window, _| {
                            window.resize(window_size);
                            window.refresh();
                        })
                        .context(OpenWindowSnafu)
                });
                if let Err(error) = opened {
                    *launch_outcome.borrow_mut() = Err(error);
                    cx.quit();
                }
            });
        outcome.replace(Ok(()))
    }
}
