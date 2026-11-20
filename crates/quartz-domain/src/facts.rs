//! Protocol-independent persisted fact values and UTC query ranges.

use crate::capture::{GapReason, StopReason};

/// Persistent application identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ApplicationId(pub i64);
/// Persistent clock-segment identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockSegmentId(pub i64);
/// Application metadata retained by a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationRef {
    pub id: ApplicationId,
    pub exe_name: Option<String>,
    pub display_name: String,
    pub aumid: Option<String>,
}

/// Persistent row identity, independent of capture's per-run row sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactId(pub i64);
pub use crate::capture::CaptureLevel;
#[derive(Debug, Clone)]
pub struct ClockSegment {
    pub id: ClockSegmentId,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub start_mono_ms: i64,
    pub end_mono_ms: i64,
}
#[derive(Debug, Clone)]
pub struct FactSpan {
    pub id: FactId,
    pub segment: ClockSegmentId,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub start_mono_ms: i64,
    pub end_mono_ms: i64,
}
/// UTC clipping uses the row's own monotonic endpoints within its clock segment.
#[derive(Debug, Clone, Copy)]
pub struct SpanDuration {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub duration_ms: i64,
}
impl SpanDuration {
    pub fn within(self, range: QueryRange) -> i64 {
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
    pub fn duration_within(&self, range: QueryRange) -> i64 {
        SpanDuration {
            start_utc_ms: self.start_utc_ms,
            end_utc_ms: self.end_utc_ms,
            duration_ms: self.end_mono_ms - self.start_mono_ms,
        }
        .within(range)
    }
}
impl ClockSegment {
    pub fn duration_within(&self, range: QueryRange) -> i64 {
        SpanDuration {
            start_utc_ms: self.start_utc_ms,
            end_utc_ms: self.end_utc_ms,
            duration_ms: self.end_mono_ms - self.start_mono_ms,
        }
        .within(range)
    }
}
#[derive(Debug, Clone)]
pub enum ForegroundKind {
    Window,
    NoForeground,
}
#[derive(Debug, Clone)]
pub struct Foreground {
    pub span: FactSpan,
    pub kind: ForegroundKind,
    pub level: CaptureLevel,
    pub app: Option<ApplicationRef>,
    pub title: Option<String>,
}
#[derive(Debug, Clone, Copy)]
pub enum Availability {
    Idle,
    Stopped(StopReason),
}
#[derive(Debug, Clone)]
pub enum InputRow {
    Clock(ClockSegment),
    Foreground(Foreground),
    Availability { span: FactSpan, state: Availability },
    Gap { span: FactSpan, reason: GapReason },
}
impl InputRow {
    pub fn span(&self) -> Option<&FactSpan> {
        match self {
            Self::Clock(_) => None,
            Self::Foreground(row) => Some(&row.span),
            Self::Availability { span, .. } | Self::Gap { span, .. } => Some(span),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryRange {
    pub start: i64,
    pub end: i64,
}
