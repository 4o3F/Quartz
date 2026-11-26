//! Geometry of the timeline without GPUI: the visible span, block placement as plot fractions,
//! axis ticks, zoom steps and which block label fits (07 section 4).

use std::time::Duration;

use num_traits::ToPrimitive as _;
use snafu::Snafu;

use crate::{
    link::UtcRange,
    model::{SECONDS_PER_MINUTE, TimeOfDay, TimeSpan},
};

/// Margin around the activity range of the default view (07 section 4.1).
pub(crate) const ACTIVITY_MARGIN_MINUTES: u32 = 15;
/// The narrowest span zooming in reaches.
pub(crate) const MIN_ZOOM_MINUTES: u32 = 5;
/// Horizontal drag distance in logical pixels required to distinguish a timeline drag-select
/// from a click (matches one design-grid step).
pub(crate) const SELECTION_DRAG_THRESHOLD_PX: f32 = 4.;
/// Granularity in minutes to which timeline drag-selected endpoints snap.
pub(crate) const SELECTION_SNAP_MINUTES: i64 = 1;
/// Milliseconds in one minute for snapping drag-selected UTC endpoints.
const SELECTION_MILLIS_PER_MINUTE: i64 = 60_000;
/// Granularity in milliseconds to which timeline drag-selected endpoints snap.
pub(crate) const SELECTION_SNAP_MILLIS: i64 = SELECTION_SNAP_MINUTES * SELECTION_MILLIS_PER_MINUTE;
/// Most axis ticks a view shows; a view picks the finest step that stays within it.
pub(crate) const MAX_TICKS: u8 = 14;
/// Fewest tick intervals; large labels may omit ticks while retaining this grid density.
const MIN_TICKS: u8 = 4;
/// Space kept between two neighboring axis labels.
pub(crate) const TICK_LABEL_GAP: f32 = 8.;
/// Narrowest painted block in logical pixels, so one-pixel blocks stay visible (07 section 4.3).
pub(crate) const MIN_BLOCK_WIDTH: f32 = 2.;
/// Candidate tick steps in minutes, finest first; the last one fits a whole day.
const TICK_STEPS_MINUTES: [u32; 11] = [1, 5, 10, 15, 30, 60, 120, 180, 360, 720, 1440];

/// Zooming in halves the visible span; zooming out doubles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ZoomStep {
    In,
    Out,
}

/// The visible part of the day; never empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct View {
    span: TimeSpan,
}

/// The factor between logical and physical pixels of a window, such as 1.5 at 150%.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScaleFactor(f32);

/// A scale factor that is not a positive finite number.
#[derive(Debug, Clone, Copy, PartialEq, Snafu)]
#[snafu(display("scale factor {value} is not a positive finite number"))]
pub(crate) struct ValidateScaleFactorError {
    value: f32,
}

impl ScaleFactor {
    /// A scale factor of `value`.
    ///
    /// # Errors
    /// Returns an error when `value` is not positive and finite.
    pub(crate) fn try_new(value: f32) -> Result<Self, ValidateScaleFactorError> {
        if value.is_finite() && value > 0. {
            Ok(Self(value))
        } else {
            Err(ValidateScaleFactorError { value })
        }
    }

    /// The physical pixels of `logical` pixels, as a whole number rounded to the nearest.
    #[must_use]
    pub(crate) fn physical(self, logical: f32) -> f32 {
        (logical * self.0).round()
    }

    /// The physical pixels of `logical` pixels, as a whole number rounded up.
    #[must_use]
    pub(crate) fn physical_ceil(self, logical: f32) -> f32 {
        (logical * self.0).ceil()
    }

    /// `physical` pixels as logical pixels.
    #[must_use]
    pub(crate) fn logical(self, physical: f32) -> f32 {
        physical / self.0
    }
}

/// Left edge and width of a block as fractions of the plot width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    pub(crate) left: f32,
    pub(crate) width: f32,
}

/// Left edge and width of a span in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg(windows)]
pub(crate) struct PixelSpan {
    pub(crate) left: f32,
    pub(crate) width: f32,
}

/// The physical pixel grid of one plot row (07 section 4.3). Every edge is a whole number of
/// physical pixels computed from the same function of the time fraction, so touching blocks share
/// an edge exactly at any scale factor. Lengths below are physical pixels held as whole `f32`s.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlotGrid {
    scale: ScaleFactor,
    /// Plot width.
    width: f32,
    /// Background gap at the right of a block: at least one physical pixel.
    gap: f32,
    /// Narrowest painted block body: [`MIN_BLOCK_WIDTH`] rounded up.
    min_body: f32,
}

/// A plot too narrow to hold a single marker (07 section 4.3).
#[derive(Debug, Clone, Copy, PartialEq, Snafu)]
#[snafu(display("plot of {plot} physical pixels is narrower than one marker of {marker}"))]
pub(crate) struct BuildPlotGridError {
    plot: f32,
    marker: f32,
}

impl PlotGrid {
    /// The grid of a plot `plot_width` logical pixels wide with a `block_gap` between blocks.
    ///
    /// # Errors
    /// Returns an error when the plot is narrower than one marker (the gap plus the minimum
    /// body). The supported window sizes leave far more room (the layout keeps
    /// `TIMELINE_MIN_PLOT_WIDTH`); the caller draws an empty plot rather than markers that
    /// would leave it.
    pub(crate) fn new(
        plot_width: f32,
        scale: ScaleFactor,
        block_gap: f32,
    ) -> Result<Self, BuildPlotGridError> {
        let grid = Self {
            scale,
            width: scale.physical(plot_width),
            gap: scale.physical(block_gap).max(1.),
            min_body: scale.physical_ceil(MIN_BLOCK_WIDTH),
        };
        let marker = grid.gap + grid.min_body;
        if grid.width < marker {
            return Err(BuildPlotGridError {
                plot: grid.width,
                marker,
            });
        }
        Ok(grid)
    }

