//! The windows: the main window shell with the Today page, the widgets and tokens they share, and
//! the GPUI start-up.
//!
//! Until the data layer exists the main window only opens in the preview, on the sample day. The
//! data layer later fills the same [`model`](crate::model) types from protocol messages; [`AppShell`]
//! reads only these view models.

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
};
pub use runtime::{Appearance, GpuiError, LoadThemeError};
pub(crate) use runtime::{GpuiSnafu, ThemeBundle, run_gpui, window_options};
pub(crate) use shell::{AppShell, Page, RangeChoice, ShellInit};
pub use tokens::LoadPaletteError;
pub(crate) use widgets::{RampExt, Widgets};
