//! The mock-data preview window used for screenshots, optionally opened off-screen and never
//! activated so it cannot take focus from the desktop.

use std::{cell::Cell, rc::Rc, str::FromStr};

use gpui_kit::{
    App, AppContext as _, Bounds, Pixels, Point, Size, Window, WindowOptions, point, px, size,
};
use quartz_i18n::Language;
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle};
use snafu::{ResultExt as _, Snafu};
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{
        SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, ShowWindow,
    },
};

use super::{
    AppShell, Page, ShellInit,
    runtime::{
        Appearance, GpuiError, GpuiSnafu, LoadThemeError, ThemeBundle, run_gpui, window_options,
    },
};
use crate::{
    format::{Formatter, Msg},
    model::{DetailsSubject, SegmentId},
    sample::SampleDay,
};

/// The default main window size of 07 section 2.
const WINDOW_SIZE: Size<f32> = Size {
    width: 1280.,
    height: 800.,
};
/// Where the hover preview puts the pointer: over the sample's hovered segment.
const HOVER_POSITION: Point<Pixels> = point(px(900.), px(330.));
/// A screen coordinate outside every display.
const FAR_AWAY: i32 = -32000;

/// Which screen state a preview opens in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PreviewState {
    #[default]
    Today,
    /// Today with the details sidebar open on a sample interval.
    TodayDetails,
    /// Today with the tooltip of a sample segment showing.
    TodayHover,
}

/// A preview state name is not one of `today`, `today-details` or `today-hover`.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
#[snafu(display("unknown preview state {name:?}; expected today, today-details or today-hover"))]
pub struct ParsePreviewStateError {
    name: String,
}

impl FromStr for PreviewState {
    type Err = ParsePreviewStateError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "today" => Ok(Self::Today),
            "today-details" => Ok(Self::TodayDetails),
            "today-hover" => Ok(Self::TodayHover),
            _ => ParsePreviewStateSnafu { name }.fail(),
        }
    }
}

/// An appearance name is not one of `system`, `light` or `dark`.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
#[snafu(display("unknown appearance {name:?}; expected system, light or dark"))]
pub struct ParseAppearanceError {
    name: String,
}

impl FromStr for Appearance {
    type Err = ParseAppearanceError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "system" => Ok(Self::System),
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            _ => ParseAppearanceSnafu { name }.fail(),
        }
    }
}

/// Where the preview window is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PreviewPlacement {
    /// Centered and activated, like a normal launch.
    #[default]
    Visible,
    /// Created hidden and unfocused, moved off the desktop, then shown without activation.
    Offscreen,
}

/// Options of the mock-data preview window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PreviewOptions {
    pub language: Language,
    pub appearance: Appearance,
    pub state: PreviewState,
    pub placement: PreviewPlacement,
}

/// Failures while placing the preview window off-screen.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum PlaceWindowError {
    /// GPUI did not provide the native window handle.
    #[snafu(display("could not get the native window handle"))]
    GetHandle { source: HandleError },
    /// The native handle is not a Win32 window.
    #[snafu(display("the native window handle is not a Win32 window"))]
    ForeignHandle,
    /// Win32 could not move the window.
    #[snafu(display("could not move the window off-screen"))]
    MoveWindow { source: windows::core::Error },
}

/// Failures while running the preview window.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum RunPreviewError {
    /// The embedded theme could not be read.
    #[snafu(display("could not load the embedded theme"))]
    LoadTheme { source: LoadThemeError },
    /// GPUI could not create the window.
    #[snafu(display("could not open the preview window"))]
    OpenWindow { source: GpuiError },
    /// The window could not be moved off-screen, so it was never shown.
    #[snafu(display("could not place the preview window off-screen"))]
    PlaceWindow { source: PlaceWindowError },
    /// The off-screen window could not be given its size.
    #[snafu(display("could not resize the preview window"))]
    ResizeWindow { source: GpuiError },
}