    /// The physical pixel boundary of `fraction` of the plot width.
    fn edge(self, fraction: f32) -> f32 {
        (fraction * self.width).round().clamp(0., self.width)
    }

    /// The position of `fraction` of the plot width on a physical pixel boundary, in logical
    /// pixels; for gridlines and ticks.
    #[must_use]
    pub(crate) fn snap(self, fraction: f32) -> f32 {
        self.scale.logical(self.edge(fraction))
    }

    /// The left edge and width of `placement` in logical pixels, without the block minimum or
    /// gap; for the selection overlay.
    #[must_use]
    #[cfg(windows)]
    pub(crate) fn span(self, placement: Placement) -> PixelSpan {
        let left = self.edge(placement.left);
        let right = self.edge(placement.left + placement.width);
        PixelSpan {
            left: self.scale.logical(left),
            width: self.scale.logical(right - left),
        }
    }

    /// The tile of `placement`.
    ///
    /// An interval whose natural width holds the gap and the minimum body is a
    /// [`PlacementKind::Block`] with the shared boundaries of the row as its edges; blocks never
    /// overlap and are never widened. A narrower interval is a [`PlacementKind::Marker`]: the row
    /// is divided into cells of `gap + min_body` physical pixels (the last cell takes the
    /// remainder), and the marker sits at the left edge of the cell its own left edge falls in.
    /// Markers therefore lie wholly inside the plot, are at least the minimum width, and never
    /// partly overlap each other; markers in one cell are grouped by [`RowTiles::arrange`].
    #[must_use]
    pub(crate) fn place(self, placement: Placement) -> PixelPlacement {
        let left = self.edge(placement.left);
        let natural = self.edge(placement.left + placement.width) - left;
        let minimum = self.gap + self.min_body;
        if natural >= minimum {
            return PixelPlacement {
                scale: self.scale,
                left,
                width: natural,
                gap: self.gap,
                kind: PlacementKind::Block,
            };
        }
        // `new` guarantees at least one cell.
        let cells = (self.width / minimum).floor();
        let cell = (left / minimum).floor().min(cells - 1.);
        PixelPlacement {
            scale: self.scale,
            left: cell * minimum,
            width: minimum,
            gap: self.gap,
            kind: PlacementKind::Marker,
        }
    }
}

/// Whether a tile shows its interval at its true extent or as a marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PlacementKind {
    /// At the interval's own edges.
    Block,
    /// A bounded marker standing for an interval too narrow for the minimum body.
    Marker,
}

/// One tile on the grid; accessors give logical pixels from the left of the plot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PixelPlacement {
    scale: ScaleFactor,
    left: f32,
    width: f32,
    gap: f32,
    kind: PlacementKind,
}

impl PixelPlacement {
    /// Left edge, including nothing of the gap.
    #[must_use]
    pub(crate) fn left(self) -> f32 {
        self.scale.logical(self.left)
    }

    /// Width of the hit area: the painted body and the gap on its right.
    #[must_use]
    pub(crate) fn width(self) -> f32 {
        self.scale.logical(self.width)
    }

    /// The background gap at the right edge.
    #[must_use]
    pub(crate) fn gap(self) -> f32 {
        self.scale.logical(self.gap)
    }

    /// Width of the painted body.
    #[must_use]
    pub(crate) fn body_width(self) -> f32 {
        self.scale.logical(self.width - self.gap)
    }

    /// Whether this is a block at the interval's true extent or a marker.
    #[must_use]
    pub(crate) const fn kind(self) -> PlacementKind {
        self.kind
    }
}

/// An interval placed on a row, with its true duration kept for choosing a group's
/// representative.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Interval<T> {
    pub(crate) placement: PixelPlacement,
    pub(crate) duration: Duration,
    pub(crate) item: T,
}

/// The markers of one cell of a row, drawn once. The representative is the longest interval of
/// the cell, the earliest of equally long ones; hovering or clicking the marker acts on it. Each
/// member keeps its own time extent in the data; only the drawing is shared.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Marker<T> {
    pub(crate) placement: PixelPlacement,
    pub(crate) representative: T,
    pub(crate) members: usize,
    longest: Duration,
}

/// One block tile at its interval's true extent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Block<T> {
    pub(crate) placement: PixelPlacement,
    pub(crate) item: T,
}

/// The tiles of one row: blocks at their true extents, then markers (drawn above the blocks, in
/// time order) with coincident ones grouped.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RowTiles<T> {
    pub(crate) blocks: Vec<Block<T>>,
    pub(crate) markers: Vec<Marker<T>>,
}

impl<T> RowTiles<T> {
    /// Sorts `intervals`, given in time order, into blocks and grouped markers.
    #[must_use]
    pub(crate) fn arrange(intervals: impl IntoIterator<Item = Interval<T>>) -> Self {
        let mut tiles = Self {
            blocks: Vec::new(),
            markers: Vec::new(),
        };
        for interval in intervals {
            match interval.placement.kind() {
                PlacementKind::Block => tiles.blocks.push(Block {
                    placement: interval.placement,
                    item: interval.item,
                }),
                PlacementKind::Marker => {
                    let cell = tiles
                        .markers
                        .iter_mut()
                        .find(|marker| marker.placement.left == interval.placement.left);
                    match cell {
                        Some(marker) => {
                            marker.members += 1;
                            if interval.duration > marker.longest {
                                marker.longest = interval.duration;
                                marker.representative = interval.item;
                            }
                        }
                        None => tiles.markers.push(Marker {
                            placement: interval.placement,
                            representative: interval.item,
                            members: 1,
                            longest: interval.duration,
                        }),
                    }
                }
            }
        }
        tiles
    }

    /// The items in paint order: blocks, then each marker's representative above them.
    #[must_use]
    #[cfg(windows)]
    pub(crate) fn into_paint_order(self) -> Vec<T> {
        let markers = self.markers.into_iter().map(|marker| marker.representative);
        self.blocks
            .into_iter()
            .map(|block| block.item)
            .chain(markers)
            .collect()
    }
}

