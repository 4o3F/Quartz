//! The mock-data preview window used for screenshots, optionally opened off-screen and never
//! activated so it cannot take focus from the desktop.

use std::{cell::Cell, num::ParseIntError, rc::Rc, str::FromStr};

use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Pixels, Point,
    Render, Styled as _, Window, WindowOptions, component::ActiveTheme as _, div, point, px, size,
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
        Appearance, CallGpuiError, CallGpuiSnafu, LoadThemeError, ThemeBundle, run_gpui,
        window_options,
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

/// A newer core and protocol deliberately disagree with the UI in the error fixture.
const MISMATCH_CORE_VERSION: &str = "0.1.0";
const MISMATCH_CORE_PROTOCOL: quartz_protocol::ProtoVersion = quartz_protocol::ProtoVersion(2);

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
    /// Today data states with a fixed snapshot and no connection.
    TodayData(TodayPreviewState),
    /// Today with application details.
    TodayAppDetails,
    /// Rules page with builtin and user sets, a disabled user set, an override marker,
    /// and a user set selected with entries.
    Rules,
    /// Rules page Categories tab with an archived category visible and the create editor open.
    RulesCategories,
    /// Rules page with a user classification set open in the editor and one diagnostic shown.
    RuleEditor,
    /// Rules page with a cleanly validated rule set and 7-day impact preview results.
    RuleEditorPreview,
    /// Rules page paste-import pane with pasted text targeting an existing user rule set.
    RuleImport,
    /// Rules page Unclassified tab grouped by application with 5 rows including a hidden entity.
    Unclassified,
    /// Rules page Unclassified tab loading with per-day query progress (120/365).
    Progress,
    /// Today page with the side panel Annotations tab listing 3 annotations, one missing category.
    TodayAnnotations,
    /// Today page with the annotation create editor open and an end-time validation error.
    AnnotationEditor,
    /// Today page with an active 45-minute timeline drag-selection band.
    TimelineSelect,
    /// Today page with the Classify-as menu open over a selected timeline segment.
    ClassifyMenu,
    /// Week page with a realistic 7-day breakdown (including one zero-activity day and one empty
    /// future day).
    Week,
    /// Week page in its initial loading state with per-day progress and static skeletons.
    WeekLoading,
    /// Settings page General card with resident UI enabled (prewarm rows hidden).
    Settings,
    /// Settings page General card with resident UI disabled and prewarm enabled (both prewarm rows shown).
    SettingsResidentOff,
    /// Settings page Data group with database info and 3 backups.
    SettingsData,
    /// Settings page Data group with the date-range deletion confirmation block visible.
    SettingsDeleteConfirm,
    /// Settings page Browser group showing connected, needs-reenable, and not-registered hosts.
    SettingsBrowser,
    /// Settings page About group with versions, core status, and logs directory.
    SettingsAbout,
    /// The shared failure/connecting screen, without a core or pipe.
    Error(ErrorPreviewState),
}

/// Fixture-only Today presentation states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TodayPreviewState {
    Loading,
    EmptyToday,
    EmptyPast,
    Paused,
    PersistenceFailed,
    QueryFailed,
    DatePicker,
    TodayNav,
}

