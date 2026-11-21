//! Snapshot values returned by bounded storage scans.

use quartz_domain::{
    capture::GapReason,
    categories::CategoryId,
    facts::{Availability, ClockSegment, FactSpan, Foreground, QueryRange},
};
use quartz_protocol::{AnnotationId, AnnotationKind};

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
