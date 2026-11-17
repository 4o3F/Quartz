//! The application shell: title bar, navigation, pages and the right-hand details panel (07 section 2).

mod charts;
mod details;
mod nav;
mod pending;
mod ranking;
mod timeline;
mod today;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, Render,
    Styled as _, Window,
    component::{ActiveTheme as _, TitleBar, label::Label},
    deferred, div, point,
    prelude::FluentBuilder as _,
    px,
};

use super::{
    format::Formatter,
    model::{Day, DetailsSubject, RankingKind, SegmentId},
    tokens::{
        Tokens,
        metrics::{NAV_WIDTH, PANEL_WIDTH, RAIL_WIDTH},
    },
    widgets::{Ramp, RampExt as _},
};

/// Top-level pages of the navigation (07 section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Today,
    Week,
    Rules,
    Settings,
}

/// Which tracks the timeline shows below the category main track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LaneMode {
    Swimlanes,
    CategoryOnly,
}

/// Visible time range of the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum RangeChoice {
    /// First to last activity with 15 minutes of margin (07 section 4.1).
    Activity,
    FullDay,
    Zoomed {
        start_minute: f32,
        end_minute: f32,
    },
}

/// What the pointer is over in the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HoverTarget {
    Segment(SegmentId),
    MainBlock(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Hover {
    pub target: HoverTarget,
    pub position: Point<Pixels>,
}

/// Initial state of the shell, so a preview can open any page.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellInit {
    pub day: Day,
    pub formatter: Formatter,
    pub page: Page,
    pub details: Option<DetailsSubject>,
    /// Opens with the tooltip of this segment showing, for previews.
    pub hover_segment: Option<SegmentId>,
}

/// Root view of the main window.
#[derive(Debug)]
pub struct AppShell {
    day: Day,
    fmt: Formatter,
    page: Page,
    nav_collapsed_by_user: bool,
    details: Option<DetailsSubject>,
    ranking: RankingKind,
    lanes: LaneMode,
    range: RangeChoice,
    hover: Option<Hover>,
}

impl AppShell {
    /// Builds the shell from its initial state.
    #[must_use]
    pub fn new(init: ShellInit) -> Self {
        Self {
            day: init.day,
            fmt: init.formatter,
            page: init.page,
            nav_collapsed_by_user: false,
            details: init.details,
            ranking: RankingKind::default(),
            lanes: LaneMode::Swimlanes,
            range: RangeChoice::Activity,
            hover: init.hover_segment.map(|id| Hover {
                target: HoverTarget::Segment(id),
                position: point(px(900.), px(330.)),
            }),
        }
    }

    fn select(&mut self, subject: DetailsSubject, cx: &mut Context<Self>) {
        self.details = Some(subject);
        self.hover = None;
        cx.notify();
    }

    fn close_details(&mut self, cx: &mut Context<Self>) {
        self.details = None;
        cx.notify();
    }

    fn set_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page != page {
            self.page = page;
            self.hover = None;
            cx.notify();
        }
    }

    fn set_hover(&mut self, target: HoverTarget, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.hover.map(|hover| hover.target) != Some(target) {
            self.hover = Some(Hover { target, position });
            cx.notify();
        }
    }

    fn clear_hover(&mut self, target: HoverTarget, cx: &mut Context<Self>) {
        if self.hover.map(|hover| hover.target) == Some(target) {
            self.hover = None;
            cx.notify();
        }
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (font_family, mode) = {
            let theme = cx.theme();
            (theme.font_family.clone(), theme.mode)
        };
        let tokens = cx.global::<Tokens>();
        let p = tokens.palette(mode).clone();
        let chrome = tokens.chrome.clone();
        let details = self
            .details
            .and_then(|subject| self.day.details(subject).map(|content| (subject, content)));
        let rail = self.nav_collapsed_by_user || details.is_some();
        let viewport = f32::from(window.viewport_size().width);
        let nav_width = if rail { RAIL_WIDTH } else { NAV_WIDTH };
        let panel_width = if details.is_some() { PANEL_WIDTH } else { 0. };
        let main_width = viewport - nav_width - panel_width;

        let title_bar = TitleBar::new()
            .h(px(chrome.title_bar_height))
            .pl(px(chrome.title_padding))
            .border_b_0()
            .bg(p.bg_base)
            .child(
                div()
                    .text_size(px(chrome.caption_font_size))
                    .line_height(px(chrome.caption_line_height))
                    .font_family(chrome.caption_font_family.clone())
                    .text_color(p.text_primary)
                    .child(Label::new(self.fmt.text("app.name"))),
            );
        let page = match self.page {
            Page::Today => self.render_today(&p, main_width, cx).into_any_element(),
            Page::Week => self.render_placeholder(&p, "nav.week").into_any_element(),
            Page::Rules => self.render_placeholder(&p, "nav.rules").into_any_element(),
            Page::Settings => self
                .render_placeholder(&p, "nav.settings")
                .into_any_element(),
        };
        let tooltip = if self.page == Page::Today {
            self.render_tooltip(&p)
        } else {
            None
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(p.bg_base)
            .text_color(p.text_primary)
            .font_family(font_family)
            .ramp(Ramp::Body)
            .child(title_bar)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_nav(
                        &p,
                        if rail {
                            nav::NavLayout::Rail
                        } else {
                            nav::NavLayout::Expanded
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .id("main-area")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .bg(p.fill_layer)
                            .border_t_1()
                            .border_l_1()
                            .border_color(p.stroke_card)
                            .rounded_tl(px(super::widgets::RADIUS_OVERLAY))
                            .child(page),
                    )
                    .when_some(details, |row, (_, content)| {
                        row.child(self.render_details(&p, &content, cx))
                    }),
            )
            .children(tooltip.map(deferred))
    }
}

impl AppShell {
    fn render_placeholder(&self, p: &super::tokens::Palette, key: &str) -> impl IntoElement {
        div()
            .p(px(super::widgets::space::XXL))
            .ramp(Ramp::Title)
            .text_color(p.text_primary)
            .child(self.fmt.text(key))
    }
}