/// Approved error mockup fixtures. All data and recovery actions stay inside the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(test, derive(strum::VariantArray))]
pub enum ErrorPreviewState {
    Connecting,
    CoreNotRunning,
    CoreNotRunningStarting,
    CoreStartFailed,
    ConnectFailed,
    MigrationFailed,
    NewerDatabase,
    VersionMismatch,
    OtherSession,
    IdentityRejected,
    HandshakeRejected,
    InvalidResponse,
    InterfaceUnavailable,
    CoreNotRunningExpanded,
    ConnectFailedExpanded,
    OtherSessionExpanded,
    CoreNotRunningStartingExpanded,
}
impl ErrorPreviewState {
    fn failure(self) -> Option<crate::failure::Failure> {
        use crate::failure::{Failure, FailureKind};
        use quartz_protocol::{CoreError, CoreErrorCode};
        Some(match self {
            Self::Connecting => return None,
            Self::CoreNotRunning
            | Self::CoreNotRunningStarting
            | Self::CoreNotRunningExpanded
            | Self::CoreNotRunningStartingExpanded => Failure::new(FailureKind::CoreNotRunning),
            Self::CoreStartFailed => Failure::new(FailureKind::CoreStartFailed),
            Self::ConnectFailed | Self::ConnectFailedExpanded => {
                Failure::new(FailureKind::ConnectTimeout)
            }
            Self::MigrationFailed => Failure::from_startup(CoreError::new(
                CoreErrorCode::MigrationFailed,
                vec![std::path::PathBuf::from(
                    r"C:\Users\alice\AppData\Local\Quartz\backups\quartz-2026-11-21T093012Z-before-v4.db",
                )],
            )),
            Self::NewerDatabase => Failure::new(FailureKind::NewerDatabase),
            Self::VersionMismatch => {
                Failure::from_connection(&quartz_pipe::CallCoreError::Protocol {
                    kind: quartz_protocol::ErrorKind::ProtoMismatch {
                        supported: quartz_protocol::ProtoRange::new(
                            MISMATCH_CORE_PROTOCOL,
                            MISMATCH_CORE_PROTOCOL,
                        )
                        .expect("preview protocol range"),
                        core_version: quartz_protocol::CoreVersion(MISMATCH_CORE_VERSION.into()),
                    },
                })
            }
            Self::OtherSession | Self::OtherSessionExpanded => {
                Failure::new(FailureKind::OtherSession)
            }
            Self::IdentityRejected => Failure::new(FailureKind::IdentityRejected),
            Self::HandshakeRejected => Failure::new(FailureKind::HandshakeRejected),
            Self::InvalidResponse => Failure::new(FailureKind::InvalidResponse),
            Self::InterfaceUnavailable => Failure::new(FailureKind::InterfaceUnavailable),
        })
    }
}

/// A preview state name does not identify a Today or error fixture.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
#[snafu(display(
    "unknown preview state {name:?}; expected a Today or error-screen state from the mockups README"
))]
pub struct ParsePreviewStateError {
    name: String,
}