/// A label with its measured width.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MeasuredLabel<'a> {
    pub(crate) text: &'a str,
    pub(crate) width: f32,
}

impl PixelPlacement {
    /// Measures each distinct nonempty candidate at most once, stopping at the first fit.
    pub(crate) fn fit_label_with<'a>(
        self,
        padding: f32,
        short: &'a str,
        full: &'a str,
        mut measure: impl FnMut(&str) -> f32,
    ) -> Option<&'a str> {
        let room = self.body_width() - 2. * padding;
        if !full.is_empty() && measure(full) <= room {
            return Some(full);
        }
        if short != full && !short.is_empty() && measure(short) <= room {
            return Some(short);
        }
        None
    }

    /// The longest of `full` and `short` that fits inside the block with `padding` on each side;
    /// `None` when neither does.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn fitting_label<'a>(
        self,
        padding: f32,
        full: MeasuredLabel<'a>,
        short: MeasuredLabel<'a>,
    ) -> Option<&'a str> {
        let room = self.body_width() - 2. * padding;
        [full, short]
            .into_iter()
            .find(|label| !label.text.is_empty() && label.width <= room)
            .map(|label| label.text)
    }
}

/// One axis tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Tick {
    pub(crate) time: TimeOfDay,
    /// Position as a fraction of the plot width.
    pub(crate) fraction: f32,
}

impl Tick {
    /// Whether a label `label_width` wide centered on the tick stays inside a plot `plot_width`
    /// wide, so it is not clipped.
    #[must_use]
    pub(crate) fn has_room_for_label(self, plot_width: f32, label_width: f32) -> bool {
        let center = self.fraction * plot_width;
        center - label_width / 2. >= 0. && center + label_width / 2. <= plot_width
    }
}

/// How many axis ticks a plot has room for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TickBudget(u8);

impl TickBudget {
    /// The most ticks a view shows.
    #[cfg(test)]
    pub(crate) const MAX: Self = Self(MAX_TICKS);

    #[cfg(windows)]
    pub(crate) const fn maximum() -> Self {
        Self(MAX_TICKS)
    }

    /// As many labels `label_width` wide, with a gap between them, as fit in a plot `plot_width`
    /// wide, within [`MIN_TICKS`] and [`MAX_TICKS`].
    #[must_use]
    pub(crate) fn fit(plot_width: f32, label_width: f32) -> Self {
        let slot = label_width + TICK_LABEL_GAP;
        let count = (MIN_TICKS..=MAX_TICKS)
            .rev()
            .find(|count| f32::from(*count) * slot <= plot_width)
            .unwrap_or(MIN_TICKS);
        Self(count)
    }
}

impl View {
    /// A view of `span`; `None` when the span is empty.
    #[must_use]
    pub(crate) fn new(span: TimeSpan) -> Option<Self> {
        (!span.is_empty()).then_some(Self { span })
    }

    /// The visible span.
    #[must_use]
    pub(crate) const fn span(self) -> TimeSpan {
        self.span
    }

    /// The whole day.
    #[must_use]
    pub(crate) const fn full_day() -> Self {
        Self {
            span: TimeSpan::DAY,
        }
    }

    /// The activity range with a margin on both sides; the whole day when there is no activity.
    #[must_use]
    pub(crate) fn activity(range: Option<TimeSpan>) -> Self {
        range
            .map(|range| {
                TimeSpan::new(
                    range.start().earlier_by_minutes(ACTIVITY_MARGIN_MINUTES),
                    range.end().later_by_minutes(ACTIVITY_MARGIN_MINUTES),
                )
            })
            .and_then(Self::new)
            .unwrap_or_else(Self::full_day)
    }

    /// Position of `time` as a fraction of the plot width; outside 0 to 1 when `time` is outside
    /// the view. This is the only place where time becomes a float.
    #[must_use]
    pub(crate) fn fraction(self, time: TimeOfDay) -> f32 {
        let start = self.span.start().millis();
        let offset = |millis: u32| Duration::from_millis(u64::from(millis)).as_secs_f32();
        let from_start = if time.millis() >= start {
            offset(time.millis() - start)
        } else {
            -offset(start - time.millis())
        };
        from_start / self.span.duration().as_secs_f32()
    }

    /// Left and width fractions of `span` clipped to the view; `None` when it lies outside.
    #[must_use]
    pub(crate) fn place(self, span: TimeSpan) -> Option<Placement> {
        let visible = span.intersection(self.span)?;
        let left = self.fraction(visible.start());
        Some(Placement {
            left,
            width: self.fraction(visible.end()) - left,
        })
    }

    fn tick_step_seconds(self, budget: TickBudget) -> u32 {
        let minutes = self.span.duration().as_secs() / u64::from(SECONDS_PER_MINUTE);
        let step = TICK_STEPS_MINUTES
            .into_iter()
            .find(|step| minutes <= u64::from(budget.0) * u64::from(*step))
            .expect("the largest tick step fits a whole day");
        step * SECONDS_PER_MINUTE
    }

    /// Axis ticks on whole multiples of the finest tick step that stays within `budget`.
    #[must_use]
    pub(crate) fn ticks(self, budget: TickBudget) -> Vec<Tick> {
        let step = self.tick_step_seconds(budget);
        let first = self.span.start().seconds().div_ceil(step) * step;
        (first..=self.span.end().seconds())
            .step_by(usize::try_from(step).expect("a tick step fits in usize"))
            .map(|seconds| {
                TimeOfDay::from_elapsed_millis(seconds * crate::model::MILLIS_PER_SECOND)
            })
            .map(|time| Tick {
                time,
                fraction: self.fraction(time),
            })
            .collect()
    }

