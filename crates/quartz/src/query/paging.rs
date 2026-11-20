//! Typed cursor validation and encoded-byte page admission.

use super::RunQueryError;
use quartz_protocol::{CursorQuery, QUERY_PAGE_BYTES, QueryCursor, QueryVersions};
use serde::Serialize;

/// Reserve envelope fields and a continuation object inside the 200 KiB limit.
const PAGE_ENVELOPE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub(crate) enum PagePosition {
    First,
    Cursor(usize),
}
impl PagePosition {
    fn index(self) -> usize {
        match self {
            Self::First => 0,
            Self::Cursor(index) => index,
        }
    }
    pub(crate) fn is_first(self) -> bool {
        matches!(self, Self::First)
    }
    pub(crate) fn from_cursor(
        cursor: Option<&QueryCursor>,
        versions: &QueryVersions,
        query: &CursorQuery,
    ) -> Result<PagePosition, RunQueryError> {
        let Some(cursor) = cursor else {
            return Ok(PagePosition::First);
        };
        if cursor.query() != query {
            return Err(RunQueryError::InvalidCursor);
        }
        if cursor.versions() != versions {
            return Err(RunQueryError::StaleCursor);
        }
        Ok(PagePosition::Cursor(cursor.position()))
    }
}
#[derive(Debug)]
pub(crate) struct PageBudget {
    remaining: usize,
}
impl PageBudget {
    pub(crate) fn new(fixed: &impl Serialize) -> Result<Self, RunQueryError> {
        let fixed_bytes = serde_json::to_vec(fixed)?.len();
        Ok(Self {
            remaining: QUERY_PAGE_BYTES
                .checked_sub(PAGE_ENVELOPE_BYTES + fixed_bytes)
                .ok_or(RunQueryError::PageTooLarge)?,
        })
    }
    pub(crate) fn page<T: Serialize>(
        mut self,
        items: Vec<T>,
        position: PagePosition,
    ) -> Result<PageItems<T>, RunQueryError> {
        let start = position.index();
        if matches!(position, PagePosition::Cursor(_)) && start >= items.len() {
            return Err(RunQueryError::InvalidCursor);
        }
        let mut page = PageItems {
            items: Vec::new(),
            next: None,
        };
        for item in items.into_iter().skip(start) {
            if !self.admit(&item)? {
                if page.items.is_empty() {
                    return Err(RunQueryError::PageTooLarge);
                }
                page.next = Some(start + page.items.len());
                break;
            }
            page.items.push(item);
        }
        Ok(page)
    }
    pub(crate) fn admit(&mut self, item: &impl Serialize) -> Result<bool, RunQueryError> {
        let bytes = serde_json::to_vec(item)?.len() + 1;
        if bytes > QUERY_PAGE_BYTES - PAGE_ENVELOPE_BYTES {
            return Err(RunQueryError::PageTooLarge);
        }
        if bytes > self.remaining {
            return Ok(false);
        }
        self.remaining -= bytes;
        Ok(true)
    }
}

#[derive(Debug)]
pub(crate) struct PageItems<T> {
    pub items: Vec<T>,
    pub next: Option<usize>,
}
