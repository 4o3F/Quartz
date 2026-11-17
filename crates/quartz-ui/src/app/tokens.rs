//! Colors from the theme JSON (11 section 9). Components read only this palette.

use std::collections::BTreeMap;

use gpui_kit::{
    Global, Hsla,
    component::{ThemeMode, try_parse_color},
};
use serde::Deserialize;
use snafu::{ResultExt, Snafu};

use super::model::CategoryColor;

/// Theme-independent metrics. Colors stay in the theme JSON; these are the chapter 11 sizes.
pub mod metrics {
    /// `radius.control` (11 section 5.1): buttons, list rows, timeline blocks.
    pub const RADIUS_CONTROL: f32 = 4.;
    /// `radius.overlay` (11 section 5.1): cards, flyouts, the content layer.
    pub const RADIUS_OVERLAY: f32 = 8.;
    /// Timeline block corner radius (11 section 5.3 exception).
    pub const BLOCK_RADIUS: f32 = 2.;

    /// Spacing ladder `space.*` (11 section 5.2).
    pub mod space {
        /// `space.xxs`
        pub const XXS: f32 = 2.;
        /// `space.xs`
        pub const XS: f32 = 4.;
        /// `space.s`
        pub const S: f32 = 8.;
        /// `space.m`
        pub const M: f32 = 12.;
        /// `space.l`
        pub const L: f32 = 16.;
        /// `space.xxl` (page side margin)
        pub const XXL: f32 = 24.;
        /// `space.xxxl` (page top margin)
        pub const XXXL: f32 = 32.;
    }

    /// Control heights by density (11 section 5.3).
    #[derive(Debug, Clone, Copy)]
    pub struct ControlHeight;

    impl ControlHeight {
        /// Standard density control height.
        pub const STANDARD: f32 = 32.;
        /// Compact density control height.
        pub const COMPACT: f32 = 24.;
    }

    /// Caption family for text of 12 px and below (11 section 4.1).
    pub const CAPTION_FONT: &str = "Segoe UI Variable Small";

    /// Navigation width (11 section 5.4).
    pub const NAV_WIDTH: f32 = 240.;
    /// Collapsed navigation icon rail width (11 section 5.4).
    pub const RAIL_WIDTH: f32 = 48.;
    /// Details sidebar width (11 section 5.4).
    pub const PANEL_WIDTH: f32 = 360.;
    /// Maximum main content width (11 section 5.4).
    pub const MAX_CONTENT_WIDTH: f32 = 1200.;
    /// Category main track height (11 section 5.3).
    pub const MAIN_TRACK_HEIGHT: f32 = 24.;
    /// Application swimlane height (11 section 5.3).
    pub const LANE_HEIGHT: f32 = 20.;
    /// Gap between swimlanes (11 section 5.3).
    pub const LANE_GAP: f32 = 4.;
    /// Time axis height (11 section 5.3).
    pub const AXIS_HEIGHT: f32 = 20.;

    /// Type ramp steps of 11 section 4.2.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Ramp {
        /// `type.caption` 12/16 Regular
        Caption,
        /// `type.body` 14/20 Regular
        Body,
        /// `type.body_strong` 14/20 Semibold
        BodyStrong,
        /// `type.body_large` 18/24 Regular
        BodyLarge,
        /// `type.subtitle` 20/28 Semibold
        Subtitle,
        /// `type.title` 28/36 Semibold
        Title,
    }

    impl Ramp {
        /// Font size, line height and whether the step is semibold.
        #[must_use]
        pub const fn metrics(self) -> (f32, f32, bool) {
            match self {
                Ramp::Caption => (12., 16., false),
                Ramp::Body => (14., 20., false),
                Ramp::BodyStrong => (14., 20., true),
                Ramp::BodyLarge => (18., 24., false),
                Ramp::Subtitle => (20., 28., true),
                Ramp::Title => (28., 36., true),
            }
        }
    }
}

/// Failures while reading the Quartz palette from the theme JSON.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum LoadPaletteError {
    /// The theme file is not valid JSON or lacks the `quartz.palette` section.
    #[snafu(display("could not decode the palette section of the theme"))]
    DecodePalette { source: serde_json::Error },
    /// A token used by the UI is absent from the theme.
    #[snafu(display("theme token {token} is missing"))]
    MissingToken { token: String },
    /// A token value is not a valid color.
    #[snafu(display("theme token {token} is not a color"))]
    ParseToken {
        token: String,
        source: anyhow::Error,
    },
}

