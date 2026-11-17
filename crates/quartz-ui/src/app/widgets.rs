//! Small Fluent-styled building blocks shared by the pages. Colors come only from the palette.

use gpui_kit::{
    App, BoxShadow, ClickEvent, Div, FontWeight, Hsla, InteractiveElement as _, ParentElement as _,
    Stateful, StatefulInteractiveElement as _, Styled, Window, div, point,
    prelude::FluentBuilder as _, px,
};
use gpui_kit::{assets::IconName, component::Icon};

use super::{model::Glyph, tokens::Palette};

use super::tokens::metrics::{CAPTION_FONT, ControlHeight};
pub use super::tokens::metrics::{RADIUS_CONTROL, RADIUS_OVERLAY, Ramp, space};

/// Applies a type ramp step to any styled element.
pub trait RampExt: Styled + Sized {
    fn ramp(self, ramp: Ramp) -> Self {
        let (size, line, semibold) = ramp.metrics();
        let styled = self.text_size(px(size)).line_height(px(line));
        let styled = if ramp == Ramp::Caption {
            styled.font_family(CAPTION_FONT)
        } else {
            styled
        };
        if semibold {
            styled.font_weight(FontWeight::SEMIBOLD)
        } else {
            styled
        }
    }
}

impl<T: Styled> RampExt for T {}

/// Row heights and control sizes (11 section 5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    Standard,
    Compact,
}

impl Density {
    const fn control_height(self) -> f32 {
        match self {
            Self::Standard => ControlHeight::STANDARD,
            Self::Compact => ControlHeight::COMPACT,
        }
    }
}

/// A themed icon.
pub fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

impl Palette {
    /// A card: translucent fill with a 1 px border and no shadow (11 section 2).
    pub fn card(&self) -> Div {
        div()
            .bg(self.fill_card)
            .border_1()
            .border_color(self.stroke_card)
            .rounded(px(RADIUS_OVERLAY))
    }
    /// The box around a chart plot: directly on `bg.base`, no card fill (07 section 10).
    pub fn plot_area(&self) -> Div {
        div().bg(self.bg_base).rounded(px(RADIUS_CONTROL))
    }
    /// A flyout surface (tooltips, menus).
    pub fn flyout(&self) -> Div {
        div()
            .bg(self.bg_flyout)
            .border_1()
            .border_color(self.stroke_surface)
            .rounded(px(RADIUS_OVERLAY))
            .shadow(vec![
                BoxShadow {
                    color: self.shadow_ambient,
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.),
                    inset: false,
                },
                BoxShadow {
                    color: self.shadow_key,
                    offset: point(px(0.), px(8.)),
                    blur_radius: px(16.),
                    spread_radius: px(0.),
                    inset: false,
                },
            ])
    }
    /// A bordered push button with an optional leading icon.
    pub fn button(
        &self,
        id: impl Into<gpui_kit::ElementId>,
        density: Density,
        label: Option<String>,
        leading: Option<IconName>,
    ) -> Stateful<Div> {
        let (hover, pressed) = (self.fill_control_hover, self.fill_control_pressed);
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(space::S))
            .h(px(density.control_height()))
            .px(px(if label.is_some() { space::M } else { space::S }))
            .bg(self.fill_control)
            .border_1()
            .border_color(self.stroke_control)
            .rounded(px(RADIUS_CONTROL))
            .text_color(self.text_primary)
            .ramp(Ramp::Body)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(pressed))
            .when_some(leading, |button, name| {
                button.child(icon(name, 16., self.text_primary))
            })
            .when_some(label, |button, text| button.child(text))
    }
    /// A segmented control; `make` builds the click listener for one value.
    pub fn segmented<V, L>(
        &self,
        id: &'static str,
        items: Vec<(String, V)>,
        selected: V,
        make: impl Fn(V) -> L,
    ) -> Stateful<Div>
    where
        V: Copy + PartialEq + 'static,
        L: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    {
        let mut control = div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(space::XXS))
            .p(px(space::XXS))
            .h(px(Density::Compact.control_height() + 2. * space::XXS))
            .bg(self.fill_control)
            .border_1()
            .border_color(self.stroke_control_strong)
            .rounded(px(RADIUS_CONTROL));
        for (index, (label, value)) in items.into_iter().enumerate() {
            let active = value == selected;
            let hover = self.fill_subtle_hover;
            control = control.child(
                div()
                    .id((id, index))
                    .flex()
                    .items_center()
                    .h(px(Density::Compact.control_height() - 2. * space::XXS))
                    .px(px(space::M - 2.))
                    .rounded(px(RADIUS_CONTROL - 1.))
                    .ramp(Ramp::Caption)
                    .text_color(if active {
                        self.text_primary
                    } else {
                        self.text_secondary
                    })
                    .when(active, |item| {
                        item.bg(self.bg_flyout)
                            .border_1()
                            .border_color(self.stroke_control)
                    })
                    .when(!active, |item| item.hover(move |style| style.bg(hover)))
                    .cursor_pointer()
                    .on_click(make(value))
                    .child(label),
            );
        }
        control
    }
}

impl Glyph {
    /// Placeholder icon of an application glyph.
    #[must_use]
    pub const fn icon_name(self) -> IconName {
        match self {
            Glyph::Editor => IconName::Code,
            Glyph::Browser | Glyph::Website => IconName::Globe,
            Glyph::Chat => IconName::MessageCircle,
            Glyph::Messenger => IconName::Send,
            Glyph::Terminal => IconName::SquareTerminal,
            Glyph::Document => IconName::FileText,
            Glyph::Generic => IconName::AppWindow,
        }
    }
}
