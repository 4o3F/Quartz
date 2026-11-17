//! Quartz tokens from the theme JSON (11 section 9): one palette per appearance and the
//! theme-independent metrics. Components read colors and sizes only from here.

use std::collections::BTreeMap;

use gpui_kit::{Hsla, component::ThemeMode};
use serde::Deserialize;
use snafu::{OptionExt as _, ResultExt as _, Snafu};

use super::metrics::Metrics;
use crate::model::{CategoryColor, PaletteSlotMap};

/// Failures while reading the Quartz tokens from the theme JSON.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum LoadPaletteError {
    /// The theme file is not valid JSON, or a Quartz token is missing or not a color.
    #[snafu(display("could not decode the Quartz tokens of the theme"))]
    DecodeTokens { source: serde_json::Error },
    /// A categorical palette slot has no color in the theme.
    #[snafu(display("theme token {token} is missing"))]
    MissingCategoryColor { token: &'static str },
}

#[derive(Debug, Deserialize)]
struct ThemeFile {
    quartz: QuartzSection,
}

#[derive(Debug, Deserialize)]
struct QuartzSection {
    metrics: Metrics,
    palette: PaletteSources,
}

/// Both palettes and the metrics.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tokens {
    light: Palette,
    dark: Palette,
    metrics: Metrics,
}

impl Tokens {
    /// Reads all tokens from the embedded theme JSON.
    ///
    /// # Errors
    /// Returns an error when the JSON is malformed or a token is missing or not a color.
    pub(crate) fn from_theme_json(json: &str) -> Result<Self, LoadPaletteError> {
        let file: ThemeFile = serde_json::from_str(json).context(DecodeTokensSnafu)?;
        let section = file.quartz;
        Ok(Self {
            light: Palette::from_source(section.palette.light)?,
            dark: Palette::from_source(section.palette.dark)?,
            metrics: section.metrics,
        })
    }

    /// Theme-independent sizes, radii, type and shadows.
    #[must_use]
    pub(crate) const fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Palette for the current appearance.
    #[must_use]
    pub(crate) const fn palette(&self, mode: ThemeMode) -> &Palette {
        if matches!(mode, ThemeMode::Dark) {
            &self.dark
        } else {
            &self.light
        }
    }
}

#[derive(Debug, Deserialize)]
struct PaletteSources {
    light: PaletteSource,
    dark: PaletteSource,
}

/// The palette tokens of one appearance, as written in the theme; a missing token or a value
/// that is not a color is a decoding error naming the token.
#[derive(Debug, Deserialize)]
struct PaletteSource {
    #[serde(rename = "text.primary")]
    text_primary: Hsla,
    #[serde(rename = "text.secondary")]
    text_secondary: Hsla,
    #[serde(rename = "text.tertiary")]
    text_tertiary: Hsla,
    #[serde(rename = "text.on_color_white")]
    text_on_color_white: Hsla,
    #[serde(rename = "text.on_color_black")]
    text_on_color_black: Hsla,
    #[serde(rename = "fill.control")]
    fill_control: Hsla,
    #[serde(rename = "fill.control_hover")]
    fill_control_hover: Hsla,
    #[serde(rename = "fill.control_pressed")]
    fill_control_pressed: Hsla,
    #[serde(rename = "fill.subtle_hover")]
    fill_subtle_hover: Hsla,
    #[serde(rename = "fill.layer")]
    fill_layer: Hsla,
    #[serde(rename = "fill.card")]
    fill_card: Hsla,
    #[serde(rename = "bg.base")]
    bg_base: Hsla,
    #[serde(rename = "bg.flyout")]
    bg_flyout: Hsla,
    #[serde(rename = "stroke.control")]
    stroke_control: Hsla,
    #[serde(rename = "stroke.control_strong")]
    stroke_control_strong: Hsla,
    #[serde(rename = "stroke.card")]
    stroke_card: Hsla,
    #[serde(rename = "stroke.surface")]
    stroke_surface: Hsla,
    #[serde(rename = "stroke.divider")]
    stroke_divider: Hsla,
    #[serde(rename = "status.neutral")]
    status_neutral: Hsla,
    #[serde(rename = "status.success")]
    status_success: Hsla,
    #[serde(rename = "status.success_bg")]
    status_success_bg: Hsla,
    #[serde(rename = "status.caution")]
    status_caution: Hsla,
    #[serde(rename = "status.critical")]
    status_critical: Hsla,
    #[serde(rename = "accent.fill")]
    accent_fill: Hsla,
    #[serde(rename = "shadow.ambient")]
    shadow_ambient: Hsla,
    #[serde(rename = "shadow.key")]
    shadow_key: Hsla,
    /// `cat.1` to `cat.14` and the tokens the UI does not read yet.
    #[serde(flatten)]
    other: BTreeMap<String, serde_json::Value>,
}