    #[cfg(windows)]
    pub(crate) fn panned(self, delta_millis: i64, bounds: TimeSpan) -> Self {
        let length = self.span.end().millis() - self.span.start().millis();
        let max_start = bounds.end().millis().saturating_sub(length);
        let start = (i64::from(self.span.start().millis()) + delta_millis)
            .clamp(i64::from(bounds.start().millis()), i64::from(max_start));
        let start = u32::try_from(start).expect("pan is clamped to the logical day");
        Self {
            span: TimeSpan::new(
                TimeOfDay::from_elapsed_millis(start),
                TimeOfDay::from_elapsed_millis(start + length),
            ),
        }
    }

    /// The view after one zoom step around the center, kept inside the day and no narrower
    /// than [`MIN_ZOOM_MINUTES`].
    #[must_use]
    #[cfg(test)]
    pub(crate) fn zoomed(self, step: ZoomStep) -> Self {
        self.zoomed_within(step, TimeSpan::DAY)
    }
    pub(crate) fn zoomed_within(self, step: ZoomStep, bounds: TimeSpan) -> Self {
        let length = self.span.end().millis() - self.span.start().millis();
        let available = bounds.end().millis() - bounds.start().millis();
        let minimum = (MIN_ZOOM_MINUTES * SECONDS_PER_MINUTE * crate::model::MILLIS_PER_SECOND)
            .min(available);
        let requested = match step {
            ZoomStep::In => length / 2,
            ZoomStep::Out => length.saturating_mul(2),
        }
        .clamp(minimum, available);
        let center = self.span.start().millis() + length / 2;
        let start = center
            .saturating_sub(requested / 2)
            .clamp(bounds.start().millis(), bounds.end().millis() - requested);
        Self {
            span: TimeSpan::new(
                TimeOfDay::from_elapsed_millis(start),
                TimeOfDay::from_elapsed_millis(start + requested),
            ),
        }
    }
}

/// Parameters for mapping a horizontal drag across the timeline plot into a snapped, clamped
/// half-open UTC range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DragSelectionSpace {
    pub(crate) view: View,
    pub(crate) plot_width: f32,
    pub(crate) day_bounds: UtcRange,
    /// Exclusive upper UTC limit for "today" (clamps selection end to "now"); `None` for past days.
    pub(crate) clamp_end_utc_ms: Option<i64>,
}

fn snap_nearest_granularity(elapsed_ms: i64) -> i64 {
    let half = SELECTION_SNAP_MILLIS / 2;
    let steps = if elapsed_ms >= 0 {
        elapsed_ms.saturating_add(half) / SELECTION_SNAP_MILLIS
    } else {
        elapsed_ms.saturating_sub(half) / SELECTION_SNAP_MILLIS
    };
    steps.saturating_mul(SELECTION_SNAP_MILLIS)
}

fn floor_granularity(elapsed_ms: i64) -> i64 {
    elapsed_ms
        .div_euclid(SELECTION_SNAP_MILLIS)
        .saturating_mul(SELECTION_SNAP_MILLIS)
}

/// Maps two plot-local horizontal coordinates (`anchor_local_x`, `current_local_x`) into a
/// non-empty half-open [`UtcRange`] snapped to [`SELECTION_SNAP_MILLIS`] and clamped to
/// `space.day_bounds` (and to `space.clamp_end_utc_ms` for today).
///
/// Returns `None` if the plot width is non-positive, inputs are non-finite, or the clamped
/// selection lies completely past the allowed upper bound (for example, entirely in the future).
#[must_use]
pub(crate) fn snap_drag_selection(
    space: DragSelectionSpace,
    anchor_local_x: f32,
    current_local_x: f32,
) -> Option<UtcRange> {
    if !space.plot_width.is_finite()
        || space.plot_width <= 0.
        || !anchor_local_x.is_finite()
        || !current_local_x.is_finite()
    {
        return None;
    }
    let raw_max_utc = match space.clamp_end_utc_ms {
        Some(now_ms) => space.day_bounds.end.min(now_ms),
        None => space.day_bounds.end,
    };
    let max_elapsed = raw_max_utc.checked_sub(space.day_bounds.start)?;
    if max_elapsed <= 0 {
        return None;
    }
    let max_snapped_elapsed = floor_granularity(max_elapsed);
    if max_snapped_elapsed <= 0 {
        return None;
    }

    let view_start_ms = i64::from(space.view.span().start().millis());
    let view_len_ms = space.view.span().end().millis() - space.view.span().start().millis();
    let px_to_elapsed = |local_x: f32| -> Option<i64> {
        let fraction = (f64::from(local_x) / f64::from(space.plot_width)).clamp(0.0, 1.0);
        let offset = (fraction * f64::from(view_len_ms)).round().to_i64()?;
        view_start_ms.checked_add(offset)
    };

    let elapsed_a = px_to_elapsed(anchor_local_x)?;
    let elapsed_b = px_to_elapsed(current_local_x)?;
    let (raw_start, raw_end) = if elapsed_a <= elapsed_b {
        (elapsed_a, elapsed_b)
    } else {
        (elapsed_b, elapsed_a)
    };
    if raw_start >= max_elapsed {
        return None;
    }

    let mut snapped_start = snap_nearest_granularity(raw_start).clamp(0, max_snapped_elapsed);
    let mut snapped_end = snap_nearest_granularity(raw_end).clamp(0, max_snapped_elapsed);
    if snapped_start >= snapped_end {
        if snapped_start.saturating_add(SELECTION_SNAP_MILLIS) <= max_snapped_elapsed {
            snapped_end = snapped_start.saturating_add(SELECTION_SNAP_MILLIS);
        } else {
            snapped_end = max_snapped_elapsed;
            snapped_start = snapped_end.saturating_sub(SELECTION_SNAP_MILLIS).max(0);
        }
    }
    if snapped_end <= snapped_start {
        return None;
    }
    Some(UtcRange {
        start: space.day_bounds.start.checked_add(snapped_start)?,
        end: space.day_bounds.start.checked_add(snapped_end)?,
    })
}

/// Motion phase of a timeline range-selection pointer gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeSelectMotion {
    Pressed,
    Dragging,
    Cancelled,
}