impl FromStr for PreviewState {
    type Err = ParsePreviewStateError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "today" => Ok(Self::Today),
            "today-app-details" => Ok(Self::TodayAppDetails),
            "loading" => Ok(Self::TodayData(TodayPreviewState::Loading)),
            "empty-today" => Ok(Self::TodayData(TodayPreviewState::EmptyToday)),
            "empty-past" => Ok(Self::TodayData(TodayPreviewState::EmptyPast)),
            "stopped-paused" => Ok(Self::TodayData(TodayPreviewState::Paused)),
            "stopped-persistence-failed" => {
                Ok(Self::TodayData(TodayPreviewState::PersistenceFailed))
            }
            "query-failed" => Ok(Self::TodayData(TodayPreviewState::QueryFailed)),
            "date-picker" => Ok(Self::TodayData(TodayPreviewState::DatePicker)),
            "today-nav" => Ok(Self::TodayData(TodayPreviewState::TodayNav)),
            "core-not-running-starting-expanded" => Ok(Self::Error(
                ErrorPreviewState::CoreNotRunningStartingExpanded,
            )),
            "core-not-running-expanded" => {
                Ok(Self::Error(ErrorPreviewState::CoreNotRunningExpanded))
            }
            "connect-failed-expanded" => Ok(Self::Error(ErrorPreviewState::ConnectFailedExpanded)),
            "other-session-expanded" => Ok(Self::Error(ErrorPreviewState::OtherSessionExpanded)),
            "today-details" => Ok(Self::TodayDetails),
            "today-hover" => Ok(Self::TodayHover),
            "today-full-day" => Ok(Self::TodayFullDay),
            "rules" => Ok(Self::Rules),
            "rules-categories" => Ok(Self::RulesCategories),
            "rule-editor" => Ok(Self::RuleEditor),
            "rule-editor-preview" => Ok(Self::RuleEditorPreview),
            "rule-import" => Ok(Self::RuleImport),
            "unclassified" => Ok(Self::Unclassified),
            "progress" => Ok(Self::Progress),
            "today-annotations" => Ok(Self::TodayAnnotations),
            "annotation-editor" => Ok(Self::AnnotationEditor),
            "timeline-select" => Ok(Self::TimelineSelect),
            "classify-menu" => Ok(Self::ClassifyMenu),
            "week" => Ok(Self::Week),
            "week-loading" => Ok(Self::WeekLoading),
            "settings" => Ok(Self::Settings),
            "settings-resident-off" => Ok(Self::SettingsResidentOff),
            "settings-data" => Ok(Self::SettingsData),
            "settings-delete-confirm" => Ok(Self::SettingsDeleteConfirm),
            "settings-browser" => Ok(Self::SettingsBrowser),
            "settings-about" => Ok(Self::SettingsAbout),
            "connecting" => Ok(Self::Error(ErrorPreviewState::Connecting)),
            "core-not-running" => Ok(Self::Error(ErrorPreviewState::CoreNotRunning)),
            "core-not-running-starting" => {
                Ok(Self::Error(ErrorPreviewState::CoreNotRunningStarting))
            }
            "core-start-failed" => Ok(Self::Error(ErrorPreviewState::CoreStartFailed)),
            "connect-failed" => Ok(Self::Error(ErrorPreviewState::ConnectFailed)),
            "migration-failed" => Ok(Self::Error(ErrorPreviewState::MigrationFailed)),
            "newer-database" => Ok(Self::Error(ErrorPreviewState::NewerDatabase)),
            "version-mismatch" => Ok(Self::Error(ErrorPreviewState::VersionMismatch)),
            "other-session" => Ok(Self::Error(ErrorPreviewState::OtherSession)),
            "identity-rejected" => Ok(Self::Error(ErrorPreviewState::IdentityRejected)),
            "handshake-rejected" => Ok(Self::Error(ErrorPreviewState::HandshakeRejected)),
            "invalid-response" => Ok(Self::Error(ErrorPreviewState::InvalidResponse)),
            "interface-unavailable" => Ok(Self::Error(ErrorPreviewState::InterfaceUnavailable)),
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
    /// Forces solid Mica fallback or fixed system high contrast colors for screenshots.
    pub material: super::system_theme::PreviewTheme,
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
    MoveWindow {
        #[snafu(source(from(windows::core::Error, crate::CallWindowsError::from)))]
        source: crate::CallWindowsError,
    },
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
    OpenWindow { source: CallGpuiError },
    /// The window could not be moved off-screen, so it was never shown.
    #[snafu(display("could not place the preview window off-screen"))]
    PlaceWindow { source: PlaceWindowError },
    /// The off-screen window could not be given its size.
    #[snafu(display("could not resize the preview window"))]
    ResizeWindow { source: CallGpuiError },
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
    /// The top-level page this preview state opens on.
    const fn page(self) -> Page {
        match self {
            Self::Week | Self::WeekLoading => Page::Week,
            Self::Settings
            | Self::SettingsResidentOff
            | Self::SettingsData
            | Self::SettingsDeleteConfirm
            | Self::SettingsBrowser
            | Self::SettingsAbout => Page::Settings,
            Self::Rules
            | Self::RulesCategories
            | Self::RuleEditor
            | Self::RuleEditorPreview
            | Self::RuleImport
            | Self::Unclassified
            | Self::Progress => Page::Rules,
            Self::Today
            | Self::TodayDetails
            | Self::TodayAppDetails
            | Self::TodayHover
            | Self::TodayFullDay
            | Self::TodayData(_)
            | Self::TodayAnnotations
            | Self::AnnotationEditor
            | Self::TimelineSelect
            | Self::ClassifyMenu
            | Self::Error(_) => Page::Today,
        }
    }

    /// The timeline range this state opens with.
    const fn range(self) -> RangeChoice {
        match self {
            Self::TodayFullDay => RangeChoice::FullDay,
            Self::Today
            | Self::TodayDetails
            | Self::TodayAppDetails
            | Self::TodayData(_)
            | Self::TodayHover
            | Self::Week
            | Self::WeekLoading
            | Self::Settings
            | Self::SettingsResidentOff
            | Self::SettingsData
            | Self::SettingsDeleteConfirm
            | Self::SettingsBrowser
            | Self::SettingsAbout
            | Self::Rules
            | Self::RulesCategories
            | Self::RuleEditor
            | Self::RuleEditorPreview
            | Self::RuleImport
            | Self::Unclassified
            | Self::Progress
            | Self::TodayAnnotations
            | Self::AnnotationEditor
            | Self::TimelineSelect
            | Self::ClassifyMenu
            | Self::Error(_) => RangeChoice::Activity,
        }
    }