/// One color per categorical palette slot.
pub(crate) type CategoryColors = PaletteSlotMap<Hsla>;

/// Resolved colors for one appearance.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Palette {
    pub(crate) text_primary: Hsla,
    pub(crate) text_secondary: Hsla,
    pub(crate) text_tertiary: Hsla,
    pub(crate) text_on_color_white: Hsla,
    pub(crate) text_on_color_black: Hsla,
    pub(crate) fill_control: Hsla,
    pub(crate) fill_control_hover: Hsla,
    pub(crate) fill_control_pressed: Hsla,
    pub(crate) fill_subtle_hover: Hsla,
    pub(crate) fill_layer: Hsla,
    pub(crate) fill_card: Hsla,
    pub(crate) bg_base: Hsla,
    pub(crate) bg_flyout: Hsla,
    pub(crate) stroke_control: Hsla,
    pub(crate) stroke_control_strong: Hsla,
    pub(crate) stroke_card: Hsla,
    pub(crate) stroke_surface: Hsla,
    pub(crate) stroke_divider: Hsla,
    pub(crate) status_neutral: Hsla,
    pub(crate) status_success: Hsla,
    pub(crate) status_success_bg: Hsla,
    pub(crate) status_caution: Hsla,
    pub(crate) status_critical: Hsla,
    pub(crate) accent_fill: Hsla,
    pub(crate) shadow_ambient: Hsla,
    pub(crate) shadow_key: Hsla,
    /// `cat.1` to `cat.14`.
    pub(crate) categories: CategoryColors,
}

impl Palette {
    fn from_source(source: PaletteSource) -> Result<Self, LoadPaletteError> {
        let categories = category_colors(&source.other)?;
        Ok(Self {
            text_primary: source.text_primary,
            text_secondary: source.text_secondary,
            text_tertiary: source.text_tertiary,
            text_on_color_white: source.text_on_color_white,
            text_on_color_black: source.text_on_color_black,
            fill_control: source.fill_control,
            fill_control_hover: source.fill_control_hover,
            fill_control_pressed: source.fill_control_pressed,
            fill_subtle_hover: source.fill_subtle_hover,
            fill_layer: source.fill_layer,
            fill_card: source.fill_card,
            bg_base: source.bg_base,
            bg_flyout: source.bg_flyout,
            stroke_control: source.stroke_control,
            stroke_control_strong: source.stroke_control_strong,
            stroke_card: source.stroke_card,
            stroke_surface: source.stroke_surface,
            stroke_divider: source.stroke_divider,
            status_neutral: source.status_neutral,
            status_success: source.status_success,
            status_success_bg: source.status_success_bg,
            status_caution: source.status_caution,
            status_critical: source.status_critical,
            accent_fill: source.accent_fill,
            shadow_ambient: source.shadow_ambient,
            shadow_key: source.shadow_key,
            categories,
        })
    }

    /// Color of a category.
    #[must_use]
    pub(crate) fn category(&self, color: CategoryColor) -> Hsla {
        match color {
            CategoryColor::Palette(slot) => *self.categories.get(slot),
            CategoryColor::Neutral => self.status_neutral,
        }
    }
}

/// Reads `cat.1` to `cat.14` from the tokens the palette source did not name.
fn category_colors(
    tokens: &BTreeMap<String, serde_json::Value>,
) -> Result<CategoryColors, LoadPaletteError> {
    CategoryColors::try_from_fn(|slot| {
        let token = slot.token_key();
        let value = tokens
            .get(token)
            .context(MissingCategoryColorSnafu { token })?;
        Hsla::deserialize(value).context(DecodeTokensSnafu)
    })
}