#[derive(Debug, Deserialize)]
struct ThemeFile {
    quartz: QuartzSection,
}

#[derive(Debug, Deserialize)]
struct QuartzSection {
    palette: PaletteSources,
    #[serde(rename = "title_bar.height")]
    title_bar_height: f32,
    #[serde(rename = "space.m")]
    title_padding: f32,
    #[serde(rename = "type.caption")]
    caption: CaptionSource,
}

#[derive(Debug, Deserialize)]
struct CaptionSource {
    font_family: String,
    font_size: f32,
    line_height: f32,
}

/// Window chrome metrics from the theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Chrome {
    pub title_bar_height: f32,
    pub title_padding: f32,
    pub caption_font_family: String,
    pub caption_font_size: f32,
    pub caption_line_height: f32,
}

/// Both appearances and the chrome metrics, installed as a GPUI global.
#[derive(Debug, Clone, PartialEq)]
pub struct Tokens {
    light: Palette,
    dark: Palette,
    pub chrome: Chrome,
}

impl Global for Tokens {}

impl Tokens {
    /// Reads all tokens from the embedded theme JSON.
    ///
    /// # Errors
    /// Returns an error when the JSON is malformed or a token is missing or not a color.
    pub fn from_theme_json(json: &str) -> Result<Self, LoadPaletteError> {
        let file: ThemeFile = serde_json::from_str(json).context(DecodePaletteSnafu)?;
        let section = file.quartz;
        Ok(Self {
            light: Palette::from_tokens(&section.palette.light, ThemeMode::Light)?,
            dark: Palette::from_tokens(&section.palette.dark, ThemeMode::Dark)?,
            chrome: Chrome {
                title_bar_height: section.title_bar_height,
                title_padding: section.title_padding,
                caption_font_family: section.caption.font_family,
                caption_font_size: section.caption.font_size,
                caption_line_height: section.caption.line_height,
            },
        })
    }

    /// Palette for the current appearance.
    #[must_use]
    pub const fn palette(&self, mode: ThemeMode) -> &Palette {
        if matches!(mode, ThemeMode::Dark) {
            &self.dark
        } else {
            &self.light
        }
    }
}

#[derive(Debug, Deserialize)]
struct PaletteSources {
    light: BTreeMap<String, String>,
    dark: BTreeMap<String, String>,
}

/// Resolved colors for one appearance.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub text_on_accent: Hsla,
    pub text_on_color_white: Hsla,
    pub text_on_color_black: Hsla,
    pub fill_control: Hsla,
    pub fill_control_hover: Hsla,
    pub fill_control_pressed: Hsla,
    pub fill_subtle_hover: Hsla,
    pub fill_subtle_pressed: Hsla,
    pub fill_layer: Hsla,
    pub fill_card: Hsla,
    pub fill_card_secondary: Hsla,
    pub bg_base: Hsla,
    pub bg_flyout: Hsla,
    pub stroke_control: Hsla,
    pub stroke_control_strong: Hsla,
    pub stroke_card: Hsla,
    pub stroke_surface: Hsla,
    pub stroke_divider: Hsla,
    pub status_neutral: Hsla,
    pub status_success: Hsla,
    pub status_success_bg: Hsla,
    pub status_caution: Hsla,
    pub status_critical: Hsla,
    pub accent_fill: Hsla,
    pub accent_text: Hsla,
    pub shadow_ambient: Hsla,
    pub shadow_key: Hsla,
    /// `cat.1` to `cat.14`.
    pub categories: [Hsla; 14],
    pub dark: bool,
}

