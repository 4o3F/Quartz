//! Fluent-styled building blocks shared by the windows. Colors and sizes come only from the
//! theme tokens.

use gpui_kit::{
    App, BoxShadow, ClickEvent, Div, ElementId, Font, FontFeatures, FontWeight, Hsla,
    InteractiveElement as _, ParentElement as _, Pixels, SharedString, Stateful,
    StatefulInteractiveElement as _, StyleRefinement, Styled, Window, WindowControlArea,
    assets::IconName,
    component::{Icon, ThemeMode},
    div, font, point,
    prelude::FluentBuilder as _,
    px,
};

use std::sync::Arc;

use super::{
    icons::{AppAssets, IconVariant},
    metrics::{Density, Metrics, Ramp, ShadowColor, ShadowLayer},
    tokens::{ContrastMode, Palette, Tokens},
};
use crate::{
    model::{CategoryColor, Glyph},
    text_scale::TextScale,
};

/// Smallest height of the segmented control: the compact control height plus a 2 px inset on each
/// side. It grows with the text (11 section 5.7).
const SEGMENTED_HEIGHT: f32 = 28.;
/// Smallest height of one segment: the compact control height minus the 2 px inset on each side.
const SEGMENTED_ITEM_HEIGHT: f32 = 20.;
/// The OpenType feature that gives digits equal widths (11 section 4.1).
const TABULAR_FIGURES: &str = "tnum";
/// Horizontal padding of one segment.
const SEGMENTED_ITEM_PADDING: f32 = 10.;
/// Corner radius of one segment, 1 px inside `radius.control` so the corners nest.
const SEGMENTED_ITEM_RADIUS: f32 = 3.;

/// Narrowest a proportion bar gets: the name and the bar share the rest of a row evenly but the
/// bar keeps this much (11 section 5.6).
pub(crate) const PROPORTION_BAR_MIN_WIDTH: f32 = 48.;

/// Side of the square category swatch in legends and chips.
const SWATCH_SIZE: f32 = 8.;
/// Native Windows caption button width for the 32 px title bar (11 section 5.4).
const CAPTION_CONTROL_WIDTH: f32 = 46.;

/// What a push button shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ButtonContent {
    Label(String),
    Icon(IconName),
    IconLabel(IconName, String),
}

