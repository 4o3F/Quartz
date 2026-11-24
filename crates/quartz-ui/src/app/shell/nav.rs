//! Left navigation: 240 px sidebar that collapses to a 48 px icon rail (11 section 5.4).

use gpui_kit::{
    Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px,
};

use super::{AppShell, Page};
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{RampExt as _, Widgets},
    },
    layout::{NavLayout, PanelState, ShellLayout},
};

/// Smallest height of a navigation item, as in the WinUI navigation view; it grows with the text.
const ITEM_HEIGHT: f32 = 36.;

impl AppShell {
    /// Only the Expanded class has a collapse button: the other classes always show the rail.
    pub(super) fn render_nav(
        &self,
        ui: &Widgets<'_>,
        shell: ShellLayout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let layout = shell.nav;
        let space = &ui.metrics.space;
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(ui.metrics.nav_width(layout))
            .h_full()
            .p(space.xs)
            .gap(space.xxs)
            .when(
                shell.shows_nav_toggle(if self.details.is_some() {
                    PanelState::Open
                } else {
                    PanelState::Closed
                }),
                |nav| {
                    nav.child(
                        div()
                            .id("nav-toggle")
                            .flex()
                            .items_center()
                            .min_h(px(ITEM_HEIGHT))
                            .px(space.m)
                            .rounded(ui.metrics.radius.control)
                            .cursor_pointer()
                            .hover(ui.subtle_hover())
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_nav(cx)))
                            .child(ui.icon(
                                IconName::Menu,
                                Density::Compact,
                                ui.palette.text_primary,
                            )),
                    )
                },
            )
            .child(
                div().flex().flex_col().gap(space.xxs).children(
                    Page::NAV_ITEMS
                        .into_iter()
                        .map(|page| self.nav_item(ui, layout, page, cx)),
                ),
            )
            .child(div().flex_1())
            .child(self.nav_item(ui, layout, Page::Settings, cx))
    }

    fn nav_item(
        &self,
        ui: &Widgets<'_>,
        layout: NavLayout,
        page: Page,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.page == page;
        let palette = ui.palette;
        div()
            .id(page.element_id())
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui.metrics.space.m)
            .min_h(px(ITEM_HEIGHT))
            .px(ui.metrics.space.m)
            .rounded(ui.metrics.radius.control)
            .ramp(ui, Ramp::Body)
            .text_color(palette.text_primary)
            .cursor_pointer()
            .when(selected, |item| {
                item.bg(palette.fill_subtle_hover)
                    .child(ui.indicator_bar(palette.accent_fill, px(ITEM_HEIGHT)))
            })
            .when(!selected, |item| item.hover(ui.subtle_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.set_page(page, cx)))
            .child(ui.variant_icon(
                page.icon(),
                Density::Standard,
                palette.text_primary,
                if selected {
                    crate::app::icons::IconVariant::Selected
                } else {
                    crate::app::icons::IconVariant::Regular
                },
            ))
            .when(layout == NavLayout::Expanded, |item| {
                item.child(
                    div()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(self.formatter.text(page.label())),
                )
            })
    }
}
