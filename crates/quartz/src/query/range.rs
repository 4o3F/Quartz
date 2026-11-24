//! Bounded day and week folding of the same final active slices used by query.day.

use super::{
    RunQueryError,
    classification::QueryCategoryRef,
    documents::{DocumentIdentity, DocumentReference},
    paging::{PageBudget, PagePosition},
    pipeline::{DerivedSegment, SegmentFacts},
};
use jiff::civil::Weekday;
use quartz_protocol::{
    ApplicationId, CategoryEntity, CategoryId, CategoryRef, FirstDayOfWeekSetting, LocalDate,
    QueryRangeRequest, QueryRangeResponse, QueryVersions, RangeBucket, RangeDimension, RangeEntity,
    RangeGroup, RangeSummaryBucket, ReferenceRange, WorkEntity,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

/// Effective week boundary shared with the existing regional preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FirstWeekday(Weekday);
impl FirstWeekday {
    pub(super) fn resolve(setting: FirstDayOfWeekSetting) -> Result<Self, RunQueryError> {
        Ok(Self(match setting {
            FirstDayOfWeekSetting::Monday => Weekday::Monday,
            FirstDayOfWeekSetting::Tuesday => Weekday::Tuesday,
            FirstDayOfWeekSetting::Wednesday => Weekday::Wednesday,
            FirstDayOfWeekSetting::Thursday => Weekday::Thursday,
            FirstDayOfWeekSetting::Friday => Weekday::Friday,
            FirstDayOfWeekSetting::Saturday => Weekday::Saturday,
            FirstDayOfWeekSetting::Sunday => Weekday::Sunday,
            FirstDayOfWeekSetting::Auto => {
                #[cfg(windows)]
                {
                    let locale = crate::platform::windows::locale::UserLocale::read()
                        .map_err(|_| RunQueryError::ReadFirstWeekdayError)?;
                    match locale.first_day_of_week {
                        quartz_protocol::Weekday::Monday => Weekday::Monday,
                        quartz_protocol::Weekday::Tuesday => Weekday::Tuesday,
                        quartz_protocol::Weekday::Wednesday => Weekday::Wednesday,
                        quartz_protocol::Weekday::Thursday => Weekday::Thursday,
                        quartz_protocol::Weekday::Friday => Weekday::Friday,
                        quartz_protocol::Weekday::Saturday => Weekday::Saturday,
                        quartz_protocol::Weekday::Sunday => Weekday::Sunday,
                    }
                }
                #[cfg(not(windows))]
                return Err(RunQueryError::ReadFirstWeekdayError);
            }
        }))
    }
    pub(super) fn bind_versions(self, versions: &QueryVersions) -> QueryVersions {
        let mut digest = Sha256::new();
        digest.update(b"quartz.range.week.v1");
        digest.update(versions.0.as_bytes());
        digest.update(self.0.to_monday_zero_offset().to_le_bytes());
        QueryVersions(quartz_domain::text::encode_hex(&digest.finalize()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum GroupIdentity {
    Category(Option<CategoryId>),
    App(ApplicationId),
    Document(Arc<DocumentIdentity>),
}
#[derive(Debug)]
enum VisibleEntity {
    Category(CategoryRef),
    App(WorkEntity),
    Document(Arc<DocumentIdentity>),
}
impl VisibleEntity {
    fn into_wire(
        self,
        range: ReferenceRange,
        versions: &QueryVersions,
    ) -> Result<RangeEntity, RunQueryError> {
        Ok(match self {
            Self::Category(category) => {
                RangeEntity::Category(CategoryEntity::Category { category })
            }
            Self::App(app) => RangeEntity::Work(app),
            Self::Document(identity) => RangeEntity::Work(WorkEntity::Document {
                document_ref: DocumentReference::issue(
                    (*identity).clone(),
                    range,
                    versions.clone(),
                )?,
                app: identity.app,
                document: identity.document.clone(),
                project: identity.project.clone(),
                profile: identity.profile.clone(),
            }),
        })
    }
}
#[derive(Debug)]
struct Group {
    identity: GroupIdentity,
    entity: VisibleEntity,
    active_ms: i64,
}
#[derive(Debug)]
struct Bucket {
    start_date: LocalDate,
    end_date: LocalDate,
    active_ms: i64,
    groups: BTreeMap<GroupIdentity, Group>,
}
impl Bucket {
    fn push(
        &mut self,
        segment: &DerivedSegment,
        facts: &SegmentFacts,
        dimensions: &[RangeDimension],
    ) {
        if !segment.activity.is_active() || segment.span.duration_ms == 0 {
            return;
        }
        self.active_ms += segment.span.duration_ms;
        for dimension in dimensions {
            let entity = match dimension {
                RangeDimension::Category => {
                    VisibleEntity::Category(facts.category(segment).clone())
                }
                RangeDimension::App => {
                    let Some(app) = facts.app(segment) else {
                        continue;
                    };
                    VisibleEntity::App(WorkEntity::App {
                        id: ApplicationId(app.id.0),
                        exe_name: app.exe_name.clone(),
                        display_name: app.display_name.clone(),
                    })
                }
                RangeDimension::Document => {
                    let Some(identity) = facts.document_identity(segment) else {
                        continue;
                    };
                    VisibleEntity::Document(Arc::new(identity))
                }
            };
            let identity = match &entity {
                VisibleEntity::Category(category) => GroupIdentity::Category(category.id()),
                VisibleEntity::App(WorkEntity::App { id, .. }) => GroupIdentity::App(*id),
                VisibleEntity::App(_) => unreachable!("application entity"),
                VisibleEntity::Document(identity) => GroupIdentity::Document(identity.clone()),
            };
            self.groups
                .entry(identity.clone())
                .or_insert(Group {
                    identity,
                    entity,
                    active_ms: 0,
                })
                .active_ms += segment.span.duration_ms;
        }
    }
}
impl GroupIdentity {
    fn dimension(&self) -> RangeDimension {
        match self {
            Self::Category(_) => RangeDimension::Category,
            Self::App(_) => RangeDimension::App,
            Self::Document(_) => RangeDimension::Document,
        }
    }
}
#[derive(Debug)]
pub(super) struct RangeAccumulator {
    dimensions: Vec<RangeDimension>,
    buckets: Vec<Bucket>,
    current: usize,
    page: Option<RangePage>,
}
impl RangeAccumulator {
    pub(super) fn new(
        request: &QueryRangeRequest,
        first: Option<FirstWeekday>,
    ) -> Result<Self, RunQueryError> {
        if request.dimensions.is_empty()
            || request
                .dimensions
                .iter()
                .enumerate()
                .any(|(index, dimension)| request.dimensions[..index].contains(dimension))
        {
            return Err(RunQueryError::InvalidRangeRequest);
        }
        let mut buckets = Vec::new();
        let mut date = request.start_date;
        while date < request.end_date {
            let start_date = date;
            date = date
                .tomorrow()
                .map_err(|_| RunQueryError::InvalidRangeRequest)?;
            if request.bucket == RangeBucket::Week {
                let first = first.ok_or(RunQueryError::ReadFirstWeekdayError)?;
                while date < request.end_date && date.weekday() != first.0 {
                    date = date
                        .tomorrow()
                        .map_err(|_| RunQueryError::InvalidRangeRequest)?;
                }
            }
            buckets.push(Bucket {
                start_date,
                end_date: date,
                active_ms: 0,
                groups: BTreeMap::new(),
            });
        }
        Ok(Self {
            dimensions: request.dimensions.clone(),
            buckets,
            current: 0,
            page: None,
        })
    }
    pub(super) fn start_page(
        &mut self,
        response: QueryRangeResponse,
        budget: PageBudget,
        position: PagePosition,
        range: ReferenceRange,
        reference_versions: QueryVersions,
    ) {
        self.page = Some(RangePage {
            response,
            budget,
            position,
            range,
            reference_versions,
            total: 0,
            next: None,
        });
    }
    pub(super) fn next_day(&mut self, date: LocalDate) -> Result<(), RunQueryError> {
        if date >= self.buckets[self.current].end_date {
            self.finish_bucket()?;
            self.current += 1;
        }
        Ok(())
    }
    pub(super) fn page_is_full(&self) -> bool {
        self.page.as_ref().is_some_and(|page| page.next.is_some())
    }
    pub(super) fn push(&mut self, segment: &DerivedSegment, facts: &SegmentFacts) {
        self.buckets[self.current].push(segment, facts, &self.dimensions);
    }
    fn finish_bucket(&mut self) -> Result<(), RunQueryError> {
        let bucket = &mut self.buckets[self.current];
        let groups = std::mem::take(&mut bucket.groups);
        self.page
            .as_mut()
            .expect("range page configured before scanning")
            .push_bucket(bucket, groups, &self.dimensions)
    }
    pub(super) fn finish(mut self) -> Result<RangePageOutput, RunQueryError> {
        while self.current < self.buckets.len() && !self.page_is_full() {
            self.finish_bucket()?;
            self.current += 1;
        }
        let page = self.page.expect("range page configured before scanning");
        if !page.position.is_first() && page.position.index() >= page.total {
            return Err(RunQueryError::InvalidCursor);
        }
        Ok(RangePageOutput {
            response: page.response,
            next: page.next,
        })
    }
}

#[derive(Debug)]
pub(super) struct RangePageOutput {
    pub(super) response: QueryRangeResponse,
    pub(super) next: Option<usize>,
}
/// Retains one encoded page while completed buckets are counted and released.
#[derive(Debug)]
struct RangePage {
    response: QueryRangeResponse,
    budget: PageBudget,
    position: PagePosition,
    range: ReferenceRange,
    reference_versions: QueryVersions,
    total: usize,
    next: Option<usize>,
}
impl RangePage {
    fn push_bucket(
        &mut self,
        bucket: &Bucket,
        groups: BTreeMap<GroupIdentity, Group>,
        dimensions: &[RangeDimension],
    ) -> Result<(), RunQueryError> {
        let header = self.total;
        let mut index = header + 1;
        self.total += 1 + groups.len();
        let start = self.position.index();
        if self.next.is_some() || start >= self.total {
            return Ok(());
        }
        let mut groups: Vec<_> = groups.into_values().collect();
        groups.sort_by(|left, right| {
            let dimension_index = |group: &Group| {
                dimensions
                    .iter()
                    .position(|dimension| *dimension == group.identity.dimension())
                    .expect("requested dimension")
            };
            dimension_index(left)
                .cmp(&dimension_index(right))
                .then_with(|| right.active_ms.cmp(&left.active_ms))
                .then_with(|| left.identity.cmp(&right.identity))
        });
        let mut wire = RangeSummaryBucket {
            start_date: bucket.start_date,
            end_date: bucket.end_date,
            active_ms: (start <= header).then_some(bucket.active_ms),
            groups: Vec::new(),
        };
        if !self.budget.admit(&wire)? {
            if self.response.buckets.is_empty() {
                return Err(RunQueryError::PageTooLarge);
            }
            self.next = Some(header);
            return Ok(());
        }
        for group in groups {
            if index < start {
                index += 1;
                continue;
            }
            let dimension = group.identity.dimension();
            let group = RangeGroup {
                dimension,
                entity: group
                    .entity
                    .into_wire(self.range, &self.reference_versions)?,
                active_ms: group.active_ms,
            };
            if !self.budget.admit(&group)? {
                if wire.active_ms.is_none()
                    && wire.groups.is_empty()
                    && self.response.buckets.is_empty()
                {
                    return Err(RunQueryError::PageTooLarge);
                }
                self.next = Some(index);
                break;
            }
            wire.groups.push(group);
            index += 1;
        }
        self.response.buckets.push(wire);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> QueryRangeRequest {
        QueryRangeRequest {
            start_date: "2026-11-04".parse().expect("Wednesday"),
            end_date: "2026-11-17".parse().expect("Tuesday"),
            bucket: RangeBucket::Week,
            dimensions: vec![RangeDimension::Category],
            cursor: None,
        }
    }
    #[test]
    fn weeks_use_sunday_and_clip_both_edges_including_empty_buckets() {
        let request = request();
        let mut accumulator =
            RangeAccumulator::new(&request, Some(FirstWeekday(Weekday::Sunday))).expect("buckets");
        let starts: Vec<_> = accumulator
            .buckets
            .iter()
            .map(|bucket| bucket.start_date.to_string())
            .collect();
        let ends: Vec<_> = accumulator
            .buckets
            .iter()
            .map(|bucket| bucket.end_date.to_string())
            .collect();
        assert_eq!(starts, ["2026-11-04", "2026-11-08", "2026-11-15"]);
        assert_eq!(ends, ["2026-11-08", "2026-11-15", "2026-11-17"]);
        let response = QueryRangeResponse {
            time_zone: "UTC".into(),
            day_start_offset_min: 0,
            reference_range: None,
            buckets: Vec::new(),
            versions: QueryVersions("test".into()),
            next_cursor: None,
        };
        let budget = PageBudget::new(&response).expect("budget");
        accumulator.start_page(
            response,
            budget,
            PagePosition::First,
            ReferenceRange {
                start_utc_ms: 0,
                end_utc_ms: 1,
            },
            QueryVersions("test".into()),
        );
        let output = accumulator.finish().expect("empty page");
        assert!(output.next.is_none());
        let response = output.response;
        assert_eq!(response.buckets.len(), 3);
        assert!(
            response
                .buckets
                .iter()
                .all(|bucket| bucket.active_ms == Some(0) && bucket.groups.is_empty())
        );
    }
    #[test]
    fn first_weekday_changes_the_week_digest_and_dimensions_are_distinct() {
        let versions = QueryVersions("base".into());
        assert_ne!(
            FirstWeekday(Weekday::Monday).bind_versions(&versions),
            FirstWeekday(Weekday::Sunday).bind_versions(&versions)
        );
        let mut request = request();
        for dimensions in [vec![], vec![RangeDimension::App, RangeDimension::App]] {
            request.dimensions = dimensions;
            assert_eq!(
                RangeAccumulator::new(&request, Some(FirstWeekday(Weekday::Sunday)))
                    .expect_err("invalid dimensions")
                    .kind(),
                quartz_protocol::ErrorKind::BadRequest
            );
        }
    }
    #[test]
    fn completed_bucket_identities_are_released_during_a_year_walk() {
        const DOCUMENTS_PER_BUCKET: usize = 100;
        let mut request = request();
        request.start_date = "2025-01-01".parse().expect("start");
        request.end_date = "2026-01-01".parse().expect("end");
        request.bucket = RangeBucket::Day;
        request.dimensions = vec![RangeDimension::Document];
        let mut accumulator = RangeAccumulator::new(&request, None).expect("range");
        let response = QueryRangeResponse {
            time_zone: "UTC".into(),
            day_start_offset_min: 0,
            reference_range: None,
            buckets: Vec::new(),
            versions: QueryVersions("test".into()),
            next_cursor: None,
        };
        let budget = PageBudget::new(&response).expect("budget");
        accumulator.start_page(
            response,
            budget,
            PagePosition::First,
            ReferenceRange {
                start_utc_ms: 0,
                end_utc_ms: 1,
            },
            QueryVersions("test".into()),
        );
        for current in 0..accumulator.buckets.len() {
            let date = accumulator.buckets[current].start_date;
            let first = Arc::new(DocumentIdentity {
                app: ApplicationId(1),
                document: format!("{date}/first"),
                project: None,
                profile: None,
            });
            let witness = Arc::downgrade(&first);
            for index in 0..DOCUMENTS_PER_BUCKET {
                let identity = if index == 0 {
                    first.clone()
                } else {
                    Arc::new(DocumentIdentity {
                        app: ApplicationId(1),
                        document: format!("{date}/{index}"),
                        project: None,
                        profile: None,
                    })
                };
                let key = GroupIdentity::Document(identity.clone());
                accumulator.buckets[current].groups.insert(
                    key.clone(),
                    Group {
                        identity: key,
                        entity: VisibleEntity::Document(identity),
                        active_ms: 1,
                    },
                );
            }
            drop(first);
            accumulator.buckets[current].active_ms =
                i64::try_from(DOCUMENTS_PER_BUCKET).expect("duration");
            if current + 1 < accumulator.buckets.len() {
                let next = accumulator.buckets[current + 1].start_date;
                accumulator.next_day(next).expect("flush bucket");
                assert!(
                    witness.upgrade().is_none(),
                    "completed bucket releases visible identities"
                );
                assert!(
                    accumulator.buckets[..=current]
                        .iter()
                        .all(|bucket| bucket.groups.is_empty())
                );
            }
        }
        let output = accumulator.finish().expect("page");
        assert!(output.next.is_some());
        assert!(
            serde_json::to_vec(&output.response)
                .expect("page JSON")
                .len()
                <= quartz_protocol::QUERY_PAGE_BYTES
        );
    }
}
