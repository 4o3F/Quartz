//! The application shell: title bar, navigation, pages and the right-hand details panel (07 section 2).

mod calendar;
mod charts;
mod details;
mod live;
mod nav;
mod pending;
mod ranking;
mod stat_grid;
mod timeline;
mod today;

use std::{fmt, rc::Rc};

use gpui_kit::{
    AnyElement, App, Bounds, Canvas, Context, Div, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Pixels, Point, Render, ScrollHandle, Stateful,
    Styled as _, Window, assets::IconName, canvas, component::ActiveTheme as _, deferred, div, px,
};

use super::{
    measure::TextMeasure,
    metrics::Ramp,
    tokens::Tokens,
    widgets::{RampExt as _, Widgets},
};
use crate::{
    format::{Formatter, Msg},
    layout::{NavLayout, PanelPlacement, PanelState, ShellLayout},
    model::{Day, DaySource, Details, DetailsSubject, RankingKind, SegmentId, TimeOfDay},
    text_scale::{LiveTextScale, TextScaleChange},
    timeline_scale::{ScaleFactor, View},
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
pub(crate) enum RangeChoice {
    /// First to last activity with 15 minutes of margin (07 section 4.1).
    Activity,
    FullDay,
    /// A view reached by zooming; never empty.
    Zoomed(View),
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

/// What the pages need to know about the current frame.
#[derive(Debug)]
pub(super) struct Frame<'a> {
    pub(super) measure: &'a TextMeasure<'a>,
    pub(super) layout: ShellLayout,
}

/// What a bounds-driven layer sees when it builds its elements during prepaint.
pub(super) struct LayerCx<'a> {
    pub(super) shell: &'a AppShell,
    pub(super) entity: &'a Entity<AppShell>,
    /// The laid-out bounds of the layer, in window coordinates.
    pub(super) bounds: Bounds<Pixels>,
    pub(super) window: &'a Window,
    pub(super) app: &'a App,
}

impl fmt::Debug for LayerCx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LayerCx")
            .field("bounds", &self.bounds)
            .finish_non_exhaustive()
    }
}

impl LayerCx<'_> {
    /// Runs `build` with the widgets, text measure and window scale factor of this frame.
    pub(super) fn with_ui<R>(
        &self,
        build: impl FnOnce(&Widgets<'_>, &TextMeasure<'_>, ScaleFactor) -> R,
    ) -> R {
        let tokens = crate::app::system_theme::SystemTheme::tokens(self.app);
        let theme = self.app.theme();
        let ui = Widgets::new(&tokens, theme.mode, self.shell.text_scale.current());
        let measure = TextMeasure::new(self.window, ui, theme.font_family.clone());
        let scale = ScaleFactor::try_new(self.window.scale_factor())
            .expect("GPUI reports a positive, finite window scale factor");
        build(&ui, &measure, scale)
    }

    /// A listener that runs `handler` on the shell, as `Context::listener` does for the
    /// render-time elements.
    pub(super) fn listen<E>(
        &self,
        handler: impl Fn(&mut AppShell, &E, &mut Window, &mut Context<AppShell>) + 'static,
    ) -> impl Fn(&E, &mut Window, &mut App) + 'static {
        self.window.listener_for(self.entity, handler)
    }
}

