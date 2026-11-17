//! Foreground segments: one application in front for a span of time.

use std::{num::NonZeroU32, time::Duration};

use super::{application::AppId, category::CategoryId, rule::RuleId, time::TimeSpan};

/// Why a segment has its category (07 section 4.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ClassificationSource {
    /// A matched rule and the value its field had.
    Rule { rule: RuleId, actual: String },
    /// A label the user set.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "manual labels arrive with the protocol mapping; the sample day has none"
        )
    )]
    Manual,
    /// No rule matched; the segment is unclassified.
    Unmatched,
}

/// Identifies a foreground segment within one day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SegmentId(u32);

impl SegmentId {
    /// Wraps a segment id from the data source.
    #[must_use]
    pub(crate) const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The raw id, for element ids.
    #[must_use]
    pub(crate) const fn get(self) -> u32 {
        self.0
    }
}

/// One foreground fact: an application in front for a span of time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Segment {
    pub(crate) id: SegmentId,
    pub(crate) span: TimeSpan,
    pub(crate) app: AppId,
    pub(crate) category: CategoryId,
    pub(crate) title: String,
    pub(crate) domain: Option<String>,
    pub(crate) document: Option<String>,
    pub(crate) source: ClassificationSource,
    /// Raw intervals merged into this segment for display (07 section 4.2).
    pub(crate) merged: NonZeroU32,
}

impl Segment {
    /// Length of the segment.
    #[must_use]
    pub(crate) fn duration(&self) -> Duration {
        self.span.duration()
    }
}
