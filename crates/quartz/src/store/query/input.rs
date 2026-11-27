//! Snapshot values returned by bounded storage scans.

use quartz_domain::{
    capture::GapReason,
    categories::CategoryId,
    facts::{Availability, ClockSegment, FactId, FactSpan, Foreground, QueryRange},
};
use quartz_protocol::{AnnotationId, AnnotationKind};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WebInterval {
    pub interval_id: FactId,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub domain: String,
}
impl std::fmt::Debug for WebInterval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebInterval")
            .field("interval_id", &self.interval_id)
            .field("start_utc_ms", &self.start_utc_ms)
            .field("end_utc_ms", &self.end_utc_ms)
            .field("domain", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct QueryAnnotation {
    pub id: AnnotationId,
    pub kind: AnnotationKind,
    pub range: QueryRange,
    pub category: Option<CategoryId>,
}
#[derive(Debug, Clone)]
pub(crate) enum InputRow {
    Clock(ClockSegment),
    Foreground(Foreground),
    Availability { span: FactSpan, state: Availability },
    Gap { span: FactSpan, reason: GapReason },
    Annotation(QueryAnnotation),
}
impl InputRow {
    pub(crate) fn span(&self) -> Option<&FactSpan> {
        match self {
            Self::Foreground(row) => Some(&row.span),
            Self::Availability { span, .. } | Self::Gap { span, .. } => Some(span),
            Self::Clock(_) | Self::Annotation(_) => None,
        }
    }
}
