//! Duration conservation assertions for capture persistence tests.

use super::{AvailabilityReason, CaptureWrite, MonoInstant, RowId, SegmentId, Timestamp, limits};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy)]
enum Mask {
    Fact,
    Stop,
    Gap,
    Idle,
}
#[derive(Debug)]
struct OpenRow {
    segment: SegmentId,
    start: Timestamp,
    mask: Mask,
}
#[derive(Debug)]
struct Span {
    start: MonoInstant,
    end: MonoInstant,
    mask: Mask,
}
#[derive(Debug)]
struct Segment {
    start: Timestamp,
    end: Option<Timestamp>,
}

/// Replays public write descriptions and checks coverage per segment in monotonic order.
/// Stop masks are unioned; idle is validated independently and never contributes coverage.
pub fn assert_conservation(writes: &[CaptureWrite]) {
    let mut rows = BTreeMap::<RowId, OpenRow>::new();
    let mut spans = BTreeMap::<SegmentId, Vec<Span>>::new();
    let mut segments = BTreeMap::<SegmentId, Segment>::new();
    let mut run = None;
    let mut run_end = None;
    for write in writes {
        match write {
            CaptureWrite::OpenRun { at, .. } => run = Some(*at),
            CaptureWrite::CloseRun { at, .. } => run_end = Some(*at),
            CaptureWrite::OpenClockSegment { segment, at } => {
                assert!(
                    segments
                        .insert(
                            *segment,
                            Segment {
                                start: *at,
                                end: None
                            }
                        )
                        .is_none()
                );
            }
            CaptureWrite::CloseClockSegment { segment, at, .. } => {
                let entry = segments.get_mut(segment).expect("opened segment");
                assert!(entry.end.replace(*at).is_none());
            }
            CaptureWrite::OpenInterval(row) => {
                assert!(
                    rows.insert(
                        row.id,
                        OpenRow {
                            segment: row.segment,
                            start: row.start,
                            mask: Mask::Fact
                        }
                    )
                    .is_none()
                );
            }
            CaptureWrite::OpenAvailability(row) => {
                assert!(
                    rows.insert(
                        row.id,
                        OpenRow {
                            segment: row.segment,
                            start: row.start,
                            mask: if row.reason == AvailabilityReason::Idle {
                                Mask::Idle
                            } else {
                                Mask::Stop
                            },
                        }
                    )
                    .is_none()
                );
            }
            CaptureWrite::CloseInterval { id, at, .. }
            | CaptureWrite::CloseAvailability { id, at, .. } => {
                let OpenRow {
                    segment,
                    start,
                    mask,
                } = rows
                    .remove(id)
                    .expect("close must refer to an open logical row");
                assert!(
                    at.utc >= start.utc && at.mono >= start.mono,
                    "negative duration: {write:?}, {start:?}"
                );
                assert!(
                    at.utc
                        .duration_since(start.utc)
                        .expect("ordered UTC endpoints")
                        <= limits::UTC_DAY,
                    "row exceeded UTC day: {write:?}"
                );
                spans.entry(segment).or_default().push(Span {
                    start: start.mono,
                    end: at.mono,
                    mask,
                });
            }
            CaptureWrite::SettlePersistedRows { segment, at, .. } => {
                let ids: Vec<_> = rows
                    .iter()
                    .filter(|(_, row)| row.segment == *segment)
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    let OpenRow {
                        segment: row_segment,
                        start,
                        mask,
                    } = rows.remove(&id).expect("persisted open row");
                    assert!(at.mono >= start.mono && at.utc >= start.utc);
                    spans.entry(row_segment).or_default().push(Span {
                        start: start.mono,
                        end: at.mono,
                        mask,
                    });
                }
            }
            CaptureWrite::RecordGap(gap) => {
                assert!(
                    gap.range.end.utc >= gap.range.start.utc
                        && gap.range.end.mono >= gap.range.start.mono
                );
                spans.entry(gap.segment).or_default().push(Span {
                    start: gap.range.start.mono,
                    end: gap.range.end.mono,
                    mask: Mask::Gap,
                });
            }
            _ => {}
        }
    }
    assert!(rows.is_empty(), "unsettled rows: {rows:?}");
    let mut previous_end = run.expect("run opened").mono;
    for (segment, Segment { start, end }) in segments {
        let end = end.expect("segment closed");
        assert_eq!(start.mono, previous_end, "segment order {segment:?}");
        assert!(end.mono >= start.mono);
        // A segment without closed rows contributes no spans to the coverage check.
        let spans = spans.remove(&segment).unwrap_or_default();
        let mut cuts = vec![start.mono, end.mono];
        for Span {
            start: left,
            end: right,
            ..
        } in &spans
        {
            assert!(
                *left >= start.mono && *right <= end.mono,
                "span outside {segment:?}"
            );
            cuts.extend([*left, *right]);
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2).filter(|pair| pair[0] < pair[1]) {
            let left = pair[0];
            let right = pair[1];
            let mut facts = 0;
            let mut stops = 0;
            let mut gaps = 0;
            let mut idle = 0;
            for Span { start, end, mask } in &spans {
                if *start <= left && *end >= right {
                    match mask {
                        Mask::Fact => facts += 1,
                        Mask::Stop => stops += 1,
                        Mask::Gap => gaps += 1,
                        Mask::Idle => idle += 1,
                    }
                }
            }
            assert!(idle <= 1, "overlapping idle masks in {segment:?}");
            assert_eq!(
                facts + usize::from(stops > 0) + gaps,
                1,
                "coverage {segment:?} [{left}, {right}): fact={facts}, stop={stops}, gap={gaps}"
            );
        }
        previous_end = end.mono;
    }
    assert_eq!(previous_end, run_end.expect("run closed").mono);
}
