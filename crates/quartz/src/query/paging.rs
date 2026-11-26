//! Typed cursor validation and encoded-byte page admission (retained for batch 6b).

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
    /// Admits one character inside an already reserved JSON string, including escaping.
    pub(crate) fn admit_text_character(&mut self, character: char) -> Result<bool, RunQueryError> {
        const JSON_STRING_QUOTES: usize = 2;
        let bytes = JsonBytes::measure(&character)? - JSON_STRING_QUOTES;
        if bytes > self.remaining {
            return Ok(false);
        }
        self.remaining -= bytes;
        Ok(true)
    }
    pub(crate) fn page<T: Serialize>(
        self,
        items: Vec<T>,
        position: PagePosition,
    ) -> Result<PageItems<T>, RunQueryError> {
        self.page_mapped(items, position, Ok)
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
pub(crate) struct JsonBytes(usize);
impl JsonBytes {
    pub(crate) fn measure(value: &impl Serialize) -> Result<usize, serde_json::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_cursors_are_validated_at_the_core_paging_boundary() {
        let versions = QueryVersions("v".into());
        let query = CursorQuery::Categories {
            include_archived: false,
        };
        for wire in [
            serde_json::json!({}),
            serde_json::json!({"versions":"v","query":{"kind":"future"},"position":1}),
            serde_json::json!({"versions":"v","query":{"kind":"categories","include_archived":false},"position":-1}),
        ] {
            let cursor: QueryCursor =
                serde_json::from_value(wire).expect("opaque object at client boundary");
            assert!(matches!(
                PagePosition::from_cursor(Some(&cursor), &versions, &query),
                Err(RunQueryError::InvalidCursor)
            ));
        }
        let cursor: QueryCursor = serde_json::from_value(serde_json::json!({
            "versions":"v","query":{"kind":"categories","include_archived":false},"position":1
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
                &CursorQuery::Categories {
                    include_archived: true
                }
            ),
            Err(RunQueryError::InvalidCursor)
        ));
    }
}
