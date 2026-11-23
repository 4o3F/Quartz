//! Snapshot annotation pages with deterministic text-fragment positions.

use super::{
    RunQueryError,
    paging::{PageBudget, PagePosition},
};
use crate::store::query::{AnnotationScan, ReadSnapshot};
use quartz_domain::facts::QueryRange;
use quartz_protocol::{
    AnnotationText, AnnotationTextChunk, CursorQuery, QueryAnnotationsResponse, QueryCursor,
    QueryVersions, SavedAnnotation,
};

#[derive(Debug)]
pub(super) struct AnnotationPage {
    scan: AnnotationScan,
    current: Option<EntityChunks>,
    index: usize,
    position: PagePosition,
    budget: PageBudget,
    response: QueryAnnotationsResponse,
    parameters: CursorQuery,
}
impl AnnotationPage {
    pub(super) fn new(
        versions: QueryVersions,
        parameters: CursorQuery,
        position: PagePosition,
    ) -> Result<Self, RunQueryError> {
        let response = QueryAnnotationsResponse {
            entities: Vec::new(),
            versions,
            next_cursor: None,
        };
        let budget = Self::budget(&response, &parameters)?;
        Ok(Self {
            scan: AnnotationScan::default(),
            current: None,
            index: 0,
            position,
            budget,
            response,
            parameters,
        })
    }
    fn budget(
        response: &QueryAnnotationsResponse,
        parameters: &CursorQuery,
    ) -> Result<PageBudget, RunQueryError> {
        PageBudget::new(response)?.with_cursor(&QueryCursor::new(
            response.versions.clone(),
            parameters.clone(),
            usize::MAX,
        ))
    }
    /// Processes one stored row or fragment before yielding to pending writes.
    pub(super) fn advance(
        &mut self,
        snapshot: &mut ReadSnapshot,
        range: QueryRange,
    ) -> Result<Option<QueryAnnotationsResponse>, RunQueryError> {
        if self.current.is_none() {
            let Some(entity) = snapshot.read_saved_annotation(range, &mut self.scan)? else {
                if !self.position.is_first() && self.response.entities.is_empty() {
                    return Err(RunQueryError::InvalidCursor);
                }
                return Ok(Some(self.finish(None)));
            };
            // Fragment boundaries depend on the empty page budget, never on preceding rows.
            let empty = QueryAnnotationsResponse {
                entities: Vec::new(),
                versions: self.response.versions.clone(),
                next_cursor: None,
            };
            self.current = Some(EntityChunks::new(
                entity,
                &Self::budget(&empty, &self.parameters)?,
            )?);
        }
        let chunks = self.current.as_mut().expect("current annotation retained");
        let Some(entity) = chunks.next() else {
            self.current = None;
            return Ok(None);
        };
        if self.index >= self.position.index() {
            if !self.budget.admit(&entity)? {
                if self.response.entities.is_empty() {
                    return Err(RunQueryError::PageTooLarge);
                }
                return Ok(Some(self.finish(Some(self.index))));
            }
            self.response.entities.push(entity);
        }
        self.index += 1;
        Ok(None)
    }
    fn finish(&mut self, next: Option<usize>) -> QueryAnnotationsResponse {
        QueryAnnotationsResponse {
            entities: std::mem::take(&mut self.response.entities),
            versions: self.response.versions.clone(),
            next_cursor: next.map(|index| {
                QueryCursor::new(
                    self.response.versions.clone(),
                    self.parameters.clone(),
                    index,
                )
            }),
        }
    }
}

#[derive(Debug)]
enum EntityChunks {
    Complete(Option<SavedAnnotation>),
    Fragmented {
        metadata: SavedAnnotation,
        label: Option<TextChunks>,
        note: Option<TextChunks>,
        chunk_bytes: usize,
        progress: ChunkProgress,
    },
}
#[derive(Debug, PartialEq, Eq)]
enum ChunkProgress {
    More,
    Complete,
}
impl EntityChunks {
    fn new(mut entity: SavedAnnotation, budget: &PageBudget) -> Result<Self, RunQueryError> {
        if budget.fits(&entity)? {
            return Ok(Self::Complete(Some(entity)));
        }
        let label = entity.label.take().map(TextChunks::new);
        let note = entity.note.take().map(TextChunks::new);
        let mut metadata_budget = entity.clone();
        metadata_budget.label = label.as_ref().map(|_| TextChunks::budget_value());
        metadata_budget.note = note.as_ref().map(|_| TextChunks::budget_value());
        let chunk_bytes = budget.annotation_chunk_bytes(&metadata_budget)?;
        Ok(Self::Fragmented {
            metadata: entity,
            label,
            note,
            chunk_bytes,
            progress: ChunkProgress::More,
        })
    }
    fn next(&mut self) -> Option<SavedAnnotation> {
        match self {
            Self::Complete(entity) => entity.take(),
            Self::Fragmented {
                metadata,
                label,
                note,
                chunk_bytes,
                progress,
            } => {
                if *progress == ChunkProgress::Complete {
                    return None;
                }
                let mut entity = metadata.clone();
                entity.label = label.as_mut().map(|text| text.next(*chunk_bytes));
                entity.note = note.as_mut().map(|text| text.next(*chunk_bytes));
                if label.as_ref().is_none_or(TextChunks::is_complete)
                    && note.as_ref().is_none_or(TextChunks::is_complete)
                {
                    *progress = ChunkProgress::Complete;
                }
                Some(entity)
            }
        }
    }
}
#[derive(Debug)]
struct TextChunks {
    text: String,
    offset_utf8: usize,
}
impl TextChunks {
    fn new(text: AnnotationText) -> Self {
        let AnnotationText::Complete(text) = text else {
            unreachable!("snapshot reads complete stored text")
        };
        Self {
            text,
            offset_utf8: 0,
        }
    }
    fn budget_value() -> AnnotationText {
        AnnotationText::Chunk(AnnotationTextChunk {
            text: String::new(),
            offset_utf8: usize::MAX,
            complete: false,
        })
    }
    fn is_complete(&self) -> bool {
        self.offset_utf8 == self.text.len()
    }
    fn next(&mut self, budget: usize) -> AnnotationText {
        if self.text.len() <= budget {
            self.offset_utf8 = self.text.len();
            return AnnotationText::Complete(self.text.clone());
        }
        let start = self.offset_utf8;
        let mut end = start.saturating_add(budget).min(self.text.len());
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        self.offset_utf8 = end;
        AnnotationText::Chunk(AnnotationTextChunk {
            text: self.text[start..end].to_owned(),
            offset_utf8: start,
            complete: self.is_complete(),
        })
    }
}

#[cfg(test)]
mod tests;
