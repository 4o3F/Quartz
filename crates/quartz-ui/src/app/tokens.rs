//! Typed Fluent palettes, immutable embedded defaults and reversible system overlays (11 section 9).

use super::{metrics::Metrics, theme_mapping::ComponentMapping};
use crate::model::{CategoryColor, PaletteSlot, PaletteSlotMap};
use gpui_kit::{Hsla, component::ThemeMode};
use serde::Deserialize;
#[cfg(test)]
use snafu::ResultExt as _;
use snafu::{OptionExt as _, Snafu};

#[derive(Debug, Snafu)]
pub enum LoadPaletteError {
    #[snafu(display("could not decode the Quartz tokens of the theme"))]
    DecodeTokens { source: serde_json::Error },
    #[snafu(display("theme token {token} is missing"))]
    MissingCategoryColor { token: &'static str },
}

/// Mica exposes the DWM backdrop; Solid paints the opaque Fluent base (11 section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Material {
    Mica,
    Solid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContrastMode {
    Standard,
    High,
}

/// All seven Windows accent variants are retained from one successful read (11 section 3.2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AccentColors {
    pub(crate) accent: Hsla,
    pub(crate) light1: Hsla,
    pub(crate) light2: Hsla,
    pub(crate) light3: Hsla,
    pub(crate) dark1: Hsla,
    pub(crate) dark2: Hsla,
    pub(crate) dark3: Hsla,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct SystemColors {
    pub(crate) window: Hsla,
    pub(crate) window_text: Hsla,
    pub(crate) highlight: Hsla,
    pub(crate) highlight_text: Hsla,
    pub(crate) gray_text: Hsla,
    pub(crate) button_face: Hsla,
}

/// Accent hover opacity (11 section 3.2).
const ACCENT_HOVER_ALPHA: f32 = 0.9;
/// Selection opacity (11 section 3.4).
const ACCENT_SELECTION_ALPHA: f32 = 0.2;

#[derive(Debug, Deserialize)]
pub(super) struct QuartzSection {
    metrics: Metrics,
    palette: PaletteSources,
    pub(super) component_mapping: ComponentMapping,
}
#[derive(Debug, Deserialize)]
struct PaletteSources {
    light: PaletteSource,
    dark: PaletteSource,
}

macro_rules! palette_fields {
    ($($name:ident => $token:literal => $variant:ident),+ $(,)?) => {
        #[derive(Debug, Deserialize)]
        struct PaletteSource {
            $(#[serde(rename = $token)] $name: Hsla,)+
            #[serde(flatten)] categories: CategorySources,
        }
        #[derive(Debug, Clone, PartialEq)]
        pub(crate) struct Palette {
            $(pub(crate) $name: Hsla,)+
            pub(crate) contrast: ContrastMode,
            pub(crate) categories: CategoryColors,
        }
        #[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
        pub(super) enum PaletteToken {
            $(#[serde(rename = $token)] $variant,)+
            #[serde(rename = "cat.1")] Cat1, #[serde(rename = "cat.2")] Cat2,
            #[serde(rename = "cat.3")] Cat3, #[serde(rename = "cat.4")] Cat4,
            #[serde(rename = "cat.5")] Cat5,
        }
        impl PaletteToken {
            #[cfg(test)]
            pub(super) const ALL: &[Self] = &[$(Self::$variant,)+Self::Cat1,Self::Cat2,Self::Cat3,Self::Cat4,Self::Cat5];
            pub(super) fn color(self, palette: &Palette) -> Hsla {
                match self {
                    $(Self::$variant => palette.$name,)+
                    Self::Cat1 => *palette.categories.get(PaletteSlot::numbered(1)),
                    Self::Cat2 => *palette.categories.get(PaletteSlot::numbered(2)),
                    Self::Cat3 => *palette.categories.get(PaletteSlot::numbered(3)),
                    Self::Cat4 => *palette.categories.get(PaletteSlot::numbered(4)),
                    Self::Cat5 => *palette.categories.get(PaletteSlot::numbered(5)),
                }
            }
        }
        impl Palette {
            fn from_source(source: PaletteSource) -> Result<Self, LoadPaletteError> {
                Ok(Self { $($name: source.$name,)+ contrast: ContrastMode::Standard, categories: source.categories.resolve()? })
            }
        }
    };
}
palette_fields! {
    text_primary => "text.primary" => TextPrimary,
    text_secondary => "text.secondary" => TextSecondary,
    text_tertiary => "text.tertiary" => TextTertiary,
    text_disabled => "text.disabled" => TextDisabled,
    text_on_accent => "text.on_accent" => TextOnAccent,
    text_on_color_white => "text.on_color_white" => TextOnColorWhite,
    text_on_color_black => "text.on_color_black" => TextOnColorBlack,
    fill_control => "fill.control" => FillControl,
    fill_control_hover => "fill.control_hover" => FillControlHover,
    fill_control_pressed => "fill.control_pressed" => FillControlPressed,
    fill_subtle_hover => "fill.subtle_hover" => FillSubtleHover,
    fill_subtle_pressed => "fill.subtle_pressed" => FillSubtlePressed,
    fill_layer => "fill.layer" => FillLayer,
    fill_card => "fill.card" => FillCard,
    fill_card_secondary => "fill.card_secondary" => FillCardSecondary,
    bg_base => "bg.base" => BgBase,
    bg_flyout => "bg.flyout" => BgFlyout,
    stroke_control => "stroke.control" => StrokeControl,
    stroke_control_strong => "stroke.control_strong" => StrokeControlStrong,
    stroke_card => "stroke.card" => StrokeCard,
    stroke_surface => "stroke.surface" => StrokeSurface,
    stroke_divider => "stroke.divider" => StrokeDivider,
    status_success => "status.success" => StatusSuccess,
    status_caution => "status.caution" => StatusCaution,
    status_critical => "status.critical" => StatusCritical,
    status_neutral => "status.neutral" => StatusNeutral,
    status_success_bg => "status.success_bg" => StatusSuccessBg,
    accent_fill => "accent.fill" => AccentFill,
    accent_fill_hover => "accent.fill_hover" => AccentFillHover,
    accent_text => "accent.text" => AccentText,
    shadow_ambient => "shadow.ambient" => ShadowAmbient,
    shadow_key => "shadow.key" => ShadowKey,
    window_background => "window.background" => WindowBackground,
    transparent => "transparent" => Transparent,
    stroke_focus_outer => "stroke.focus_outer" => StrokeFocusOuter,
    stroke_focus_inner => "stroke.focus_inner" => StrokeFocusInner,
    fill_smoke => "fill.smoke" => FillSmoke,
    accent_selection => "accent.selection" => AccentSelection,
    text_on_critical => "text.on_critical" => TextOnCritical,
    text_on_success => "text.on_success" => TextOnSuccess,
    text_on_caution => "text.on_caution" => TextOnCaution,
}

pub(crate) type CategoryColors = PaletteSlotMap<Hsla>;
#[derive(Debug, Deserialize)]
struct CategorySources {
    #[serde(rename = "cat.1")]
    cat1: Option<Hsla>,
    #[serde(rename = "cat.2")]
    cat2: Option<Hsla>,
    #[serde(rename = "cat.3")]
    cat3: Option<Hsla>,
    #[serde(rename = "cat.4")]
    cat4: Option<Hsla>,
    #[serde(rename = "cat.5")]
    cat5: Option<Hsla>,
    #[serde(rename = "cat.6")]
    cat6: Option<Hsla>,
    #[serde(rename = "cat.7")]
    cat7: Option<Hsla>,
    #[serde(rename = "cat.8")]
    cat8: Option<Hsla>,
    #[serde(rename = "cat.9")]
    cat9: Option<Hsla>,
    #[serde(rename = "cat.10")]
    cat10: Option<Hsla>,
    #[serde(rename = "cat.11")]
    cat11: Option<Hsla>,
    #[serde(rename = "cat.12")]
    cat12: Option<Hsla>,
    #[serde(rename = "cat.13")]
    cat13: Option<Hsla>,
    #[serde(rename = "cat.14")]
    cat14: Option<Hsla>,
}
impl CategorySources {
    fn resolve(self) -> Result<CategoryColors, LoadPaletteError> {
        CategoryColors::try_from_fn(|slot| {
            let color = match slot.ordinal() {
                1 => self.cat1,
                2 => self.cat2,
                3 => self.cat3,
                4 => self.cat4,
                5 => self.cat5,
                6 => self.cat6,
                7 => self.cat7,
                8 => self.cat8,
                9 => self.cat9,
                10 => self.cat10,
                11 => self.cat11,
                12 => self.cat12,
                13 => self.cat13,
                14 => self.cat14,
                _ => unreachable!("palette slots are numbered 1 to 14"),
            };
            color.context(MissingCategoryColorSnafu {
                token: slot.token_key(),
            })
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tokens {
    light: Palette,
    dark: Palette,
    metrics: Metrics,
}
impl Tokens {
    pub(super) fn from_section(section: QuartzSection) -> Result<Self, LoadPaletteError> {
        Ok(Self {
            light: Palette::from_source(section.palette.light)?,
            dark: Palette::from_source(section.palette.dark)?,
            metrics: section.metrics,
        })
    }
    #[cfg(test)]
    pub(crate) fn from_theme_json(json: &str) -> Result<Self, LoadPaletteError> {
        #[derive(Deserialize)]
        struct File {
            quartz: QuartzSection,
        }
        let file: File = serde_json::from_str(json).context(DecodeTokensSnafu)?;
        Self::from_section(file.quartz)
    }
    pub(crate) fn resolved(
        &self,
        accent: Option<&AccentColors>,
        material: Material,
        contrast: Option<&SystemColors>,
    ) -> Self {
        Self {
            light: self
                .light
                .resolved(accent, material, contrast, ThemeMode::Light),
            dark: self
                .dark
                .resolved(accent, material, contrast, ThemeMode::Dark),
            metrics: self.metrics.clone(),
        }
    }
    pub(crate) const fn metrics(&self) -> &Metrics {
        &self.metrics
    }
    pub(crate) const fn palette(&self, mode: ThemeMode) -> &Palette {
        if matches!(mode, ThemeMode::Dark) {
            &self.dark
        } else {
            &self.light
        }
    }
}
impl Palette {
    fn resolved(
        &self,
        accent: Option<&AccentColors>,
        material: Material,
        contrast: Option<&SystemColors>,
        mode: ThemeMode,
    ) -> Self {
        if let Some(colors) = contrast {
            return Self::high_contrast(colors);
        }
        let palette = match accent {
            Some(colors) => self.with_accent(colors, mode),
            None => self.clone(),
        };
        palette.with_material(material)
    }
    pub(crate) fn with_material(&self, material: Material) -> Self {
        let mut palette = self.clone();
        palette.window_background = match material {
            Material::Mica => self.transparent,
            Material::Solid => self.bg_base,
        };
        palette
    }
    pub(crate) fn with_accent(&self, colors: &AccentColors, mode: ThemeMode) -> Self {
        let mut palette = self.clone();
        palette.accent_fill = if mode.is_dark() {
            colors.light2
        } else {
            colors.dark1
        };
        palette.accent_fill_hover = palette.accent_fill.opacity(ACCENT_HOVER_ALPHA);
        palette.accent_text = if mode.is_dark() {
            colors.light3
        } else {
            colors.dark2
        };
        palette.accent_selection = palette.accent_fill.opacity(ACCENT_SELECTION_ALPHA);
        palette
    }
    /// Exhaustive construction makes every new palette field require a system-color decision (11 section 2).
    pub(crate) fn high_contrast(colors: &SystemColors) -> Self {
        Self {
            text_primary: colors.window_text,
            text_secondary: colors.window_text,
            text_tertiary: colors.gray_text,
            text_disabled: colors.gray_text,
            text_on_accent: colors.highlight_text,
            text_on_color_white: colors.window_text,
            text_on_color_black: colors.highlight_text,
            fill_control: colors.button_face,
            fill_control_hover: colors.button_face,
            fill_control_pressed: colors.button_face,
            fill_subtle_hover: colors.button_face,
            fill_subtle_pressed: colors.button_face,
            fill_layer: colors.window,
            fill_card: colors.button_face,
            fill_card_secondary: colors.button_face,
            bg_base: colors.window,
            bg_flyout: colors.window,
            stroke_control: colors.window_text,
            stroke_control_strong: colors.window_text,
            stroke_card: colors.window_text,
            stroke_surface: colors.window_text,
            stroke_divider: colors.window_text,
            status_success: colors.window_text,
            status_caution: colors.window_text,
            status_critical: colors.window_text,
            status_neutral: colors.gray_text,
            status_success_bg: colors.window,
            accent_fill: colors.highlight,
            accent_fill_hover: colors.highlight,
            accent_text: colors.window_text,
            shadow_ambient: colors.window,
            shadow_key: colors.window,
            window_background: colors.window,
            transparent: colors.window,
            stroke_focus_outer: colors.window_text,
            stroke_focus_inner: colors.window_text,
            fill_smoke: colors.window,
            accent_selection: colors.highlight,
            text_on_critical: colors.window,
            text_on_success: colors.window,
            text_on_caution: colors.window,
            contrast: ContrastMode::High,
            categories: CategoryColors::filled(colors.highlight),
        }
    }
    pub(crate) fn category(&self, color: CategoryColor) -> Hsla {
        match color {
            CategoryColor::Palette(slot) => *self.categories.get(slot),
            CategoryColor::Neutral => self.bg_base.blend(self.status_neutral),
        }
    }
    pub(crate) fn text_on(&self, background: Hsla) -> Hsla {
        let luminance = relative_luminance(background);
        let contrast = |color: Hsla| {
            let other = relative_luminance(color);
            (luminance.max(other) + CONTRAST_FLARE) / (luminance.min(other) + CONTRAST_FLARE)
        };
        if contrast(self.text_on_color_white) >= contrast(self.text_on_color_black) {
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

    /// Minimum category and body-text ratios from 11 sections 3.3 and 9.3.
    const MIN_CATEGORY_CONTRAST: f32 = 3.;
    const MIN_BODY_CONTRAST: f32 = 4.5;

    fn contrast(foreground: Hsla, background: Hsla) -> f32 {
        let foreground = relative_luminance(background.blend(foreground));
        let background = relative_luminance(background);
        (foreground.max(background) + CONTRAST_FLARE)
            / (foreground.min(background) + CONTRAST_FLARE)
    }

    #[test]
    fn embedded_categories_and_body_text_meet_wcag_contrast() {
        let theme: serde_json::Value = serde_json::from_str(THEME).expect("embedded JSON");
        let tokens = Tokens::from_theme_json(THEME).expect("embedded tokens");
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let palette = tokens.palette(mode);
            let mode_name = if mode.is_dark() { "dark" } else { "light" };
            let source = theme
                .pointer(&format!("/quartz/palette/{mode_name}"))
                .expect("theme palette");
            let color = |name: &str| {
                Hsla::deserialize(source.get(name).expect("named color"))
                    .expect("named theme color")
            };
            for slot in PaletteSlot::ALL {
                let category = *palette.categories.get(slot);
                let ratio = contrast(category, palette.bg_base);
                println!("{mode_name} {} / bg.base: {ratio:.4}", slot.token_key());
                assert!(
                    ratio >= MIN_CATEGORY_CONTRAST,
                    "{mode_name} {}: {ratio} < {MIN_CATEGORY_CONTRAST}",
                    slot.token_key()
                );
                assert!(
                    contrast(palette.text_on(category), category) >= MIN_BODY_CONTRAST,
                    "{mode_name} {} block label",
                    slot.token_key()
                );
            }
            let neutral_color = palette.category(CategoryColor::Neutral);
            let neutral_ratio = contrast(neutral_color, palette.bg_base);
            assert!(
                contrast(palette.text_on(neutral_color), neutral_color) >= MIN_BODY_CONTRAST,
                "{mode_name} neutral block label"
            );
            println!("{mode_name} neutral / bg.base: {neutral_ratio:.4}");
            assert!(
                neutral_ratio >= MIN_CATEGORY_CONTRAST,
                "{mode_name} neutral: {neutral_ratio}"
            );
            let base = palette.bg_base;
            let layer = base.blend(palette.fill_layer);
            let card = layer.blend(palette.fill_card);
            struct Surface {
                name: &'static str,
                color: Hsla,
            }
            let surfaces = [
                Surface {
                    name: "bg.base",
                    color: base,
                },
                Surface {
                    name: "fill.layer / bg.base",
                    color: layer,
                },
                Surface {
                    name: "fill.card / fill.layer / bg.base",
                    color: card,
                },
                Surface {
                    name: "fill.control / card",
                    color: card.blend(palette.fill_control),
                },
                Surface {
                    name: "fill.control_hover / card",
                    color: card.blend(palette.fill_control_hover),
                },
                Surface {
                    name: "fill.control_pressed / card",
                    color: card.blend(palette.fill_control_pressed),
                },
                Surface {
                    name: "fill.subtle_hover / bg.base",
                    color: base.blend(palette.fill_subtle_hover),
                },
                Surface {
                    name: "fill.subtle_pressed / bg.base",
                    color: base.blend(color("fill.subtle_pressed")),
                },
                Surface {
                    name: "bg.flyout",
                    color: palette.bg_flyout,
                },
            ];
            for token in ["text.primary", "text.secondary", "accent.text"] {
                for surface in &surfaces {
                    let ratio = contrast(color(token), surface.color);
                    println!("{mode_name} {token} / {}: {ratio:.4}", surface.name);
                    assert!(
                        ratio >= MIN_BODY_CONTRAST,
                        "{mode_name} {token} / {}: {ratio} < {MIN_BODY_CONTRAST}",
                        surface.name
                    );
                }
            }
            struct TextPair {
                text: &'static str,
                background: &'static str,
            }
            for pair in [
                TextPair {
                    text: "text.on_accent",
                    background: "accent.fill",
                },
                TextPair {
                    text: "text.on_accent",
                    background: "accent.fill_hover",
                },
                TextPair {
                    text: "status.success",
                    background: "status.success_bg",
                },
                TextPair {
                    text: "text.on_critical",
                    background: "status.critical",
                },
                TextPair {
                    text: "text.on_success",
                    background: "status.success",
                },
                TextPair {
                    text: "text.on_caution",
                    background: "status.caution",
                },
            ] {
                let ratio = contrast(color(pair.text), card.blend(color(pair.background)));
                println!(
                    "{mode_name} {} / {}: {ratio:.4}",
                    pair.text, pair.background
                );
                assert!(
                    ratio >= MIN_BODY_CONTRAST,
                    "{mode_name} {} / {}: {ratio}",
                    pair.text,
                    pair.background
                );
            }
        }
    }

    fn contains_color_literal(source: &str) -> bool {
        let constructors = ["rgb(", "rgba(", "hsl(", "hsla("];
        for constructor in constructors {
            for (index, _) in source.match_indices(constructor) {
                if index == 0
                    || source[..index]
                        .chars()
                        .next_back()
                        .is_none_or(|character| !character.is_alphanumeric() && character != '_')
                {
                    return true;
                }
            }
        }
        source.split("0x").skip(1).any(|tail| {
            matches!(
                tail.chars().take_while(char::is_ascii_hexdigit).count(),
                6 | 8
            )
        }) || source.split('#').skip(1).any(|tail| {
            matches!(
                tail.chars().take_while(char::is_ascii_hexdigit).count(),
                3 | 4 | 6 | 8
            )
        })
    }

    fn check_source_colors(directory: &std::path::Path) {
        for entry in std::fs::read_dir(directory).expect("UI source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                check_source_colors(&path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                // Only this exact theme decoding module is exempt; the system observer and every view are checked.
                if path
                    == std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/tokens.rs")
                {
                    continue;
                }
                assert!(
                    !contains_color_literal(&std::fs::read_to_string(&path).expect("Rust source")),
                    "color literal outside theme loader: {}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn ui_source_has_no_color_literals_outside_the_theme_loader() {
        check_source_colors(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        assert!(contains_color_literal("rgb(1, 2, 3)"));
        assert!(contains_color_literal("rgba(1, 2, 3, 1)"));
        assert!(contains_color_literal("hsl(0, 0%, 0%)"));
        assert!(contains_color_literal("hsla(0, 0%, 0%, 1)"));
        assert!(contains_color_literal("#123456"));
        assert!(contains_color_literal("0xffaabb"));
        assert!(!contains_color_literal("color.to_rgb()"));
    }
}
