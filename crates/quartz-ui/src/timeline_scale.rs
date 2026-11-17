//! Geometry of the timeline without GPUI: the visible span, block placement as plot fractions,
//! axis ticks, zoom steps and which block label fits (07 section 4).

use std::time::Duration;

use crate::model::{SECONDS_PER_MINUTE, TimeOfDay, TimeSpan};

/// Margin around the activity range of the default view (07 section 4.1).
pub(crate) const ACTIVITY_MARGIN_MINUTES: u32 = 15;
/// The narrowest span zooming in reaches.
pub(crate) const MIN_ZOOM_MINUTES: u32 = 5;
/// Most axis ticks a view shows; a view picks the finest step that stays within it.
pub(crate) const MAX_TICKS: u32 = 14;
/// Candidate tick steps in minutes, finest first; the last one fits a whole day.
const TICK_STEPS_MINUTES: [u32; 9] = [1, 5, 10, 15, 30, 60, 120, 180, 360];
/// Ticks closer than this fraction to either plot edge get no label, so labels are not clipped.
const AXIS_LABEL_EDGE: f32 = 0.02;

/// Average advance of an ASCII glyph in 12 px caption text.
const NARROW_GLYPH_WIDTH: f32 = 6.5;
/// Advance of a wide (CJK) glyph in 12 px caption text.
const WIDE_GLYPH_WIDTH: f32 = 12.;
/// Horizontal padding a block label needs on top of its text.
const LABEL_PADDING: f32 = 12.;

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

/// Left edge and width of a block as fractions of the plot width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    pub(crate) left: f32,
    pub(crate) width: f32,
}

/// One axis tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Tick {
    pub(crate) time: TimeOfDay,
    /// Position as a fraction of the plot width.
    pub(crate) fraction: f32,
}

impl Tick {
    /// Whether the tick is far enough from the plot edges to carry a label.
    #[must_use]
    pub(crate) fn has_room_for_label(self) -> bool {
        (AXIS_LABEL_EDGE..=1. - AXIS_LABEL_EDGE).contains(&self.fraction)
    }
}

