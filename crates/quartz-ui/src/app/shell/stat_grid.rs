//! The status card's cells in equal columns, with the column count chosen from the width the
//! layout gives the grid (07 section 2.1).

use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, ParentElement as _, Pixels, Size, Style,
    Styled as _, Window, div, px, relative,
};

use crate::layout::StatColumns;

/// Cells laid out in `StatColumns` equal columns. The grid's height is measured from the same
/// column choice that the cells are placed with, so the card is as tall as its rows.
pub(super) struct StatGrid {
    cells: Option<Vec<AnyElement>>,
    count: u8,
    /// Width of the widest cell's content.
    widest: f32,
    cell_height: Pixels,
    column_gap: Pixels,
    row_gap: Pixels,
}

impl StatGrid {
    /// A grid of `cells`, each at least `widest` wide and `cell_height` tall.
    ///
    /// # Panics
    /// Panics when there are more than 255 cells; the status card has six.
    pub(super) fn new(
        cells: Vec<AnyElement>,
        widest: f32,
        cell_height: Pixels,
        column_gap: Pixels,
        row_gap: Pixels,
    ) -> Self {
        Self {
            count: u8::try_from(cells.len()).expect("the status card has a handful of cells"),
            cells: Some(cells),
            widest,
            cell_height,
            column_gap,
            row_gap,
        }
    }

    fn columns(widest: f32, column_gap: Pixels, width: Pixels) -> StatColumns {
        StatColumns::choose(f32::from(width), widest, f32::from(column_gap))
    }
}

impl std::fmt::Debug for StatGrid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatGrid")
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl IntoElement for StatGrid {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for StatGrid {
    type RequestLayoutState = ();
    type PrepaintState = AnyElement;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let (count, widest, cell_height) = (self.count, self.widest, self.cell_height);
        let (column_gap, row_gap) = (self.column_gap, self.row_gap);
        let layout = window.request_measured_layout(style, move |known, available, _, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                AvailableSpace::MinContent | AvailableSpace::MaxContent => None,
            });
            let columns = match (width, available.width) {
                (Some(width), _) => Self::columns(widest, column_gap, width),
                (None, AvailableSpace::MaxContent) => StatColumns::widest_layout(),
                (None, _) => StatColumns::narrowest_layout(),
            };
            let columns_f = f32::from(columns.get());
            Size {
                width: width.unwrap_or(px(widest * columns_f) + column_gap * (columns_f - 1.)),
                height: px(columns.height(count, f32::from(cell_height), f32::from(row_gap))),
            }
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let columns = Self::columns(self.widest, self.column_gap, bounds.size.width);
        let per_row = usize::from(columns.get());
        let mut cells = self
            .cells
            .take()
            .expect("an element is prepainted once per frame");
        let mut grid = div().flex().flex_col().w_full().gap(self.row_gap);
        while !cells.is_empty() {
            let rest = cells.split_off(per_row.min(cells.len()));
            let row = std::mem::replace(&mut cells, rest);
            let missing = per_row - row.len();
            // Empty cells keep the last row's columns as wide as the others.
            grid = grid.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(self.column_gap)
                    .children(row)
                    .children((0..missing).map(|_| div().flex_1().min_w(px(0.)))),
            );
        }
        let mut element = grid.into_any_element();
        element.prepaint_as_root(bounds.origin, bounds.size.into(), window, cx);
        element
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        prepaint: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        prepaint.paint(window, cx);
    }
}
