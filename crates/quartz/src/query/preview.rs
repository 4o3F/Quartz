//! Resumable two-snapshot previews over one retained SQLite read transaction.

use super::{
    QueryResponse, RunQueryError,
    calendar::DayCalendar,
    classification::{Classifier, QueryCategoryRef},
    control::QueryControl,
    day_walker::DayWalker,
    documents::{DocumentDuration, DocumentIdentity},
    input::QueryRange,
    paging::PagePosition,
    pipeline::{DayAccumulator, DerivedSegment, Pipeline, QUERY_BATCH_ROWS, SegmentFacts},
    versions::VersionState,
};
use crate::{
    rules::{registry::RegistryShell, write::RuleMutation},
    store::{ReadOnlyStore, query::ReadSnapshot},
};
use quartz_domain::{
    facts::{CaptureLevel, Foreground},
    rules::{self as domain, registry::RuleSnapshot},
};
use quartz_protocol::{self as wire, ErrorKind, ProtocolError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[cfg(test)]
#[derive(Debug, Default, Clone, Copy)]
struct PreviewWork {
    classification_fields: usize,
    privacy_evaluations: usize,
    sample_outputs: usize,
}
#[cfg(test)]
std::thread_local! {static PREVIEW_WORK: std::cell::Cell<PreviewWork> = const {std::cell::Cell::new(PreviewWork {classification_fields: 0, privacy_evaluations: 0, sample_outputs: 0})};}
mod page;
use page::{PageProgress, PageSources, PreviewPage, PreviewReferences};

/// Builds both date ranges used by preview statistics and document references.
#[derive(Debug)]
struct PreviewCalendar {
    calendar: DayWalker,
    reference_range: QueryRange,
}
impl PreviewCalendar {
    fn new(
        start_date: wire::LocalDate,
        end_date: wire::LocalDate,
        offset: super::calendar::DayStartOffset,
        zone: jiff::tz::TimeZone,
    ) -> Result<Self, RunQueryError> {
        let calendar = DayWalker::new(start_date, end_date, offset, zone.clone())?;
        let reference_range =
            super::details::EntityCalendar::in_zone(end_date.yesterday()?, offset, zone)?.range;
        Ok(Self {
            calendar,
            reference_range,
        })
    }
}

pub(crate) struct PreviewRequest {
    mutation: RuleMutation,
    start_date: wire::LocalDate,
    end_date: wire::LocalDate,
    expected_versions: wire::QueryVersions,
    parameters: wire::CursorQuery,
    cursor: Option<wire::QueryCursor>,
    sample_limit: usize,
    pub(crate) control: QueryControl,
}
impl std::fmt::Debug for PreviewRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreviewRequest")
            .field("start_date", &self.start_date)
            .field("end_date", &self.end_date)
            .finish_non_exhaustive()
    }
}
impl PreviewRequest {
    pub(crate) fn decode(op: wire::Op, body: serde_json::Value) -> Result<Self, ProtocolError> {
        match op {
            wire::Op::RulesPreview => Self::from_request(
                serde_json::from_value::<wire::RulesPreviewRequest>(body)
                    .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?,
            ),
            wire::Op::RulesTextPreview => Self::from_request(
                serde_json::from_value::<wire::RulesTextPreviewRequest>(body)
                    .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?,
            ),
            _ => Err(ProtocolError::new(ErrorKind::BadRequest)),
        }
    }
    fn from_request<M: Serialize + Into<RuleMutation>>(
        request: wire::RulePreviewRequest<M>,
    ) -> Result<Self, ProtocolError>
    where
        wire::RulePreviewRequest<M>: PreviewParameters,
    {
        request
            .validate_dates()
            .map_err(|_| ProtocolError::new(ErrorKind::InvalidRange))?;
        let digest = quartz_domain::text::encode_hex(&Sha256::digest(
            serde_json::to_vec(&request.mutation)
                .map_err(|_| ProtocolError::new(ErrorKind::Internal))?,
        ));
        let parameters = request.parameters(digest);
        if let Some(cursor) = &request.cursor {
            cursor
                .validate_query(&parameters)
                .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        }
        Ok(Self {
            control: QueryControl::with_deadline(request.deadline_ms.unwrap_or_default().get()),
            sample_limit: usize::try_from(request.sample_limit.unwrap_or_default().get())
                .expect("sample limit fits usize"),
            mutation: request.mutation.into(),
            start_date: request.start_date,
            end_date: request.end_date,
            expected_versions: request.expected_versions,
            parameters,
            cursor: request.cursor,
        })
    }
}
trait PreviewParameters {
    fn parameters(&self, digest: String) -> wire::CursorQuery;
}
impl PreviewParameters for wire::RulesPreviewRequest {
    fn parameters(&self, digest: String) -> wire::CursorQuery {
        self.cursor_parameters(digest)
    }
}
impl PreviewParameters for wire::RulesTextPreviewRequest {
    fn parameters(&self, digest: String) -> wire::CursorQuery {
        self.cursor_parameters(digest)
    }
}
impl From<ProtocolError> for RunQueryError {
    fn from(error: ProtocolError) -> Self {
        Self::RejectQueryError { error }
    }
}

