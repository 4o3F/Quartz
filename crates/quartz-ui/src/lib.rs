//! The GPUI interface: display, interaction, rule editing and labeling. Within the workspace it
//! depends only on quartz-protocol and quartz-i18n; data comes from the core over the protocol, so
//! it never opens the database or matches rules itself.
//!
//! The view models, text formatting and timeline geometry have no GPUI dependency, so their tests
//! run on every platform; the windows themselves are Windows-only. The only public items are what
//! the `quartz` binary and the preview example call.
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
        reason = "nothing builds a day until the protocol mapping exists; only the preview opens the main window"
    )
)]

#[cfg(windows)]
mod app;
#[cfg(any(windows, test))]
mod core_error;
#[cfg(any(windows, test))]
mod format;
#[cfg(windows)]
mod launch;
#[cfg(any(windows, test))]
mod layout;
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
    ParseAppearanceError, ParsePreviewSizeError, ParsePreviewStateError, PlaceWindowError,
    PreviewOptions, PreviewPlacement, PreviewSize, PreviewState, RunPreviewError,
};
#[cfg(windows)]
pub use launch::{RunOptions, RunUiError};
#[cfg(all(windows, feature = "preview"))]
pub use text_scale::{ParseTextScaleError, TextScale};
#[cfg(windows)]
pub use text_scale::{ReadTextScaleError, ValidateTextScaleError};
