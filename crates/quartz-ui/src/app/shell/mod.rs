//! The application shell: title bar, navigation, pages and the right-hand details panel (07 section 2).

mod charts;
mod details;
mod nav;
mod pending;
mod ranking;
mod timeline;
mod today;

use std::rc::Rc;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, Render,
    Styled as _, Window, assets::IconName, component::ActiveTheme as _, deferred, div,
    prelude::FluentBuilder as _, px,
};

use super::{
    metrics::{Metrics, Ramp},
    tokens::Tokens,
    widgets::{RampExt as _, Widgets},
};
use crate::{
    format::{Formatter, Msg},
    model::{Day, Details, DetailsSubject, RankingKind, SegmentId, TimeOfDay, TimeSpan},
};

/// Top-level pages of the navigation (07 section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Page {
    Today,
    Week,
    Rules,
    Settings,
}

impl Page {
    /// Pages listed at the top of the navigation; settings sits at the bottom.
    const NAV_ITEMS: [Self; 3] = [Self::Today, Self::Week, Self::Rules];

    /// Navigation icon.
    const fn icon(self) -> IconName {
        match self {
            Self::Today => IconName::Calendar,
            Self::Week => IconName::CalendarRange,
            Self::Rules => IconName::Tag,
            Self::Settings => IconName::Settings,
        }
    }

    /// Navigation label and page title.
    const fn label(self) -> Msg {
        match self {
            Self::Today => Msg::NavToday,
            Self::Week => Msg::NavWeek,
            Self::Rules => Msg::NavRules,
            Self::Settings => Msg::NavSettings,
        }
    }

    /// Element id of the navigation item.
    const fn element_id(self) -> &'static str {
        match self {
            Self::Today => "nav-today",
            Self::Week => "nav-week",
            Self::Rules => "nav-rules",
            Self::Settings => "nav-settings",
        }
    }
}

/// Which tracks the timeline shows below the category main track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum LaneMode {
    Swimlanes,
    CategoryOnly,
}

/// Visible time range of the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum RangeChoice {
    /// First to last activity with 15 minutes of margin (07 section 4.1).
    Activity,
    FullDay,
    /// A span reached by zooming; never empty.
    Zoomed(TimeSpan),
}

/// What the pointer is over in the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum HoverTarget {
    Segment(SegmentId),
    /// The main-track block starting at this time; main-track blocks never overlap.
    MainBlock(TimeOfDay),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Hover {
    pub(crate) target: HoverTarget,
    pub(crate) position: Point<Pixels>,
}

/// How the navigation is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum NavLayout {
    Expanded,
    /// The 48 px icon rail.
    Rail,
}

impl NavLayout {
    const fn toggled(self) -> Self {
        match self {
            Self::Expanded => Self::Rail,
            Self::Rail => Self::Expanded,
        }
    }

    fn width(self, metrics: &Metrics) -> Pixels {
        match self {
            Self::Expanded => metrics.layout.nav_width,
            Self::Rail => metrics.layout.rail_width,
        }
    }
}

/// Initial state of the shell, so a preview can open any page.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ShellInit {
    pub(crate) day: Day,
    pub(crate) formatter: Formatter,
    pub(crate) tokens: Rc<Tokens>,
    pub(crate) page: Page,
    pub(crate) details: Option<DetailsSubject>,
}

/// The open details sidebar: its subject and the content computed when it opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenDetails {
    pub(crate) subject: DetailsSubject,
    pub(crate) content: Details,
}

/// Root view of the main window.
#[derive(Debug)]
pub(crate) struct AppShell {
    day: Day,
    formatter: Formatter,
    tokens: Rc<Tokens>,
    page: Page,
    /// The navigation the user chose; an open details panel collapses it to the rail anyway.
    nav: NavLayout,
    details: Option<OpenDetails>,
    ranking: RankingKind,
    lanes: LaneMode,
    range: RangeChoice,
    hover: Option<Hover>,
}

