//! Node-by-node paging with one header per owning fragment on each page.
use super::{PagePosition, QUERY_BATCH_ROWS, QueryControl, RunQueryError, wire};
use crate::query::paging::PageBudget;

#[derive(Debug)]
struct FragmentNodes {
    header: wire::ExplainFragment,
    nodes: std::vec::IntoIter<wire::ExplainNode>,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PageProgress {
    More,
    Complete,
}
#[derive(Debug)]
pub(super) struct ExplainPage {
    pub(super) response: wire::RulesExplainResponse,
    budget: PageBudget,
    parameters: wire::CursorQuery,
    position: PagePosition,
    fragments: std::vec::IntoIter<wire::ExplainFragment>,
    current: Option<FragmentNodes>,
    index: usize,
    admitted: usize,
    fragment_index: usize,
    last_owner: Option<usize>,
}
impl ExplainPage {
    pub(super) fn new(
        fragments: Vec<wire::ExplainFragment>,
        versions: wire::QueryVersions,
        parameters: wire::CursorQuery,
        position: PagePosition,
    ) -> Result<Self, RunQueryError> {
        let response = wire::RulesExplainResponse {
            fragments: Vec::new(),
            versions,
            next_cursor: None,
        };
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
            fragments: fragments.into_iter(),
            current: None,
            index: 0,
            admitted: 0,
            fragment_index: 0,
            last_owner: None,
        })
    }
    pub(super) fn advance(
        &mut self,
        control: &QueryControl,
    ) -> Result<PageProgress, RunQueryError> {
        for _ in 0..QUERY_BATCH_ROWS {
            control.check()?;
            if self.current.is_none() {
                let Some(mut header) = self.fragments.next() else {
                    if !self.position.is_first() && self.index <= self.position.index() {
                        return Err(RunQueryError::InvalidCursor);
                    }
                    return Ok(PageProgress::Complete);
                };
                let nodes = std::mem::take(&mut header.nodes).into_iter();
                self.current = Some(FragmentNodes { header, nodes });
                self.fragment_index += 1;
            }
            let current = self
                .current
                .as_mut()
                .expect("current fragment selected above");
            let node = current.nodes.next();
            if self.index < self.position.index() {
                self.index += 1;
                if current.nodes.len() == 0 {
                    self.current = None;
                }
                continue;
            }
            let continuing = self.last_owner == Some(self.fragment_index);
            let admitted = if continuing {
                self.budget
                    .admit(node.as_ref().ok_or(RunQueryError::AdvanceQueryError)?)?
            } else {
                let mut item = current.header.clone();
                item.nodes.extend(node.iter().cloned());
                self.budget.admit(&item)?
            };
            if !admitted {
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
            if continuing {
                self.response
                    .fragments
                    .last_mut()
                    .expect("continuing fragment exists")
                    .nodes
                    .extend(node);
            } else {
                let mut header = current.header.clone();
                header.nodes.extend(node);
                self.response.fragments.push(header);
                self.last_owner = Some(self.fragment_index);
            }
            if current.nodes.len() == 0 {
                self.current = None;
            }
            self.index += 1;
            self.admitted += 1;
        }
        Ok(PageProgress::More)
    }
}