/// Active horizontal drag-selection gesture on the timeline plot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RangeSelectGesture {
    anchor_window_x: f32,
    current_window_x: f32,
    view: View,
    motion: RangeSelectMotion,
}

impl RangeSelectGesture {
    /// Starts a candidate selection gesture at `anchor_window_x`.
    #[must_use]
    pub(crate) const fn new(anchor_window_x: f32, view: View) -> Self {
        Self {
            anchor_window_x,
            current_window_x: anchor_window_x,
            view,
            motion: RangeSelectMotion::Pressed,
        }
    }

    /// Updates the pointer X coordinate; crosses into dragging once horizontal displacement
    /// reaches [`SELECTION_DRAG_THRESHOLD_PX`]. Returns whether the selection band changed.
    pub(crate) fn move_to(&mut self, window_x: f32) -> bool {
        if self.motion == RangeSelectMotion::Cancelled {
            return false;
        }
        if (window_x - self.anchor_window_x).abs() >= SELECTION_DRAG_THRESHOLD_PX {
            self.motion = RangeSelectMotion::Dragging;
        }
        if self.motion == RangeSelectMotion::Dragging {
            self.current_window_x = window_x;
            return true;
        }
        false
    }

    /// Cancels the active drag selection (for example on `Escape`) while still suppressing a tile
    /// click until pointer release.
    pub(crate) fn cancel(&mut self) -> bool {
        let was_active = self.motion != RangeSelectMotion::Cancelled;
        self.motion = RangeSelectMotion::Cancelled;
        was_active
    }

    /// Whether this gesture never exceeded [`SELECTION_DRAG_THRESHOLD_PX`] and was not cancelled,
    /// so releasing it keeps normal click behavior.
    #[must_use]
    pub(crate) const fn is_click(self) -> bool {
        matches!(self.motion, RangeSelectMotion::Pressed)
    }

    /// Whether a selection band is actively being dragged.
    #[must_use]
    pub(crate) const fn is_dragging(self) -> bool {
        matches!(self.motion, RangeSelectMotion::Dragging)
    }