    /// The details subject and hovered segment this state opens on the sample day.
    fn initial_view(self, sample: &SampleDay) -> InitialView {
        match self {
            Self::Today
            | Self::TodayFullDay
            | Self::TodayData(_)
            | Self::Week
            | Self::WeekLoading
            | Self::Settings
            | Self::SettingsResidentOff
            | Self::SettingsData
            | Self::SettingsDeleteConfirm
            | Self::SettingsBrowser
            | Self::SettingsAbout
            | Self::Rules
            | Self::RulesCategories
            | Self::RuleEditor
            | Self::RuleEditorPreview
            | Self::RuleImport
            | Self::Unclassified
            | Self::Progress
            | Self::TodayAnnotations
            | Self::AnnotationEditor
            | Self::TimelineSelect
            | Self::ClassifyMenu
            | Self::Error(_) => InitialView::default(),
            Self::TodayAppDetails => InitialView {
                details: sample
                    .day
                    .segment(sample.details_segment)
                    .map(|segment| DetailsSubject::App(segment.app)),
                hover: None,
            },
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
        let mut theme = ThemeBundle::embedded().context(LoadThemeSnafu)?;
        theme.preview = self.material;
        let sample = SampleDay::new();
        let initial = self.state.initial_view(&sample);
        let tokens = theme.tokens();
        let text_scale_source = self
            .text_scale
            .map_or(TextScaleSource::System, TextScaleSource::Fixed);
        run_gpui(
            self.language,
            self.appearance,
            theme,
            super::WindowClosePolicy::Quit,
            move |cx| {
                // The system setting is read through WinRT, which would initialize COM on this thread
                // as a multithreaded apartment if GPUI had not made it a single-threaded one first.
                let init = ShellInit {
                    day: sample.day,
                    formatter: Formatter::new(self.language),
                    tokens,
                    page: self.state.page(),
                    details: initial.details,
                    range: self.state.range(),
                    text_scale: LiveTextScale::read(text_scale_source)
                        .context(ReadTextScaleSnafu)?,
                };
                match self.state {
                    PreviewState::Error(state) => {
                        self.open_error_window(state, init.text_scale, cx)
                    }
                    _ => self.open_window(init, initial.hover, cx),
                }
            },
        )
    }

    fn open_error_window(
        self,
        state: ErrorPreviewState,
        scale: LiveTextScale,
        cx: &mut App,
    ) -> Result<(), RunPreviewError> {
        let window_size = self.size.content_size();
        let visible = self.placement == PreviewPlacement::Visible;
        let formatter = Formatter::new(self.language);
        let options = WindowOptions {
            focus: visible,
            show: visible,
            ..window_options(
                formatter.text(Msg::AppName),
                Bounds::centered(None, window_size, cx),
                cx,
            )
        };
        let placement: Rc<Cell<Option<PlaceWindowError>>> = Rc::new(Cell::new(None));
        let placement_failure = Rc::clone(&placement);
        let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
            if !visible && let Err(error) = place_offscreen(window) {
                placement_failure.set(Some(error));
            }
            self.appearance.observe(window, cx);
            cx.new(|cx| {
                let screen = cx.new(|cx| {
                    let mut screen = super::failure_screen::FailureScreen::new(
                        state.failure(),
                        formatter,
                        scale.current(),
                    );
                    if matches!(
                        state,
                        ErrorPreviewState::CoreNotRunningExpanded
                            | ErrorPreviewState::ConnectFailedExpanded
                            | ErrorPreviewState::OtherSessionExpanded
                            | ErrorPreviewState::CoreNotRunningStartingExpanded
                    ) {
                        screen.expand_preview_details();
                    }
                    if matches!(
                        state,
                        ErrorPreviewState::CoreNotRunningStarting
                            | ErrorPreviewState::CoreNotRunningStartingExpanded
                    ) {
                        screen.set_activity(
                            super::failure_screen::RecoveryActivity::StartingCore,
                            cx,
                        );
                    }
                    screen
                });
                cx.subscribe(&screen, |_, _, action, cx| {
                    if *action == crate::failure::FailureAction::Exit {
                        cx.quit();
                    }
                })
                .detach();
                ErrorPreviewWindow {
                    screen,
                    formatter,
                    scale,
                }
            })
        })
        .context(CallGpuiSnafu)
        .context(OpenWindowSnafu)?;
        if let Some(error) = placement.take() {
            return Err(RunPreviewError::PlaceWindow { source: error });
        }
        if !visible {
            handle
                .update(cx, |_, window, _| {
                    window.resize(window_size);
                    window.refresh();
                })
                .context(CallGpuiSnafu)
                .context(ResizeWindowSnafu)?;
        }
        Ok(())
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
            ..window_options(init.formatter.text(Msg::AppName), bounds, cx)
        };
        let placement: Rc<Cell<Option<PlaceWindowError>>> = Rc::new(Cell::new(None));
        let placement_failure = Rc::clone(&placement);
        let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
            if self.placement == PreviewPlacement::Offscreen
                && let Err(error) = place_offscreen(window)
            {
                placement_failure.set(Some(error));
            }
            self.appearance.observe(window, cx);
            cx.new(|cx| {
                super::system_theme::SystemTheme::observe_activation(window, cx);
                let mut shell = AppShell::new(init, window, cx);
                shell.configure_preview(self.state, window, cx);
                if let Some(segment) = hover {
                    shell.show_tooltip(segment, HOVER_POSITION);
                }
                shell
            })
        })
        .context(CallGpuiSnafu)
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
            .context(CallGpuiSnafu)
            .context(ResizeWindowSnafu)
    }
}

