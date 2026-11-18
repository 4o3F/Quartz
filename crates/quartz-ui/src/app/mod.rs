//! The windows: the main window shell with the Today page, the widgets and tokens they share, and
//! the GPUI start-up.
//!
//! Until the data layer exists the main window only opens in the preview, on the sample day. The
//! data layer later fills the same [`model`](crate::model) types from protocol messages; [`AppShell`]
//! reads only these view models.

mod measure;
mod metrics;
#[cfg(feature = "preview")]
mod preview;
mod runtime;
mod shell;
mod tokens;
mod widgets;

pub(crate) use metrics::Ramp;
#[cfg(feature = "preview")]
pub use preview::{
    ParseAppearanceError, ParsePreviewSizeError, ParsePreviewStateError, PlaceWindowError,
    PreviewOptions, PreviewPlacement, PreviewSize, PreviewState, RunPreviewError,
};
pub use runtime::{Appearance, GpuiError, LoadThemeError};
pub(crate) use runtime::{GpuiSnafu, ThemeBundle, run_gpui, window_options};
pub(crate) use shell::{AppShell, Page, RangeChoice, ShellInit};
pub use tokens::LoadPaletteError;
pub(crate) use tokens::Tokens;
pub(crate) use widgets::{RampExt, Widgets};
