//! Retained-fact explanations over the same boundary sweep used by query details.

use super::{
    QueryResponse, RunQueryError,
    classification::Classifier,
    control::QueryControl,
    input::QueryRange,
    paging::PagePosition,
    pipeline::{CategorySource, DerivedSegment, Pipeline, QUERY_BATCH_ROWS, SegmentFacts},
    versions::VersionState,
};
use crate::store::{ReadOnlyStore, query::ReadSnapshot};
use quartz_domain::rules::{self as domain, registry::RuleSnapshot};
use quartz_protocol::{self as wire, ErrorKind, ProtocolError};
use sha2::{Digest, Sha256};
use std::sync::Arc;

mod page;
mod tree;
use page::{ExplainPage, PageProgress};
use tree::{ExplainRules, FragmentExplanation};

#[derive(Debug)]
pub(crate) struct ExplainRequest {
    range: QueryRange,
    instant: Option<i64>,
    clock: Option<wire::ClockSegmentId>,
    expected_versions: Option<wire::QueryVersions>,
    cursor: Option<wire::QueryCursor>,
    parameters: wire::CursorQuery,
    pub(crate) control: QueryControl,
}
impl ExplainRequest {
    pub(crate) fn decode(body: serde_json::Value) -> Result<Self, ProtocolError> {
        let request: wire::RulesExplainRequest =
            serde_json::from_value(body).map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        let parameters = request.parameters();
        let result = match request {
            wire::RulesExplainRequest::Instant(value) => Self {
                range: QueryRange {
                    start: value.at_utc_ms,
                    end: value
                        .at_utc_ms
                        .checked_add(1)
                        .ok_or_else(|| ProtocolError::new(ErrorKind::BadRequest))?,
                },
                instant: Some(value.at_utc_ms),
                clock: value.clock_segment_id,
                expected_versions: value.expected_versions,
                cursor: value.cursor,
                parameters,
                control: QueryControl::with_deadline(value.deadline_ms.unwrap_or_default().get()),
            },
            wire::RulesExplainRequest::Range(value) => Self {
                range: QueryRange {
                    start: value.start_utc_ms,
                    end: value.end_utc_ms,
                },
                instant: None,
                clock: value.clock_segment_id,
                expected_versions: value.expected_versions,
                cursor: value.cursor,
                parameters,
                control: QueryControl::with_deadline(value.deadline_ms.unwrap_or_default().get()),
            },
        };
        if result
            .range
            .end
            .checked_sub(result.range.start)
            .is_none_or(|duration| duration <= 0 || duration > wire::SEGMENT_DETAIL_MAX_SPAN_MS)
        {
            return Err(ProtocolError::new(ErrorKind::BadRequest));
        }
        for endpoint in [result.range.start, result.range.end] {
            jiff::Timestamp::from_millisecond(endpoint)
                .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        }
        if let Some(cursor) = &result.cursor {
            cursor
                .validate_query(&result.parameters)
                .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        }
        Ok(result)
    }
}

