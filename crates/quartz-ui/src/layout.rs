//! Window size classes, the shell layout they imply and content-driven column widths, without
//! GPUI so the decisions are unit-tested on every platform (07 section 2, 11 sections 5.5
//! and 5.6). Lengths are logical pixels.

/// The narrowest width of the Medium class; below it the window is Compact (11 section 5.5).
/// WinUI `NavigationView` uses the same breakpoints (641 and 1008 DIPs).
pub(crate) const MEDIUM_MIN_WIDTH: f32 = 641.;
/// The narrowest width of the Expanded class (11 section 5.5).
pub(crate) const EXPANDED_MIN_WIDTH: f32 = 1008.;
/// The smallest window width (07 section 2.3).
pub(crate) const MIN_WINDOW_WIDTH: f32 = 640.;
/// The smallest window height (07 section 2.3).
pub(crate) const MIN_WINDOW_HEIGHT: f32 = 480.;
/// The narrowest timeline plot that stays usable: about six labeled ticks and readable block
/// labels (11 section 5.5). Raising it moves the point where the details panel turns into an
/// overlay to wider windows.
pub(crate) const TIMELINE_MIN_PLOT_WIDTH: f32 = 480.;
/// The widest the timeline label column grows; longer application names are ellipsized
/// (11 section 5.6).
pub(crate) const LABEL_COLUMN_MAX_WIDTH: f32 = 160.;

/// The window width class (11 section 5.5), derived from the window width on every render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WindowClass {
    /// Narrower than [`MEDIUM_MIN_WIDTH`].
    Compact,
    /// From [`MEDIUM_MIN_WIDTH`] up to but excluding [`EXPANDED_MIN_WIDTH`].
    Medium,
    /// [`EXPANDED_MIN_WIDTH`] and wider.
    Expanded,
}

impl WindowClass {
    /// The class of a window `width` logical pixels wide.
    #[must_use]
    pub(crate) fn of_width(width: f32) -> Self {
        if width >= EXPANDED_MIN_WIDTH {
            Self::Expanded
        } else if width >= MEDIUM_MIN_WIDTH {
            Self::Medium
        } else {
            Self::Compact
        }
    }

    /// How the hourly chart and the ranking share a row (07 section 2.1).
    #[must_use]
    pub(crate) const fn chart_arrangement(self) -> ChartArrangement {
        match self {
            Self::Expanded => ChartArrangement::SideBySide,
            Self::Compact | Self::Medium => ChartArrangement::Stacked,
        }
    }
}

/// How the hourly chart and the ranking are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ChartArrangement {
    SideBySide,
    Stacked,
}

/// How the navigation is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NavLayout {
    /// The 240 px sidebar with labels.
    Expanded,
    /// The 48 px icon rail.
    Rail,
}

impl NavLayout {
    /// The other layout.
    #[must_use]
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Expanded => Self::Rail,
            Self::Rail => Self::Expanded,
        }
    }
}

/// Whether a right-hand panel is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PanelState {
    Closed,
    Open,
}

/// Where the right-hand panel is (07 section 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PanelPlacement {
    Closed,
    /// Takes width from the main area.
    Beside,
    /// Floats over the main area, which does not reflow.
    Overlay,
}

/// The sizes the shell layout decision needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ShellSizes {
    nav: f32,
    rail: f32,
    panel: f32,
    timeline_min_main: f32,
}

impl ShellSizes {
    /// The expanded sidebar width `nav`, the rail width `rail`, the panel width `panel`, the
    /// `page_margin` and `card_padding` on each side of the timeline card, and the `border` width
    /// of the main area (one border) and of the card (two), and the `label_gap` between the label
    /// column and the plot. The smallest main area that keeps the timeline usable is the
    /// margins, the paddings, the borders, the widest label column, the gap and
    /// [`TIMELINE_MIN_PLOT_WIDTH`].
    #[must_use]
    pub(crate) fn new(
        nav: f32,
        rail: f32,
        panel: f32,
        page_margin: f32,
        card_padding: f32,
        border: f32,
        label_gap: f32,
    ) -> Self {
        Self {
            nav,
            rail,
            panel,
            timeline_min_main: 2. * page_margin
                + 2. * card_padding
                + 3. * border
                + label_gap
                + LABEL_COLUMN_MAX_WIDTH
                + TIMELINE_MIN_PLOT_WIDTH,
        }
    }

