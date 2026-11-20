//! Typed snapshot inputs shared by storage and the pure query pipeline.

use quartz_protocol::{ApplicationRef, ClockSegmentId, QueryGapReason, QueryStopReason};

/// Persistent row identity, independent of capture's per-run row sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FactId(pub i64);
pub(crate) use quartz_domain::capture::CaptureLevel;
#[derive(Debug, Clone)]
pub(crate) struct ClockSegment {
    pub id: ClockSegmentId,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub start_mono_ms: i64,
    pub end_mono_ms: i64,
}
#[derive(Debug, Clone)]
pub(crate) struct FactSpan {
    pub id: FactId,
    pub segment: ClockSegmentId,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub start_mono_ms: i64,
    pub end_mono_ms: i64,
}
/// UTC clipping uses the row's own monotonic endpoints within its clock segment.
#[derive(Debug, Clone, Copy)]
struct SpanDuration {
    start_utc_ms: i64,
    end_utc_ms: i64,
    duration_ms: i64,
}
impl SpanDuration {
    fn within(self, range: QueryRange) -> i64 {
        if self.end_utc_ms == self.start_utc_ms {
            return 0;
        }
        let utc_duration = i128::from(self.end_utc_ms) - i128::from(self.start_utc_ms);
        let at = |utc: i64| {
            (i128::from(utc) - i128::from(self.start_utc_ms)) * i128::from(self.duration_ms)
                / utc_duration
        };
        i64::try_from(at(range.end) - at(range.start))
            .expect("clipped duration stays within stored monotonic span")
    }
}
impl FactSpan {
    pub(crate) fn duration_within(&self, range: QueryRange) -> i64 {
        SpanDuration {
            start_utc_ms: self.start_utc_ms,
            end_utc_ms: self.end_utc_ms,
            duration_ms: self.end_mono_ms - self.start_mono_ms,
        }
        .within(range)
    }
}
impl ClockSegment {
    pub(crate) fn duration_within(&self, range: QueryRange) -> i64 {
        SpanDuration {
            start_utc_ms: self.start_utc_ms,
            end_utc_ms: self.end_utc_ms,
            duration_ms: self.end_mono_ms - self.start_mono_ms,
        }
        .within(range)
    }
}
impl super::pipeline::DerivedSegment {
    pub(crate) fn duration_within(&self, range: QueryRange) -> i64 {
        SpanDuration {
            start_utc_ms: self.display.start_utc_ms,
            end_utc_ms: self.display.end_utc_ms,
            duration_ms: self.display.duration_ms,
        }
        .within(range)
    }
}
#[derive(Debug, Clone)]
pub(crate) enum ForegroundKind {
    Window,
    NoForeground,
}
#[derive(Debug, Clone)]
pub(crate) struct Foreground {
    pub span: FactSpan,
    pub kind: ForegroundKind,
    pub level: CaptureLevel,
    pub app: Option<ApplicationRef>,
    pub title: Option<String>,
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum Availability {
    Idle,
    Stopped(QueryStopReason),
}
#[derive(Debug, Clone)]
pub(crate) enum InputRow {
    Clock(ClockSegment),
    Foreground(Foreground),
    Availability {
        span: FactSpan,
        state: Availability,
    },
    Gap {
        span: FactSpan,
        reason: QueryGapReason,
    },
}
impl InputRow {
    pub(crate) fn span(&self) -> Option<&FactSpan> {
        match self {
            Self::Clock(_) => None,
            Self::Foreground(row) => Some(&row.span),
            Self::Availability { span, .. } | Self::Gap { span, .. } => Some(span),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueryRange {
    pub start: i64,
    pub end: i64,
}
