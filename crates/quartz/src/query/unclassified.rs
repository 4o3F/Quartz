//! Daily folding of final unclassified active slices into visible work groups.

use super::{
    RunQueryError,
    calendar::{DayCalendar, DayStartOffset},
    documents::{DocumentIdentity, DocumentReference},
    input::QueryRange,
    pipeline::{DerivedSegment, SegmentFacts},
};
use jiff::tz::TimeZone;
use quartz_domain::text::FoldedKey;
use quartz_protocol::{
    ApplicationId, CategoryRef, QueryUnclassifiedRequest, QueryVersions, ReferenceRange,
    UNCLASSIFIED_MAX_DAYS, UNCLASSIFIED_MAX_SAMPLES, UNCLASSIFIED_MAX_SEARCH_SCALARS,
    UnclassifiedGroupBy, UnclassifiedItem, UnclassifiedSampleRange, WorkEntity,
};
use std::collections::{BTreeMap, VecDeque};

/// Retains only day boundaries while the snapshot scans one day at a time.
#[derive(Debug)]
pub(super) struct UnclassifiedCalendar {
    pub(super) range: QueryRange,
    pub(super) time_zone: String,
    pub(super) offset: DayStartOffset,
    days: VecDeque<QueryRange>,
}
impl UnclassifiedCalendar {
    pub(super) fn new(
        request: &QueryUnclassifiedRequest,
        offset: DayStartOffset,
        zone: TimeZone,
    ) -> Result<Self, RunQueryError> {
        if request.start_date >= request.end_date
            || request
                .search
                .as_ref()
                .is_some_and(|search| search.chars().count() > UNCLASSIFIED_MAX_SEARCH_SCALARS)
        {
            return Err(RunQueryError::InvalidUnclassifiedRequest);
        }
        let mut date = request.start_date;
        let mut days = VecDeque::new();
        while date < request.end_date {
            if days.len() == UNCLASSIFIED_MAX_DAYS {
                return Err(RunQueryError::InvalidUnclassifiedRequest);
            }
            days.push_back(
                DayCalendar::in_zone(date, offset, zone.clone())
                    .map_err(|_| RunQueryError::InvalidUnclassifiedRequest)?
                    .range,
            );
            date = date
                .tomorrow()
                .map_err(|_| RunQueryError::InvalidUnclassifiedRequest)?;
        }
        let range = QueryRange {
            start: days.front().expect("validated nonempty dates").start,
            end: days.back().expect("validated nonempty dates").end,
        };
        Ok(Self {
            range,
            time_zone: jiff::fmt::temporal::DateTimePrinter::new()
                .time_zone_to_string(&zone)
                .map_err(|source| RunQueryError::Calendar { source })?,
            offset,
            days,
        })
    }
    pub(super) fn next_range(&mut self) -> Option<QueryRange> {
        self.days.pop_front()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum GroupIdentity {
    App(ApplicationId),
    Document(DocumentIdentity),
}
#[derive(Debug)]
enum VisibleEntity {
    App(WorkEntity),
    Document(DocumentIdentity),
}
impl VisibleEntity {
    fn into_wire(
        self,
        range: ReferenceRange,
        versions: &QueryVersions,
    ) -> Result<WorkEntity, RunQueryError> {
        Ok(match self {
            Self::App(entity) => entity,
            Self::Document(identity) => WorkEntity::Document {
                document_ref: DocumentReference::issue(identity.clone(), range, versions.clone())?,
                app: identity.app,
                document: identity.document,
                project: identity.project,
                profile: identity.profile,
            },
        })
    }
}
#[derive(Debug)]
pub(super) struct UnclassifiedGroup {
    identity: GroupIdentity,
    entity: VisibleEntity,
    active_ms: i64,
    samples: Vec<UnclassifiedSampleRange>,
}
impl UnclassifiedGroup {
    fn push(&mut self, segment: &DerivedSegment) {
        self.active_ms += segment.span.duration_ms;
        let sample = UnclassifiedSampleRange {
            start_utc_ms: segment.span.start_utc_ms,
            end_utc_ms: segment.span.end_utc_ms,
            clock_segment_id: segment
                .clock_segment_id
                .map(|id| quartz_protocol::ClockSegmentId(id.0)),
        };
        // Clock corrections can revisit earlier UTC times; keep the earliest five globally.
        let index = self.samples.partition_point(|existing| existing <= &sample);
        if index < UNCLASSIFIED_MAX_SAMPLES {
            self.samples.insert(index, sample);
            self.samples.truncate(UNCLASSIFIED_MAX_SAMPLES);
        }
    }
    pub(super) fn into_item(
        self,
        range: ReferenceRange,
        versions: &QueryVersions,
    ) -> Result<UnclassifiedItem, RunQueryError> {
        Ok(UnclassifiedItem {
            entity: self.entity.into_wire(range, versions)?,
            active_ms: self.active_ms,
            sample_ranges: self.samples,
        })
    }
}
#[derive(Debug)]
pub(super) struct UnclassifiedAccumulator {
    group_by: UnclassifiedGroupBy,
    search: Option<FoldedKey>,
    groups: BTreeMap<GroupIdentity, UnclassifiedGroup>,
}
impl UnclassifiedAccumulator {
    pub(super) fn new(request: &QueryUnclassifiedRequest) -> Self {
        Self {
            group_by: request.group_by,
            search: request.search.as_deref().map(FoldedKey::new),
            groups: BTreeMap::new(),
        }
    }
    fn matches(&self, text: &str) -> bool {
        self.search
            .as_ref()
            .is_none_or(|search| FoldedKey::new(text).contains(search))
    }
    pub(super) fn push(&mut self, segment: &DerivedSegment, facts: &SegmentFacts) {
        if !segment.activity.is_active()
            || segment.span.duration_ms == 0
            || *facts.category(segment) != CategoryRef::Unclassified
        {
            return;
        }
        let Some(app) = facts.app(segment) else {
            return;
        };
        let entity = match self.group_by {
            UnclassifiedGroupBy::App => VisibleEntity::App(WorkEntity::App {
                id: ApplicationId(app.id.0),
                exe_name: app.exe_name.clone(),
                display_name: app.display_name.clone(),
            }),
            UnclassifiedGroupBy::Document => {
                let Some(identity) = facts.document_identity(segment) else {
                    return;
                };
                VisibleEntity::Document(identity)
            }
        };
        let identity = match &entity {
            VisibleEntity::App(_) => GroupIdentity::App(ApplicationId(app.id.0)),
            VisibleEntity::Document(identity) => GroupIdentity::Document(identity.clone()),
        };
        if let Some(group) = self.groups.get_mut(&identity) {
            group.push(segment);
            return;
        }
        let app_matches = self.matches(&app.display_name)
            || app
                .exe_name
                .as_deref()
                .is_some_and(|name| self.matches(name));
        let document_matches = match &entity {
            VisibleEntity::Document(identity) => {
                self.matches(&identity.document)
                    || identity
                        .project
                        .as_deref()
                        .is_some_and(|name| self.matches(name))
                    || identity
                        .profile
                        .as_deref()
                        .is_some_and(|name| self.matches(name))
            }
            VisibleEntity::App(_) => false,
        };
        if !app_matches && !document_matches {
            return;
        }
        let group = self
            .groups
            .entry(identity.clone())
            .or_insert(UnclassifiedGroup {
                identity,
                entity,
                active_ms: 0,
                samples: Vec::with_capacity(UNCLASSIFIED_MAX_SAMPLES),
            });
        group.push(segment);
    }
    pub(super) fn finish(self) -> Vec<UnclassifiedGroup> {
        let mut groups: Vec<_> = self.groups.into_values().collect();
        groups.sort_by(|left, right| {
            right
                .active_ms
                .cmp(&left.active_ms)
                .then_with(|| left.identity.cmp(&right.identity))
        });
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::UnclassifiedGroupBy;

    fn request(start: &str, end: &str) -> QueryUnclassifiedRequest {
        QueryUnclassifiedRequest {
            start_date: start.parse().expect("start date"),
            end_date: end.parse().expect("end date"),
            group_by: UnclassifiedGroupBy::App,
            search: None,
            cursor: None,
        }
    }
    #[test]
    fn calendar_accepts_one_to_366_logical_days_and_rejects_search_over_256_scalars() {
        let offset = DayStartOffset::MIDNIGHT;
        for range in [
            request("2024-01-01", "2024-01-02"),
            request("2024-01-01", "2025-01-01"),
        ] {
            let calendar =
                UnclassifiedCalendar::new(&range, offset, TimeZone::UTC).expect("bounded dates");
            assert!((1..=UNCLASSIFIED_MAX_DAYS).contains(&calendar.days.len()));
        }
        for range in [
            request("2024-01-01", "2024-01-01"),
            request("2024-01-02", "2024-01-01"),
            request("2024-01-01", "2025-01-02"),
        ] {
            assert_eq!(
                UnclassifiedCalendar::new(&range, offset, TimeZone::UTC)
                    .expect_err("invalid dates")
                    .kind(),
                quartz_protocol::ErrorKind::BadRequest
            );
        }
        let mut range = request("2024-01-01", "2024-01-02");
        range.search = Some("界".repeat(UNCLASSIFIED_MAX_SEARCH_SCALARS));
        assert!(UnclassifiedCalendar::new(&range, offset, TimeZone::UTC).is_ok());
        range.search.as_mut().expect("search").push('界');
        assert_eq!(
            UnclassifiedCalendar::new(&range, offset, TimeZone::UTC)
                .expect_err("scalar bound")
                .kind(),
            quartz_protocol::ErrorKind::BadRequest
        );
    }
    #[test]
    fn calendar_follows_dst_and_day_start_offsets_without_24_hour_assumptions() {
        struct Case {
            start: &'static str,
            end: &'static str,
            offset: i64,
            hours: i64,
        }
        for case in [
            Case {
                start: "2026-03-08",
                end: "2026-03-09",
                offset: 0,
                hours: 23,
            },
            Case {
                start: "2026-11-01",
                end: "2026-11-02",
                offset: 0,
                hours: 25,
            },
            Case {
                start: "2026-03-07",
                end: "2026-03-09",
                offset: 240,
                hours: 47,
            },
            Case {
                start: "2026-10-31",
                end: "2026-11-02",
                offset: 240,
                hours: 49,
            },
        ] {
            let zone = TimeZone::get("America/Los_Angeles").expect("zone");
            let mut calendar = UnclassifiedCalendar::new(
                &request(case.start, case.end),
                DayStartOffset::try_from(case.offset).expect("offset"),
                zone.clone(),
            )
            .expect("calendar");
            assert_eq!(calendar.time_zone, "America/Los_Angeles");
            assert_eq!(
                calendar.range.end - calendar.range.start,
                case.hours * 3_600_000
            );
            let mut boundary = calendar.range.start;
            while let Some(day) = calendar.next_range() {
                assert_eq!(day.start, boundary);
                let local_start = jiff::Timestamp::from_millisecond(day.start)
                    .expect("timestamp")
                    .to_zoned_datetime(zone.clone());
                assert_eq!(
                    i64::from(local_start.hour()) * 60 + i64::from(local_start.minute()),
                    case.offset
                );
                boundary = day.end;
            }
            assert_eq!(boundary, calendar.range.end);
        }
    }
}