impl AppShell {
    /// A canvas filling its parent whose elements are built from the parent's laid-out bounds,
    /// during prepaint of the same frame. Nothing is measured one frame late and nothing
    /// requests a frame by itself: the layer repaints exactly when its parent does.
    pub(super) fn bounds_layer(
        &self,
        cx: &Context<Self>,
        build: impl FnOnce(&LayerCx<'_>) -> AnyElement + 'static,
    ) -> Canvas<AnyElement> {
        let entity = cx.entity();
        canvas(
            move |bounds, window, app| {
                let mut element = {
                    let shell = entity.read(app);
                    build(&LayerCx {
                        shell,
                        entity: &entity,
                        bounds,
                        window,
                        app,
                    })
                };
                element.prepaint_as_root(bounds.origin, bounds.size.into(), window, app);
                element
            },
            |_, mut element, window, app| element.paint(window, app),
        )
        .absolute()
        .size_full()
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
    /// The visible time range the timeline opens with.
    pub(crate) range: RangeChoice,
    /// The text scale the window opens with and where later values come from.
    pub(crate) text_scale: LiveTextScale,
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
    live: Option<live::LivePage>,
    calendar: Option<calendar::CalendarMonth>,
    day: Day,
    formatter: Formatter,
    tokens: Rc<Tokens>,
    page: Page,
    /// The navigation the user chose; the layout may show the rail instead (07 section 2).
    nav: NavLayout,
    details: Option<OpenDetails>,
    text_scale: LiveTextScale,
    /// Scroll position of the details panel, kept across beside and overlay placement.
    details_scroll: ScrollHandle,
    /// The placement the previous frame used, to notice when the panel starts to float.
    placement: PanelPlacement,
    /// Focus of the details panel, so `Esc` reaches it while it floats.
    panel_focus: FocusHandle,
    /// Focus of the main area, where it returns when the panel closes.
    root_focus: FocusHandle,
    ranking: RankingKind,
    lanes: LaneMode,
    range: RangeChoice,
    hover: Option<Hover>,
    plot_width: Option<f32>,
    pan: Option<timeline::PanGesture>,
}

impl AppShell {
    /// Builds the shell from its initial state; re-reads the text size whenever `window` is
    /// activated, since Windows has no notification the window can rely on while in the background.
    #[must_use]
    pub(crate) fn new(init: ShellInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_window_activation(window, |this, _, cx| {
            crate::app::system_theme::SystemTheme::refresh(cx);
            this.refresh_text_scale(cx);
        })
        .detach();
        let details = init
            .details
            .and_then(|subject| open_details(&init.day, subject));
        Self {
            live: None,
            calendar: None,
            day: init.day,
            formatter: init.formatter,
            tokens: init.tokens,
            page: init.page,
            nav: NavLayout::Expanded,
            details,
            text_scale: init.text_scale,
            details_scroll: ScrollHandle::new(),
            placement: PanelPlacement::Closed,
            panel_focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            ranking: RankingKind::default(),
            lanes: LaneMode::Swimlanes,
            range: init.range,
            hover: None,
            plot_width: None,
            pan: None,
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

    /// The layout of a window `window` wide with the current navigation choice and panel.
    fn layout_in(&self, window: &Window) -> ShellLayout {
        let panel = if self.details.is_some() {
            PanelState::Open
        } else {
            PanelState::Closed
        };
        ShellLayout::resolve(
            f32::from(window.viewport_size().width),
            self.nav,
            panel,
            &self.tokens.metrics().shell_sizes(),
        )
    }

    /// Re-reads the text size (see [`LiveTextScale::refresh`]).
    fn refresh_text_scale(&mut self, cx: &mut Context<Self>) {
        if self.text_scale.refresh() == TextScaleChange::Changed {
            cx.notify();
        }
    }

    /// Notes the placement of this frame. When the panel starts to float, the tooltip of the
    /// content below it is dropped and the keyboard focus moves into the panel, so `Esc` reaches
    /// it whether it opened floating or a resize made it float (07 section 2.2).
    fn enter_placement(
        &mut self,
        placement: PanelPlacement,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let previous = std::mem::replace(&mut self.placement, placement);
        if placement == PanelPlacement::Overlay && previous != PanelPlacement::Overlay {
            self.hover = None;
            let focus = self.panel_focus.clone();
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }
    }

    fn select(&mut self, subject: DetailsSubject, window: &mut Window, cx: &mut Context<Self>) {
        self.details = open_details(&self.day, subject);
        self.hover = None;
        if self.details.is_some() {
            window.focus(&self.panel_focus, cx);
        }
        cx.notify();
    }

    fn close_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.details = None;
        window.focus(&self.root_focus, cx);
        cx.notify();
    }

    /// Closes the panel on `Esc` while it floats over the main area (07 section 2.2).
    fn dismiss_overlay_on_escape(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape"
            && self.layout_in(window).panel == PanelPlacement::Overlay
        {
            self.close_details(window, cx);
            cx.stop_propagation();
        }
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
        let tokens = crate::app::system_theme::SystemTheme::tokens(cx);
        let ui = Widgets::new(&tokens, mode, self.text_scale.current());
        let layout = self.layout_in(window);
        self.enter_placement(layout.panel, window, cx);
        let measure = TextMeasure::new(window, ui, font_family.clone());
        let frame = Frame {
            measure: &measure,
            layout,
        };
        let page = match self.page {
            Page::Today => self.render_today(&ui, &frame, cx).into_any_element(),
            page @ (Page::Week | Page::Rules | Page::Settings) => {
                self.render_placeholder(&ui, page).into_any_element()
            }
        };
        let tooltip = match (self.page, layout.panel) {
            (Page::Today, PanelPlacement::Closed | PanelPlacement::Beside) => {
                self.render_tooltip(&ui)
            }
            (Page::Today, PanelPlacement::Overlay)
            | (Page::Week | Page::Rules | Page::Settings, _) => None,
        };
        let panel = self
            .details
            .as_ref()
            .map(|details| self.render_details(&ui, &measure, &details.content, layout.panel, cx));
        let (beside, overlay) = match layout.panel {
            PanelPlacement::Closed => (None, None),
            PanelPlacement::Beside => (panel, None),
            PanelPlacement::Overlay => (None, panel),
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&self.root_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.calendar.take().is_some() {
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .bg(ui.palette.window_background)
            .text_color(ui.palette.text_primary)
            .font_family(font_family)
            .ramp(&ui, Ramp::Body)
            .child(ui.title_bar(self.formatter.text(Msg::AppName), window))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_nav(&ui, layout, cx))
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
                    .children(beside),
            )
            .children(overlay.map(|panel| deferred(self.overlay_layer(&ui, panel, cx))))
            .children(tooltip.map(deferred))
    }
}

impl AppShell {
    /// The floating details panel with the transparent layer that dismisses it on a click
    /// outside; the layer starts below the title bar so window buttons and dragging keep working
    /// (07 section 2.2).
    fn overlay_layer(
        &self,
        ui: &Widgets<'_>,
        panel: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id("details-scrim")
            .absolute()
            .top(ui.metrics.layout.title_bar_height)
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .occlude()
            .on_any_mouse_down(cx.listener(|this, _, window, cx| this.close_details(window, cx)))
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .right(px(0.))
                    .bottom(px(0.))
                    .occlude()
                    // The content layer is translucent; the opaque base keeps the content below
                    // from showing through.
                    .bg(ui.palette.bg_base)
                    .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                    .child(panel),
            )
    }

    fn render_placeholder(&self, ui: &Widgets<'_>, page: Page) -> impl IntoElement {
        div()
            .p(ui.metrics.space.xxl)
            .ramp(ui, Ramp::Title)
            .text_color(ui.palette.text_primary)
            .child(self.formatter.text(page.label()))
    }
}
