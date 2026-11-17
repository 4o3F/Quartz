//! Fluent-styled building blocks shared by the windows. Colors and sizes come only from the
//! theme tokens.

use gpui_kit::{
    App, BoxShadow, ClickEvent, Div, ElementId, Font, Hsla, InteractiveElement as _,
    ParentElement as _, Pixels, Stateful, StatefulInteractiveElement as _, StyleRefinement, Styled,
    Window,
    assets::IconName,
    component::{Icon, ThemeMode, TitleBar, label::Label},
    div, font, point,
    prelude::FluentBuilder as _,
    px,
};

use super::{
    metrics::{Density, Metrics, Ramp, ShadowColor},
    tokens::{Palette, Tokens},
};
use crate::model::Glyph;

/// Height of the segmented control: the compact control height plus a 2 px inset on each side.
const SEGMENTED_HEIGHT: f32 = 28.;
/// Height of one segment: the compact control height minus the 2 px inset on each side.
const SEGMENTED_ITEM_HEIGHT: f32 = 20.;
/// Horizontal padding of one segment.
const SEGMENTED_ITEM_PADDING: f32 = 10.;
/// Corner radius of one segment, 1 px inside `radius.control` so the corners nest.
const SEGMENTED_ITEM_RADIUS: f32 = 3.;

/// Side of the square category swatch in legends and chips.
const SWATCH_SIZE: f32 = 8.;

/// What a push button shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ButtonContent {
    Label(String),
    Icon(IconName),
    IconLabel(IconName, String),
}

/// One choice of a segmented control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment<V> {
    /// Element id, unique within the control.
    pub(crate) id: &'static str,
    pub(crate) label: String,
    pub(crate) value: V,
}

/// The palette of the current appearance with the metrics, and the builders that use them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Widgets<'a> {
    pub(crate) palette: &'a Palette,
    pub(crate) metrics: &'a Metrics,
}

impl<'a> Widgets<'a> {
    /// The widgets of the `mode` appearance.
    pub(crate) const fn new(tokens: &'a Tokens, mode: ThemeMode) -> Self {
        Self {
            palette: tokens.palette(mode),
            metrics: tokens.metrics(),
        }
    }

    /// A themed icon sized for `density` (11 section 7).
    pub(crate) fn icon(&self, name: IconName, density: Density, color: Hsla) -> Icon {
        Icon::new(name)
            .size(self.metrics.density(density).icon_size)
            .text_color(color)
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
            .size(px(SWATCH_SIZE))
            .rounded(self.metrics.timeline.block_radius)
            .bg(color)
    }

    /// The custom title bar on `bg.base` with the window title in caption type.
    pub(crate) fn title_bar(&self, title: String) -> TitleBar {
        TitleBar::new()
            .h(self.metrics.layout.title_bar_height)
            .pl(self.metrics.space.m)
            .border_b_0()
            .bg(self.palette.bg_base)
            .child(
                div()
                    .ramp(self, Ramp::Caption)
                    .text_color(self.palette.text_primary)
                    .child(Label::new(title)),
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

    /// A tooltip surface with the `shadow.16` elevation (11 sections 5.1 and 6).
    pub(crate) fn flyout(&self) -> Div {
        let shadows = self
            .metrics
            .shadow
            .tooltip
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
            .collect();
        div()
            .bg(self.palette.bg_flyout)
            .border_1()
            .border_color(self.palette.stroke_surface)
            .rounded(self.metrics.radius.control)
            .shadow(shadows)
    }

    /// A bordered push button. Labeled buttons get the wider padding and, when their row is too
    /// narrow, shrink and end their label with an ellipsis (10 section 6.3); icons never shrink.
    pub(crate) fn button(
        &self,
        id: impl Into<ElementId>,
        density: Density,
        content: ButtonContent,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let space = &self.metrics.space;
        let (hover, pressed) = (palette.fill_control_hover, palette.fill_control_pressed);
        let (icon, label) = match content {
            ButtonContent::Label(label) => (None, Some(label)),
            ButtonContent::Icon(icon) => (Some(icon), None),
            ButtonContent::IconLabel(icon, label) => (Some(icon), Some(label)),
        };
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(space.s)
            .h(self.metrics.density(density).control_height)
            .px(if label.is_some() { space.m } else { space.s })
            .bg(palette.fill_control)
            .border_1()
            .border_color(palette.stroke_control)
            .rounded(self.metrics.radius.control)
            .text_color(palette.text_primary)
            .ramp(self, Ramp::Body)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(pressed))
            .when_some(icon, |button, name| {
                button.child(div().flex_shrink_0().child(self.icon(
                    name,
                    density,
                    palette.text_primary,
                )))
            })
            .map(|button| match label {
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
            .flex()
            .flex_row()
            .items_center()
            .gap(space.xxs)
            .p(space.xxs)
            .h(px(SEGMENTED_HEIGHT))
            .bg(palette.fill_control)
            .border_1()
            .border_color(palette.stroke_control_strong)
            .rounded(self.metrics.radius.control);
        control.children(items.into_iter().map(|item| {
            let active = item.value == selected;
            div()
                .id(item.id)
                .flex()
                .items_center()
                .h(px(SEGMENTED_ITEM_HEIGHT))
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
    /// Sets the size, line height, weight and, for small text, the family of `ramp`.
    fn ramp(self, widgets: &Widgets<'_>, ramp: Ramp) -> Self {
        let step = widgets.metrics.ramp(ramp);
        let styled = self
            .text_size(step.font_size)
            .line_height(step.line_height)
            .font_weight(step.weight);
        match &step.font_family {
            Some(family) => styled.font_family(family.clone()),
            None => styled,
        }
    }
}

impl<T: Styled> RampExt for T {}

impl From<Glyph> for IconName {
    fn from(glyph: Glyph) -> Self {
        match glyph {
            Glyph::Editor => Self::Code,
            Glyph::Browser => Self::Globe,
            Glyph::Chat => Self::MessageCircle,
            Glyph::Messenger => Self::Send,
            Glyph::Terminal => Self::SquareTerminal,
        }
    }
}
