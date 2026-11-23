//! Daily folding of final unclassified active slices into visible work groups.

use super::{
    RunQueryError,
    documents::{DocumentIdentity, DocumentReference},
    pipeline::{DerivedSegment, SegmentFacts},
};
use quartz_domain::text::FoldedKey;
use quartz_protocol::{
    ApplicationId, CategoryRef, QueryUnclassifiedRequest, QueryVersions, ReferenceRange,
    UNCLASSIFIED_MAX_SAMPLES, UnclassifiedGroupBy, UnclassifiedItem, UnclassifiedSampleRange,
    WorkEntity,
};
use std::collections::BTreeMap;

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