impl Palette {
    fn from_tokens(
        source: &BTreeMap<String, String>,
        mode: ThemeMode,
    ) -> Result<Self, LoadPaletteError> {
        let get = |token: &str| -> Result<Hsla, LoadPaletteError> {
            let text = source
                .get(token)
                .ok_or_else(|| LoadPaletteError::MissingToken {
                    token: token.to_owned(),
                })?;
            try_parse_color(text).context(ParseTokenSnafu { token })
        };
        let mut categories = [Hsla::default(); 14];
        for (index, slot) in categories.iter_mut().enumerate() {
            *slot = get(&format!("cat.{}", index + 1))?;
        }
        Ok(Self {
            text_primary: get("text.primary")?,
            text_secondary: get("text.secondary")?,
            text_tertiary: get("text.tertiary")?,
            text_on_accent: get("text.on_accent")?,
            text_on_color_white: get("text.on_color_white")?,
            text_on_color_black: get("text.on_color_black")?,
            fill_control: get("fill.control")?,
            fill_control_hover: get("fill.control_hover")?,
            fill_control_pressed: get("fill.control_pressed")?,
            fill_subtle_hover: get("fill.subtle_hover")?,
            fill_subtle_pressed: get("fill.subtle_pressed")?,
            fill_layer: get("fill.layer")?,
            fill_card: get("fill.card")?,
            fill_card_secondary: get("fill.card_secondary")?,
            bg_base: get("bg.base")?,
            bg_flyout: get("bg.flyout")?,
            stroke_control: get("stroke.control")?,
            stroke_control_strong: get("stroke.control_strong")?,
            stroke_card: get("stroke.card")?,
            stroke_surface: get("stroke.surface")?,
            stroke_divider: get("stroke.divider")?,
            status_neutral: get("status.neutral")?,
            status_success: get("status.success")?,
            status_success_bg: get("status.success_bg")?,
            status_caution: get("status.caution")?,
            status_critical: get("status.critical")?,
            accent_fill: get("accent.fill")?,
            accent_text: get("accent.text")?,
            shadow_ambient: get("shadow.ambient")?,
            shadow_key: get("shadow.key")?,
            categories,
            dark: mode.is_dark(),
        })
    }

    /// Color of a category.
    #[must_use]
    pub fn category(&self, color: CategoryColor) -> Hsla {
        match color {
            CategoryColor::Palette(slot) => self
                .categories
                .get(slot.index())
                .copied()
                .unwrap_or(self.status_neutral),
            CategoryColor::Neutral => self.status_neutral,
        }
    }
}

impl Palette {
    /// White or black text, whichever contrasts more with an opaque `background`.
    #[must_use]
    pub fn text_on(&self, background: Hsla) -> Hsla {
        let luminance = relative_luminance(background);
        let white = relative_luminance(self.text_on_color_white);
        let black = relative_luminance(self.text_on_color_black);
        let contrast = |other: f32| (luminance.max(other) + 0.05) / (luminance.min(other) + 0.05);
        if contrast(white) >= contrast(black) {
            self.text_on_color_white
        } else {
            self.text_on_color_black
        }
    }
}

fn relative_luminance(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    let linear = |channel: f32| {
        if channel <= 0.040_45 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: &str = include_str!("../../themes/fluent.json");

    #[test]
    fn both_modes_resolve_every_token() {
        let tokens = Tokens::from_theme_json(THEME).expect("tokens resolve");
        assert!(!tokens.palette(gpui_kit::component::ThemeMode::Light).dark);
        assert!(tokens.palette(gpui_kit::component::ThemeMode::Dark).dark);
        assert_ne!(
            tokens
                .palette(gpui_kit::component::ThemeMode::Light)
                .categories[0],
            tokens
                .palette(gpui_kit::component::ThemeMode::Dark)
                .categories[0]
        );
        assert!((tokens.chrome.title_bar_height - 32.).abs() < f32::EPSILON);
    }

    #[test]
    fn text_on_picks_the_higher_contrast_color() {
        let tokens = Tokens::from_theme_json(THEME).expect("tokens resolve");
        let light = tokens.palette(gpui_kit::component::ThemeMode::Light);
        // cat.8 in light is a dark blue, cat.2 a mid blue that needs black text.
        assert_eq!(
            light.text_on(light.categories[7]),
            light.text_on_color_white
        );
        assert_eq!(
            light.text_on(light.categories[1]),
            light.text_on_color_black
        );
    }

    #[test]
    fn missing_token_is_reported_by_name() {
        let json = r##"{"quartz":{"title_bar.height":32,"space.m":12,
            "type.caption":{"font_family":"x","font_size":12,"line_height":16},
            "palette":{"light":{},"dark":{}}}}"##;
        let error = Tokens::from_theme_json(json).expect_err("empty palette");
        assert!(matches!(error, LoadPaletteError::MissingToken { .. }));
    }
}