#[derive(Debug)]
enum Stage {
    SelectInstant {
        range: Option<QueryRange>,
        classifier: Classifier,
    },
    Read(Pipeline),
    Sweep(Pipeline),
    Explain {
        segments: std::vec::IntoIter<DerivedSegment>,
        facts: SegmentFacts,
    },
    Page(ExplainPage),
    Complete,
}
#[derive(Debug)]
pub(crate) struct ExplainJob {
    snapshot: ReadSnapshot,
    rules: Arc<RuleSnapshot>,
    explanation: ExplainRules,
    request: ExplainRequest,
    versions: wire::QueryVersions,
    position: PagePosition,
    version_state: VersionState,
    scan_range: QueryRange,
    fragments: Vec<wire::ExplainFragment>,
    stage: Stage,
}
impl ExplainJob {
    pub(crate) fn start(
        reader: &mut Option<ReadOnlyStore>,
        request: ExplainRequest,
        rules: Arc<RuleSnapshot>,
    ) -> Result<Self, RunQueryError> {
        request.control.check()?;
        let mut snapshot = reader
            .take()
            .ok_or(RunQueryError::ReaderUnavailable)?
            .snapshot();
        let prepared = (|| {
            snapshot.begin()?;
            let settings = snapshot.settings()?;
            let metadata_version = settings.metadata_version;
            let state = VersionState {
                days: snapshot
                    .versions(request.range)?
                    .into_iter()
                    .map(|row| (row.key, row.version))
                    .collect(),
                settings,
                rule_hashes: rules.hashes().clone(),
                time_zone: RunQueryError::system_time_zone(jiff::tz::TimeZone::try_system)?,
            };
            let inputs = state.digest(request.range)?;
            if request
                .expected_versions
                .as_ref()
                .is_some_and(|expected| expected != &inputs)
            {
                return Err(ProtocolError::new(if request.cursor.is_some() {
                    ErrorKind::StaleCursor
                } else {
                    ErrorKind::StaleInputs {
                        current_versions: inputs,
                    }
                })
                .into());
            }
            let revisions = crate::rules::read::ReadInputs {
                snapshot: rules.clone(),
                metadata_version,
            }
            .versions();
            let mut digest = Sha256::new();
            digest.update(inputs.0.as_bytes());
            digest.update(revisions.0.as_bytes());
            let versions = wire::QueryVersions(quartz_domain::text::encode_hex(&digest.finalize()));
            let position = if request.instant.is_some() {
                PagePosition::First
            } else {
                PagePosition::from_cursor(request.cursor.as_ref(), &versions, &request.parameters)?
            };
            let categories = snapshot.categories()?;
            let explanation = ExplainRules::prepare(&rules, &request.control)?;
            Ok::<_, RunQueryError>(Prepared {
                versions,
                position,
                categories,
                explanation,
                state,
            })
        })();
        match prepared {
            Ok(prepared) => {
                let classifier = Classifier::new(rules.clone(), prepared.categories);
                let stage = if request.instant.is_some() {
                    Stage::SelectInstant {
                        range: None,
                        classifier,
                    }
                } else {
                    Stage::Read(Pipeline::new(request.range, classifier))
                };
                Ok(Self {
                    snapshot,
                    rules,
                    explanation: prepared.explanation,
                    scan_range: request.range,
                    request,
                    versions: prepared.versions,
                    position: prepared.position,
                    version_state: prepared.state,
                    fragments: Vec::new(),
                    stage,
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
    pub(crate) fn advance(&mut self) -> Result<Option<QueryResponse>, RunQueryError> {
        self.request.control.check()?;
        match std::mem::replace(&mut self.stage, Stage::Complete) {
            Stage::SelectInstant {
                mut range,
                classifier,
            } => match self.snapshot.read_batch(self.request.range)? {
                Some(rows) => {
                    for input in rows {
                        self.request.control.check()?;
                        let selected = match input {
                            super::input::InputRow::Foreground(row)
                                if self
                                    .request
                                    .clock
                                    .is_none_or(|clock| clock.0 == row.span.segment.0) =>
                            {
                                Some(QueryRange {
                                    start: row.span.start_utc_ms,
                                    end: row.span.end_utc_ms,
                                })
                            }
                            super::input::InputRow::Annotation(annotation)
                                if annotation.kind == wire::AnnotationKind::OfflineActivity
                                    && self.request.clock.is_none() =>
                            {
                                Some(annotation.range)
                            }
                            _ => None,
                        };
                        if let Some(selected) = selected {
                            let at = self
                                .request
                                .instant
                                .ok_or(RunQueryError::AdvanceQueryError)?;
                            let margin = wire::SEGMENT_DETAIL_MAX_SPAN_MS / 2;
                            let selected = QueryRange {
                                start: selected
                                    .start
                                    .max(at.saturating_sub(margin))
                                    .max(jiff::Timestamp::MIN.as_millisecond()),
                                end: selected
                                    .end
                                    .min(at.saturating_add(margin))
                                    .min(jiff::Timestamp::MAX.as_millisecond()),
                            };
                            range = Some(match range {
                                Some(range) => QueryRange {
                                    start: range.start.min(selected.start),
                                    end: range.end.max(selected.end),
                                },
                                None => selected,
                            });
                        }
                    }
                    self.stage = Stage::SelectInstant { range, classifier };
                }
                None => {
                    self.scan_range = range.unwrap_or(self.request.range);
                    let state = VersionState {
                        days: self
                            .snapshot
                            .versions(self.scan_range)?
                            .into_iter()
                            .map(|row| (row.key, row.version))
                            .collect(),
                        ..self.version_state.clone()
                    };
                    let expanded = state.digest(self.scan_range)?;
                    let mut digest = Sha256::new();
                    digest.update(self.versions.0.as_bytes());
                    digest.update(expanded.0.as_bytes());
                    self.versions =
                        wire::QueryVersions(quartz_domain::text::encode_hex(&digest.finalize()));
                    self.position = PagePosition::from_cursor(
                        self.request.cursor.as_ref(),
                        &self.versions,
                        &self.request.parameters,
                    )?;
                    self.snapshot.restart_scan();
                    self.stage = Stage::Read(Pipeline::new(self.scan_range, classifier));
                }
            },
            Stage::Read(mut pipeline) => match self.snapshot.read_batch(self.scan_range)? {
                Some(rows) => {
                    for input in rows {
                        self.request.control.check()?;
                        let row = pipeline.classified(input);
                        pipeline.ingest_classified(row)?;
                    }
                    self.stage = Stage::Read(pipeline);
                }
                None => {
                    pipeline.finish_input();
                    self.stage = Stage::Sweep(pipeline);
                }
            },
            Stage::Sweep(mut pipeline) => {
                if pipeline.advance() {
                    let derived = pipeline.into_segments();
                    self.stage = Stage::Explain {
                        segments: derived.segments.into_iter(),
                        facts: derived.facts,
                    };
                } else {
                    self.stage = Stage::Sweep(pipeline);
                }
            }
            Stage::Explain {
                mut segments,
                facts,
            } => {
                for _ in 0..QUERY_BATCH_ROWS {
                    self.request.control.check()?;
                    let Some(segment) = segments.next() else {
                        self.stage = Stage::Page(ExplainPage::new(
                            std::mem::take(&mut self.fragments),
                            self.versions.clone(),
                            self.request.parameters.clone(),
                            self.position,
                        )?);
                        return Ok(None);
                    };
                    if self.request.instant.is_some_and(|at| {
                        at < segment.span.start_utc_ms || at >= segment.span.end_utc_ms
                    }) {
                        continue;
                    }
                    let clock = segment
                        .clock_segment_id
                        .map(|id| wire::ClockSegmentId(id.0));
                    if self
                        .request
                        .clock
                        .is_some_and(|selected| Some(selected) != clock)
                    {
                        continue;
                    }
                    // Only retained facts and manual offline activity have an explanation.
                    if facts.explain_foreground(&segment).is_none()
                        && !matches!(segment.category_source, CategorySource::Annotation(_))
                    {
                        continue;
                    }
                    let fragment = FragmentExplanation::new(
                        &self.rules,
                        &self.explanation,
                        &self.request.control,
                    )
                    .fragment(&segment, &facts)?;
                    if let Some(previous) = self
                        .fragments
                        .last_mut()
                        .filter(|previous| Self::identical_neighbours(previous, &fragment))
                    {
                        previous.end_utc_ms = fragment.end_utc_ms;
                    } else {
                        self.fragments.push(fragment);
                    }
                }
                self.stage = Stage::Explain { segments, facts };
            }
            Stage::Page(mut page) => {
                if page.advance(&self.request.control)? == PageProgress::Complete {
                    self.request.control.check()?;
                    return Ok(Some(QueryResponse::Explain(page.response)));
                }
                self.stage = Stage::Page(page);
            }
            Stage::Complete => return Err(RunQueryError::AdvanceQueryError),
        }
        Ok(None)
    }
    fn identical_neighbours(left: &wire::ExplainFragment, right: &wire::ExplainFragment) -> bool {
        left.end_utc_ms == right.start_utc_ms
            && left.clock_segment_id == right.clock_segment_id
            && left.category == right.category
            && left.category_source == right.category_source
            && left.extraction == right.extraction
            && left.privacy == right.privacy
            && left.nodes == right.nodes
    }
    pub(crate) fn finish(self) -> Result<ReadOnlyStore, RunQueryError> {
        self.snapshot
            .finish()
            .map_err(|source| RunQueryError::Cleanup { source })
    }
}
struct Prepared {
    state: VersionState,
    explanation: ExplainRules,
    versions: wire::QueryVersions,
    position: PagePosition,
    categories: Vec<quartz_domain::categories::Category>,
}

#[cfg(test)]
mod tests;
