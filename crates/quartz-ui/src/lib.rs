//! The GPUI interface: display, interaction, rule editing and labeling. Within the workspace it
//! depends only on quartz-protocol, quartz-pipe and quartz-i18n; data comes from the core over the protocol, so
//! it never opens the database or matches rules itself.
//!
//! The view models, text formatting and timeline geometry have no GPUI dependency, so their tests
//! run on every platform; the windows themselves are Windows-only. The link API supplies typed
//! requests and ordered events to views through a dedicated pipe thread.

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
#[cfg(any(windows, test))]
mod prewarm;
#[cfg(all(feature = "preview", any(windows, test)))]
mod sample;
#[cfg(any(windows, test))]
mod text_scale;
#[cfg(any(windows, test))]
mod timeline_scale;
#[cfg(windows)]
mod windows_error;

#[cfg(windows)]
pub use windows_error::CallWindowsError;

#[cfg(windows)]
pub use app::{Appearance, CallGpuiError, LoadPaletteError, LoadThemeError};
#[cfg(all(windows, feature = "preview"))]
pub use app::{
    ErrorPreviewState, ParseAppearanceError, ParsePreviewSizeError, ParsePreviewStateError,
    PlaceWindowError, PreviewOptions, PreviewPlacement, PreviewSize, PreviewState, PreviewTheme,
    RunPreviewError, TodayPreviewState,
};
#[cfg(windows)]
pub use launch::{RunOptions, RunUiError};
#[cfg(all(windows, feature = "preview"))]
pub use text_scale::{ParseTextScaleError, TextScale};
#[cfg(windows)]
pub use text_scale::{ReadTextScaleError, ValidateTextScaleError};

#[cfg(test)]
mod tests {
    #[test]
    fn production_library_has_no_crate_wide_lint_suppressions() {
        let source = include_str!("lib.rs")
            .split("\n#[cfg(test)]")
            .next()
            .expect("library declarations");
        for attribute in source.split("#![").skip(1) {
            let attribute = attribute.split(']').next().expect("crate attribute");
            assert!(
                !attribute.contains("allow(") && !attribute.contains("expect("),
                "library-wide lint suppression: {attribute}"
            );
        }
    }
}
