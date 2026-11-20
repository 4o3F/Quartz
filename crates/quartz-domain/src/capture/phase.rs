//! Capture phases and the data valid during each phase.

use super::{
    CaptureState, CapturedFields, GapReason, IntervalFact, NoForegroundReason, OpenInterval, RowId,
    SegmentId, SessionKey, SessionReadiness, StopReason, Timestamp, WindowId,
};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionOrigin {
    Startup,
    Retry,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResumeTarget {
    Review,
    Gap(GapReason),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GapSpan {
    pub since: Timestamp,
    pub reason: GapReason,
}
#[derive(Debug, Clone)]
pub(super) struct IntervalTiming {
    id: RowId,
    segment: SegmentId,
    session: SessionKey,
    start: Timestamp,
}
#[derive(Debug, Clone)]
pub(super) struct WindowTiming {
    timing: IntervalTiming,
    instance: Option<WindowId>,
    fields: CapturedFields,
}
#[derive(Debug, Clone)]
pub(super) struct EmptyTiming {
    timing: IntervalTiming,
    reason: NoForegroundReason,
}
#[derive(Debug, Clone)]
pub(super) enum ActiveInterval {
    Window(WindowTiming),
    Empty(EmptyTiming),
}
impl ActiveInterval {
    fn from_row(row: OpenInterval) -> Self {
        let timing = IntervalTiming {
            id: row.id,
            segment: row.segment,
            session: row.session,
            start: row.start,
        };
        match row.fact {
            IntervalFact::Window { instance, fields } => Self::Window(WindowTiming {
                timing,
                instance,
                fields,
            }),
            IntervalFact::NoForeground(reason) => Self::Empty(EmptyTiming { timing, reason }),
        }
    }
    fn row(&self) -> OpenInterval {
        let (timing, fact) = match self {
            Self::Window(row) => (
                &row.timing,
                IntervalFact::Window {
                    instance: row.instance,
                    fields: row.fields.clone(),
                },
            ),
            Self::Empty(row) => (&row.timing, IntervalFact::NoForeground(row.reason)),
        };
        OpenInterval {
            id: timing.id,
            segment: timing.segment,
            session: timing.session,
            start: timing.start,
            fact,
        }
    }
}
#[derive(Debug, Clone)]
pub(super) enum ReviewCoverage {
    Uncovered,
    Waiting(Timestamp),
    Open(ActiveInterval),
}
#[derive(Debug, Clone)]
pub(super) enum ForegroundCoverage {
    Unrecorded,
    Open(EmptyTiming),
}
#[derive(Debug, Clone)]
pub(super) struct StopReasons {
    first: StopReason,
    others: BTreeSet<StopReason>,
}
impl StopReasons {
    fn from_set(mut reasons: BTreeSet<StopReason>) -> Option<Self> {
        reasons.pop_first().map(|first| Self {
            first,
            others: reasons,
        })
    }
    fn values(&self) -> BTreeSet<StopReason> {
        let mut reasons = self.others.clone();
        reasons.insert(self.first);
        reasons
    }
}

#[derive(Debug, Clone)]
pub(super) enum Phase {
    AwaitingSession {
        origin: SessionOrigin,
        reasons: BTreeSet<StopReason>,
        resume: ResumeTarget,
    },
    AwaitingReview(ReviewCoverage),
    Tracking(WindowTiming),
    Stopped {
        reasons: StopReasons,
        resume: ResumeTarget,
    },
    NoForeground(ForegroundCoverage),
    Gap(GapSpan),
    Ended,
}
impl Phase {
    pub(super) fn startup() -> Self {
        Self::AwaitingSession {
            origin: SessionOrigin::Startup,
            reasons: BTreeSet::new(),
            resume: ResumeTarget::Review,
        }
    }
    pub(super) fn ready(&self) -> bool {
        !matches!(self, Self::AwaitingSession { .. } | Self::Ended)
    }
    pub(super) fn initialized(&self) -> bool {
        !matches!(
            self,
            Self::AwaitingSession {
                origin: SessionOrigin::Startup,
                ..
            }
        )
    }
    pub(super) fn ended(&self) -> bool {
        matches!(self, Self::Ended)
    }
    pub(super) fn needs_review(&self) -> bool {
        matches!(
            self,
            Self::AwaitingSession { .. }
                | Self::AwaitingReview(_)
                | Self::Stopped { .. }
                | Self::Gap(_)
        )
    }
    pub(super) fn reasons(&self) -> BTreeSet<StopReason> {
        match self {
            Self::AwaitingSession { reasons, .. } => reasons.clone(),
            Self::Stopped { reasons, .. } => reasons.values(),
            _ => BTreeSet::new(),
        }
    }
    pub(super) fn resume_target(&self) -> ResumeTarget {
        match self {
            Self::AwaitingSession { resume, .. } | Self::Stopped { resume, .. } => *resume,
            Self::Gap(gap) => ResumeTarget::Gap(gap.reason),
            _ => ResumeTarget::Review,
        }
    }
    pub(super) fn interval(&self) -> Option<OpenInterval> {
        match self {
            Self::Tracking(row) => Some(ActiveInterval::Window(row.clone()).row()),
            Self::NoForeground(ForegroundCoverage::Open(row)) => {
                Some(ActiveInterval::Empty(row.clone()).row())
            }
            Self::AwaitingReview(ReviewCoverage::Open(row)) => Some(row.row()),
            _ => None,
        }
    }
    pub(super) fn take_interval(&mut self) -> Option<OpenInterval> {
        let row = self.interval()?;
        *self = if self.needs_review() {
            Self::AwaitingReview(ReviewCoverage::Uncovered)
        } else {
            Self::NoForeground(ForegroundCoverage::Unrecorded)
        };
        Some(row)
    }
    pub(super) fn open_interval(&mut self, row: OpenInterval) {
        let active = ActiveInterval::from_row(row);
        *self = if matches!(self, Self::AwaitingReview(_)) {
            Self::AwaitingReview(ReviewCoverage::Open(active))
        } else {
            match active {
                ActiveInterval::Window(row) => Self::Tracking(row),
                ActiveInterval::Empty(row) => Self::NoForeground(ForegroundCoverage::Open(row)),
            }
        };
    }
    pub(super) fn request_review(&mut self) {
        if !self.ready()
            || !self.reasons().is_empty()
            || matches!(self, Self::Gap(_) | Self::AwaitingReview(_))
        {
            return;
        }
        *self = Self::AwaitingReview(match self.interval() {
            Some(row) => ReviewCoverage::Open(ActiveInterval::from_row(row)),
            None => ReviewCoverage::Uncovered,
        });
    }
    pub(super) fn reviewed(&mut self) {
        if let Self::AwaitingReview(coverage) = self {
            *self = match coverage {
                ReviewCoverage::Open(ActiveInterval::Window(row)) => Self::Tracking(row.clone()),
                ReviewCoverage::Open(ActiveInterval::Empty(row)) => {
                    Self::NoForeground(ForegroundCoverage::Open(row.clone()))
                }
                _ => Self::NoForeground(ForegroundCoverage::Unrecorded),
            };
        }
    }
    pub(super) fn waiting(&self) -> Option<Timestamp> {
        match self {
            Self::AwaitingReview(ReviewCoverage::Waiting(at)) => Some(*at),
            _ => None,
        }
    }
    pub(super) fn wait(&mut self, at: Timestamp) {
        if self.waiting().is_none() {
            *self = Self::AwaitingReview(ReviewCoverage::Waiting(at));
        }
    }
    pub(super) fn take_waiting(&mut self) -> Option<Timestamp> {
        let at = self.waiting()?;
        *self = Self::AwaitingReview(ReviewCoverage::Uncovered);
        Some(at)
    }
    pub(super) fn gap(&self) -> Option<GapSpan> {
        match self {
            Self::Gap(gap) => Some(*gap),
            _ => None,
        }
    }
    pub(super) fn take_gap(&mut self) -> Option<GapSpan> {
        let gap = self.gap()?;
        *self = Self::AwaitingReview(ReviewCoverage::Uncovered);
        Some(gap)
    }
    pub(super) fn stop(&mut self, reasons: BTreeSet<StopReason>, resume: ResumeTarget) {
        if let Self::AwaitingSession {
            reasons: current,
            resume: target,
            ..
        } = self
        {
            *current = reasons;
            *target = resume;
            return;
        }
        *self = match StopReasons::from_set(reasons) {
            Some(reasons) => Self::Stopped { reasons, resume },
            None => Self::AwaitingReview(ReviewCoverage::Uncovered),
        };
    }
    pub(super) fn session_ready(&mut self) {
        if let Self::AwaitingSession {
            reasons, resume, ..
        } = self
        {
            *self = match StopReasons::from_set(reasons.clone()) {
                Some(reasons) => Self::Stopped {
                    reasons,
                    resume: *resume,
                },
                None => Self::AwaitingReview(ReviewCoverage::Uncovered),
            };
        }
    }
    pub(super) fn session_unavailable(&mut self) {
        *self = Self::AwaitingSession {
            origin: if self.initialized() {
                SessionOrigin::Retry
            } else {
                SessionOrigin::Startup
            },
            reasons: self.reasons(),
            resume: self.resume_target(),
        };
    }
    pub(super) fn mark_gap(&mut self, span: GapSpan) {
        *self = Self::Gap(span);
    }
    pub(super) fn state(&self) -> CaptureState {
        match self {
            Self::Ended => CaptureState::Ended,
            Self::AwaitingSession { reasons, .. } => CaptureState::Stopped {
                reasons: reasons.clone(),
                session: SessionReadiness::AwaitingSession,
            },
            Self::Stopped { reasons, .. } => CaptureState::Stopped {
                reasons: reasons.values(),
                session: SessionReadiness::Ready,
            },
            Self::Gap(gap) => CaptureState::Gap(gap.reason),
            _ => match self.interval().map(|row| row.fact) {
                Some(fact @ IntervalFact::Window { .. }) => CaptureState::Tracking(fact),
                Some(IntervalFact::NoForeground(reason)) => CaptureState::NoForeground(reason),
                None => CaptureState::NoForeground(NoForegroundReason::Empty),
            },
        }
    }
}
