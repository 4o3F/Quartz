//! Bounded two-snapshot previews over one retained SQLite read transaction.

use super::{
    QueryResponse, RunQueryError,
    classification::{Classifier, QueryCategoryRef},
    day_walker::DayWalker,
    input::QueryRange,
    paging::JsonBytes,
    pipeline::{DerivedSegment, Pipeline, QUERY_BATCH_ROWS, SegmentFacts},
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
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Reserve space for the enclosing JSON response envelope (`t`, `id`, `ok`, `body`).
const PREVIEW_ENVELOPE_RESERVE_BYTES: usize = 256;
/// Maximum encoded JSON size of `RulesPreviewResponse` within the 256 KiB frame limit.
const PREVIEW_MAX_RESPONSE_BYTES: usize = wire::MAX_FRAME_LEN - PREVIEW_ENVELOPE_RESERVE_BYTES;

pub(crate) struct PreviewRequest {
    mutation: RuleMutation,
    start_date: wire::LocalDate,
    end_date: wire::LocalDate,
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
    pub(crate) fn decode(body: serde_json::Value) -> Result<Self, ProtocolError> {
        let request: wire::RulesPreviewRequest =
            serde_json::from_value(body).map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        request
            .validate_dates()
            .map_err(|_| ProtocolError::new(ErrorKind::InvalidRange))?;
        Ok(Self {
            mutation: request.mutation.into(),
            start_date: request.start_date,
            end_date: request.end_date,
        })
    }
}
impl From<ProtocolError> for RunQueryError {
    fn from(error: ProtocolError) -> Self {
        Self::RejectQueryError { error }
    }
}

#[derive(Debug, Default)]
struct CategoryTotals {
    active_ms: i64,
    categories: BTreeMap<Option<wire::CategoryId>, wire::PreviewCategoryRank>,
}
impl CategoryTotals {
    fn push(&mut self, segment: &DerivedSegment, facts: &SegmentFacts) {
        if segment.activity.is_active() && segment.span.duration_ms > 0 {
            self.active_ms += segment.span.duration_ms;
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
    fn finish(self) -> wire::RulePreviewSideTotals {
        let mut categories: Vec<_> = self.categories.into_values().collect();
        categories.sort_by(|left, right| {
            right
                .active_ms
                .cmp(&left.active_ms)
                .then(left.category.id().cmp(&right.category.id()))
        });
        wire::RulePreviewSideTotals {
            active_ms: self.active_ms,
            categories,
        }
    }
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
    Complete,
}

#[derive(Debug)]
struct AnalyzeDay {
    before: std::vec::IntoIter<DerivedSegment>,
    after: std::vec::IntoIter<DerivedSegment>,
    before_facts: SegmentFacts,
    after_facts: SegmentFacts,
}

#[derive(Debug)]
struct PreparedPreview {
    calendar: DayWalker,
    current: QueryRange,
    categories: Vec<quartz_domain::categories::Category>,
}

#[derive(Debug)]
pub(crate) struct PreviewJob {
    snapshot: ReadSnapshot,
    before: Arc<RuleSnapshot>,
    after: Arc<RuleSnapshot>,
    kind: domain::RuleKind,
    privacy_actions: BTreeMap<String, domain::PrivacyAction>,
    calendar: DayWalker,
    current: QueryRange,
    categories: Vec<quartz_domain::categories::Category>,
    stage: Stage,
    before_totals: CategoryTotals,
    after_totals: CategoryTotals,
    document_change_count: u64,
    further_hidden_count: u64,
    statistics: BTreeMap<String, wire::RulePreviewStatistics>,
    samples: BTreeMap<SampleOrder, wire::RulePreviewSample>,
    samples_truncated: bool,
}
impl PreviewJob {
    pub(crate) fn start(
        reader: &mut Option<ReadOnlyStore>,
        request: PreviewRequest,
        registry: &RegistryShell,
    ) -> Result<Self, RunQueryError> {
        let before = registry.snapshots().load();
        let mut snapshot = reader
            .take()
            .ok_or(RunQueryError::ReaderUnavailable)?
            .snapshot();
        let result = Self::prepare(&mut snapshot, &request).and_then(|prepared| {
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
            Ok((prepared, after, kind))
        });
        match result {
            Ok((prepared, after, kind)) => {
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
                                matched_count: 0,
                                matched_ms: 0,
                                effective_ms: 0,
                            },
                        );
                    }
                }
                let stage = Stage::Read {
                    before: Pipeline::new(
                        prepared.current,
                        Classifier::new(before.clone(), prepared.categories.clone()),
                    ),
                    after: Pipeline::new(
                        prepared.current,
                        Classifier::new(after.clone(), prepared.categories.clone()),
                    ),
                };
                Ok(Self {
                    snapshot,
                    before,
                    after,
                    kind,
                    privacy_actions,
                    calendar: prepared.calendar,
                    current: prepared.current,
                    categories: prepared.categories,
                    stage,
                    before_totals: CategoryTotals::default(),
                    after_totals: CategoryTotals::default(),
                    document_change_count: 0,
                    further_hidden_count: 0,
                    statistics,
                    samples: BTreeMap::new(),
                    samples_truncated: false,
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
    ) -> Result<PreparedPreview, RunQueryError> {
        snapshot.begin()?;
        let settings = snapshot.settings()?;
        let zone = RunQueryError::system_time_zone(jiff::tz::TimeZone::try_system)?;
        let mut calendar = DayWalker::new(
            request.start_date,
            request.end_date,
            settings.day_start_offset,
            zone,
        )?;
        let current = calendar.next_range().expect("validated preview dates");
        Ok(PreparedPreview {
            calendar,
            current,
            categories: snapshot.categories()?,
        })
    }
    pub(crate) fn advance(&mut self) -> Result<Option<QueryResponse>, RunQueryError> {
        match std::mem::replace(&mut self.stage, Stage::Complete) {
            Stage::Read {
                mut before,
                mut after,
            } => match self.snapshot.read_batch(self.current)? {
                Some(rows) => {
                    for input in rows {
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
                let after_done = after.advance();
                if before_done && after_done {
                    let before = before.into_segments();
                    let after = after.into_segments();
                    self.stage = Stage::Analyze(Box::new(AnalyzeDay {
                        before: before.segments.into_iter(),
                        after: after.segments.into_iter(),
                        before_facts: before.facts,
                        after_facts: after.facts,
                    }));
                } else {
                    self.stage = Stage::Sweep { before, after };
                }
            }
            Stage::Analyze(mut day) => {
                for _ in 0..QUERY_BATCH_ROWS {
                    let Some(after) = day.after.next() else {
                        if day.before.next().is_some() {
                            return Err(RunQueryError::InvalidRuleEntry);
                        }
                        if let Some(range) = self.calendar.next_range() {
                            self.snapshot.restart_scan();
                            self.current = range;
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
                        return Ok(Some(QueryResponse::Preview(self.build_response()?)));
                    };
                    let before = day.before.next().ok_or(RunQueryError::InvalidRuleEntry)?;
                    self.before_totals.push(&before, &day.before_facts);
                    self.after_totals.push(&after, &day.after_facts);
                    if after.activity.is_active()
                        && after.span.duration_ms > 0
                        && day.before_facts.document_identity(&before)
                            != day.after_facts.document_identity(&after)
                    {
                        self.document_change_count += 1;
                    }
                    self.observe(&before, &after, &day.before_facts, &day.after_facts);
                }
                self.stage = Stage::Analyze(day);
            }
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
    ) {
        let Some(row) = after_facts.retained_foreground(after) else {
            return;
        };
        let range = QueryRange {
            start: after.span.start_utc_ms,
            end: after.span.end_utc_ms,
        };
        let duration = row.span.duration_within(range);
        let accepts_input = match self.kind {
            domain::RuleKind::Classification | domain::RuleKind::Privacy => {
                row.is_retained_window()
            }
            domain::RuleKind::Extraction => Classifier::accepts_extraction(row),
        };
        if !accepts_input {
            return;
        }
        let evaluation = match self.kind {
            domain::RuleKind::Classification => {
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
        };
        let after_action = (self.kind == domain::RuleKind::Privacy).then_some(evaluation.privacy);
        let hidden = match after_action {
            Some(action) => HiddenFields::new(row, action),
            None => HiddenFields::default(),
        };
        if self.kind == domain::RuleKind::Privacy && !hidden.fields.is_empty() {
            self.further_hidden_count += 1;
        }
        let mut sample_privacy = None;
        for outcome in evaluation.rules {
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
            };
            let statistics = self
                .statistics
                .get_mut(&outcome.rule_id)
                .expect("evaluation belongs to draft rules");
            statistics.matched_count += 1;
            statistics.matched_ms += duration;
            let active = match self.kind {
                domain::RuleKind::Privacy => after.activity.is_captured_active(),
                _ => after.activity.is_active(),
            };
            if active && duration > 0 && contribution {
                statistics.effective_ms += duration;
            }
            if self.accepts_sample(row, range.start, &outcome.rule_id) {
                let privacy =
                    *sample_privacy.get_or_insert_with(|| self.sample_privacy(row, after_action));
                let before_output = self.output(before, before_facts, privacy.before, privacy.mask);
                let after_output = self.output(after, after_facts, privacy.after, privacy.mask);
                let order = SampleOrder {
                    start: range.start,
                    clock: row.span.segment.0,
                    fact: row.span.id.0,
                    rule: outcome.rule_id.clone(),
                };
                let inserted = self
                    .samples
                    .insert(
                        order,
                        wire::RulePreviewSample {
                            start_utc_ms: range.start,
                            end_utc_ms: range.end,
                            clock_segment_id: wire::ClockSegmentId(row.span.segment.0),
                            rule: outcome.rule_id,
                            before: before_output,
                            after: after_output,
                        },
                    )
                    .is_none();
                if inserted && self.samples.len() > wire::PREVIEW_MAX_SAMPLES {
                    self.samples.pop_last();
                    self.samples_truncated = true;
                }
            } else {
                self.samples_truncated = true;
            }
        }
    }
    fn evaluate_privacy(
        &self,
        row: &Foreground,
        rules: &domain::CompiledRules,
    ) -> domain::Evaluation {
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
        if self.samples.len() < wire::PREVIEW_MAX_SAMPLES {
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
    ) -> wire::RulePreviewOutput {
        match self.kind {
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
        }
    }
    fn build_response(&mut self) -> Result<wire::RulesPreviewResponse, RunQueryError> {
        let response = wire::RulesPreviewResponse {
            rules: std::mem::take(&mut self.statistics).into_values().collect(),
            totals: wire::RulePreviewTotals {
                before: std::mem::take(&mut self.before_totals).finish(),
                after: std::mem::take(&mut self.after_totals).finish(),
            },
            document_change_count: self.document_change_count,
            further_hidden_count: self.further_hidden_count,
            samples: std::mem::take(&mut self.samples).into_values().collect(),
            samples_truncated: self.samples_truncated,
        };
        fit_to_frame(response, PREVIEW_MAX_RESPONSE_BYTES)
    }
    pub(crate) fn finish(self) -> Result<ReadOnlyStore, RunQueryError> {
        Ok(self.snapshot.finish()?)
    }
}

fn fit_to_frame(
    mut response: wire::RulesPreviewResponse,
    max_bytes: usize,
) -> Result<wire::RulesPreviewResponse, RunQueryError> {
    if JsonBytes::measure(&response)? <= max_bytes {
        return Ok(response);
    }
    let samples = std::mem::take(&mut response.samples);
    response.samples_truncated = true;
    let mut used = JsonBytes::measure(&response)?;
    if used <= max_bytes {
        for sample in samples {
            let sample_bytes = JsonBytes::measure(&sample)? + 1;
            if used.saturating_add(sample_bytes) > max_bytes {
                break;
            }
            used += sample_bytes;
            response.samples.push(sample);
        }
    } else {
        let rules = std::mem::take(&mut response.rules);
        let mut used = JsonBytes::measure(&response)?;
        for rule in rules {
            let rule_bytes = JsonBytes::measure(&rule)? + 1;
            if used.saturating_add(rule_bytes) > max_bytes {
                break;
            }
            used += rule_bytes;
            response.rules.push(rule);
        }
    }
    Ok(response)
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
}

#[cfg(test)]
mod tests;