#[derive(Debug)]
struct ButtonParts {
    icon: Option<IconName>,
    label: Option<String>,
}
impl ButtonContent {
    fn into_parts(self) -> ButtonParts {
        match self {
            Self::Label(label) => ButtonParts {
                icon: None,
                label: Some(label),
            },
            Self::Icon(icon) => ButtonParts {
                icon: Some(icon),
                label: None,
            },
            Self::IconLabel(icon, label) => ButtonParts {
                icon: Some(icon),
                label: Some(label),
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ButtonStyle {
    Standard,
    Disabled,
    Accent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InfoBarStyle {
    Neutral,
    Critical,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct InfoBarColors {
    pub(crate) background: Hsla,
    pub(crate) border: Hsla,
    pub(crate) foreground: Hsla,
    pub(crate) secondary: Hsla,
    pub(crate) icon: Hsla,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SkeletonTone {
    Strong,
    Subtle,
}

#[derive(Debug, Clone, Copy)]
struct ButtonColors {
    rest: Hsla,
    hover: Hsla,
    pressed: Hsla,
    foreground: Hsla,
    border: Hsla,
}
impl ButtonStyle {
    fn colors(self, palette: &Palette) -> ButtonColors {
        match self {
            Self::Standard => ButtonColors {
                rest: palette.fill_control,
                hover: palette.fill_control_hover,
                pressed: palette.fill_control_pressed,
                foreground: palette.text_primary,
                border: palette.stroke_control,
            },
            Self::Disabled => ButtonColors {
                rest: palette.fill_control_disabled,
                hover: palette.fill_control_disabled,
                pressed: palette.fill_control_disabled,
                foreground: palette.text_disabled,
                border: palette.stroke_control,
            },
            Self::Accent => ButtonColors {
                rest: palette.accent_fill,
                hover: palette.accent_fill_hover,
                pressed: palette.accent_fill,
                foreground: palette.text_on_accent,
                border: palette.accent_fill,
            },
        }
    }
}

/// One choice of a segmented control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment<V> {
    /// Element id, unique within the control.
    pub(crate) id: &'static str,
    pub(crate) label: String,
    pub(crate) value: V,
}

/// One type ramp step after the text scale (11 section 5.7).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScaledStep {
    pub(crate) font_size: Pixels,
    pub(crate) line_height: Pixels,
    pub(crate) weight: FontWeight,
    /// Set for steps that use an optical-size family; the theme family otherwise.
    pub(crate) font_family: Option<SharedString>,
}

/// The palette of the current appearance with the metrics and text scale, and the builders that
/// use them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Widgets<'a> {
    pub(crate) palette: &'a Palette,
    pub(crate) metrics: &'a Metrics,
    pub(crate) text_scale: TextScale,
}

impl<'a> Widgets<'a> {
    pub(crate) fn info_bar_colors(&self, style: InfoBarStyle) -> InfoBarColors {
        let palette = self.palette;
        if palette.contrast == ContrastMode::High {
            return InfoBarColors {
                background: palette.bg_base,
                border: palette.stroke_card,
                foreground: palette.text_primary,
                secondary: palette.text_primary,
                icon: palette.text_primary,
            };
        }
        let mut colors = InfoBarColors {
            background: palette.fill_card,
            border: palette.stroke_card,
            foreground: palette.text_primary,
            secondary: palette.text_secondary,
            icon: palette.text_secondary,
        };
        if style == InfoBarStyle::Critical {
            colors.background = palette.status_critical_bg;
            colors.border = palette
                .status_critical
                .opacity(self.metrics.opacity.notice_border);
            colors.icon = palette.status_critical;
        }
        colors
    }

    pub(crate) fn skeleton_block(&self, tone: SkeletonTone) -> Div {
        if self.palette.contrast == ContrastMode::High {
            return div()
                .bg(self.palette.bg_base)
                .border(self.metrics.layout.divider)
                .border_color(self.palette.text_tertiary);
        }
        div().bg(match tone {
            SkeletonTone::Strong => self
                .palette
                .text_tertiary
                .opacity(self.metrics.opacity.skeleton),
            SkeletonTone::Subtle => self.palette.fill_subtle_hover,
        })
    }

    /// The widgets of the `mode` appearance with text scaled by `text_scale`.
    pub(crate) const fn new(tokens: &'a Tokens, mode: ThemeMode, text_scale: TextScale) -> Self {
        Self {
            palette: tokens.palette(mode),
            metrics: tokens.metrics(),
            text_scale,
        }
    }

    /// `ramp` with its size and line height multiplied by the text scale.
    pub(crate) fn step(&self, ramp: Ramp) -> ScaledStep {
        let step = self.metrics.ramp(ramp);
        let scale = self.text_scale.for_gpui();
        ScaledStep {
            font_size: step.font_size * scale,
            line_height: step.line_height * scale,
            weight: step.weight,
            font_family: step.font_family.clone(),
        }
    }

    /// The line height of `ramp` after the text scale.
    pub(crate) fn line_height(&self, ramp: Ramp) -> Pixels {
        self.step(ramp).line_height
    }

    /// `minimum` or, when text has grown taller than it, the line height of `ramp`: a height that
    /// is a floor and grows with the text (11 section 5.7).
    pub(crate) fn height_for_text(&self, minimum: Pixels, ramp: Ramp) -> Pixels {
        minimum.max(self.line_height(ramp))
    }

    /// Digits of equal width, so columns of changing numbers do not jitter and can be measured.
    pub(crate) fn tabular_figures() -> FontFeatures {
        FontFeatures(Arc::new(vec![(TABULAR_FIGURES.to_owned(), 1)]))
    }

    /// A themed icon sized for `density` (11 section 7).
    pub(crate) fn icon(&self, name: IconName, density: Density, color: Hsla) -> Icon {
        self.variant_icon(name, density, color, IconVariant::Regular)
    }

    pub(crate) fn variant_icon(
        &self,
        name: IconName,
        density: Density,
        color: Hsla,
        variant: IconVariant,
    ) -> Icon {
        Icon::default()
            .path(AppAssets::icon_path(name, density, variant))
            .size(self.metrics.density(density).icon_size)
            .text_color(color)
    }

    /// High contrast marks preserve their full extent and share the legend pattern (11 section 2).
    pub(crate) fn category_mark(&self, body: Div, category: CategoryColor) -> Div {
        if self.palette.contrast == ContrastMode::Standard {
            return body.bg(self.palette.category(category));
        }
        body.relative()
            .overflow_hidden()
            .bg(self.palette.bg_base)
            .child(
                super::data_pattern::DataPattern::for_category(category)
                    .canvas(
                        self.palette.bg_base,
                        self.palette.accent_fill,
                        self.palette.text_primary,
                    )
                    .absolute()
                    .inset_0(),
            )
    }

    /// High contrast state marks retain opaque system colors (11 section 2).
    pub(crate) fn state_color(&self, color: Hsla, opacity: f32) -> Hsla {
        match self.palette.contrast {
            ContrastMode::Standard => color.opacity(opacity),
            ContrastMode::High => self.palette.bg_base,
        }
    }

    /// The monospace font with its fallbacks.
    pub(crate) fn mono_font(&self) -> Font {
        Font {
            fallbacks: Some(self.metrics.font.mono_fallbacks.clone()),
            ..font(self.metrics.font.mono.clone())
        }
    }

    /// Background change of list rows and borderless buttons under the pointer.
    pub(crate) fn subtle_hover(&self) -> impl Fn(StyleRefinement) -> StyleRefinement + use<> {
        let hover = self.palette.fill_subtle_hover;
        move |style| style.bg(hover)
    }

    /// The accent bar on the left edge of a selected row, centered in `row_height`.
    pub(crate) fn indicator_bar(&self, color: Hsla, row_height: Pixels) -> Div {
        let layout = &self.metrics.layout;
        div()
            .absolute()
            .left(px(0.))
            .top((row_height - layout.indicator_height) / 2.)
            .w(layout.indicator_width)
            .h(layout.indicator_height)
            .rounded_full()
            .bg(color)
    }

    /// A small square in a category color, for legends and chips.
    pub(crate) fn swatch(&self, color: Hsla) -> Div {
        div()
            .size(if self.palette.contrast == ContrastMode::High {
                self.metrics.density(Density::Compact).icon_size
            } else {
                px(SWATCH_SIZE)
            })
            .rounded(self.metrics.timeline.block_radius)
            .bg(color)
    }

    /// Caption buttons retain native Windows hit testing with compact Fluent glyphs (11 sections 5.4 and 7).
    pub(crate) fn title_bar(&self, title: String, window: &Window) -> Div {
        let supported = window.window_controls();
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(self.metrics.layout.title_bar_height)
            .bg(self.palette.window_background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .h_full()
                    .pl(self.metrics.space.m)
                    .window_control_area(WindowControlArea::Drag)
                    .ramp(self, Ramp::Caption)
                    .text_color(self.palette.text_primary)
                    .child(title),
            );
        if supported.minimize {
            bar = bar.child(self.caption_control(IconName::WindowMinimize, WindowControlArea::Min));
        }
        if supported.maximize {
            let icon = if window.is_maximized() {
                IconName::WindowRestore
            } else {
                IconName::WindowMaximize
            };
            bar = bar.child(self.caption_control(icon, WindowControlArea::Max));
        }
        bar.child(self.caption_control(IconName::WindowClose, WindowControlArea::Close))
    }

    fn caption_control(&self, icon: IconName, area: WindowControlArea) -> Stateful<Div> {
        let close = area == WindowControlArea::Close;
        let hover = if close {
            self.palette.status_critical
        } else {
            self.palette.fill_control_hover
        };
        let foreground = if close {
            self.palette.text_on(hover)
        } else {
            self.palette.text_primary
        };
        let pressed = if close {
            hover
        } else {
            self.palette.fill_control_pressed
        };
        div()
            .id(AppAssets::icon_path(
                icon,
                Density::Compact,
                IconVariant::Regular,
            ))
            .flex()
            .items_center()
            .justify_center()
            .w(px(CAPTION_CONTROL_WIDTH))
            .h_full()
            .window_control_area(area)
            .text_color(self.palette.text_primary)
            .hover(move |style| style.bg(hover).text_color(foreground))
            .active(move |style| style.bg(pressed).text_color(foreground))
            .child(
                Icon::default()
                    .path(AppAssets::icon_path(
                        icon,
                        Density::Compact,
                        IconVariant::Regular,
                    ))
                    .size(self.metrics.density(Density::Compact).icon_size),
            )
    }

    /// A card: translucent fill with a 1 px border and no shadow (11 section 2).
    pub(crate) fn card(&self) -> Div {
        div()
            .bg(self.palette.fill_card)
            .border_1()
            .border_color(self.palette.stroke_card)
            .rounded(self.metrics.radius.overlay)
    }

    /// The box around a chart plot: directly on `bg.base`, no card fill (07 section 10).
    pub(crate) fn plot_area(&self) -> Div {
        div()
            .bg(self.palette.bg_base)
            .rounded(self.metrics.radius.control)
    }

    /// The box shadows of the elevation `layers`.
    pub(crate) fn shadows(&self, layers: &[ShadowLayer]) -> Vec<BoxShadow> {
        layers
            .iter()
            .map(|layer| BoxShadow {
                color: match layer.color {
                    ShadowColor::Ambient => self.palette.shadow_ambient,
                    ShadowColor::Key => self.palette.shadow_key,
                },
                offset: point(px(0.), layer.offset_y),
                blur_radius: layer.blur,
                spread_radius: px(0.),
                inset: false,
            })
            .collect()
    }

    /// A tooltip surface with the `shadow.16` elevation (11 sections 5.1 and 6).
    pub(crate) fn flyout(&self) -> Div {
        div()
            .bg(self.palette.bg_flyout)
            .border_1()
            .border_color(self.palette.stroke_surface)
            .rounded(self.metrics.radius.control)
            .shadow(self.shadows(&self.metrics.shadow.tooltip))
    }

    /// A bordered push button. Labeled buttons get the wider padding and, when their row is too
    /// narrow, shrink and end their label with an ellipsis (10 section 6.3); icons never shrink.
    pub(crate) fn button(
        &self,
        id: impl Into<ElementId>,
        density: Density,
        content: ButtonContent,
    ) -> Stateful<Div> {
        self.styled_button(id, density, content, ButtonStyle::Standard)
    }

    pub(crate) fn disabled_button(
        &self,
        id: impl Into<ElementId>,
        density: Density,
        content: ButtonContent,
    ) -> Stateful<Div> {
        self.styled_button(id, density, content, ButtonStyle::Disabled)
    }

    /// Accent actions use the matching icon and label foreground, including high contrast.
    pub(crate) fn accent_button(
        &self,
        id: impl Into<ElementId>,
        density: Density,
        content: ButtonContent,
    ) -> Stateful<Div> {
        self.styled_button(id, density, content, ButtonStyle::Accent)
    }

    fn styled_button(
        &self,
        id: impl Into<ElementId>,
        density: Density,
        content: ButtonContent,
        appearance: ButtonStyle,
    ) -> Stateful<Div> {
        let colors = appearance.colors(self.palette);
        let space = &self.metrics.space;
        let parts = content.into_parts();
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(space.s)
            .min_h(self.metrics.density(density).control_height)
            .px(if parts.label.is_some() {
                space.m
            } else {
                space.s
            })
            .bg(colors.rest)
            .border_1()
            .border_color(colors.border)
            .rounded(self.metrics.radius.control)
            .text_color(colors.foreground)
            .ramp(self, Ramp::Body)
            .map(|button| match appearance {
                ButtonStyle::Disabled => button.cursor_default(),
                ButtonStyle::Standard | ButtonStyle::Accent => button.cursor_pointer(),
            })
            .hover(move |style| style.bg(colors.hover))
            .active(move |style| style.bg(colors.pressed))
            .when_some(parts.icon, |button, name| {
                button.child(div().flex_shrink_0().child(self.icon(
                    name,
                    density,
                    colors.foreground,
                )))
            })
            .map(|button| match parts.label {
                Some(text) => button.min_w(px(0.)).child(
                    div()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(text),
                ),
                None => button.flex_shrink_0(),
            })
    }

    /// A segmented control; `make` builds the click listener for one value.
    pub(crate) fn segmented<V, L>(
        &self,
        id: &'static str,
        items: impl IntoIterator<Item = Segment<V>>,
        selected: V,
        make: impl Fn(V) -> L,
    ) -> Stateful<Div>
    where
        V: Copy + PartialEq + 'static,
        L: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    {
        let palette = self.palette;
        let space = &self.metrics.space;
        let control = div()
            .id(id)
            .min_w(px(0.))
            .max_w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(space.xxs)
            .p(space.xxs)
            .min_h(px(SEGMENTED_HEIGHT))
            .bg(palette.fill_control)
            .border_1()
            .border_color(palette.stroke_control_strong)
            .rounded(self.metrics.radius.control);
        control.children(items.into_iter().map(|item| {
            let active = item.value == selected;
            div()
                .id(item.id)
                .flex()
                .min_w(px(0.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .items_center()
                .min_h(px(SEGMENTED_ITEM_HEIGHT))
                .px(px(SEGMENTED_ITEM_PADDING))
                .rounded(px(SEGMENTED_ITEM_RADIUS))
                .ramp(self, Ramp::Caption)
                .text_color(if active {
                    palette.text_primary
                } else {
                    palette.text_secondary
                })
                .when(active, |item| {
                    item.bg(palette.bg_flyout)
                        .border_1()
                        .border_color(palette.stroke_control)
                })
                .when(!active, |item| item.hover(self.subtle_hover()))
                .cursor_pointer()
                .on_click(make(item.value))
                .child(item.label)
        }))
    }
}

/// Applies a type ramp step to any styled element.
pub(crate) trait RampExt: Styled + Sized {
    /// Sets the size, line height, weight and, for small text, the family of `ramp`, scaled by the
    /// text scale of `widgets`.
    fn ramp(self, widgets: &Widgets<'_>, ramp: Ramp) -> Self {
        let step = widgets.step(ramp);
        let styled = self
            .text_size(step.font_size)
            .line_height(step.line_height)
            .font_weight(step.weight);
        match step.font_family {
            Some(family) => styled.font_family(family),
            None => styled,
        }
    }

    /// Equal-width digits for text that changes or lines up in columns.
    fn tabular_figures(self) -> Self {
        self.font_features(Widgets::tabular_figures())
    }
}

impl<T: Styled> RampExt for T {}

impl From<Glyph> for IconName {
    fn from(glyph: Glyph) -> Self {
        match glyph {
            Glyph::Editor => IconName::Code,
            Glyph::Browser => IconName::Globe,
            Glyph::Chat => IconName::MessageCircle,
            Glyph::Messenger => IconName::Send,
            Glyph::Terminal => IconName::SquareTerminal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{runtime::ThemeBundle, tokens::SystemColors};

    #[test]
    fn high_contrast_skeletons_have_opaque_system_surfaces_and_gray_text_borders() {
        let theme = ThemeBundle::embedded().expect("embedded theme");
        let tokens = theme.tokens();
        let system: SystemColors =
            serde_json::from_str(include_str!("../../themes/preview-high-contrast.json"))
                .expect("system colors");
        let palette = Palette::high_contrast(&system);
        let ui = Widgets {
            palette: &palette,
            metrics: tokens.metrics(),
            text_scale: TextScale::UNSCALED,
        };
        for tone in [SkeletonTone::Strong, SkeletonTone::Subtle] {
            let mut skeleton = ui.skeleton_block(tone);
            assert_eq!(skeleton.style().background, Some(system.window.into()));
            assert_eq!(skeleton.style().border_color, Some(system.gray_text));
            assert_eq!(
                skeleton.style().border_widths.top,
                Some(tokens.metrics().layout.divider.into())
            );
        }
    }

    #[test]
    fn every_button_constructor_builds_without_a_window() {
        let theme = ThemeBundle::embedded().expect("embedded theme");
        let tokens = theme.tokens();
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let ui = Widgets::new(&tokens, mode, TextScale::UNSCALED);
            for density in [Density::Standard, Density::Compact] {
                for content in [
                    ButtonContent::Label("Button".into()),
                    ButtonContent::Icon(IconName::Calendar),
                    ButtonContent::IconLabel(IconName::Calendar, "Button".into()),
                ] {
                    drop(ui.button("standard", density, content.clone()));
                    drop(ui.disabled_button("disabled", density, content.clone()));
                    drop(ui.accent_button("accent", density, content));
                }
            }
        }
    }

    #[test]
    fn caption_and_segmented_controls_build_without_a_window() {
        let theme = ThemeBundle::embedded().expect("embedded theme");
        let tokens = theme.tokens();
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let ui = Widgets::new(&tokens, mode, TextScale::UNSCALED);
            drop(ui.caption_control(IconName::WindowMinimize, WindowControlArea::Min));
            drop(ui.caption_control(IconName::WindowMaximize, WindowControlArea::Max));
            drop(ui.caption_control(IconName::WindowRestore, WindowControlArea::Max));
            drop(ui.caption_control(IconName::WindowClose, WindowControlArea::Close));
            for selected in [ThemeMode::Light, ThemeMode::Dark] {
                drop(ui.segmented(
                    "appearance",
                    [
                        Segment {
                            id: "light",
                            label: "Light".into(),
                            value: ThemeMode::Light,
                        },
                        Segment {
                            id: "dark",
                            label: "Dark".into(),
                            value: ThemeMode::Dark,
                        },
                    ],
                    selected,
                    |_| |_, _, _| {},
                ));
            }
        }
    }

    #[test]
    fn button_colors_preserve_standard_disabled_and_accent_states() {
        let theme = ThemeBundle::embedded().expect("embedded theme");
        let tokens = theme.tokens();
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let palette = tokens.palette(mode);
            let standard = ButtonStyle::Standard.colors(palette);
            assert_eq!(standard.rest, palette.fill_control);
            assert_eq!(standard.hover, palette.fill_control_hover);
            assert_eq!(standard.pressed, palette.fill_control_pressed);
            assert_eq!(standard.foreground, palette.text_primary);
            assert_eq!(standard.border, palette.stroke_control);

            let disabled = ButtonStyle::Disabled.colors(palette);
            for fill in [disabled.rest, disabled.hover, disabled.pressed] {
                assert_eq!(fill, palette.fill_control_disabled);
            }
            assert_eq!(disabled.foreground, palette.text_disabled);
            assert_eq!(disabled.border, palette.stroke_control);

            let accent = ButtonStyle::Accent.colors(palette);
            assert_eq!(accent.rest, palette.accent_fill);
            assert_eq!(accent.hover, palette.accent_fill_hover);
            assert_eq!(accent.pressed, palette.accent_fill);
            assert_eq!(accent.foreground, palette.text_on_accent);
            assert_eq!(accent.border, palette.accent_fill);
        }
    }
}