#[derive(Debug, Default)]
struct Totals {
    active_ms: i64,
    categories: BTreeMap<Option<wire::CategoryId>, wire::PreviewCategoryRank>,
    applications: BTreeMap<wire::ApplicationId, wire::ApplicationRank>,
    documents: BTreeMap<DocumentIdentity, i64>,
}
impl Totals {
    fn empty(active_ms: Option<i64>) -> wire::RulePreviewSideTotals {
        wire::RulePreviewSideTotals {
            active_ms,
            categories: Vec::new(),
            applications: Vec::new(),
            documents: Vec::new(),
        }
    }
    fn category(&mut self, segment: &DerivedSegment, facts: &SegmentFacts) {
        if segment.activity.is_active() && segment.span.duration_ms > 0 {
            let category = facts.category(segment);
            self.categories
                .entry(category.id())
                .or_insert_with(|| wire::PreviewCategoryRank {
                    category: category.clone(),
                    active_ms: 0,
                })
                .active_ms += segment.span.duration_ms;
        }
    }
    fn day(&mut self, day: super::pipeline::DayAggregation) {
        self.active_ms += day.metrics.active_ms;
        for row in day.ranking {
            self.applications
                .entry(row.app.id)
                .or_insert_with(|| wire::ApplicationRank {
                    app: row.app.clone(),
                    active_ms: 0,
                })
                .active_ms += row.active_ms;
        }
        for row in day.documents {
            *self.documents.entry(row.identity).or_default() += row.active_ms;
        }
    }
    fn finish(
        self,
        position: PagePosition,
        control: &QueryControl,
    ) -> Result<UnencodedTotals, RunQueryError> {
        let first = position.is_first();
        control.check()?;
        let mut categories: Vec<_> = self.categories.into_values().collect();
        categories.sort_by(|left, right| {
            right
                .active_ms
                .cmp(&left.active_ms)
                .then(left.category.id().cmp(&right.category.id()))
        });
        let mut applications: Vec<_> = self.applications.into_values().collect();
        applications.sort_by(|left, right| {
            right
                .active_ms
                .cmp(&left.active_ms)
                .then(left.app.id.cmp(&right.app.id))
        });
        let mut documents: Vec<_> = self
            .documents
            .into_iter()
            .map(|(identity, active_ms)| DocumentDuration {
                identity,
                active_ms,
            })
            .collect();
        documents.sort_by(|left, right| {
            right
                .active_ms
                .cmp(&left.active_ms)
                .then(left.identity.cmp(&right.identity))
        });
        Ok(UnencodedTotals {
            active_ms: first.then_some(self.active_ms),
            categories,
            applications,
            documents,
        })
    }
}
#[derive(Debug)]
struct UnencodedTotals {
    active_ms: Option<i64>,
    categories: Vec<wire::PreviewCategoryRank>,
    applications: Vec<wire::ApplicationRank>,
    documents: Vec<DocumentDuration>,
}
#[derive(Debug, Clone, Copy)]
struct SamplePrivacy {
    before: domain::PrivacyAction,
    after: domain::PrivacyAction,
    mask: domain::PrivacyAction,
}
impl SamplePrivacy {
    fn new(row: &Foreground, before: domain::PrivacyAction, after: domain::PrivacyAction) -> Self {
        let retained = HiddenFields::retention(row);
        Self {
            before: retained.merge(before),
            after: retained.merge(after),
            mask: retained.merge(before).merge(after),
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SampleOrder {
    start: i64,
    clock: i64,
    fact: i64,
    rule: String,
}
#[derive(Debug)]
enum Stage {
    Read { before: Pipeline, after: Pipeline },
    Sweep { before: Pipeline, after: Pipeline },
    Analyze(Box<AnalyzeDay>),
    Page(Box<PreviewPage>),
    Complete,
}
#[derive(Debug)]
struct AnalyzeDay {
    before: std::vec::IntoIter<DerivedSegment>,
    after: std::vec::IntoIter<DerivedSegment>,
    before_facts: SegmentFacts,
    after_facts: SegmentFacts,
    before_day: DayAccumulator,
    after_day: DayAccumulator,
}
#[derive(Debug)]
pub(crate) struct PreviewJob {
    snapshot: ReadSnapshot,
    before: Arc<RuleSnapshot>,
    after: Arc<RuleSnapshot>,
    kind: domain::RuleKind,
    privacy_actions: BTreeMap<String, domain::PrivacyAction>,
    control: QueryControl,
    parameters: wire::CursorQuery,
    position: PagePosition,
    versions: wire::QueryVersions,
    before_versions: wire::QueryVersions,
    after_versions: wire::QueryVersions,
    calendar: DayWalker,
    reference_range: QueryRange,
    current: DayCalendar,
    categories: Vec<quartz_domain::categories::Category>,
    stage: Stage,
    before_totals: Totals,
    after_totals: Totals,
    statistics: BTreeMap<String, wire::RulePreviewStatistics>,
    hidden: wire::FurtherHidden,
    samples: BTreeMap<SampleOrder, wire::RulePreviewSample>,
    sample_limit: usize,
}
impl PreviewJob {
    pub(crate) fn start(
        reader: &mut Option<ReadOnlyStore>,
        request: PreviewRequest,
        registry: &RegistryShell,
    ) -> Result<Self, RunQueryError> {
        request.control.check()?;
        let before = registry.snapshots().load();
        let mut snapshot = reader
            .take()
            .ok_or(RunQueryError::ReaderUnavailable)?
            .snapshot();
        let result = Self::prepare(&mut snapshot, &request, &before).and_then(|mut prepared| {
            request.control.check()?;
            let key: domain::RuleSetKey = request
                .mutation
                .target_key()?
                .parse()
                .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
            let after = request.mutation.draft(registry)?;
            let kind = after
                .state()
                .sets
                .get(&key)
                .ok_or_else(|| ProtocolError::new(ErrorKind::Internal))?
                .current
                .set
                .header
                .kind;
            request.control.check()?;
            prepared.after_versions = VersionState {
                rule_hashes: after.hashes().clone(),
                ..prepared.state.clone()
            }
            .digest(prepared.reference_range)?;
            Ok(PreparedDraft {
                prepared,
                after,
                kind,
            })
        });
        match result {
            Ok(PreparedDraft {
                prepared,
                after,
                kind,
            }) => {
                let mut statistics = BTreeMap::new();
                let mut privacy_actions = BTreeMap::new();
                if let Some(resolution) = after.resolution(kind) {
                    for rule in &resolution.effective.rules {
                        let id = rule.explanation_id();
                        if let domain::RuleBody::Privacy { action, when, .. } = &rule.rule.body {
                            privacy_actions.insert(id.clone(), action.for_conditions(when));
                        }

                        statistics.insert(
                            id.clone(),
                            wire::RulePreviewStatistics {
                                rule: id,
                                condition_match: Default::default(),
                                effective_contribution: Default::default(),
                                further_hidden: (kind == domain::RuleKind::Privacy)
                                    .then(Default::default),
                            },
                        );
                    }
                }
                let stage = Stage::Read {
                    before: Pipeline::new(
                        prepared.current.range,
                        Classifier::new(before.clone(), prepared.categories.clone()),
                    ),
                    after: Pipeline::new(
                        prepared.current.range,
                        Classifier::new(after.clone(), prepared.categories.clone()),
                    ),
                };
                Ok(Self {
                    snapshot,
                    before,
                    after,
                    kind,
                    privacy_actions,
                    control: request.control,
                    parameters: request.parameters,
                    position: prepared.position,
                    versions: prepared.versions,
                    before_versions: prepared.before_versions,
                    after_versions: prepared.after_versions,
                    calendar: prepared.calendar,
                    reference_range: prepared.reference_range,
                    current: prepared.current,
                    categories: prepared.categories,
                    stage,
                    before_totals: Totals::default(),
                    after_totals: Totals::default(),
                    statistics,
                    hidden: Default::default(),
                    samples: BTreeMap::new(),
                    sample_limit: request.sample_limit,
                })
            }
            Err(error) => {
                *reader = Some(
                    snapshot
                        .finish()
                        .map_err(|source| RunQueryError::Cleanup { source })?,
                );
                Err(error)
            }
        }
    }
    fn prepare(
        snapshot: &mut ReadSnapshot,
        request: &PreviewRequest,
        before: &RuleSnapshot,
    ) -> Result<PreparedPreview, RunQueryError> {
        snapshot.begin()?;
        let settings = snapshot.settings()?;
        let zone = RunQueryError::system_time_zone(jiff::tz::TimeZone::try_system)?;
        let PreviewCalendar {
            mut calendar,
            reference_range,
        } = PreviewCalendar::new(
            request.start_date,
            request.end_date,
            settings.day_start_offset,
            zone.clone(),
        )?;
        let state = VersionState {
            days: snapshot
                .versions(reference_range)?
                .into_iter()
                .map(|row| (row.key, row.version))
                .collect(),
            settings,
            rule_hashes: before.hashes().clone(),
            time_zone: zone.clone(),
        };
        let input_versions = state.digest(calendar.range)?;
        let before_versions = state.digest(reference_range)?;
        // Include every revision, including an edit whose effective contents are unchanged.
        let mut digest = Sha256::new();
        digest.update(input_versions.0.as_bytes());
        digest.update(before_versions.0.as_bytes());
        for (key, entry) in &before.state().sets {
            let length = u64::try_from(key.as_str().len())
                .map_err(|_| ProtocolError::new(ErrorKind::Internal))?;
            digest.update(length.to_le_bytes());
            digest.update(key.as_str().as_bytes());
            digest.update(entry.current.version.to_le_bytes());
        }
        let versions = wire::QueryVersions(quartz_domain::text::encode_hex(&digest.finalize()));
        if request.expected_versions != input_versions {
            return Err(ProtocolError::new(if request.cursor.is_some() {
                ErrorKind::StaleCursor
            } else {
                ErrorKind::StaleInputs {
                    current_versions: input_versions,
                }
            })
            .into());
        }
        let position =
            PagePosition::from_cursor(request.cursor.as_ref(), &versions, &request.parameters)?;
        let after_versions = before_versions.clone();
        let range = calendar.next_range().expect("validated preview dates");
        let current = DayCalendar {
            range,
            hours: Vec::new(),
        };
        Ok(PreparedPreview {
            state,
            position,
            versions,
            before_versions,
            after_versions,
            calendar,
            reference_range,
            current,
            categories: snapshot.categories()?,
        })
    }
    pub(crate) fn advance(&mut self) -> Result<Option<QueryResponse>, RunQueryError> {
        self.control.check()?;
        match std::mem::replace(&mut self.stage, Stage::Complete) {
            Stage::Read {
                mut before,
                mut after,
            } => match self.snapshot.read_batch(self.current.range)? {
                Some(rows) => {
                    for input in rows {
                        self.control.check()?;
                        let row = before.classified(input.clone());
                        before.ingest_classified(row)?;
                        let row = after.classified(input);
                        after.ingest_classified(row)?;
                    }
                    self.stage = Stage::Read { before, after };
                }
                None => {
                    before.finish_input();
                    after.finish_input();
                    self.stage = Stage::Sweep { before, after };
                }
            },
            Stage::Sweep {
                mut before,
                mut after,
            } => {
                let before_done = before.advance();
                self.control.check()?;
                let after_done = after.advance();
                if before_done && after_done {
                    let before = before.into_segments();
                    let after = after.into_segments();
                    self.stage = Stage::Analyze(Box::new(AnalyzeDay {
                        before: before.segments.into_iter(),
                        after: after.segments.into_iter(),
                        before_facts: before.facts,
                        after_facts: after.facts,
                        before_day: DayAccumulator::new(self.current.clone()),
                        after_day: DayAccumulator::new(self.current.clone()),
                    }));
                } else {
                    self.stage = Stage::Sweep { before, after };
                }
            }
            Stage::Analyze(mut day) => {
                for _ in 0..QUERY_BATCH_ROWS {
                    self.control.check()?;
                    let Some(after) = day.after.next() else {
                        if day.before.next().is_some() {
                            return Err(RunQueryError::InvalidRuleEntry);
                        }
                        self.before_totals.day(day.before_day.finish());
                        self.after_totals.day(day.after_day.finish());
                        if let Some(range) = self.calendar.next_range() {
                            self.snapshot.restart_scan();
                            self.current = DayCalendar {
                                range,
                                hours: Vec::new(),
                            };
                            self.stage = Stage::Read {
                                before: Pipeline::new(
                                    range,
                                    Classifier::new(self.before.clone(), self.categories.clone()),
                                ),
                                after: Pipeline::new(
                                    range,
                                    Classifier::new(self.after.clone(), self.categories.clone()),
                                ),
                            };
                            return Ok(None);
                        }
                        self.stage = Stage::Page(Box::new(self.prepare_page()?));
                        return Ok(None);
                    };
                    let before = day.before.next().ok_or(RunQueryError::InvalidRuleEntry)?;
                    day.before_day.push(&before, &day.before_facts);
                    day.after_day.push(&after, &day.after_facts);
                    self.before_totals.category(&before, &day.before_facts);
                    self.after_totals.category(&after, &day.after_facts);
                    self.observe(&before, &after, &day.before_facts, &day.after_facts)?;
                }
                self.stage = Stage::Analyze(day);
            }
            Stage::Page(mut page) => match page.advance(&self.control)? {
                PageProgress::More => self.stage = Stage::Page(page),
                PageProgress::Complete => {
                    self.control.check()?;
                    return Ok(Some(QueryResponse::Preview(page.finish())));
                }
            },
            Stage::Complete => return Err(RunQueryError::InvalidRuleEntry),
        }
        Ok(None)
    }
    fn observe(
        &mut self,
        before: &DerivedSegment,
        after: &DerivedSegment,
        before_facts: &SegmentFacts,
        after_facts: &SegmentFacts,
    ) -> Result<(), RunQueryError> {
        let Some(row) = after_facts.retained_foreground(after) else {
            return Ok(());
        };
        let range = QueryRange {
            start: after.span.start_utc_ms,
            end: after.span.end_utc_ms,
        };
        let duration = row.span.duration_within(range);
        let accepts_input = match self.kind {
            domain::RuleKind::Classification => row.is_retained_window(),
            domain::RuleKind::Extraction => Classifier::accepts_extraction(row),
            domain::RuleKind::Privacy => row.is_retained_window(),
            _ => return Err(RunQueryError::InvalidRuleEntry),
        };
        if !accepts_input {
            return Ok(());
        }
        let evaluation = match self.kind {
            domain::RuleKind::Classification => {
                #[cfg(test)]
                PREVIEW_WORK.update(|mut work| {
                    work.classification_fields += 1;
                    work
                });
                let fields = after_facts.preview_fields(after, self.after.classification());
                self.after.classification().evaluate(&fields)
            }
            domain::RuleKind::Extraction => {
                self.after
                    .extraction()
                    .evaluate(&domain::FieldSnapshot::from_foreground(
                        row,
                        self.after
                            .extraction()
                            .fields()
                            .chain([domain::Field::RawTitle, domain::Field::DisplayName]),
                        None,
                    ))
            }
            domain::RuleKind::Privacy => self.evaluate_privacy(row, self.after.privacy()),
            _ => return Err(RunQueryError::InvalidRuleEntry),
        };
        let after_action = (self.kind == domain::RuleKind::Privacy).then_some(evaluation.privacy);
        let hidden = match after_action {
            Some(action) => HiddenFields::new(row, action),
            None => HiddenFields::default(),
        };
        if self.kind == domain::RuleKind::Privacy {
            hidden.accumulate(&mut self.hidden, duration);
        }
        let mut sample_privacy = None;
        for outcome in evaluation.rules {
            self.control.check()?;
            if outcome.status == domain::RuleStatus::DidNotMatch {
                continue;
            }
            let participating = if self.kind == domain::RuleKind::Privacy {
                self.privacy_participation(&outcome.rule_id, &hidden, evaluation.privacy)
            } else {
                HiddenFields::default()
            };
            let contribution = match self.kind {
                domain::RuleKind::Classification => {
                    after_facts.provides_category(after, &outcome.rule_id)
                }
                domain::RuleKind::Extraction => {
                    outcome.status == domain::RuleStatus::Selected
                        && after_facts.app(after).is_some()
                }
                domain::RuleKind::Privacy => !participating.fields.is_empty(),
                _ => false,
            };
            let statistics = self
                .statistics
                .get_mut(&outcome.rule_id)
                .expect("evaluation belongs to draft rules");
            statistics.condition_match.segments += 1;
            statistics.condition_match.duration_ms += duration;
            let active = match self.kind {
                domain::RuleKind::Privacy => after.activity.is_captured_active(),
                _ => after.activity.is_active(),
            };
            if active && duration > 0 && contribution {
                statistics.effective_contribution.segments += 1;
                statistics.effective_contribution.active_ms += duration;
            }
            if let Some(total) = &mut statistics.further_hidden
                && contribution
            {
                participating.accumulate(total, duration);
            }
            if self.accepts_sample(row, range.start, &outcome.rule_id) {
                let privacy =
                    *sample_privacy.get_or_insert_with(|| self.sample_privacy(row, after_action));
                let before_output =
                    self.output(before, before_facts, privacy.before, privacy.mask)?;
                let after_output = self.output(after, after_facts, privacy.after, privacy.mask)?;
                let order = SampleOrder {
                    start: range.start,
                    clock: row.span.segment.0,
                    fact: row.span.id.0,
                    rule: outcome.rule_id.clone(),
                };
                self.samples.insert(
                    order,
                    wire::RulePreviewSample {
                        start_utc_ms: range.start,
                        end_utc_ms: range.end,
                        clock_segment_id: wire::ClockSegmentId(row.span.segment.0),
                        rule: outcome.rule_id,
                        before: before_output,
                        after: after_output,
                    },
                );
                if self.samples.len() > self.sample_limit {
                    self.samples.pop_last();
                }
            }
        }
        Ok(())
    }
    fn evaluate_privacy(
        &self,
        row: &Foreground,
        rules: &domain::CompiledRules,
    ) -> domain::Evaluation {
        #[cfg(test)]
        PREVIEW_WORK.update(|mut work| {
            work.privacy_evaluations += 1;
            work
        });
        let fields = domain::FieldSnapshot::from_foreground(row, rules.fields(), None);
        rules.evaluate(&fields)
    }
    fn sample_privacy(
        &self,
        row: &Foreground,
        after_action: Option<domain::PrivacyAction>,
    ) -> SamplePrivacy {
        let current = self.evaluate_privacy(row, self.before.privacy()).privacy;
        let draft = after_action.unwrap_or(current);
        SamplePrivacy::new(row, current, draft)
    }
    fn accepts_sample(&self, row: &Foreground, start: i64, rule: &str) -> bool {
        if self.sample_limit == 0 {
            return false;
        }
        if self.samples.len() < self.sample_limit {
            return true;
        }
        self.samples.last_key_value().is_some_and(|(last, _)| {
            last.start
                .cmp(&start)
                .then(last.clock.cmp(&row.span.segment.0))
                .then(last.fact.cmp(&row.span.id.0))
                .then(last.rule.as_str().cmp(rule))
                .is_gt()
        })
    }
    fn privacy_participation(
        &self,
        id: &str,
        hidden: &HiddenFields,
        final_action: domain::PrivacyAction,
    ) -> HiddenFields {
        let action = self
            .privacy_actions
            .get(id)
            .expect("privacy outcome belongs to draft");
        HiddenFields {
            fields: hidden
                .fields
                .iter()
                .copied()
                .filter(|field| match field {
                    HiddenField::App => action.app == final_action.app,
                    HiddenField::Title => action.window_title == final_action.window_title,
                })
                .collect(),
        }
    }
    fn output(
        &self,
        segment: &DerivedSegment,
        facts: &SegmentFacts,
        privacy: domain::PrivacyAction,
        mask: domain::PrivacyAction,
    ) -> Result<wire::RulePreviewOutput, RunQueryError> {
        #[cfg(test)]
        PREVIEW_WORK.update(|mut work| {
            work.sample_outputs += 1;
            work
        });
        Ok(match self.kind {
            domain::RuleKind::Classification => wire::RulePreviewOutput::Classification {
                category: if mask.app == domain::Action::Drop {
                    wire::CategoryRef::Unclassified
                } else {
                    facts.category(segment).clone()
                },
            },
            domain::RuleKind::Extraction => wire::RulePreviewOutput::Extraction {
                extraction: if mask.app == domain::Action::Drop
                    || mask.window_title == domain::Action::Drop
                {
                    Default::default()
                } else {
                    facts.extraction(segment)
                },
            },
            domain::RuleKind::Privacy => wire::RulePreviewOutput::Privacy {
                app: match privacy.app {
                    domain::Action::Keep => wire::RuleAction::Keep,
                    domain::Action::Drop => wire::RuleAction::Drop,
                },
                window_title: match privacy.window_title {
                    domain::Action::Keep => wire::RuleAction::Keep,
                    domain::Action::Drop => wire::RuleAction::Drop,
                },
                web: match privacy.web {
                    domain::WebAction::Full => wire::RuleWebAction::Full,
                    domain::WebAction::DomainOnly => wire::RuleWebAction::DomainOnly,
                    domain::WebAction::None => wire::RuleWebAction::None,
                },
            },
            domain::RuleKind::Precedent | domain::RuleKind::PolicyCheck => {
                return Err(RunQueryError::InvalidRuleEntry);
            }
        })
    }
    fn prepare_page(&mut self) -> Result<PreviewPage, RunQueryError> {
        let before =
            std::mem::take(&mut self.before_totals).finish(self.position, &self.control)?;
        let after = std::mem::take(&mut self.after_totals).finish(self.position, &self.control)?;
        let response = wire::RulesPreviewResponse {
            rules: Vec::new(),
            totals: Some(wire::RulePreviewTotals {
                before: Totals::empty(before.active_ms),
                after: Totals::empty(after.active_ms),
            }),
            further_hidden: (self.position.is_first() && self.kind == domain::RuleKind::Privacy)
                .then(|| self.hidden.clone()),
            samples: Vec::new(),
            versions: self.versions.clone(),
            next_cursor: None,
        };
        PreviewPage::new(
            response,
            self.parameters.clone(),
            self.position,
            PreviewReferences {
                range: self.reference_range,
                before: self.before_versions.clone(),
                after: self.after_versions.clone(),
            },
            PageSources {
                before,
                after,
                statistics: std::mem::take(&mut self.statistics),
                samples: std::mem::take(&mut self.samples),
            },
        )
    }
    pub(crate) fn finish(self) -> Result<ReadOnlyStore, RunQueryError> {
        Ok(self.snapshot.finish()?)
    }
}
#[derive(Debug)]
struct PreparedDraft {
    prepared: PreparedPreview,
    after: Arc<RuleSnapshot>,
    kind: domain::RuleKind,
}
#[derive(Debug)]
struct PreparedPreview {
    state: VersionState,
    position: PagePosition,
    versions: wire::QueryVersions,
    before_versions: wire::QueryVersions,
    after_versions: wire::QueryVersions,
    calendar: DayWalker,
    reference_range: QueryRange,
    current: DayCalendar,
    categories: Vec<quartz_domain::categories::Category>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HiddenField {
    App,
    Title,
}
#[derive(Debug, Default)]
struct HiddenFields {
    fields: BTreeSet<HiddenField>,
}
impl HiddenFields {
    fn retention(row: &Foreground) -> domain::PrivacyAction {
        let mut action = domain::PrivacyAction::default();
        match row.capture_level() {
            CaptureLevel::Full => {}
            CaptureLevel::TitleHidden => action.window_title = domain::Action::Drop,
            CaptureLevel::Excluded => action.app = domain::Action::Drop,
        }
        domain::PrivacyAction::default().merge(action)
    }
    fn new(row: &Foreground, action: domain::PrivacyAction) -> Self {
        let mut fields = BTreeSet::new();
        if row.visible_app().is_some() && action.app == domain::Action::Drop {
            fields.insert(HiddenField::App);
        }
        if row.capture_level() == CaptureLevel::Full
            && row.visible_title().is_some()
            && action.window_title == domain::Action::Drop
        {
            fields.insert(HiddenField::Title);
        }
        Self { fields }
    }
    fn accumulate(&self, total: &mut wire::FurtherHidden, duration: i64) {
        if !self.fields.is_empty() {
            total.segments += 1;
            total.fields += u64::try_from(self.fields.len()).expect("field count fits u64");
            total.duration_ms += duration;
        }
    }
}

#[cfg(test)]
mod tests;
