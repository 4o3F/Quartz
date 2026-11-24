//! The windows: the main window shell with the Today page, the widgets and tokens they share, and
//! the GPUI start-up.
//!
//! The real window maps core protocol responses into [`model`](crate::model); the preview supplies
//! fixture models. [`AppShell`] shares layout and drawing between both inputs.

mod data_pattern;
pub(crate) mod failure_screen;
mod icons;
pub(crate) mod system_theme;
#[cfg(feature = "preview")]
pub use system_theme::PreviewTheme;
mod measure;
pub(crate) mod metrics;
#[cfg(feature = "preview")]
mod preview;
mod runtime;
mod shell;
mod theme_mapping;
mod tokens;
pub(crate) mod widgets;

pub(crate) use metrics::Ramp;
#[cfg(feature = "preview")]
pub use preview::{
    ErrorPreviewState, ParseAppearanceError, ParsePreviewSizeError, ParsePreviewStateError,
    PlaceWindowError, PreviewOptions, PreviewPlacement, PreviewSize, PreviewState, RunPreviewError,
    TodayPreviewState,
};
pub use runtime::{Appearance, CallGpuiError, LoadThemeError};
pub(crate) use runtime::{CallGpuiSnafu, ThemeBundle, WindowClosePolicy, run_gpui, window_options};
pub(crate) use shell::{AppShell, Page, QueryGenerations, RangeChoice, ShellInit};
pub use tokens::LoadPaletteError;
pub(crate) use widgets::{RampExt, Widgets};
