//! Theme-independent design tokens of 11 sections 3.4 to 7: radii, spacing, density, layout
//! sizes, timeline sizes, opacities, the type ramp, fonts and shadows. They are read from the
//! `quartz.metrics` section of the theme JSON, the single token source (11 section 9.1).

use gpui_kit::{FontFallbacks, FontWeight, Pixels, SharedString};
use serde::Deserialize;

/// Corner radii (11 section 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(crate) struct Radius {
    /// `radius.control`: buttons, list rows, tooltips.
    pub(crate) control: Pixels,
    /// `radius.overlay`: windows, cards, menus.
    pub(crate) overlay: Pixels,
}

/// The spacing ladder `space.*` (11 section 5.2).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(crate) struct Space {
    pub(crate) xxs: Pixels,
    pub(crate) xs: Pixels,
    pub(crate) s: Pixels,
    pub(crate) m: Pixels,
    pub(crate) l: Pixels,
    pub(crate) xl: Pixels,
    pub(crate) xxl: Pixels,
    pub(crate) xxxl: Pixels,
}

/// Row heights, control heights and icon sizes (11 sections 5.3 and 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Density {
    Standard,
    Compact,
}

/// The sizes of one density.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(crate) struct DensitySizes {
    pub(crate) control_height: Pixels,
    pub(crate) row_height: Pixels,
    pub(crate) icon_size: Pixels,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
struct Densities {
    standard: DensitySizes,
    compact: DensitySizes,
}

/// Window layout sizes (11 section 5.4).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(crate) struct Layout {
    pub(crate) title_bar_height: Pixels,
    pub(crate) nav_width: Pixels,
    pub(crate) rail_width: Pixels,
    pub(crate) panel_width: Pixels,
    pub(crate) max_content_width: Pixels,
    /// Width of the accent bar beside the selected row.
    pub(crate) indicator_width: Pixels,
    /// Height of the accent bar beside the selected row.
    pub(crate) indicator_height: Pixels,
    /// Width of dividers and gridlines.
    pub(crate) divider: Pixels,
}

/// Timeline sizes (11 sections 3.3, 3.4 and 5.3).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(crate) struct Timeline {
    pub(crate) main_track_height: Pixels,
    pub(crate) lane_height: Pixels,
    pub(crate) lane_gap: Pixels,
    pub(crate) axis_height: Pixels,
    /// Corner radius of chart blocks, smaller than `radius.control` so narrow blocks stay solid.
    pub(crate) block_radius: Pixels,
    /// Background gap between neighboring blocks.
    pub(crate) block_gap: Pixels,
    /// Height of the clock anomaly mark at the top of a block.
    pub(crate) clock_mark_height: Pixels,
    /// Line width of the idle hatching.
    pub(crate) hatch_width: f32,
    /// Distance between idle hatching lines.
    pub(crate) hatch_spacing: f32,
}

/// Opacities of timeline states (11 section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(crate) struct Opacity {
    /// Idle spans over the category color.
    pub(crate) idle: f32,
    /// No foreground, excluded and collection gaps over `status.neutral`.
    pub(crate) neutral_state: f32,
    /// Selected range over `accent.fill`.
    pub(crate) selection: f32,
}

/// Steps of the type ramp (11 section 4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Ramp {
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

/// Size, line height, weight and, for small text, the family of one ramp step.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct TypeStep {
    pub(crate) font_size: Pixels,
    pub(crate) line_height: Pixels,
    pub(crate) weight: FontWeight,
    /// Set for steps that use an optical-size family, such as `Segoe UI Variable Small`.
    #[serde(default)]
    pub(crate) font_family: Option<SharedString>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct TypeRamp {
    caption: TypeStep,
    body: TypeStep,
    body_strong: TypeStep,
    body_large: TypeStep,
    subtitle: TypeStep,
    title: TypeStep,
}

/// Font families outside the type ramp (11 section 4.1).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct Fonts {
    /// Monospace family for rule conditions, paths and commands.
    pub(crate) mono: SharedString,
    /// Families tried when [`Fonts::mono`] is not installed.
    pub(crate) mono_fallbacks: FontFallbacks,
}

/// Which palette shadow color a shadow layer uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShadowColor {
    /// `shadow.ambient`
    Ambient,
    /// `shadow.key`
    Key,
}

/// One layer of an elevation shadow.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(crate) struct ShadowLayer {
    pub(crate) offset_y: Pixels,
    pub(crate) blur: Pixels,
    pub(crate) color: ShadowColor,
}

/// Elevation shadows (11 section 6).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct Shadows {
    /// `shadow.16`: tooltips and the timeline hover card.
    pub(crate) tooltip: Vec<ShadowLayer>,
}

/// All theme-independent tokens.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct Metrics {
    pub(crate) radius: Radius,
    pub(crate) space: Space,
    density: Densities,
    pub(crate) layout: Layout,
    pub(crate) timeline: Timeline,
    pub(crate) opacity: Opacity,
    #[serde(rename = "type")]
    type_ramp: TypeRamp,
    pub(crate) font: Fonts,
    pub(crate) shadow: Shadows,
}

impl Metrics {
    /// The sizes of `density`.
    #[must_use]
    pub(crate) const fn density(&self, density: Density) -> &DensitySizes {
        match density {
            Density::Standard => &self.density.standard,
            Density::Compact => &self.density.compact,
        }
    }

    /// One step of the type ramp.
    #[must_use]
    pub(crate) const fn ramp(&self, ramp: Ramp) -> &TypeStep {
        match ramp {
            Ramp::Caption => &self.type_ramp.caption,
            Ramp::Body => &self.type_ramp.body,
            Ramp::BodyStrong => &self.type_ramp.body_strong,
            Ramp::BodyLarge => &self.type_ramp.body_large,
            Ramp::Subtitle => &self.type_ramp.subtitle,
            Ramp::Title => &self.type_ramp.title,
        }
    }
}
