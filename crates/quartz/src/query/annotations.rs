//! Single-response snapshot annotation reader bounded by item cap and frame limit.

use super::{QUERY_MAX_RESPONSE_BYTES, RunQueryError, paging::JsonBytes};
use crate::store::query::{AnnotationScan, ReadSnapshot};
use quartz_domain::facts::QueryRange;
use quartz_protocol::{
    ANNOTATIONS_MAX_ENTITIES, QueryAnnotationsResponse, QueryVersions, SavedAnnotation,
};

#[derive(Debug)]
pub(super) struct AnnotationRead {
    scan: AnnotationScan,
    entities: Vec<SavedAnnotation>,
    versions: QueryVersions,
    used_bytes: usize,
    truncated: bool,
}
impl AnnotationRead {
    pub(super) fn new(versions: QueryVersions) -> Result<Self, RunQueryError> {
        let empty = QueryAnnotationsResponse {
            entities: Vec::new(),
            versions: versions.clone(),
            truncated: false,
        };
        let used_bytes = JsonBytes::measure(&empty)?;
        Ok(Self {
            scan: AnnotationScan::default(),
            entities: Vec::new(),
            versions,
            used_bytes,
            truncated: false,
        })
    }
    /// Reads one stored row per turn so capture writes can interleave.
    pub(super) fn advance(
        &mut self,
        snapshot: &mut ReadSnapshot,
        range: QueryRange,
    ) -> Result<Option<QueryAnnotationsResponse>, RunQueryError> {
        let Some(entity) = snapshot.read_saved_annotation(range, &mut self.scan)? else {
            return Ok(Some(self.finish()));
        };
        if self.entities.len() >= ANNOTATIONS_MAX_ENTITIES {
            self.truncated = true;
            return Ok(Some(self.finish()));
        }
        let item_bytes = JsonBytes::measure(&entity)? + 1;
        if self.used_bytes.saturating_add(item_bytes) > QUERY_MAX_RESPONSE_BYTES {
            self.truncated = true;
            return Ok(Some(self.finish()));
        }
        self.used_bytes += item_bytes;
        self.entities.push(entity);
        Ok(None)
    }
    fn finish(&mut self) -> QueryAnnotationsResponse {
        QueryAnnotationsResponse {
            entities: std::mem::take(&mut self.entities),
            versions: self.versions.clone(),
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests;
