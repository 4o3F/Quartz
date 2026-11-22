//! The GPUI interface: display, interaction, rule editing and labeling. Within the workspace it
//! depends only on quartz-protocol, quartz-pipe and quartz-i18n; data comes from the core over the protocol, so
//! it never opens the database or matches rules itself.
//!
//! The view models, text formatting and timeline geometry have no GPUI dependency, so their tests
//! run on every platform; the windows themselves are Windows-only. The link API supplies typed
//! requests and ordered events to views through a dedicated pipe thread.
#![cfg_attr(
    all(test, not(windows)),
    expect(
        dead_code,
        unused_imports,
        reason = "the views that use most of the model only compile on Windows"
    )
)]
#![cfg_attr(
    all(windows, not(test), not(feature = "preview")),
    expect(
        dead_code,
        unused_imports,
        reason = "domain and document models remain available to preview before M4 and M5"
    )
)]

#[cfg(windows)]
mod app;
#[cfg(any(windows, test))]
pub mod failure;
#[cfg(any(windows, test))]
mod format;
#[cfg(any(windows, test))]
mod instance;
#[cfg(windows)]
mod launch;
#[cfg(any(windows, test))]
mod layout;
#[cfg(any(windows, test))]
pub mod link;
#[cfg(any(windows, test))]
mod model;
#[cfg(all(feature = "preview", any(windows, test)))]
mod sample;
#[cfg(any(windows, test))]
mod text_scale;
#[cfg(any(windows, test))]
mod timeline_scale;

#[cfg(windows)]
pub use app::{Appearance, GpuiError, LoadPaletteError, LoadThemeError};
#[cfg(all(windows, feature = "preview"))]
pub use app::{
    ErrorPreviewState, ParseAppearanceError, ParsePreviewSizeError, ParsePreviewStateError,
    PlaceWindowError, PreviewOptions, PreviewPlacement, PreviewSize, PreviewState, PreviewTheme,
    RunPreviewError,
};
#[cfg(windows)]
pub use launch::{RunOptions, RunUiError};
#[cfg(all(windows, feature = "preview"))]
pub use text_scale::{ParseTextScaleError, TextScale};
#[cfg(windows)]
pub use text_scale::{ReadTextScaleError, ValidateTextScaleError};