    /// The width of the navigation in `layout`.
    #[must_use]
    pub(crate) const fn nav_width(&self, layout: NavLayout) -> f32 {
        match layout {
            NavLayout::Expanded => self.nav,
            NavLayout::Rail => self.rail,
        }
    }

    /// The width of the right-hand panel.
    #[must_use]
    pub(crate) const fn panel_width(&self) -> f32 {
        self.panel
    }

    /// The smallest main area width that keeps the timeline usable.
    #[must_use]
    pub(crate) const fn timeline_min_main(&self) -> f32 {
        self.timeline_min_main
    }
}

/// The resolved layout of one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ShellLayout {
    pub(crate) class: WindowClass,
    pub(crate) nav: NavLayout,
    pub(crate) panel: PanelPlacement,
}

impl ShellLayout {
    /// Resolves the layout of a window `viewport_width` wide when the user chose `nav_choice`.
    ///
    /// Only the Expanded class shows the full sidebar. An open panel sits beside the main area
    /// when that keeps [`ShellSizes::timeline_min_main`]; failing that the sidebar collapses to
    /// the rail (07 section 2), and failing that the panel floats over the main area.
    #[must_use]
    pub(crate) fn resolve(
        viewport_width: f32,
        nav_choice: NavLayout,
        panel: PanelState,
        sizes: &ShellSizes,
    ) -> Self {
        let class = WindowClass::of_width(viewport_width);
        let nav = match class {
            WindowClass::Expanded => nav_choice,
            WindowClass::Compact | WindowClass::Medium => NavLayout::Rail,
        };
        let beside = |nav: NavLayout| {
            viewport_width - sizes.nav_width(nav) - sizes.panel_width() >= sizes.timeline_min_main()
        };
        let (nav, panel) = match (panel, class) {
            (PanelState::Closed, _) => (nav, PanelPlacement::Closed),
            (PanelState::Open, WindowClass::Compact | WindowClass::Medium) => {
                (nav, PanelPlacement::Overlay)
            }
            (PanelState::Open, WindowClass::Expanded) if beside(nav) => {
                (nav, PanelPlacement::Beside)
            }
            (PanelState::Open, WindowClass::Expanded) if beside(NavLayout::Rail) => {
                (NavLayout::Rail, PanelPlacement::Beside)
            }
            (PanelState::Open, WindowClass::Expanded) => (nav, PanelPlacement::Overlay),
        };
        Self { class, nav, panel }
    }
}

/// How many equal columns the status card uses (07 section 2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatColumns(u8);

impl StatColumns {
    /// The column counts the card may use, most first: one row of six, two rows of three, three
    /// rows of two, or a single column. They all divide the six cells evenly, so no row is
    /// left with a lone cell.
    const ALLOWED: [u8; 4] = [6, 3, 2, 1];

    /// The largest allowed count whose columns, each as wide as the widest of the cells
    /// (`widest`) with `gap` between columns, fit in `available`. One column when nothing fits.
    #[must_use]
    pub(crate) fn choose(available: f32, widest: f32, gap: f32) -> Self {
        let fits = |count: u8| {
            let count = f32::from(count);
            count * widest + (count - 1.) * gap <= available
        };
        Self(
            Self::ALLOWED
                .into_iter()
                .find(|count| fits(*count))
                .unwrap_or(1),
        )
    }

    /// The count for an unbounded width: the widest layout.
    #[must_use]
    pub(crate) const fn widest_layout() -> Self {
        Self(Self::ALLOWED[0])
    }

    /// The count for no room at all: a single column.
    #[must_use]
    pub(crate) const fn narrowest_layout() -> Self {
        Self(1)
    }

    /// The number of columns.
    #[must_use]
    pub(crate) const fn get(self) -> u8 {
        self.0
    }

    /// The rows needed for `cells` cells.
    #[must_use]
    pub(crate) fn rows(self, cells: u8) -> u8 {
        cells.div_ceil(self.0)
    }

