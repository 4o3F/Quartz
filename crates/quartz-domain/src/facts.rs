//! Protocol-independent persisted fact values and UTC query ranges.

use crate::capture::{GapReason, StopReason};

/// Persistent application identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApplicationId(pub i64);
/// Persistent clock-segment identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClockSegmentId(pub i64);
/// Application metadata retained by a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationRef {
    pub id: ApplicationId,
    pub identity_key: String,
    pub exe_name: Option<String>,
    pub display_name: String,
    pub aumid: Option<String>,
}

/// Persistent title dictionary identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TitleId(pub i64);
/// Persistent window-instance identity, independent of capture's per-run window sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowInstanceId(pub i64);

/// Persistent row identity, independent of capture's per-run row sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
/// Window, application, and title values supplied by the persistence reader.
#[derive(Debug, Clone, Default)]
pub struct ForegroundContent {
    pub window_instance: Option<WindowInstanceId>,
    pub window_class: Option<String>,
    pub app: Option<std::sync::Arc<ApplicationRef>>,
    pub title: Option<String>,
    pub title_id: Option<TitleId>,
}
/// A foreground fact whose visible fields obey its persisted capture level.
#[derive(Debug, Clone)]
pub struct Foreground {
    pub span: FactSpan,
    pub kind: ForegroundKind,
    level: CaptureLevel,
    content: ForegroundContent,
}
impl Foreground {
    /// Projects privacy once when a stored fact is read: excluded rows lose the application, and only full rows retain title text and identity.
    pub fn new(
        span: FactSpan,
        kind: ForegroundKind,
        level: CaptureLevel,
        mut content: ForegroundContent,
    ) -> Self {
        if level == CaptureLevel::Excluded {
            content.app = None;
        }
        if level == CaptureLevel::Excluded || matches!(kind, ForegroundKind::NoForeground) {
            content.window_instance = None;
            content.window_class = None;
        }
        if level != CaptureLevel::Full {
            content.title = None;
            content.title_id = None;
        }
        Self {
            span,
            kind,
            level,
            content,
        }
    }
    /// Whether this fact retains foreground window fields for rule evaluation.
    pub fn is_retained_window(&self) -> bool {
        self.level != CaptureLevel::Excluded && matches!(self.kind, ForegroundKind::Window)
    }
    /// Returns the persisted capture level for activity and cache identity.
    pub fn capture_level(&self) -> CaptureLevel {
        self.level
    }
    /// Returns application metadata after the read-time privacy projection.
    pub fn visible_app(&self) -> Option<&std::sync::Arc<ApplicationRef>> {
        self.content.app.as_ref()
    }
    /// Returns the persisted window identity after the read-time privacy projection.
    pub fn visible_window_instance(&self) -> Option<WindowInstanceId> {
        self.content.window_instance
    }
    /// Returns the stored window class after the read-time privacy projection.
    pub fn visible_window_class(&self) -> Option<&str> {
        self.content.window_class.as_deref()
    }
    /// Returns title text after the read-time privacy projection.
    pub fn visible_title(&self) -> Option<&str> {
        self.content.title.as_deref()
    }
    /// Returns the title identity used by the per-query extraction and classification caches.
    pub fn visible_title_id(&self) -> Option<TitleId> {
        self.content.title_id
    }
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
