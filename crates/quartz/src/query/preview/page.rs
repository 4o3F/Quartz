//! Byte-bounded preview pages with lazy document references and bounded turns.

use super::{SampleOrder, UnencodedTotals};
use crate::query::{
    RunQueryError,
    documents::DocumentDuration,
    input::QueryRange,
    paging::{PageBudget, PagePosition},
    pipeline::QUERY_BATCH_ROWS,
};
use quartz_protocol as wire;
use std::collections::{BTreeMap, VecDeque, btree_map::IntoValues};

#[derive(Debug)]
pub(super) struct PageSources {
    pub before: UnencodedTotals,
    pub after: UnencodedTotals,
    pub statistics: BTreeMap<String, wire::RulePreviewStatistics>,
    pub samples: BTreeMap<SampleOrder, wire::RulePreviewSample>,
}
#[derive(Debug)]
pub(super) struct PreviewReferences {
    pub range: QueryRange,
    pub before: wire::QueryVersions,
    pub after: wire::QueryVersions,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PageProgress {
    More,
    Complete,
}
#[derive(Debug)]
pub(super) struct PreviewPage {
    response: wire::RulesPreviewResponse,
    budget: PageBudget,
    parameters: wire::CursorQuery,
    position: PagePosition,
    references: PreviewReferences,
    sections: VecDeque<PageSection>,
    index: usize,
    admitted: usize,
}
impl PreviewPage {
    pub(super) fn new(
        response: wire::RulesPreviewResponse,
        parameters: wire::CursorQuery,
        position: PagePosition,
        references: PreviewReferences,
        sources: PageSources,
    ) -> Result<Self, RunQueryError> {
        let budget = PageBudget::new(&response)?.with_cursor(&wire::QueryCursor::new(
            response.versions.clone(),
            parameters.clone(),
            usize::MAX,
        ))?;
        Ok(Self {
            response,
            budget,
            parameters,
            position,
            references,
            sections: VecDeque::from([
                PageSection::Rules(sources.statistics.into_values()),
                PageSection::BeforeCategory(sources.before.categories.into_iter()),
                PageSection::BeforeApp(sources.before.applications.into_iter()),
                PageSection::BeforeDocument(sources.before.documents.into_iter()),
                PageSection::AfterCategory(sources.after.categories.into_iter()),
                PageSection::AfterApp(sources.after.applications.into_iter()),
                PageSection::AfterDocument(sources.after.documents.into_iter()),
                PageSection::Samples(sources.samples.into_values()),
            ]),
            index: 0,
            admitted: 0,
        })
    }
    pub(super) fn advance(&mut self) -> Result<PageProgress, RunQueryError> {
        for _ in 0..QUERY_BATCH_ROWS {
            let Some(item) = self.next_item() else {
                if !self.position.is_first() && self.index <= self.position.index() {
                    return Err(RunQueryError::InvalidCursor);
                }
                return Ok(PageProgress::Complete);
            };
            if self.index < self.position.index() {
                self.index += 1;
                continue;
            }
            let item = item.prepare(&self.references)?;
            if !item.admit(&mut self.budget)? {
                if self.admitted == 0 {
                    return Err(RunQueryError::PageTooLarge);
                }
                self.response.next_cursor = Some(wire::QueryCursor::new(
                    self.response.versions.clone(),
                    self.parameters.clone(),
                    self.index,
                ));
                return Ok(PageProgress::Complete);
            }
            item.push(&mut self.response);
            self.index += 1;
            self.admitted += 1;
        }
        Ok(PageProgress::More)
    }
    fn next_item(&mut self) -> Option<PendingItem> {
        while let Some(section) = self.sections.front_mut() {
            if let Some(item) = section.next() {
                return Some(item);
            }
            self.sections.pop_front();
        }
        None
    }
    pub(super) fn finish(self) -> wire::RulesPreviewResponse {
        self.response
    }
}
#[derive(Debug)]
enum PageSection {
    Rules(IntoValues<String, wire::RulePreviewStatistics>),
    BeforeCategory(std::vec::IntoIter<wire::PreviewCategoryRank>),
    BeforeApp(std::vec::IntoIter<wire::ApplicationRank>),
    BeforeDocument(std::vec::IntoIter<DocumentDuration>),
    AfterCategory(std::vec::IntoIter<wire::PreviewCategoryRank>),
    AfterApp(std::vec::IntoIter<wire::ApplicationRank>),
    AfterDocument(std::vec::IntoIter<DocumentDuration>),
    Samples(IntoValues<SampleOrder, wire::RulePreviewSample>),
}
impl PageSection {
    fn next(&mut self) -> Option<PendingItem> {
        match self {
            Self::Rules(rows) => rows.next().map(PageItem::Rule).map(PendingItem::Ready),
            Self::BeforeCategory(rows) => rows
                .next()
                .map(PageItem::BeforeCategory)
                .map(PendingItem::Ready),
            Self::BeforeApp(rows) => rows.next().map(PageItem::BeforeApp).map(PendingItem::Ready),
            Self::BeforeDocument(rows) => rows.next().map(PendingItem::BeforeDocument),
            Self::AfterCategory(rows) => rows
                .next()
                .map(PageItem::AfterCategory)
                .map(PendingItem::Ready),
            Self::AfterApp(rows) => rows.next().map(PageItem::AfterApp).map(PendingItem::Ready),
            Self::AfterDocument(rows) => rows.next().map(PendingItem::AfterDocument),
            Self::Samples(rows) => rows
                .next()
                .map(Box::new)
                .map(PageItem::Sample)
                .map(PendingItem::Ready),
        }
    }
}
#[derive(Debug)]
enum PendingItem {
    Ready(PageItem),
    BeforeDocument(DocumentDuration),
    AfterDocument(DocumentDuration),
}
impl PendingItem {
    fn prepare(self, references: &PreviewReferences) -> Result<PageItem, RunQueryError> {
        match self {
            Self::Ready(item) => Ok(item),
            Self::BeforeDocument(row) => row
                .into_rank(references.range, &references.before)
                .map(PageItem::BeforeDocument),
            Self::AfterDocument(row) => row
                .into_rank(references.range, &references.after)
                .map(PageItem::AfterDocument),
        }
    }
}
#[derive(Debug)]
enum PageItem {
    Rule(wire::RulePreviewStatistics),
    BeforeCategory(wire::PreviewCategoryRank),
    BeforeApp(wire::ApplicationRank),
    BeforeDocument(wire::DocumentRank),
    AfterCategory(wire::PreviewCategoryRank),
    AfterApp(wire::ApplicationRank),
    AfterDocument(wire::DocumentRank),
    // Samples hold two outputs; keep the other page items compact.
    Sample(Box<wire::RulePreviewSample>),
}
impl PageItem {
    fn admit(&self, budget: &mut PageBudget) -> Result<bool, RunQueryError> {
        match self {
            Self::Rule(row) => budget.admit(row),
            Self::BeforeCategory(row) | Self::AfterCategory(row) => budget.admit(row),
            Self::BeforeApp(row) | Self::AfterApp(row) => budget.admit(row),
            Self::BeforeDocument(row) | Self::AfterDocument(row) => budget.admit(row),
            Self::Sample(row) => budget.admit(row),
        }
    }
    fn push(self, response: &mut wire::RulesPreviewResponse) {
        let totals = response.totals.as_mut().expect("preview totals");
        match self {
            Self::Rule(row) => response.rules.push(row),
            Self::BeforeCategory(row) => totals.before.categories.push(row),
            Self::BeforeApp(row) => totals.before.applications.push(row),
            Self::BeforeDocument(row) => totals.before.documents.push(row),
            Self::AfterCategory(row) => totals.after.categories.push(row),
            Self::AfterApp(row) => totals.after.applications.push(row),
            Self::AfterDocument(row) => totals.after.documents.push(row),
            Self::Sample(row) => response.samples.push(*row),
        }
    }
}