#[derive(Debug)]
struct ErrorPreviewWindow {
    screen: Entity<super::failure_screen::FailureScreen>,
    formatter: Formatter,
    scale: LiveTextScale,
}
impl Render for ErrorPreviewWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = super::system_theme::SystemTheme::tokens(cx);
        let ui = super::Widgets::new(&tokens, cx.theme().mode, self.scale.current());
        self.screen.update(cx, |screen, cx| {
            screen.set_format(self.formatter, self.scale.current(), cx)
        });
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(ui.palette.window_background)
            .text_color(ui.palette.text_primary)
            .child(ui.title_bar(self.formatter.text(Msg::AppName), window))
            .child(div().flex_1().min_h(px(0.)).child(self.screen.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_mismatch_fixture_displays_different_core_and_ui_versions() {
        use crate::failure::{DetailLine, FailurePresentation};

        assert_ne!(MISMATCH_CORE_VERSION, env!("CARGO_PKG_VERSION"));
        assert_ne!(MISMATCH_CORE_PROTOCOL, quartz_protocol::PROTOCOL_VERSION);
        let failure = ErrorPreviewState::VersionMismatch
            .failure()
            .expect("version mismatch fixture");
        let formatter = Formatter::new(Language::English);
        let presentation = FailurePresentation::new(&failure, formatter);
        for expected in [
            DetailLine::Value {
                label: formatter.text(Msg::FailureCoreVersion),
                value: MISMATCH_CORE_VERSION.into(),
            },
            DetailLine::Value {
                label: formatter.text(Msg::FailureUiVersion),
                value: env!("CARGO_PKG_VERSION").into(),
            },
            DetailLine::Value {
                label: formatter.text(Msg::FailureProtocol),
                value: format!("{}–{}", MISMATCH_CORE_PROTOCOL.0, MISMATCH_CORE_PROTOCOL.0),
            },
        ] {
            assert!(presentation.details.contains(&expected));
        }
    }

    #[test]
    fn every_error_fixture_uses_the_shared_model_without_creating_a_window() {
        use strum::VariantArray as _;
        let names = [
            "connecting",
            "core-not-running",
            "core-not-running-starting",
            "core-start-failed",
            "connect-failed",
            "migration-failed",
            "newer-database",
            "version-mismatch",
            "other-session",
            "identity-rejected",
            "handshake-rejected",
            "invalid-response",
            "interface-unavailable",
            "core-not-running-expanded",
            "connect-failed-expanded",
            "other-session-expanded",
            "core-not-running-starting-expanded",
        ];
        assert_eq!(names.len(), ErrorPreviewState::VARIANTS.len());
        for (name, &state) in names.into_iter().zip(ErrorPreviewState::VARIANTS) {
            assert_eq!(name.parse::<PreviewState>(), Ok(PreviewState::Error(state)));
            match state.failure() {
                None => assert_eq!(state, ErrorPreviewState::Connecting),
                Some(failure) => {
                    let text = crate::failure::FailurePresentation::new(
                        &failure,
                        Formatter::new(Language::English),
                    );
                    assert!(!text.title.is_empty());
                    assert!(!text.details.is_empty());
                }
            }
        }
    }

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
    fn data_states_and_application_details_are_typed_fixtures() {
        let states = [
            ("loading", TodayPreviewState::Loading),
            ("empty-today", TodayPreviewState::EmptyToday),
            ("empty-past", TodayPreviewState::EmptyPast),
            ("stopped-paused", TodayPreviewState::Paused),
            (
                "stopped-persistence-failed",
                TodayPreviewState::PersistenceFailed,
            ),
            ("query-failed", TodayPreviewState::QueryFailed),
            ("date-picker", TodayPreviewState::DatePicker),
            ("today-nav", TodayPreviewState::TodayNav),
        ];
        let sample = SampleDay::new();
        for (name, state) in states {
            assert_eq!(
                name.parse::<PreviewState>(),
                Ok(PreviewState::TodayData(state))
            );
            assert_eq!(
                PreviewState::TodayData(state).initial_view(&sample),
                InitialView::default()
            );
        }
        let selected = sample
            .day
            .segment(sample.details_segment)
            .expect("details sample segment");
        assert_eq!(
            "today-app-details".parse::<PreviewState>(),
            Ok(PreviewState::TodayAppDetails)
        );
        assert_eq!(
            PreviewState::TodayAppDetails.initial_view(&sample).details,
            Some(DetailsSubject::App(selected.app))
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

    #[test]
    fn m4_editing_scenarios_parse_and_select_expected_pages() {
        let cases = [
            ("rules", PreviewState::Rules, Page::Rules),
            (
                "rules-categories",
                PreviewState::RulesCategories,
                Page::Rules,
            ),
            ("rule-editor", PreviewState::RuleEditor, Page::Rules),
            (
                "rule-editor-preview",
                PreviewState::RuleEditorPreview,
                Page::Rules,
            ),
            ("rule-import", PreviewState::RuleImport, Page::Rules),
            ("unclassified", PreviewState::Unclassified, Page::Rules),
            ("progress", PreviewState::Progress, Page::Rules),
            (
                "today-annotations",
                PreviewState::TodayAnnotations,
                Page::Today,
            ),
            (
                "annotation-editor",
                PreviewState::AnnotationEditor,
                Page::Today,
            ),
            ("timeline-select", PreviewState::TimelineSelect, Page::Today),
            ("classify-menu", PreviewState::ClassifyMenu, Page::Today),
            ("week", PreviewState::Week, Page::Week),
            ("week-loading", PreviewState::WeekLoading, Page::Week),
            ("settings", PreviewState::Settings, Page::Settings),
            (
                "settings-resident-off",
                PreviewState::SettingsResidentOff,
                Page::Settings,
            ),
            ("settings-data", PreviewState::SettingsData, Page::Settings),
            (
                "settings-delete-confirm",
                PreviewState::SettingsDeleteConfirm,
                Page::Settings,
            ),
            (
                "settings-browser",
                PreviewState::SettingsBrowser,
                Page::Settings,
            ),
            (
                "settings-about",
                PreviewState::SettingsAbout,
                Page::Settings,
            ),
        ];
        let sample = SampleDay::new();
        for (name, state, page) in cases {
            assert_eq!(name.parse::<PreviewState>(), Ok(state), "scenario {name}");
            assert_eq!(state.page(), page, "page for {name}");
            assert_eq!(state.range(), RangeChoice::Activity, "range for {name}");
            assert_eq!(
                state.initial_view(&sample),
                InitialView::default(),
                "initial view for {name}"
            );
        }
    }

    fn extract_ts_string_array<'a>(source: &'a str, marker: &str) -> Vec<&'a str> {
        let after_marker = source
            .split_once(marker)
            .map(|(_, rest)| rest)
            .expect("scenario array marker exists in preview-screenshots.ts");
        let body = after_marker
            .split_once(']')
            .map(|(inside, _)| inside)
            .expect("scenario array closes with ']'");
        let mut items = Vec::new();
        let mut rest = body;
        while let Some((_, after_open)) = rest.split_once('"') {
            let (value, tail) = after_open
                .split_once('"')
                .expect("quoted scenario string is closed");
            items.push(value);
            rest = tail;
        }
        items
    }

    #[test]
    fn every_scenario_in_screenshot_script_parses() {
        let script_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/preview-screenshots.ts");
        let source = std::fs::read_to_string(&script_path)
            .expect("screenshot script exists at workspace scripts/preview-screenshots.ts");

        let scenarios = extract_ts_string_array(&source, "export const scenarios = [");
        assert!(
            !scenarios.is_empty(),
            "preview-screenshots.ts scenarios list must not be empty"
        );
        for name in &scenarios {
            assert!(
                name.parse::<PreviewState>().is_ok(),
                "scenario {name:?} in scripts/preview-screenshots.ts failed to parse in PreviewState"
            );
        }

        let narrow = extract_ts_string_array(&source, "export const narrowScenarios = [");
        assert!(
            !narrow.is_empty(),
            "preview-screenshots.ts narrowScenarios list must not be empty"
        );
        for name in &narrow {
            assert!(
                name.parse::<PreviewState>().is_ok(),
                "narrow scenario {name:?} in scripts/preview-screenshots.ts failed to parse in PreviewState"
            );
        }
    }
}