impl AppShell {
    /// Builds the shell from its initial state.
    #[must_use]
    pub(crate) fn new(init: ShellInit) -> Self {
        let details = init
            .details
            .and_then(|subject| open_details(&init.day, subject));
        Self {
            day: init.day,
            formatter: init.formatter,
            tokens: init.tokens,
            page: init.page,
            nav: NavLayout::Expanded,
            details,
            ranking: RankingKind::default(),
            lanes: LaneMode::Swimlanes,
            range: RangeChoice::Activity,
            hover: None,
        }
    }

    /// Shows the tooltip of `segment` as if the pointer were at `position`.
    pub(crate) fn show_tooltip(&mut self, segment: SegmentId, position: Point<Pixels>) {
        self.hover = Some(Hover {
            target: HoverTarget::Segment(segment),
            position,
        });
    }

    /// The subject of the open details sidebar.
    fn details_subject(&self) -> Option<DetailsSubject> {
        self.details.as_ref().map(|details| details.subject)
    }

    /// The navigation as shown: the rail while the details panel is open (07 section 2).
    fn nav_layout(&self) -> NavLayout {
        if self.details.is_some() {
            NavLayout::Rail
        } else {
            self.nav
        }
    }

    /// Width left for the page between the navigation and the details panel.
    fn main_width(&self, viewport: Pixels, metrics: &Metrics) -> Pixels {
        let panel = if self.details.is_some() {
            metrics.layout.panel_width
        } else {
            px(0.)
        };
        viewport - self.nav_layout().width(metrics) - panel
    }

    fn select(&mut self, subject: DetailsSubject, cx: &mut Context<Self>) {
        self.details = open_details(&self.day, subject);
        self.hover = None;
        cx.notify();
    }

    fn close_details(&mut self, cx: &mut Context<Self>) {
        self.details = None;
        cx.notify();
    }

    fn toggle_nav(&mut self, cx: &mut Context<Self>) {
        self.nav = self.nav.toggled();
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

/// Opens the details of `subject`; `None` when it is not part of `day`, so no panel is open.
fn open_details(day: &Day, subject: DetailsSubject) -> Option<OpenDetails> {
    day.details(subject)
        .map(|content| OpenDetails { subject, content })
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (font_family, mode) = {
            let theme = cx.theme();
            (theme.font_family.clone(), theme.mode)
        };
        let tokens = Rc::clone(&self.tokens);
        let ui = Widgets::new(&tokens, mode);
        let main_width = self.main_width(window.viewport_size().width, ui.metrics);
        let page = match self.page {
            Page::Today => self.render_today(&ui, main_width, cx).into_any_element(),
            page @ (Page::Week | Page::Rules | Page::Settings) => {
                self.render_placeholder(&ui, page).into_any_element()
            }
        };
        let tooltip = match self.page {
            Page::Today => self.render_tooltip(&ui),
            Page::Week | Page::Rules | Page::Settings => None,
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(ui.palette.bg_base)
            .text_color(ui.palette.text_primary)
            .font_family(font_family)
            .ramp(&ui, Ramp::Body)
            .child(ui.title_bar(self.formatter.text(Msg::AppName)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_nav(&ui, cx))
                    .child(
                        div()
                            .id("main-area")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .bg(ui.palette.fill_layer)
                            .border_t_1()
                            .border_l_1()
                            .border_color(ui.palette.stroke_card)
                            .rounded_tl(ui.metrics.radius.overlay)
                            .child(page),
                    )
                    .when_some(self.details.as_ref(), |row, details| {
                        row.child(self.render_details(&ui, &details.content, cx))
                    }),
            )
            .children(tooltip.map(deferred))
    }
}

impl AppShell {
    fn render_placeholder(&self, ui: &Widgets<'_>, page: Page) -> impl IntoElement {
        div()
            .p(ui.metrics.space.xxl)
            .ramp(ui, Ramp::Title)
            .text_color(ui.palette.text_primary)
            .child(self.formatter.text(page.label()))
    }
}
