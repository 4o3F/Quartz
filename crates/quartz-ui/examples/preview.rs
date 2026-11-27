//! Opens the main window on a synthetic fixture state without starting a core or pipe.
//!
//! `QUARTZ_PREVIEW_STATE` values:
//! - Today views: `today`, `today-details`, `today-app-details`, `today-hover`, `today-full-day`.
//! - Today data states: `loading`, `empty-today`, `empty-past`, `stopped-paused`,
//!   `stopped-persistence-failed`, `query-failed`, `date-picker`, `today-nav`.
//! - M4 editing flows: `rules`, `rules-categories`, `rule-editor`, `rule-editor-preview`,
//!   `rule-import`, `unclassified`, `today-annotations`, `annotation-editor`,
//!   `timeline-select`, `classify-menu`.
//! - Error states (shared error component with sample diagnostics): `connecting`,
//!   `core-not-running`, `core-not-running-starting`, `core-start-failed`, `connect-failed`,
//!   `migration-failed`, `newer-database`, `version-mismatch`, `other-session`,
//!   `identity-rejected`, `handshake-rejected`, `invalid-response`, `interface-unavailable`,
//!   `core-not-running-expanded`, `connect-failed-expanded`, `other-session-expanded`,
//!   `core-not-running-starting-expanded`.
//!
//! Other environment variables:
//! `QUARTZ_PREVIEW_THEME=system|light|dark`, `QUARTZ_PREVIEW_LANG=zh-CN|en|en-XA`,
//! `QUARTZ_PREVIEW_OFFSCREEN=0|1` (with 1 the window is created hidden, moved off every display
//! and shown without activation), `QUARTZ_PREVIEW_SIZE=<width>x<height>` in logical pixels from
//! 640x480 up (default 1280x800) and `QUARTZ_PREVIEW_TEXT_SCALE=<factor>` from 1.0 to 2.25
//! (default: the Windows "Text size" setting). Unset variables take the first value; any other
//! value is an error written to standard error.
//!
//! `QUARTZ_PREVIEW_MICA_FALLBACK=0|1` forces the solid background; `QUARTZ_PREVIEW_HIGH_CONTRAST=0|1` uses the preview-only fixed system-color palette. High contrast takes precedence. Both switches exist only with the `preview` feature.
//!
//! Build and capture the appearance/language matrix without starting the core (run in a Windows terminal):
//! ```text
//! cargo build -p quartz-ui --example preview --features preview --target-dir target/windows
//! deno task preview-screenshots --mica-wallpaper=reference
//! ```
//! The final capture activates the preview and reads desktop pixels. `--mica-wallpaper=reference` temporarily applies a blue wallpaper for that capture and restores the original wallpaper before returning. `--mica-wallpaper=current` keeps the current wallpaper. Earlier captures use the off-screen window helper.
//!
//! `QUARTZ_PREVIEW_DIAGNOSTICS=<path>` records the actual root, sidebar and title-bar colors and transparency after rendering; the capture helper writes `real-mica-layers.json`.
//!
//! GUI subsystem on Windows so launching it never opens a console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod preview {
    use std::{env, error::Error, ffi::OsString, str::FromStr};

    use quartz_i18n::{Language, ParseLanguageError};
    use quartz_ui::{
        Appearance, ParseAppearanceError, ParsePreviewSizeError, ParsePreviewStateError,
        ParseTextScaleError, PreviewOptions, PreviewPlacement, PreviewSize, PreviewState,
        PreviewTheme, RunPreviewError, TextScale,
    };
    use snafu::{IntoError, ResultExt as _, Snafu};

    /// A preview environment variable has a value the preview does not understand.
    #[derive(Debug, Snafu)]
    pub enum ReadPreviewEnvError {
        #[snafu(display("QUARTZ_PREVIEW_THEME"))]
        Theme { source: ParseAppearanceError },
        #[snafu(display("QUARTZ_PREVIEW_STATE"))]
        State { source: ParsePreviewStateError },
        #[snafu(display("QUARTZ_PREVIEW_LANG"))]
        Language { source: ParseLanguageError },
        #[snafu(display("QUARTZ_PREVIEW_SIZE"))]
        Size { source: ParsePreviewSizeError },
        #[snafu(display("QUARTZ_PREVIEW_TEXT_SCALE"))]
        TextScale { source: ParseTextScaleError },
        #[snafu(display("QUARTZ_PREVIEW_OFFSCREEN must be 0 or 1, not {value:?}"))]
        Offscreen { value: String },
        #[snafu(display("{variable} must be 0 or 1, not {value:?}"))]
        Material {
            variable: &'static str,
            value: String,
        },
        #[snafu(display("{variable} is not valid Unicode: {value:?}"))]
        NotUnicode {
            variable: &'static str,
            value: OsString,
        },
    }

    /// The preview could not start.
    #[derive(Debug, Snafu)]
    pub enum PreviewError {
        #[snafu(display("invalid preview setting"))]
        ReadEnv { source: ReadPreviewEnvError },
        #[snafu(display("preview failed"))]
        Run { source: RunPreviewError },
    }

    /// The value of `variable`, or `None` when it is unset.
    fn variable(variable: &'static str) -> Result<Option<String>, ReadPreviewEnvError> {
        match env::var_os(variable) {
            None => Ok(None),
            Some(value) => value
                .into_string()
                .map(Some)
                .map_err(|value| ReadPreviewEnvError::NotUnicode { variable, value }),
        }
    }

    /// Parses the variable `name` with `context` describing a bad value; `default` when unset.
    fn setting<T, C>(name: &'static str, default: T, context: C) -> Result<T, ReadPreviewEnvError>
    where
        T: FromStr,
        T::Err: Error + 'static,
        C: IntoError<ReadPreviewEnvError, Source = T::Err>,
    {
        match variable(name)? {
            None => Ok(default),
            Some(value) => value.parse().context(context),
        }
    }

    fn options() -> Result<PreviewOptions, ReadPreviewEnvError> {
        let placement = match variable("QUARTZ_PREVIEW_OFFSCREEN")?.as_deref() {
            None | Some("0") => PreviewPlacement::Visible,
            Some("1") => PreviewPlacement::Offscreen,
            Some(value) => {
                return OffscreenSnafu {
                    value: value.to_owned(),
                }
                .fail();
            }
        };
        let mut material = PreviewTheme::System;
        for name in [
            "QUARTZ_PREVIEW_MICA_FALLBACK",
            "QUARTZ_PREVIEW_HIGH_CONTRAST",
        ] {
            match variable(name)?.as_deref() {
                None | Some("0") => {}
                Some("1") => {
                    material = if name == "QUARTZ_PREVIEW_HIGH_CONTRAST" {
                        PreviewTheme::HighContrast
                    } else {
                        PreviewTheme::Fallback
                    }
                }
                Some(value) => {
                    return MaterialSnafu {
                        variable: name,
                        value,
                    }
                    .fail();
                }
            }
        }
        Ok(PreviewOptions {
            material,
            language: setting(
                "QUARTZ_PREVIEW_LANG",
                Language::SimplifiedChinese,
                LanguageSnafu,
            )?,
            appearance: setting("QUARTZ_PREVIEW_THEME", Appearance::System, ThemeSnafu)?,
            state: setting("QUARTZ_PREVIEW_STATE", PreviewState::Today, StateSnafu)?,
            placement,
            size: setting("QUARTZ_PREVIEW_SIZE", PreviewSize::default(), SizeSnafu)?,
            text_scale: variable("QUARTZ_PREVIEW_TEXT_SCALE")?
                .map(|value| value.parse::<TextScale>().context(TextScaleSnafu))
                .transpose()?,
        })
    }

    pub fn run() -> Result<(), PreviewError> {
        options().context(ReadEnvSnafu)?.run().context(RunSnafu)
    }

    /// Writes `error` and its causes to standard error, which a terminal or a redirect shows even
    /// though the GUI subsystem has no console.
    pub fn report(error: &PreviewError) {
        let mut message = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        eprintln!("{message}");
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match preview::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            preview::report(&error);
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() {}
