//! The category main track and the application swimlanes of the timeline (07 section 4).

use std::{collections::BTreeMap, time::Duration};

use super::{
    application::AppId, category::CategoryId, day::Day, segment::SegmentId, state::StateBlock,
    time::TimeSpan,
};

/// One entry of the category main track after merging same-category neighbors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MainBlock {
    Active {
        span: TimeSpan,
        category: CategoryId,
    },
    State(StateBlock),
}

impl MainBlock {
    /// Time covered by the block.
    #[must_use]
    pub(crate) const fn span(&self) -> TimeSpan {
        match self {
            Self::Active { span, .. } => *span,
            Self::State(block) => block.span,
        }
    }
}

/// One application swimlane.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Lane {
    pub(crate) app: AppId,
    /// The application's segments in time order.
    pub(crate) segments: Vec<SegmentId>,
}

impl Day {
    pub(super) fn compute_main_track(&self) -> Vec<MainBlock> {
        let mut items: Vec<MainBlock> = self
            .segments
            .iter()
            .map(|segment| MainBlock::Active {
                span: segment.span,
                category: segment.category,
            })
            .chain(self.states.iter().copied().map(MainBlock::State))
            .collect();
        items.sort_by_key(|item| item.span().start());
        let mut merged: Vec<MainBlock> = Vec::with_capacity(items.len());
        for item in items {
            if let (
                Some(MainBlock::Active {
                    span: last_span,
                    category: last_category,
                }),
                MainBlock::Active { span, category },
            ) = (merged.last_mut(), item)
                && *last_category == category
                && last_span.end() == span.start()
            {
                *last_span = last_span.extended_to(span.end());
                continue;
            }
            merged.push(item);
        }
        merged
    }

    pub(super) fn compute_lanes(&self) -> Vec<Lane> {
        let mut totals: BTreeMap<AppId, Duration> = BTreeMap::new();
        for segment in &self.segments {
            *totals.entry(segment.app).or_default() += segment.duration();
        }
        let mut apps: Vec<(AppId, Duration)> = totals.into_iter().collect();
        apps.sort_by_key(|(_, total)| std::cmp::Reverse(*total));
        apps.into_iter()
            .map(|(app, _)| Lane {
                app,
                segments: self
                    .segments
                    .iter()
                    .filter(|segment| segment.app == app)
                    .map(|segment| segment.id)
                    .collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        StateKind,
        fixture::{BROWSER, EDITOR, PLAY, WORK, day, segment, span, state},
    };

    #[test]
    fn main_track_merges_touching_blocks_of_one_category_only() {
        let day = day(
            vec![
                segment(1, EDITOR, WORK, (9, 0), (9, 30)),
                segment(2, BROWSER, WORK, (9, 30), (10, 0)),
                segment(3, EDITOR, WORK, (10, 10), (10, 20)),
                segment(4, BROWSER, PLAY, (10, 20), (10, 30)),
            ],
            vec![state(StateKind::Locked, (10, 0), (10, 10))],
        );
        let spans: Vec<TimeSpan> = day.main_track().iter().map(MainBlock::span).collect();
        assert_eq!(
            spans,
            [
                span((9, 0), (10, 0)),
                span((10, 0), (10, 10)),
                span((10, 10), (10, 20)),
                span((10, 20), (10, 30)),
            ]
        );
    }

    #[test]
    fn lanes_put_the_longest_application_first() {
        let day = day(
            vec![
                segment(1, EDITOR, WORK, (9, 0), (9, 10)),
                segment(2, BROWSER, PLAY, (9, 10), (10, 0)),
                segment(3, EDITOR, WORK, (10, 0), (10, 10)),
            ],
            vec![],
        );
        let lanes = day.lanes();
        assert_eq!(
            lanes.iter().map(|lane| lane.app).collect::<Vec<_>>(),
            [BROWSER, EDITOR]
        );
        let editor = lanes.get(1).expect("editor lane");
        let ids: Vec<SegmentId> = day
            .lane_segments(editor)
            .map(|segment| segment.id)
            .collect();
        assert_eq!(ids, [SegmentId::new(1), SegmentId::new(3)]);
    }
}