/// Moves the hidden `window` outside every display, then shows it without activating it.
fn place_offscreen(window: &Window) -> Result<(), PlaceWindowError> {
    let handle = HasWindowHandle::window_handle(window).context(GetHandleSnafu)?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return ForeignHandleSnafu.fail();
    };
    let hwnd = HWND(std::ptr::with_exposed_provenance_mut(
        handle.hwnd.get().cast_unsigned(),
    ));
    // SAFETY: `hwnd` is the live top-level window of this process, taken from GPUI just now; the
    // call only moves it, and the flags forbid resizing, restacking and activation.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            FAR_AWAY,
            FAR_AWAY,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
        )
    }
    .context(MoveWindowSnafu)?;
    // SAFETY: the same live window handle; SW_SHOWNOACTIVATE shows it without taking focus. The
    // return value is the previous visibility, not an error.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    Ok(())
}

impl PreviewState {
    /// The details subject and hovered segment this state opens on the sample day.
    fn initial_view(self, sample: &SampleDay) -> (Option<DetailsSubject>, Option<SegmentId>) {
        match self {
            Self::Today => (None, None),
            Self::TodayDetails => (Some(DetailsSubject::Segment(sample.details_segment)), None),
            Self::TodayHover => (None, Some(sample.hover_segment)),
        }
    }
}

impl PreviewOptions {
    /// Opens the main window on the sample day and runs until it closes.
    ///
    /// # Errors
    /// Returns theme loading, window creation and placement failures.
    pub fn run(self) -> Result<(), RunPreviewError> {
        let theme = ThemeBundle::embedded().context(LoadThemeSnafu)?;
        let sample = SampleDay::new();
        let (details, hover) = self.state.initial_view(&sample);
        let init = ShellInit {
            day: sample.day,
            formatter: Formatter::new(self.language),
            tokens: theme.tokens(),
            page: Page::Today,
            details,
        };
        run_gpui(self.language, self.appearance, theme, move |cx| {
            self.open_window(init, hover, cx)
        })
    }

    /// Opens the shell window, off-screen when asked, with the tooltip of `hover` showing.
    fn open_window(
        self,
        init: ShellInit,
        hover: Option<SegmentId>,
        cx: &mut App,
    ) -> Result<(), RunPreviewError> {
        let window_size = size(px(WINDOW_SIZE.width), px(WINDOW_SIZE.height));
        // GPUI moves bounds outside every display back onto the primary one, so the window is
        // created on-screen but hidden and moved away before it is first shown.
        let bounds = Bounds::centered(None, window_size, cx);
        let visible = self.placement == PreviewPlacement::Visible;
        let options = WindowOptions {
            focus: visible,
            show: visible,
            ..window_options(init.formatter.text(Msg::AppName), bounds)
        };
        let placement: Rc<Cell<Option<PlaceWindowError>>> = Rc::new(Cell::new(None));
        let placement_failure = Rc::clone(&placement);
        let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
            if self.placement == PreviewPlacement::Offscreen
                && let Err(error) = place_offscreen(window)
            {
                placement_failure.set(Some(error));
            }
            self.appearance.observe(window);
            cx.new(|_| {
                let mut shell = AppShell::new(init);
                if let Some(segment) = hover {
                    shell.show_tooltip(segment, HOVER_POSITION);
                }
                shell
            })
        })
        .context(GpuiSnafu)
        .context(OpenWindowSnafu)?;
        if let Some(error) = placement.take() {
            return Err(RunPreviewError::PlaceWindow { source: error });
        }
        if visible {
            return Ok(());
        }
        // The hidden window comes up with GPUI's default size and the DPI of wherever it was
        // moved; restore the requested content size once GPUI has registered it.
        handle
            .update(cx, |_, window, _| {
                window.resize(window_size);
                window.refresh();
            })
            .context(GpuiSnafu)
            .context(ResizeWindowSnafu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_state_opens_its_sample_segment() {
        let sample = SampleDay::new();
        assert_eq!(PreviewState::Today.initial_view(&sample), (None, None));
        assert_eq!(
            PreviewState::TodayDetails.initial_view(&sample),
            (Some(DetailsSubject::Segment(sample.details_segment)), None)
        );
        assert_eq!(
            PreviewState::TodayHover.initial_view(&sample),
            (None, Some(sample.hover_segment))
        );
    }

    #[test]
    fn preview_names_parse_and_typos_are_rejected() {
        assert_eq!("today-hover".parse(), Ok(PreviewState::TodayHover));
        assert_eq!("dark".parse(), Ok(Appearance::Dark));
        assert!("ligth".parse::<Appearance>().is_err());
        assert!("today-detail".parse::<PreviewState>().is_err());
    }
}