impl Palette {
    /// White or black text, whichever contrasts more with an opaque `background`.
    #[must_use]
    pub(crate) fn text_on(&self, background: Hsla) -> Hsla {
        let luminance = relative_luminance(background);
        let white = relative_luminance(self.text_on_color_white);
        let black = relative_luminance(self.text_on_color_black);
        let contrast = |other: f32| {
            (luminance.max(other) + CONTRAST_FLARE) / (luminance.min(other) + CONTRAST_FLARE)
        };
        if contrast(white) >= contrast(black) {
            self.text_on_color_white
        } else {
            self.text_on_color_black
        }
    }
}

// Relative luminance and contrast ratio as WCAG 2.2 defines them:
// https://www.w3.org/TR/WCAG22/#dfn-relative-luminance and #dfn-contrast-ratio.
/// sRGB channel values at or below this are on the linear segment of the transfer curve.
const SRGB_LINEAR_LIMIT: f32 = 0.040_45;
/// Slope of the linear segment.
const SRGB_LINEAR_SLOPE: f32 = 12.92;
/// Offset of the gamma segment.
const SRGB_GAMMA_OFFSET: f32 = 0.055;
/// Exponent of the gamma segment.
const SRGB_GAMMA: f32 = 2.4;
/// Red, green and blue weights of relative luminance.
const LUMA_RED: f32 = 0.2126;
const LUMA_GREEN: f32 = 0.7152;
const LUMA_BLUE: f32 = 0.0722;
/// Flare term added to both luminances of a contrast ratio.
const CONTRAST_FLARE: f32 = 0.05;

fn relative_luminance(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    let linear = |channel: f32| {
        if channel <= SRGB_LINEAR_LIMIT {
            channel / SRGB_LINEAR_SLOPE
        } else {
            ((channel + SRGB_GAMMA_OFFSET) / (1. + SRGB_GAMMA_OFFSET)).powf(SRGB_GAMMA)
        }
    };
    LUMA_RED * linear(rgb.r) + LUMA_GREEN * linear(rgb.g) + LUMA_BLUE * linear(rgb.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PaletteSlot;

    const THEME: &str = include_str!("../../themes/fluent.json");

    #[test]
    fn both_modes_resolve_every_token() {
        let tokens = Tokens::from_theme_json(THEME).expect("tokens resolve");
        let first = PaletteSlot::numbered(1);
        assert_ne!(
            tokens.palette(ThemeMode::Light).categories.get(first),
            tokens.palette(ThemeMode::Dark).categories.get(first)
        );
        assert_eq!(tokens.metrics().layout.title_bar_height, gpui_kit::px(32.));
    }

    #[test]
    fn text_on_picks_the_higher_contrast_color() {
        let tokens = Tokens::from_theme_json(THEME).expect("tokens resolve");
        let light = tokens.palette(ThemeMode::Light);
        // cat.8 in light is a dark blue, cat.2 a mid blue that needs black text (11 section 3.3).
        let color = |number| *light.categories.get(PaletteSlot::numbered(number));
        assert_eq!(light.text_on(color(8)), light.text_on_color_white);
        assert_eq!(light.text_on(color(2)), light.text_on_color_black);
    }

    #[test]
    fn missing_tokens_fail_to_load() {
        let mut theme: serde_json::Value = serde_json::from_str(THEME).expect("theme JSON");
        theme
            .pointer_mut("/quartz/palette/light")
            .and_then(serde_json::Value::as_object_mut)
            .expect("light palette object")
            .remove("text.primary");
        let error = Tokens::from_theme_json(&theme.to_string()).expect_err("text.primary removed");
        assert!(matches!(error, LoadPaletteError::DecodeTokens { .. }));
    }

    #[test]
    fn missing_category_color_is_reported_by_token() {
        let mut theme: serde_json::Value = serde_json::from_str(THEME).expect("theme JSON");
        theme
            .pointer_mut("/quartz/palette/dark")
            .and_then(serde_json::Value::as_object_mut)
            .expect("dark palette object")
            .remove("cat.14");
        let error = Tokens::from_theme_json(&theme.to_string()).expect_err("cat.14 removed");
        assert!(matches!(
            error,
            LoadPaletteError::MissingCategoryColor { token: "cat.14" }
        ));
    }
}
