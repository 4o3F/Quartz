//! Typed cursor validation and encoded-byte page admission.

use super::RunQueryError;
use quartz_protocol::{CursorQuery, QUERY_PAGE_BYTES, QueryCursor, QueryVersions};
use serde::Serialize;

/// Reserve envelope fields and a continuation object inside the 200 KiB limit.
const PAGE_ENVELOPE_BYTES: usize = 4096;
/// JSON can encode one control byte as six bytes (`\u0000`).
const MAX_JSON_BYTES_PER_TEXT_BYTE: usize = 6;
/// Both editable text fields receive an independent allowance in each fragment.
const ANNOTATION_TEXT_FIELDS: usize = 2;

#[derive(Debug, Clone, Copy)]
pub(crate) enum PagePosition {
    First,
    Cursor(usize),
}
impl PagePosition {
    pub(super) fn index(self) -> usize {
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
        let cursor = cursor.decode().map_err(|_| RunQueryError::InvalidCursor)?;
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
    #[cfg(test)]
    pub(super) fn remaining_bytes(&self) -> usize {
        self.remaining
    }

    pub(super) fn fits(&self, item: &impl Serialize) -> Result<bool, RunQueryError> {
        Ok(JsonBytes::measure(item)? < self.remaining)
    }
    /// Reserves worst-case JSON escaping and metadata for both annotation text fields.
    pub(super) fn annotation_chunk_bytes(
        &self,
        metadata: &impl Serialize,
    ) -> Result<usize, RunQueryError> {
        let bytes = self
            .remaining
            .checked_sub(JsonBytes::measure(metadata)? + 1)
            .ok_or(RunQueryError::PageTooLarge)?
            / (ANNOTATION_TEXT_FIELDS * MAX_JSON_BYTES_PER_TEXT_BYTE);
        if bytes < char::MAX.len_utf8() {
            return Err(RunQueryError::PageTooLarge);
        }
        Ok(bytes)
    }
    pub(crate) fn new(fixed: &impl Serialize) -> Result<Self, RunQueryError> {
        let fixed_bytes = JsonBytes::measure(fixed)?;
        Ok(Self {
            remaining: QUERY_PAGE_BYTES
                .checked_sub(PAGE_ENVELOPE_BYTES + fixed_bytes)
                .ok_or(RunQueryError::PageTooLarge)?,
        })
    }
    pub(crate) fn with_cursor(mut self, cursor: &QueryCursor) -> Result<Self, RunQueryError> {
        self.remaining = self
            .remaining
            .checked_sub(JsonBytes::measure(cursor)?)
            .ok_or(RunQueryError::PageTooLarge)?;
        Ok(self)
    }
    pub(crate) fn page<T: Serialize>(
        self,
        items: Vec<T>,
        position: PagePosition,
    ) -> Result<PageItems<T>, RunQueryError> {
        self.page_mapped(items, position, Ok)
    }
    /// Prepares only candidate page items before measuring their complete wire size.
    pub(crate) fn page_with<T: Serialize>(
        self,
        items: Vec<T>,
        position: PagePosition,
        mut prepare: impl FnMut(&mut T) -> Result<(), RunQueryError>,
    ) -> Result<PageItems<T>, RunQueryError> {
        self.page_mapped(items, position, |mut item| {
            prepare(&mut item)?;
            Ok(item)
        })
    }
    /// Encodes references only for items considered for the returned page.
    pub(crate) fn page_mapped<T, U: Serialize>(
        mut self,
        items: Vec<T>,
        position: PagePosition,
        mut project: impl FnMut(T) -> Result<U, RunQueryError>,
    ) -> Result<PageItems<U>, RunQueryError> {
        let start = position.index();
        if matches!(position, PagePosition::Cursor(_)) && start >= items.len() {
            return Err(RunQueryError::InvalidCursor);
        }
        let mut page = PageItems {
            items: Vec::new(),
            next: None,
        };
        for item in items.into_iter().skip(start) {
            let item = project(item)?;
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
    /// Pages the first list to completion before starting the second list.
    pub(crate) fn lists<T: Serialize, U: Serialize>(
        self,
        first: Vec<T>,
        second: Vec<U>,
        position: PagePosition,
    ) -> Result<PageLists<T, U>, RunQueryError> {
        self.lists_mapped(first, second, position, Ok)
    }
    pub(crate) fn lists_mapped<T: Serialize, U, V: Serialize>(
        self,
        first: Vec<T>,
        second: Vec<U>,
        position: PagePosition,
        project: impl FnMut(U) -> Result<V, RunQueryError>,
    ) -> Result<PageLists<T, V>, RunQueryError> {
        let first_len = first.len();
        let index = position.index();
        if index < first_len {
            let page = self.page(first, position)?;
            Ok(PageLists {
                first: page.items,
                second: Vec::new(),
                next: page
                    .next
                    .or_else(|| (!second.is_empty()).then_some(first_len)),
            })
        } else {
            let second_position = match position {
                PagePosition::First => PagePosition::First,
                PagePosition::Cursor(_) => PagePosition::Cursor(index - first_len),
            };
            let page = self.page_mapped(second, second_position, project)?;
            Ok(PageLists {
                first: Vec::new(),
                second: page.items,
                next: page.next.map(|next| first_len + next),
            })
        }
    }
    pub(crate) fn admit(&mut self, item: &impl Serialize) -> Result<bool, RunQueryError> {
        let bytes = JsonBytes::measure(item)? + 1;
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

/// Counts encoded bytes without retaining a serialized copy.
#[derive(Debug, Default)]
struct JsonBytes(usize);
impl JsonBytes {
    fn measure(value: &impl Serialize) -> Result<usize, serde_json::Error> {
        let mut writer = Self::default();
        serde_json::to_writer(&mut writer, value)?;
        Ok(writer.0)
    }
}
impl std::io::Write for JsonBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("encoded JSON size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct PageItems<T> {
    pub items: Vec<T>,
    pub next: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct PageLists<T, U> {
    pub first: Vec<T>,
    pub second: Vec<U>,
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
    #[test]
    fn separate_lists_finish_in_order_and_only_encode_candidate_documents() {
        const DOCUMENT_COUNT: usize = 1000;
        const DOCUMENT_BYTES: usize = QUERY_PAGE_BYTES / 8;
        let mut encodings = 0;
        let first = PageBudget::new(&())
            .expect("budget")
            .lists_mapped(
                vec!["app"],
                vec![(); DOCUMENT_COUNT],
                PagePosition::First,
                |()| {
                    encodings += 1;
                    Ok("x".repeat(DOCUMENT_BYTES))
                },
            )
            .expect("application page");
        assert_eq!(first.first, vec!["app"]);
        assert!(first.second.is_empty());
        assert_eq!(encodings, 0);
        let next = first.next.expect("document continuation");
        let second = PageBudget::new(&())
            .expect("budget")
            .lists_mapped(
                vec!["app"],
                vec![(); DOCUMENT_COUNT],
                PagePosition::Cursor(next),
                |()| {
                    encodings += 1;
                    Ok("x".repeat(DOCUMENT_BYTES))
                },
            )
            .expect("document page");
        assert!(second.first.is_empty());
        assert!(!second.second.is_empty());
        assert!(encodings < DOCUMENT_COUNT);
        assert_eq!(
            second.next.expect("more documents"),
            next + second.second.len()
        );
    }
    #[test]
    fn opaque_cursors_are_validated_at_the_core_paging_boundary() {
        let versions = QueryVersions("v".into());
        let query = CursorQuery::Day {
            local_date: "2025-01-02".parse().expect("date"),
        };
        for wire in [
            serde_json::json!({}),
            serde_json::json!({"versions":"v","query":{"kind":"future"},"position":1}),
            serde_json::json!({"versions":"v","query":{"kind":"day","local_date":"2025-01-02"},"position":-1}),
        ] {
            let cursor: QueryCursor =
                serde_json::from_value(wire).expect("opaque object at client boundary");
            assert!(matches!(
                PagePosition::from_cursor(Some(&cursor), &versions, &query),
                Err(RunQueryError::InvalidCursor)
            ));
        }
        let cursor: QueryCursor = serde_json::from_value(serde_json::json!({
            "versions":"v","query":{"kind":"day","local_date":"2025-01-02"},"position":1
        }))
        .expect("cursor");
        assert_eq!(
            PagePosition::from_cursor(Some(&cursor), &versions, &query)
                .expect("valid cursor")
                .index(),
            1
        );
        assert!(matches!(
            PagePosition::from_cursor(Some(&cursor), &QueryVersions("changed".into()), &query),
            Err(RunQueryError::StaleCursor)
        ));
        assert!(matches!(
            PagePosition::from_cursor(
                Some(&cursor),
                &versions,
                &CursorQuery::PendingAway {
                    local_date: "2025-01-02".parse().expect("date")
                }
            ),
            Err(RunQueryError::InvalidCursor)
        ));
    }
}