    /// Resolves the snapped and clamped UTC range of this drag gesture when actively dragging.
    #[must_use]
    pub(crate) fn resolve_range(
        self,
        plot_origin_x: f32,
        plot_width: f32,
        day_bounds: UtcRange,
        clamp_end_utc_ms: Option<i64>,
    ) -> Option<UtcRange> {
        if !self.is_dragging() {
            return None;
        }
        snap_drag_selection(
            DragSelectionSpace {
                view: self.view,
                plot_width,
                day_bounds,
                clamp_end_utc_ms,
            },
            self.anchor_window_x - plot_origin_x,
            self.current_window_x - plot_origin_x,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(from: (u32, u32), to: (u32, u32)) -> TimeSpan {
        TimeSpan::new(TimeOfDay::hm(from.0, from.1), TimeOfDay::hm(to.0, to.1))
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn edge_zoom_out_reaches_exact_bounds_and_preserves_requested_length() {
        for bounds in [TimeSpan::DAY, span((1, 0), (23, 0))] {
            let mut view = View::new(TimeSpan::new(
                bounds.start(),
                bounds.start().later_by_minutes(5),
            ))
            .expect("view");
            for _ in 0..20 {
                let expected = (view.span().duration() * 2).min(bounds.duration());
                view = view.zoomed_within(ZoomStep::Out, bounds);
                assert_eq!(view.span().duration(), expected);
                assert!(view.span().start() >= bounds.start());
                assert!(view.span().end() <= bounds.end());
            }
            assert_eq!(view.span(), bounds);
        }
    }

    #[test]
    fn identical_label_candidates_are_measured_once_and_empty_candidates_are_skipped() {
        let grid = PlotGrid::new(480., ScaleFactor::try_new(1.).expect("scale"), 1.).expect("grid");
        let block = grid.place(Placement {
            left: 0.,
            width: 1.,
        });
        let mut count = 0;
        assert_eq!(
            block.fit_label_with(6., "title", "title", |_| {
                count += 1;
                1000.
            }),
            None
        );
        assert_eq!(count, 1);
        assert_eq!(
            block.fit_label_with(6., "", "", |_| panic!("empty text must not be measured")),
            None
        );
    }

    #[test]
    fn empty_block_labels_are_absent() {
        let grid = PlotGrid::new(480., ScaleFactor::try_new(1.).expect("scale"), 1.).expect("grid");
        let block = grid.place(Placement {
            left: 0.,
            width: 1.,
        });
        let empty = MeasuredLabel {
            text: "",
            width: 0.,
        };
        assert_eq!(block.fitting_label(6., empty, empty), None);
    }

    #[test]
    fn activity_view_adds_margins_and_falls_back_to_the_whole_day() {
        let view = View::activity(Some(span((8, 33), (18, 45))));
        assert_eq!(Some(view), View::new(span((8, 18), (19, 0))));
        assert_eq!(View::activity(None), View::full_day());
        assert_eq!(
            View::activity(Some(span((0, 5), (23, 55)))),
            View::full_day()
        );
        assert_eq!(View::new(span((9, 0), (9, 0))), None);
    }

    #[test]
    fn place_clips_to_the_view() {
        let view = View::new(span((10, 0), (12, 0))).expect("non-empty");
        let inside = view.place(span((10, 30), (11, 0))).expect("inside");
        assert!(
            close(inside.left, 0.25) && close(inside.width, 0.25),
            "{inside:?}"
        );
        let clipped = view.place(span((9, 0), (10, 30))).expect("overlapping");
        assert!(
            close(clipped.left, 0.) && close(clipped.width, 0.25),
            "{clipped:?}"
        );
        assert_eq!(view.place(span((12, 0), (13, 0))), None);
        assert!(close(view.fraction(TimeOfDay::hm(9, 0)), -0.5));
    }

    #[test]
    fn ticks_use_the_finest_step_within_the_budget() {
        let day = View::full_day().ticks(TickBudget::MAX);
        assert_eq!(day.len(), 13, "every 2 hours including both ends");
        let workday = View::new(span((8, 18), (19, 0)))
            .expect("non-empty")
            .ticks(TickBudget::MAX);
        let times: Vec<String> = workday.iter().map(|tick| tick.time.to_string()).collect();
        assert_eq!(times.first().map(String::as_str), Some("09:00"));
        assert_eq!(times.last().map(String::as_str), Some("19:00"));
        assert_eq!(times.len(), 11);
        let narrow = View::new(span((10, 0), (10, 10)))
            .expect("non-empty")
            .ticks(TickBudget::MAX);
        assert_eq!(narrow.len(), 11, "one-minute steps");
    }

    #[test]
    fn a_narrow_plot_gets_fewer_and_coarser_ticks() {
        let workday = View::new(span((8, 18), (19, 0))).expect("non-empty");
        let wide = workday.ticks(TickBudget::fit(900., 36.));
        let narrow = workday.ticks(TickBudget::fit(300., 36.));
        assert!(
            narrow.len() < wide.len(),
            "{} vs {}",
            narrow.len(),
            wide.len()
        );
        // Neighboring labels of 36 px keep their 8 px gap at the chosen budget.
        let spacing = narrow
            .windows(2)
            .map(|pair| match pair {
                [a, b] => (b.fraction - a.fraction) * 300.,
                _ => f32::INFINITY,
            })
            .fold(f32::INFINITY, f32::min);
        assert!(spacing >= 36. + TICK_LABEL_GAP, "tick spacing {spacing}");
        assert_eq!(
            TickBudget::fit(10., 36.),
            TickBudget(MIN_TICKS),
            "never below the minimum"
        );
        assert_eq!(TickBudget::fit(100_000., 36.), TickBudget::MAX);
        assert_eq!(
            View::full_day().ticks(TickBudget(MIN_TICKS)).len(),
            5,
            "even the minimum budget has a step that covers a day"
        );
    }

    #[test]
    fn labels_that_would_be_clipped_are_dropped() {
        let view = View::new(span((9, 0), (11, 0))).expect("non-empty");
        let labeled: Vec<bool> = view
            .ticks(TickBudget::MAX)
            .iter()
            .map(|tick| tick.has_room_for_label(400., 36.))
            .collect();
        assert_eq!(labeled.first(), Some(&false), "centered on the left edge");
        assert_eq!(labeled.last(), Some(&false), "centered on the right edge");
        assert!(
            labeled
                .iter()
                .skip(1)
                .take(labeled.len() - 2)
                .all(|room| *room)
        );
        let edge = Tick {
            time: TimeOfDay::hm(9, 0),
            fraction: 18. / 400.,
        };
        assert!(
            edge.has_room_for_label(400., 36.),
            "exactly touching the edge fits"
        );
    }

    fn grid(width: f32, scale: f32) -> PlotGrid {
        PlotGrid::new(width, ScaleFactor::try_new(scale).expect("valid"), 1.).expect("wide enough")
    }

    fn whole(value: f32) -> bool {
        (value - value.round()).abs() < 1e-3
    }

    #[test]
    fn snapped_positions_are_whole_physical_pixels() {
        for scale in [1., 1.25, 1.5] {
            let grid = grid(1001., scale);
            for fraction in [0., 0.1234, 0.5, 0.999, 1.] {
                let logical = grid.snap(fraction);
                assert!(whole(logical * scale), "scale {scale} fraction {fraction}");
            }
        }
    }

    #[test]
    fn touching_blocks_share_an_edge_at_every_scale() {
        for scale in [1., 1.25, 1.5] {
            let grid = grid(777., scale);
            let cuts = [0., 0.1, 0.3, 0.55, 0.8, 1.];
            let blocks: Vec<PixelPlacement> = cuts
                .windows(2)
                .filter_map(|pair| match pair {
                    [a, b] => Some(grid.place(Placement {
                        left: *a,
                        width: b - a,
                    })),
                    _ => None,
                })
                .collect();
            for pair in blocks.windows(2) {
                if let [first, second] = pair {
                    assert_eq!(first.kind(), PlacementKind::Block, "scale {scale}");
                    assert!(
                        (first.left() + first.width() - second.left()).abs() < 1e-3,
                        "scale {scale}: {first:?} {second:?}"
                    );
                }
            }
            for block in &blocks {
                assert!(whole(block.left() * scale) && whole(block.width() * scale));
                assert!(whole(block.gap() * scale) && block.gap() * scale >= 1. - 1e-3);
            }
        }
    }

    /// The tile of an interval of `fraction` of the plot starting at `left`.
    fn tile_of(grid: PlotGrid, left: f32, fraction: f32) -> PixelPlacement {
        grid.place(Placement {
            left,
            width: fraction,
        })
    }

    /// Every tile lies wholly inside a plot `width` logical pixels wide at `scale` and its
    /// painted body is at least 2 logical pixels rounded up to physical pixels.
    fn assert_visible(tile: PixelPlacement, width: f32, scale: f32, context: &str) {
        let slack = 1e-3;
        assert!(tile.left() >= -slack, "{context}: starts left of the plot");
        assert!(
            tile.left() + tile.width() <= width + slack,
            "{context}: ends right of the plot ({tile:?})"
        );
        let body_px = tile.body_width() * scale;
        let minimum = (MIN_BLOCK_WIDTH * scale).ceil();
        assert!(
            body_px >= minimum - slack,
            "{context}: body {body_px} physical px, minimum {minimum}"
        );
        assert!(whole(body_px), "{context}: whole physical pixels");
    }

    #[test]
    fn undersized_intervals_become_visible_markers_inside_the_plot() {
        // 2 logical pixels are 2, 3 (2.5 rounded up) and 3 physical pixels, plus a gap of
        // 1, 1 and 2 physical pixels (1.25 and 1.5 rounded to nearest).
        for (scale, body_px, gap_px) in [(1., 2., 1.), (1.25, 3., 1.), (1.5, 3., 2.)] {
            let width = 1000.;
            let grid = grid(width, scale);
            // Left, middle, and the very right edge, where widening to the right would leave
            // the plot.
            for left in [0., 0.5, 0.9999, 1.] {
                let tiny = tile_of(grid, left, 0.0001);
                let context = format!("scale {scale} left {left}");
                assert_eq!(tiny.kind(), PlacementKind::Marker, "{context}");
                assert_visible(tiny, width, scale, &context);
                assert!(
                    (tiny.body_width() * scale - body_px).abs() < 1e-3,
                    "{context}"
                );
                assert!((tiny.gap() * scale - gap_px).abs() < 1e-3, "{context}");
            }
            let wide = tile_of(grid, 0.1, 0.2);
            assert_eq!(wide.kind(), PlacementKind::Block);
        }
    }

    #[test]
    fn a_plot_narrower_than_one_marker_is_refused_and_the_narrowest_holds_one() {
        for (scale, marker) in [(1., 3.), (1.25, 4.), (1.5, 5.)] {
            let factor = ScaleFactor::try_new(scale).expect("valid");
            // One marker is gap + body physical pixels: 3, 4 and 5.
            assert!(
                PlotGrid::new(factor.logical(marker - 1.), factor, 1.).is_err(),
                "scale {scale}"
            );
            let width = factor.logical(marker + 1.);
            let grid = PlotGrid::new(width, factor, 1.).expect("one marker fits");
            for left in [0., 0.5, 1.] {
                let tile = tile_of(grid, left, 0.0001);
                assert_eq!(tile.kind(), PlacementKind::Marker);
                assert_visible(tile, width, scale, &format!("scale {scale} left {left}"));
            }
        }
    }

    fn interval(grid: PlotGrid, left: f32, fraction: f32, seconds: u64, id: u32) -> Interval<u32> {
        Interval {
            placement: tile_of(grid, left, fraction),
            duration: Duration::from_secs(seconds),
            item: id,
        }
    }

    #[test]
    fn coincident_markers_form_one_group_whose_representative_is_the_longest() {
        for scale in [1., 1.25, 1.5] {
            let width = 100.;
            let grid = grid(width, scale);
            // Ten intervals within a fraction of a pixel of each other; interval 6 is the
            // longest and 3 and 4 tie for second.
            let seconds = [5, 7, 3, 9, 9, 2, 12, 1, 4, 8];
            let intervals = seconds.iter().enumerate().map(|(index, seconds)| {
                let id = u32::try_from(index).expect("small");
                interval(
                    grid,
                    f32::from(u8::try_from(id).expect("small")) * 0.0004,
                    0.0001,
                    *seconds,
                    id,
                )
            });
            let tiles = RowTiles::arrange(intervals);
            assert!(tiles.blocks.is_empty(), "scale {scale}");
            assert_eq!(tiles.markers.len(), 1, "scale {scale}: one group");
            let marker = tiles.markers.first().expect("one group");
            assert_eq!(marker.members, 10);
            assert_eq!(marker.representative, 6, "the longest interval");
            assert_visible(marker.placement, width, scale, &format!("scale {scale}"));
        }
    }

    #[test]
    fn equally_long_coincident_markers_choose_the_earliest() {
        let grid = grid(100., 1.);
        let tiles = RowTiles::arrange([
            interval(grid, 0.5, 0.0001, 9, 1),
            interval(grid, 0.5, 0.0001, 9, 2),
        ]);
        assert_eq!(tiles.markers.len(), 1);
        assert_eq!(
            tiles.markers.first().map(|marker| marker.representative),
            Some(1)
        );
    }

    #[test]
    fn markers_in_different_cells_stay_separate_and_never_partly_overlap() {
        for scale in [1., 1.25, 1.5] {
            let width = 100.;
            let grid = grid(width, scale);
            let intervals = (0..40u8)
                .map(|index| interval(grid, f32::from(index) * 0.025, 0.0001, 1, u32::from(index)));
            let tiles = RowTiles::arrange(intervals);
            assert!(tiles.markers.len() > 1, "scale {scale}");
            for marker in &tiles.markers {
                assert_visible(marker.placement, width, scale, &format!("scale {scale}"));
            }
            for pair in tiles.markers.windows(2) {
                if let [first, second] = pair {
                    assert!(
                        first.placement.left() + first.placement.width()
                            <= second.placement.left() + 1e-3,
                        "scale {scale}: {first:?} {second:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn blocks_stay_at_their_true_extent_next_to_markers() {
        let grid = grid(500., 1.25);
        let tiles = RowTiles::arrange([
            interval(grid, 0.0, 0.3, 100, 1),
            interval(grid, 0.3, 0.0001, 1, 2),
            interval(grid, 0.3001, 0.3, 100, 3),
        ]);
        assert_eq!(tiles.blocks.len(), 2);
        assert_eq!(tiles.markers.len(), 1);
        let [first, second] = tiles.blocks.as_slice() else {
            panic!("two blocks");
        };
        assert!((first.placement.left()).abs() < 1e-3);
        assert!(
            (first.placement.left() + first.placement.width() - 150.).abs() < 1.,
            "the block keeps its own right edge, not stretched: {first:?}"
        );
        assert!(second.placement.left() >= first.placement.left() + first.placement.width() - 1e-3);
    }

    #[test]
    fn scale_factors_must_be_positive_and_finite() {
        assert!(ScaleFactor::try_new(1.).is_ok());
        for invalid in [0., -1., f32::NAN, f32::INFINITY] {
            assert!(ScaleFactor::try_new(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn zoom_halves_and_doubles_within_bounds() {
        let view = View::new(span((10, 0), (12, 0))).expect("non-empty");
        assert_eq!(view.zoomed(ZoomStep::In).span(), span((10, 30), (11, 30)));
        assert_eq!(view.zoomed(ZoomStep::Out).span(), span((9, 0), (13, 0)));
        let narrow = View::new(span((10, 0), (10, 6))).expect("non-empty");
        assert_eq!(
            narrow.zoomed(ZoomStep::In).span().duration(),
            Duration::from_secs(5 * 60)
        );
        let early = View::new(span((0, 0), (4, 0))).expect("non-empty");
        assert_eq!(early.zoomed(ZoomStep::Out).span(), span((0, 0), (8, 0)));
        assert_eq!(View::full_day().zoomed(ZoomStep::Out).span(), TimeSpan::DAY);
    }

    #[test]
    fn labels_fall_back_from_full_to_short_to_none() {
        let full = MeasuredLabel {
            text: "开发 4 小时",
            width: 72.,
        };
        let short = MeasuredLabel {
            text: "开发",
            width: 24.,
        };
        let block = |body: f32| {
            PlotGrid::new(1000., ScaleFactor::try_new(1.).expect("valid"), 1.)
                .expect("wide enough")
                .place(Placement {
                    left: 0.,
                    width: (body + 1.) / 1000.,
                })
        };
        assert_eq!(
            block(100.).fitting_label(6., full, short),
            Some("开发 4 小时")
        );
        assert_eq!(
            block(84.).fitting_label(6., full, short),
            Some("开发 4 小时"),
            "exact fit"
        );
        assert_eq!(block(83.).fitting_label(6., full, short), Some("开发"));
        assert_eq!(block(36.).fitting_label(6., full, short), Some("开发"));
        assert_eq!(block(35.).fitting_label(6., full, short), None);
    }

    #[test]
    fn drag_selection_snaps_to_one_minute_and_clamps_to_day_and_now() {
        const DAY_START: i64 = 1_763_683_200_000; // 2025-11-21T00:00:00Z
        const HOUR_MS: i64 = 60 * SELECTION_SNAP_MILLIS;
        let day_bounds = UtcRange {
            start: DAY_START,
            end: DAY_START + 24 * HOUR_MS,
        };
        // 4-hour view [08:00, 12:00) across 480 px => 2 px per minute.
        let view = View::new(span((8, 0), (12, 0))).expect("view");
        let past_space = DragSelectionSpace {
            view,
            plot_width: 480.,
            day_bounds,
            clamp_end_utc_ms: None,
        };

        // 08:30:20 (60.67 px) to 09:44:50 (209.67 px) snaps to [08:30, 09:45).
        let forward = snap_drag_selection(past_space, 60.67, 209.67).expect("snapped range");
        assert_eq!(
            forward,
            UtcRange {
                start: DAY_START + 8 * HOUR_MS + 30 * SELECTION_SNAP_MILLIS,
                end: DAY_START + 9 * HOUR_MS + 45 * SELECTION_SNAP_MILLIS,
            }
        );

        // Right-to-left drag orders endpoints and clamps coordinates outside [0, plot_width].
        let reversed = snap_drag_selection(past_space, 520., -30.).expect("clamped view range");
        assert_eq!(
            reversed,
            UtcRange {
                start: DAY_START + 8 * HOUR_MS,
                end: DAY_START + 12 * HOUR_MS,
            }
        );

        // Today clamp: now = 10:15:45 clamps a drag crossing now to 10:15:00 (snapped <= now).
        let now_utc_ms = DAY_START + 10 * HOUR_MS + 15 * SELECTION_SNAP_MILLIS + 45_000;
        let today_space = DragSelectionSpace {
            clamp_end_utc_ms: Some(now_utc_ms),
            ..past_space
        };
        let clamped_to_now =
            snap_drag_selection(today_space, 120., 480.).expect("clamped to today now");
        assert_eq!(
            clamped_to_now,
            UtcRange {
                start: DAY_START + 9 * HOUR_MS,
                end: DAY_START + 10 * HOUR_MS + 15 * SELECTION_SNAP_MILLIS,
            }
        );
        assert!(clamped_to_now.end <= now_utc_ms);

        // Dragging wholly in the future past now produces no range.
        assert_eq!(snap_drag_selection(today_space, 300., 420.), None);

        // Full-day past drag reaches 24:00 without truncation.
        let full_past = DragSelectionSpace {
            view: View::full_day(),
            plot_width: 1440.,
            day_bounds,
            clamp_end_utc_ms: None,
        };
        assert_eq!(
            snap_drag_selection(full_past, 1410., 1500.),
            Some(UtcRange {
                start: DAY_START + 23 * HOUR_MS + 30 * SELECTION_SNAP_MILLIS,
                end: day_bounds.end,
            })
        );
    }

    #[test]
    fn range_select_gesture_treats_below_threshold_as_click_and_cancel_stops_selection() {
        const DAY_START: i64 = 1_763_683_200_000;
        const HOUR_MS: i64 = 60 * SELECTION_SNAP_MILLIS;
        let day_bounds = UtcRange {
            start: DAY_START,
            end: DAY_START + 24 * HOUR_MS,
        };
        let view = View::new(span((8, 0), (12, 0))).expect("view");

        let mut click = RangeSelectGesture::new(200., view);
        assert!(!click.move_to(200. + SELECTION_DRAG_THRESHOLD_PX - 0.5));
        assert!(click.is_click());
        assert!(!click.is_dragging());
        assert_eq!(click.resolve_range(100., 480., day_bounds, None), None);

        let mut drag = RangeSelectGesture::new(200., view);
        assert!(drag.move_to(200. + SELECTION_DRAG_THRESHOLD_PX));
        assert!(!drag.is_click());
        assert!(drag.is_dragging());
        assert!(drag.resolve_range(100., 480., day_bounds, None).is_some());

        // Escape cancellation clears active drag and prevents click fallback.
        assert!(drag.cancel());
        assert!(!drag.is_click());
        assert!(!drag.is_dragging());
        assert!(!drag.move_to(350.));
        assert_eq!(drag.resolve_range(100., 480., day_bounds, None), None);
    }
}
