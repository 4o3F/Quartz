//! The mock-data preview window used for screenshots, optionally opened off-screen and never
//! activated so it cannot take focus from the desktop.

use std::{cell::Cell, num::ParseIntError, rc::Rc, str::FromStr};

use gpui_kit::{
    App, AppContext as _, Bounds, Pixels, Point, Window, WindowOptions, point, px, size,
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
    AppShell, Page, RangeChoice, ShellInit,
    runtime::{
        Appearance, GpuiError, GpuiSnafu, LoadThemeError, ThemeBundle, run_gpui, window_options,
    },
};
use crate::{
    format::{Formatter, Msg},
    layout::{MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH},
    model::{DetailsSubject, SegmentId},
    sample::SampleDay,
    text_scale::{LiveTextScale, ReadTextScaleError, TextScale, TextScaleSource},
};

/// The default main window width of 07 section 2.
const DEFAULT_WIDTH: u16 = 1280;
/// The default main window height of 07 section 2.
const DEFAULT_HEIGHT: u16 = 800;
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
    /// Today with the timeline showing the whole day, where short intervals become markers.
    TodayFullDay,
}

/// A preview state name is not one of `today`, `today-details`, `today-hover` or `today-full-day`.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
#[snafu(display(
    "unknown preview state {name:?}; expected today, today-details, today-hover or today-full-day"
))]
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
            "today-full-day" => Ok(Self::TodayFullDay),
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

/// The preview window size in logical pixels, at least the minimum window size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreviewSize {
    width: u16,
    height: u16,
}

impl Default for PreviewSize {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        }
    }
}

/// A preview size is not `<width>x<height>` in logical pixels from 640x480 up.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
pub enum ParsePreviewSizeError {
    /// The text has no `x` between two numbers.
    #[snafu(display("preview size {text:?} is not <width>x<height>"))]
    Format { text: String },
    /// A dimension is not a whole number of pixels.
    #[snafu(display("preview size {text:?} has a dimension that is not a whole number"))]
    Dimension { text: String, source: ParseIntError },
    /// The window would be smaller than the minimum window size.
    #[snafu(display(
        "preview size {width}x{height} is below the minimum {MIN_WINDOW_WIDTH}x{MIN_WINDOW_HEIGHT}"
    ))]
    TooSmall { width: u16, height: u16 },
}

impl FromStr for PreviewSize {
    type Err = ParsePreviewSizeError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let Some((width, height)) = text.split_once('x') else {
            return FormatSnafu { text }.fail();
        };
        let width: u16 = width.parse().context(DimensionSnafu { text })?;
        let height: u16 = height.parse().context(DimensionSnafu { text })?;
        if f32::from(width) < MIN_WINDOW_WIDTH || f32::from(height) < MIN_WINDOW_HEIGHT {
            return TooSmallSnafu { width, height }.fail();
        }
        Ok(Self { width, height })
    }
}

impl PreviewSize {
    /// The size as GPUI content size.
    fn content_size(self) -> gpui_kit::Size<Pixels> {
        size(px(f32::from(self.width)), px(f32::from(self.height)))
    }
}

/// Options of the mock-data preview window.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PreviewOptions {
    pub language: Language,
    pub appearance: Appearance,
    pub state: PreviewState,
    pub placement: PreviewPlacement,
    pub size: PreviewSize,
    /// Overrides the Windows "Text size" setting; `None` follows the system.
    pub text_scale: Option<TextScale>,
}

/// Failures while placing the preview window off-screen.
#[derive(Debug, Snafu)]
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
pub enum RunPreviewError {
    /// The Windows text size could not be read.
    #[snafu(display("could not read the text size"))]
    ReadTextScale { source: ReadTextScaleError },
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

/// Initial details panel subject and hovered segment of a preview window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct InitialView {
    details: Option<DetailsSubject>,
    hover: Option<SegmentId>,
}

impl PreviewState {
    /// The timeline range this state opens with.
    const fn range(self) -> RangeChoice {
        match self {
            Self::TodayFullDay => RangeChoice::FullDay,
            Self::Today | Self::TodayDetails | Self::TodayHover => RangeChoice::Activity,
        }
    }

    /// The details subject and hovered segment this state opens on the sample day.
    fn initial_view(self, sample: &SampleDay) -> InitialView {
        match self {
            Self::Today | Self::TodayFullDay => InitialView::default(),
            Self::TodayDetails => InitialView {
                details: Some(DetailsSubject::Segment(sample.details_segment)),
                hover: None,
            },
            Self::TodayHover => InitialView {
                details: None,
                hover: Some(sample.hover_segment),
            },
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
        let initial = self.state.initial_view(&sample);
        let tokens = theme.tokens();
        let text_scale_source = self
            .text_scale
            .map_or(TextScaleSource::System, TextScaleSource::Fixed);
        run_gpui(self.language, self.appearance, theme, move |cx| {
            // The system setting is read through WinRT, which would initialize COM on this thread
            // as a multithreaded apartment if GPUI had not made it a single-threaded one first.
            let init = ShellInit {
                day: sample.day,
                formatter: Formatter::new(self.language),
                tokens,
                page: Page::Today,
                details: initial.details,
                range: self.state.range(),
                text_scale: LiveTextScale::read(text_scale_source).context(ReadTextScaleSnafu)?,
            };
            self.open_window(init, initial.hover, cx)
        })
    }

    /// Opens the shell window, off-screen when asked, with the tooltip of `hover` showing.
    fn open_window(
        self,
        init: ShellInit,
        hover: Option<SegmentId>,
        cx: &mut App,
    ) -> Result<(), RunPreviewError> {
        let window_size = self.size.content_size();
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
            cx.new(|cx| {
                let mut shell = AppShell::new(init, window, cx);
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
        assert_eq!(
            PreviewState::Today.initial_view(&sample),
            InitialView::default()
        );
        assert_eq!(
            PreviewState::TodayDetails.initial_view(&sample),
            InitialView {
                details: Some(DetailsSubject::Segment(sample.details_segment)),
                hover: None,
            }
        );
        assert_eq!(
            PreviewState::TodayHover.initial_view(&sample),
            InitialView {
                details: None,
                hover: Some(sample.hover_segment),
            }
        );
    }

    #[test]
    fn only_the_full_day_state_changes_the_timeline_range() {
        assert_eq!("today-full-day".parse(), Ok(PreviewState::TodayFullDay));
        assert_eq!(PreviewState::TodayFullDay.range(), RangeChoice::FullDay);
        assert_eq!(PreviewState::TodayDetails.range(), RangeChoice::Activity);
    }

    #[test]
    fn preview_sizes_parse_strictly_and_respect_the_minimum() {
        assert_eq!(
            "960x700".parse(),
            Ok(PreviewSize {
                width: 960,
                height: 700
            })
        );
        assert_eq!(
            "640x480".parse(),
            Ok(PreviewSize {
                width: 640,
                height: 480
            })
        );
        assert!(matches!(
            "639x480".parse::<PreviewSize>(),
            Err(ParsePreviewSizeError::TooSmall { width: 639, .. })
        ));
        assert!(matches!(
            "640x479".parse::<PreviewSize>(),
            Err(ParsePreviewSizeError::TooSmall { height: 479, .. })
        ));
        assert!(matches!(
            "960".parse::<PreviewSize>(),
            Err(ParsePreviewSizeError::Format { .. })
        ));
        assert!(matches!(
            "wide x tall".parse::<PreviewSize>(),
            Err(ParsePreviewSizeError::Dimension { .. })
        ));
        assert_eq!(
            PreviewSize::default().content_size(),
            size(px(1280.), px(800.))
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