    /// The height of `cells` cells `cell` tall in these columns with `row_gap` between rows.
    /// Every cell is one line that is clipped, not wrapped, so the height depends only on the
    /// row count, also when one column is narrower than its widest cell (07 section 2.1).
    #[must_use]
    pub(crate) fn height(self, cells: u8, cell: f32, row_gap: f32) -> f32 {
        let rows = f32::from(self.rows(cells));
        cell * rows + row_gap * (rows - 1.)
    }
}

/// The allowed width of a column that sizes itself to its content (11 section 5.6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ColumnRange {
    min: f32,
    max: f32,
}

impl ColumnRange {
    /// The range from `min` to `max`.
    ///
    /// # Panics
    /// Panics when `min` is greater than `max`, which is a mistake in the constant.
    #[must_use]
    pub(crate) const fn new(min: f32, max: f32) -> Self {
        assert!(min <= max, "a column range must not be inverted");
        Self { min, max }
    }

    /// The column width for cells of the measured `widths`: the widest, kept within the range.
    /// Content wider than the range is ellipsized by the cell.
    #[must_use]
    pub(crate) fn select(self, widths: impl IntoIterator<Item = f32>) -> f32 {
        widths
            .into_iter()
            .fold(0., f32::max)
            .clamp(self.min, self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The metrics of `themes/fluent.json`: 240 and 48 sidebar, 360 panel, 24 margin, 16 padding,
    /// 1 border, 8 label gap.
    fn sizes() -> ShellSizes {
        ShellSizes::new(240., 48., 360., 24., 16., 1., 8.)
    }

    fn resolve(width: f32, nav: NavLayout, panel: PanelState) -> ShellLayout {
        ShellLayout::resolve(width, nav, panel, &sizes())
    }

    #[test]
    fn class_boundaries_follow_the_winui_breakpoints() {
        let class = WindowClass::of_width;
        assert_eq!(class(640.), WindowClass::Compact);
        assert_eq!(class(640.9), WindowClass::Compact);
        assert_eq!(class(641.), WindowClass::Medium);
        assert_eq!(class(1007.), WindowClass::Medium);
        assert_eq!(class(1007.9), WindowClass::Medium);
        assert_eq!(class(1008.), WindowClass::Expanded);
        assert_eq!(class(MIN_WINDOW_WIDTH), WindowClass::Compact);
    }

    #[test]
    fn only_expanded_windows_put_the_charts_side_by_side() {
        assert_eq!(
            WindowClass::Expanded.chart_arrangement(),
            ChartArrangement::SideBySide
        );
        for class in [WindowClass::Compact, WindowClass::Medium] {
            assert_eq!(class.chart_arrangement(), ChartArrangement::Stacked);
        }
    }

    #[test]
    fn narrow_windows_always_use_the_rail_whatever_the_user_chose() {
        for width in [640., 800., 1007.] {
            let layout = resolve(width, NavLayout::Expanded, PanelState::Closed);
            assert_eq!(layout.nav, NavLayout::Rail, "width {width}");
            assert_eq!(layout.panel, PanelPlacement::Closed);
        }
        let wide = resolve(1008., NavLayout::Expanded, PanelState::Closed);
        assert_eq!(wide.nav, NavLayout::Expanded);
        let collapsed = resolve(1400., NavLayout::Rail, PanelState::Closed);
        assert_eq!(collapsed.nav, NavLayout::Rail);
    }

    #[test]
    fn the_panel_is_an_overlay_below_the_expanded_class() {
        for width in [640., 960., 1007.] {
            let layout = resolve(width, NavLayout::Expanded, PanelState::Open);
            assert_eq!(layout.panel, PanelPlacement::Overlay, "width {width}");
            assert_eq!(layout.nav, NavLayout::Rail, "the overlay does not reflow");
        }
    }

    #[test]
    fn an_expanded_panel_sits_beside_only_while_the_timeline_keeps_its_minimum() {
        let minimum = sizes().timeline_min_main();
        assert!((minimum - 731.).abs() < f32::EPSILON, "{minimum}");
        // 240 + 360 + 731: fits with the full sidebar; 731 is 48 margins, 32 paddings, 3 borders,
        // the 8 label gap, the 160 label column and the 480 plot.
        let full = resolve(1331., NavLayout::Expanded, PanelState::Open);
        assert_eq!(
            (full.nav, full.panel),
            (NavLayout::Expanded, PanelPlacement::Beside)
        );
        // One pixel less: the sidebar collapses to the rail and the panel still fits beside.
        let railed = resolve(1330., NavLayout::Expanded, PanelState::Open);
        assert_eq!(
            (railed.nav, railed.panel),
            (NavLayout::Rail, PanelPlacement::Beside)
        );
        // 48 + 360 + 731 = 1139 is the least the rail allows.
        let least = resolve(1139., NavLayout::Expanded, PanelState::Open);
        assert_eq!(least.panel, PanelPlacement::Beside);
        // Below it the panel floats and the user's sidebar choice stays.
        let floating = resolve(1138., NavLayout::Expanded, PanelState::Open);
        assert_eq!(
            (floating.nav, floating.panel),
            (NavLayout::Expanded, PanelPlacement::Overlay)
        );
        let default_window = resolve(1280., NavLayout::Expanded, PanelState::Open);
        assert_eq!(
            (default_window.nav, default_window.panel),
            (NavLayout::Rail, PanelPlacement::Beside),
            "the default 1280 window keeps the original 07 behavior"
        );
    }

    #[test]
    fn a_column_is_as_wide_as_its_widest_cell_within_the_range() {
        let range = ColumnRange::new(40., 96.);
        assert!(
            (range.select([]) - 40.).abs() < f32::EPSILON,
            "empty column"
        );
        assert!(
            (range.select([12., 30.]) - 40.).abs() < f32::EPSILON,
            "raised to the minimum"
        );
        assert!((range.select([52.5, 61., 18.]) - 61.).abs() < f32::EPSILON);
        assert!(
            (range.select([61., 140.]) - 96.).abs() < f32::EPSILON,
            "capped at the maximum"
        );
    }

    #[test]
    fn the_status_card_picks_the_largest_allowed_column_count_that_fits() {
        let choose = |available| StatColumns::choose(available, 100., 16.).get();
        // 6 columns need 6 * 100 + 5 * 16 = 680.
        assert_eq!(choose(680.), 6);
        assert_eq!(choose(679.), 3, "never five columns with one orphan");
        // 3 columns need 332, 2 need 216, 1 needs 100.
        assert_eq!(choose(332.), 3);
        assert_eq!(choose(331.), 2);
        assert_eq!(choose(216.), 2);
        assert_eq!(choose(215.), 1);
        assert_eq!(choose(10.), 1, "one column even when nothing fits");
    }

    #[test]
    fn a_cell_wider_than_the_card_falls_back_to_one_column_of_unchanged_height() {
        // Pseudo-localised or enlarged text: the widest cell is 300 px in a 120 px or 60 px card.
        let narrow = StatColumns::choose(120., 300., 16.);
        let narrower = StatColumns::choose(60., 300., 16.);
        assert_eq!((narrow.get(), narrower.get()), (1, 1));
        let height = |columns: StatColumns| columns.height(6, 48., 8.);
        assert!(
            (height(narrow) - 328.).abs() < f32::EPSILON,
            "6 rows of 48 and 5 gaps of 8"
        );
        assert!((height(narrow) - height(narrower)).abs() < f32::EPSILON);
        // The same single line per cell as in a wide card: only the row count differs.
        assert!(
            (StatColumns::choose(2000., 300., 16.).height(6, 48., 8.) - 48.).abs() < f32::EPSILON
        );
    }

    #[test]
    fn larger_text_lowers_the_column_count() {
        // The same 560 px card with cells 80, 110 and 190 px wide (text scales 1, 1.4, 2.4).
        let counts: Vec<u8> = [80., 110., 190.]
            .into_iter()
            .map(|widest| StatColumns::choose(560., widest, 16.).get())
            .collect();
        assert_eq!(counts, [6, 3, 2]);
    }

    #[test]
    fn the_rows_follow_from_the_column_count() {
        let rows = |columns: u8, cells| StatColumns(columns).rows(cells);
        assert_eq!(
            [rows(6, 6), rows(3, 6), rows(2, 6), rows(1, 6)],
            [1, 2, 3, 6]
        );
        assert_eq!(StatColumns::widest_layout().get(), 6);
        assert_eq!(StatColumns::narrowest_layout().get(), 1);
    }

    #[test]
    #[should_panic(expected = "inverted")]
    fn an_inverted_range_is_rejected() {
        let _ = ColumnRange::new(10., 5.);
    }
}
