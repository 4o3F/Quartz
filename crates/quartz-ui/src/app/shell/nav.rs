//! Left navigation: 240 px sidebar that collapses to a 48 px icon rail (11 section 5.4).

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
    prelude::FluentBuilder as _, px,
};

use super::{AppShell, Page};
use crate::app::{
    tokens::{
        Palette,
        metrics::{NAV_WIDTH, RAIL_WIDTH},
    },
    widgets::{RADIUS_CONTROL, Ramp, RampExt as _, icon, space},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NavLayout {
    Expanded,
    Rail,
}

const ITEM_HEIGHT: f32 = 36.;

impl AppShell {
    pub(super) fn render_nav(
        &self,
        p: &Palette,
        layout: NavLayout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let items = [
            (Page::Today, IconName::Calendar, "nav.today"),
            (Page::Week, IconName::CalendarRange, "nav.week"),
            (Page::Rules, IconName::Tag, "nav.rules"),
        ];
        let mut list = div().flex().flex_col().gap(px(space::XXS));
        for (page, glyph, key) in items {
            list = list.child(self.nav_item(p, layout, page, glyph, key, cx));
        }
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(if layout == NavLayout::Rail {
                RAIL_WIDTH
            } else {
                NAV_WIDTH
            }))
            .h_full()
            .p(px(space::XS))
            .gap(px(space::XXS))
            .child(
                div()
                    .id("nav-toggle")
                    .flex()
                    .items_center()
                    .h(px(ITEM_HEIGHT))
                    .px(px(space::M))
                    .rounded(px(RADIUS_CONTROL))
                    .cursor_pointer()
                    .hover({
                        let hover = p.fill_subtle_hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.nav_collapsed_by_user = !this.nav_collapsed_by_user;
                        cx.notify();
                    }))
                    .child(icon(IconName::Menu, 16., p.text_primary)),
            )
            .child(list)
            .child(div().flex_1())
            .child(self.nav_item(
                p,
                layout,
                Page::Settings,
                IconName::Settings,
                "nav.settings",
                cx,
            ))
    }

    fn nav_item(
        &self,
        p: &Palette,
        layout: NavLayout,
        page: Page,
        glyph: IconName,
        key: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.page == page;
        let hover = p.fill_subtle_hover;
        div()
            .id(("nav", page as usize))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(space::M))
            .h(px(ITEM_HEIGHT))
            .px(px(space::M))
            .rounded(px(RADIUS_CONTROL))
            .ramp(Ramp::Body)
            .text_color(p.text_primary)
            .cursor_pointer()
            .when(selected, |item| item.bg(p.fill_subtle_hover))
            .when(!selected, |item| item.hover(move |style| style.bg(hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.set_page(page, cx)))
            .when(selected, |item| {
                item.child(
                    div()
                        .absolute()
                        .left(px(0.))
                        .top(px((ITEM_HEIGHT - 16.) / 2.))
                        .w(px(3.))
                        .h(px(16.))
                        .rounded_full()
                        .bg(p.accent_fill),
                )
            })
            .child(icon(glyph, 16., p.text_primary))
            .when(layout == NavLayout::Expanded, |item| {
                item.child(self.fmt.text(key))
            })
    }
}