impl View {
    /// A view of `span`; `None` when the span is empty.
    #[must_use]
    pub(crate) fn new(span: TimeSpan) -> Option<Self> {
        (!span.is_empty()).then_some(Self { span })
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
        let start = self.span.start().seconds();
        let offset = |seconds: u32| Duration::from_secs(u64::from(seconds)).as_secs_f32();
        let from_start = if time.seconds() >= start {
            offset(time.seconds() - start)
        } else {
            -offset(start - time.seconds())
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

    fn tick_step_seconds(self) -> u32 {
        let minutes = self.span.duration().as_secs() / u64::from(SECONDS_PER_MINUTE);
        let step = TICK_STEPS_MINUTES
            .into_iter()
            .find(|step| minutes <= u64::from(MAX_TICKS) * u64::from(*step))
            .expect("the largest tick step fits a whole day");
        step * SECONDS_PER_MINUTE
    }

    /// Axis ticks on whole multiples of the tick step inside the view.
    #[must_use]
    pub(crate) fn ticks(self) -> Vec<Tick> {
        let step = self.tick_step_seconds();
        let first = self.span.start().seconds().div_ceil(step) * step;
        (first..=self.span.end().seconds())
            .step_by(usize::try_from(step).expect("a tick step fits in usize"))
            .filter_map(TimeOfDay::from_seconds)
            .map(|time| Tick {
                time,
                fraction: self.fraction(time),
            })
            .collect()
    }

    /// The span after one zoom step around the center, kept inside the day and no narrower
    /// than [`MIN_ZOOM_MINUTES`].
    #[must_use]
    pub(crate) fn zoomed(self, step: ZoomStep) -> TimeSpan {
        let length = self.span.end().seconds() - self.span.start().seconds();
        let center = self.span.start().seconds() + length / 2;
        let half = match step {
            ZoomStep::In => length / 4,
            ZoomStep::Out => length,
        };
        let half = half.clamp(
            MIN_ZOOM_MINUTES * SECONDS_PER_MINUTE / 2,
            TimeOfDay::END.seconds() / 2,
        );
        let start = TimeOfDay::from_seconds(center.saturating_sub(half))
            .expect("the start lies before the center");
        let end =
            TimeOfDay::from_seconds(center.saturating_add(half).min(TimeOfDay::END.seconds()))
                .expect("the end is clamped to the day");
        TimeSpan::new(start, end)
    }
}

/// Estimated width of `text` in caption type, including label padding.
fn estimate_label_width(text: &str) -> f32 {
    let glyphs: f32 = text
        .chars()
        .map(|c| {
            if c.is_ascii() {
                NARROW_GLYPH_WIDTH
            } else {
                WIDE_GLYPH_WIDTH
            }
        })
        .sum();
    glyphs + LABEL_PADDING
}

/// Chooses the longest label that fits in `pixels`: `full`, then `short`, else none.
#[must_use]
pub(crate) fn fitting_label(pixels: f32, short: &str, full: &str) -> Option<String> {
    [full, short]
        .into_iter()
        .find(|text| estimate_label_width(text) <= pixels)
        .map(str::to_owned)
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
    fn ticks_use_the_finest_step_within_the_limit() {
        let day = View::full_day().ticks();
        assert_eq!(day.len(), 13, "every 2 hours including both ends");
        let workday = View::new(span((8, 18), (19, 0)))
            .expect("non-empty")
            .ticks();
        let times: Vec<String> = workday.iter().map(|tick| tick.time.to_string()).collect();
        assert_eq!(times.first().map(String::as_str), Some("09:00"));
        assert_eq!(times.last().map(String::as_str), Some("19:00"));
        assert_eq!(times.len(), 11);
        let narrow = View::new(span((10, 0), (10, 10)))
            .expect("non-empty")
            .ticks();
        assert_eq!(narrow.len(), 11, "one-minute steps");
    }

    #[test]
    fn edge_ticks_get_no_label() {
        let view = View::new(span((9, 0), (11, 0))).expect("non-empty");
        let labeled: Vec<bool> = view
            .ticks()
            .iter()
            .map(|tick| tick.has_room_for_label())
            .collect();
        assert_eq!(labeled.first(), Some(&false));
        assert_eq!(labeled.last(), Some(&false));
        assert!(
            labeled
                .iter()
                .skip(1)
                .take(labeled.len() - 2)
                .all(|room| *room)
        );
    }

    #[test]
    fn zoom_halves_and_doubles_within_bounds() {
        let view = View::new(span((10, 0), (12, 0))).expect("non-empty");
        assert_eq!(view.zoomed(ZoomStep::In), span((10, 30), (11, 30)));
        assert_eq!(view.zoomed(ZoomStep::Out), span((9, 0), (13, 0)));
        let narrow = View::new(span((10, 0), (10, 6))).expect("non-empty");
        assert_eq!(
            narrow.zoomed(ZoomStep::In).duration(),
            Duration::from_secs(5 * 60)
        );
        let early = View::new(span((0, 0), (4, 0))).expect("non-empty");
        assert_eq!(early.zoomed(ZoomStep::Out), span((0, 0), (6, 0)));
        assert_eq!(View::full_day().zoomed(ZoomStep::Out), TimeSpan::DAY);
    }

    #[test]
    fn labels_fall_back_from_full_to_short_to_none() {
        assert_eq!(
            fitting_label(200., "开发", "开发 4 小时"),
            Some("开发 4 小时".to_owned())
        );
        assert_eq!(
            fitting_label(40., "开发", "开发 4 小时"),
            Some("开发".to_owned())
        );
        assert_eq!(fitting_label(20., "开发", "开发 4 小时"), None);
    }
}
