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
        self,
        items: Vec<T>,
        position: PagePosition,
    ) -> Result<PageItems<T>, RunQueryError> {
        self.page_with(items, position, |_| Ok(()))
    }
    /// Prepares only candidate page items before measuring their complete wire size.
    pub(crate) fn page_with<T: Serialize>(
        mut self,
        items: Vec<T>,
        position: PagePosition,
        mut prepare: impl FnMut(&mut T) -> Result<(), RunQueryError>,
    ) -> Result<PageItems<T>, RunQueryError> {
        let start = position.index();
        if matches!(position, PagePosition::Cursor(_)) && start >= items.len() {
            return Err(RunQueryError::InvalidCursor);
        }
        let mut page = PageItems {
            items: Vec::new(),
            next: None,
        };
        for mut item in items.into_iter().skip(start) {
            prepare(&mut item)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_preparation_is_bounded_by_the_page_and_includes_encoded_bytes() {
        const ITEM_COUNT: usize = 1000;
        const ITEM_BYTES: usize = QUERY_PAGE_BYTES / 4;
        let payload = "x".repeat(ITEM_BYTES);
        let items = (0..ITEM_COUNT).map(|index| index.to_string()).collect();
        let mut prepared = 0;
        let page = PageBudget::new(&())
            .expect("fixed budget")
            .page_with(items, PagePosition::First, |item| {
                prepared += 1;
                item.push_str(&payload);
                Ok(())
            })
            .expect("first page");
        assert!(!page.items.is_empty());
        assert!(
            prepared < ITEM_COUNT,
            "unreturned references remain unencoded"
        );
        assert!(serde_json::to_vec(&page.items).expect("page JSON").len() < QUERY_PAGE_BYTES);
        let next = page.next.expect("remaining items");
        assert_eq!(next, page.items.len());
        let items = (0..ITEM_COUNT).map(|index| index.to_string()).collect();
        let continuation = PageBudget::new(&())
            .expect("fixed budget")
            .page_with(items, PagePosition::Cursor(next), |item| {
                item.push_str(&payload);
                Ok(())
            })
            .expect("continuation");
        assert_eq!(continuation.items[0], format!("{next}{payload}"));
    }
}
