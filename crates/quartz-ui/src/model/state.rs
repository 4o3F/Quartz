//! Non-foreground states drawn on the category main track (07 section 4.1, 11 section 3.4).

use super::{category::CategoryId, time::TimeSpan};

/// Whether an idle span longer than the away threshold still needs a label (07 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AwayLabel {
    /// Shown in the pending-away row until the user labels or dismisses it.
    Pending,
    /// Already labeled, dismissed, or too short to ask about.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the sample day's only idle span is pending")
    )]
    Settled,
}

/// What kind of non-foreground span a state block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StateKind {
    /// No input; drawn in the color of the preceding activity.
    Idle {
        preceding: Option<CategoryId>,
        away: AwayLabel,
    },
    Locked,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the sample day has no such span")
    )]
    Sleep,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the sample day has no such span")
    )]
    CoreStopped,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the sample day has no such span")
    )]
    NoForeground,
    Excluded,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the sample day has no such span")
    )]
    CollectionGap,
}

/// Whether the clock jumped inside a range (02 section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum ClockAnomaly {
    #[default]
    None,
    Detected,
}

/// A non-foreground span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct StateBlock {
    pub(crate) span: TimeSpan,
    pub(crate) kind: StateKind,
    pub(crate) clock: ClockAnomaly,
}

impl StateBlock {
    /// Whether this is an idle span that still needs a label.
    #[must_use]
    pub(crate) const fn is_pending_away(&self) -> bool {
        matches!(
            self.kind,
            StateKind::Idle {
                away: AwayLabel::Pending,
                ..
            }
        )
    }
}
