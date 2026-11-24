//! Visible document identities and stateless, integrity-checked query references.

use super::RunQueryError;
use quartz_domain::text::{NfcText, encode_hex};
use quartz_protocol::{
    ApplicationId, DocumentRank, DocumentRef, ExtractionStatus, QueryVersions, ReferenceRange,
    TimelineSegment, TitleExtraction,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, btree_map::Entry};

/// The prefix versions both the payload schema and its checksum domain.
const REFERENCE_PREFIX: &str = "quartz.document.v1:";
/// The frame bound also bounds work spent decoding an untrusted reference.
const MAX_REFERENCE_BYTES: usize = quartz_protocol::MAX_FRAME_LEN;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DocumentIdentity {
    pub(super) app: ApplicationId,
    pub(super) document: String,
    pub(super) project: Option<String>,
    pub(super) profile: Option<String>,
}
impl DocumentIdentity {
    pub(super) fn from_segment(segment: &TimelineSegment) -> Option<Self> {
        Self::new(segment.app.as_ref()?.id, segment.extraction.as_ref()?)
    }
    pub(super) fn new(app: ApplicationId, extraction: &TitleExtraction) -> Option<Self> {
        if !matches!(
            extraction.status?,
            ExtractionStatus::Extracted | ExtractionStatus::Fallback
        ) {
            return None;
        }
        let document = extraction
            .document
            .as_deref()
            .filter(|text| !text.is_empty())?;
        Some(Self {
            app,
            document: NfcText::new(document).into_string(),
            project: extraction
                .project
                .as_deref()
                .map(|text| NfcText::new(text).into_string()),
            profile: extraction
                .profile
                .as_deref()
                .map(|text| NfcText::new(text).into_string()),
        })
    }
    pub(super) fn matches_extraction(
        &self,
        app: ApplicationId,
        fields: &quartz_domain::rules::Extraction,
    ) -> bool {
        use quartz_domain::rules::ExtractionStatus;
        self.app == app
            && matches!(
                fields.status,
                ExtractionStatus::Extracted | ExtractionStatus::Fallback
            )
            && fields
                .document
                .as_deref()
                .filter(|text| !text.is_empty())
                .is_some_and(|text| NfcText::new(text).as_str() == self.document)
            && fields
                .project
                .as_deref()
                .map(NfcText::new)
                .as_ref()
                .map(NfcText::as_str)
                == self.project.as_deref()
            && fields
                .profile
                .as_deref()
                .map(NfcText::new)
                .as_ref()
                .map(NfcText::as_str)
                == self.profile.as_deref()
    }
    fn is_canonical(&self) -> bool {
        !self.document.is_empty()
            && NfcText::new(&self.document).as_str() == self.document
            && self
                .project
                .as_deref()
                .is_none_or(|text| NfcText::new(text).as_str() == text)
            && self
                .profile
                .as_deref()
                .is_none_or(|text| NfcText::new(text).as_str() == text)
    }
}

#[derive(Debug)]
pub(super) struct DocumentDuration {
    pub(super) identity: DocumentIdentity,
    pub(super) active_ms: i64,
}
impl DocumentDuration {
    pub(super) fn into_rank(
        self,
        range: super::input::QueryRange,
        versions: &QueryVersions,
    ) -> Result<DocumentRank, RunQueryError> {
        let reference = DocumentReference::issue(
            self.identity.clone(),
            ReferenceRange {
                start_utc_ms: range.start,
                end_utc_ms: range.end,
            },
            versions.clone(),
        )?;
        Ok(DocumentRank {
            document_ref: reference,
            app: self.identity.app,
            document: self.identity.document,
            project: self.identity.project,
            profile: self.identity.profile,
            active_ms: self.active_ms,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "ReferenceRange", deny_unknown_fields)]
struct DocumentReferenceRange {
    start_utc_ms: i64,
    end_utc_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentReference {
    pub(super) identity: DocumentIdentity,
    #[serde(with = "DocumentReferenceRange")]
    range: ReferenceRange,
    versions: QueryVersions,
}
#[cfg(test)]
std::thread_local! { pub(super) static ISSUED_DOCUMENT_REFERENCES: std::cell::Cell<usize> = const {std::cell::Cell::new(0)}; }
impl DocumentReference {
    pub(super) fn issue(
        identity: DocumentIdentity,
        range: ReferenceRange,
        versions: QueryVersions,
    ) -> Result<DocumentRef, RunQueryError> {
        #[cfg(test)]
        ISSUED_DOCUMENT_REFERENCES.update(|count| count + 1);
        let payload = serde_json::to_string(&Self {
            identity,
            range,
            versions,
        })?;
        let checksum = Self::checksum(&payload);
        Ok(DocumentRef::from_encoded(format!(
            "{REFERENCE_PREFIX}{checksum}:{payload}"
        )))
    }
    fn checksum(payload: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(REFERENCE_PREFIX.as_bytes());
        digest.update(payload.as_bytes());
        encode_hex(&digest.finalize())
    }
    pub(super) fn validate(
        reference: &DocumentRef,
        range: ReferenceRange,
        versions: &QueryVersions,
    ) -> Result<Self, RunQueryError> {
        let encoded = reference.as_str();
        if encoded.len() > MAX_REFERENCE_BYTES {
            return Err(RunQueryError::InvalidDocumentRef);
        }
        let body = encoded
            .strip_prefix(REFERENCE_PREFIX)
            .ok_or(RunQueryError::InvalidDocumentRef)?;
        let separator = body.find(':').ok_or(RunQueryError::InvalidDocumentRef)?;
        let checksum = &body[..separator];
        let payload = &body[separator + 1..];
        if checksum != Self::checksum(payload) {
            return Err(RunQueryError::InvalidDocumentRef);
        }
        let decoded: Self =
            serde_json::from_str(payload).map_err(|_| RunQueryError::InvalidDocumentRef)?;
        if decoded.range != range
            || range.start_utc_ms >= range.end_utc_ms
            || !decoded.identity.is_canonical()
        {
            return Err(RunQueryError::InvalidDocumentRef);
        }
        if &decoded.versions != versions {
            return Err(RunQueryError::StaleDocumentRef);
        }
        Ok(decoded)
    }
}

/// Reuses encodings within one page while keeping its range and versions fixed.
#[derive(Debug)]
pub(crate) struct PageDocumentReferences {
    range: ReferenceRange,
    versions: QueryVersions,
    references: BTreeMap<DocumentIdentity, DocumentRef>,
    #[cfg(test)]
    encodings: usize,
}
impl PageDocumentReferences {
    pub(crate) fn new(range: ReferenceRange, versions: &QueryVersions) -> Self {
        Self {
            range,
            versions: versions.clone(),
            references: BTreeMap::new(),
            #[cfg(test)]
            encodings: 0,
        }
    }
    pub(crate) fn attach(&mut self, segment: &mut TimelineSegment) -> Result<(), RunQueryError> {
        let Some(identity) = DocumentIdentity::from_segment(segment) else {
            segment.document_ref = None;
            return Ok(());
        };
        let reference = match self.references.entry(identity) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let reference = DocumentReference::issue(
                    entry.key().clone(),
                    self.range,
                    self.versions.clone(),
                )?;
                #[cfg(test)]
                {
                    self.encodings += 1;
                }
                entry.insert(reference)
            }
        };
        segment.document_ref = Some(reference.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extraction(document: &str) -> TitleExtraction {
        TitleExtraction {
            document: Some(document.into()),
            status: Some(ExtractionStatus::Extracted),
            ..Default::default()
        }
    }
    fn segment() -> TimelineSegment {
        TimelineSegment {
            start_utc_ms: 0,
            end_utc_ms: 1,
            duration_ms: 1,
            clock_segment_id: None,
            activity: quartz_protocol::Activity::Active,
            category: quartz_protocol::CategoryRef::Unclassified,
            category_source: quartz_protocol::CategorySource::Unclassified,
            app: Some(quartz_protocol::ApplicationRef {
                id: ApplicationId(1),
                exe_name: Some("editor.exe".into()),
                display_name: "Editor".into(),
                aumid: None,
            }),
            window_instance: None,
            title: Some("main.rs".into()),
            extraction: Some(extraction("main.rs")),
            document_ref: None,
        }
    }
    #[test]
    fn repeated_identity_is_encoded_once_per_page_and_every_reference_counts_towards_bytes() {
        use super::super::paging::{PageBudget, PagePosition};
        const SEGMENT_COUNT: usize = 1000;
        let range = ReferenceRange {
            start_utc_ms: 0,
            end_utc_ms: 1000,
        };
        let versions = QueryVersions("page-version".into());
        let mut references = PageDocumentReferences::new(range, &versions);
        let mut segments = vec![segment(); SEGMENT_COUNT];
        segments[1].extraction.as_mut().expect("extraction").dirty = Some(true);
        let page = PageBudget::new(&())
            .expect("budget")
            .page_with(segments, PagePosition::First, |segment| {
                references.attach(segment)
            })
            .expect("page");
        assert!(page.items.len() > 1);
        assert!(
            page.next.is_some(),
            "repeated reference strings still consume page bytes"
        );
        assert_eq!(
            references.encodings, 1,
            "one serialization and checksum for this identity"
        );
        let first = page.items[0].document_ref.as_ref().expect("reference");
        for segment in &page.items {
            assert_eq!(
                segment
                    .document_ref
                    .as_ref()
                    .expect("reference")
                    .as_str()
                    .as_bytes(),
                first.as_str().as_bytes()
            );
        }
        let encoded_bytes = serde_json::to_vec(&page.items).expect("page JSON").len();
        assert!(encoded_bytes < quartz_protocol::QUERY_PAGE_BYTES);
        let reference_bytes = serde_json::to_vec(first).expect("reference JSON").len();
        let mut without_references = page.items.clone();
        for segment in &mut without_references {
            segment.document_ref = None;
        }
        let without_reference_bytes = serde_json::to_vec(&without_references)
            .expect("null reference JSON")
            .len();
        let null_bytes = serde_json::to_vec(&Option::<DocumentRef>::None)
            .expect("null JSON")
            .len();
        assert_eq!(
            encoded_bytes - without_reference_bytes,
            page.items.len() * (reference_bytes - null_bytes)
        );
    }
    #[test]
    fn page_memoization_separates_identities_and_does_not_reuse_other_page_versions() {
        let range = ReferenceRange {
            start_utc_ms: 0,
            end_utc_ms: 1000,
        };
        let versions = QueryVersions("first-page-version".into());
        let mut references = PageDocumentReferences::new(range, &versions);
        let mut first = segment();
        references.attach(&mut first).expect("first identity");
        let mut other_project = segment();
        other_project
            .extraction
            .as_mut()
            .expect("extraction")
            .project = Some(String::new());
        references
            .attach(&mut other_project)
            .expect("other project");
        assert_eq!(references.encodings, 2);
        assert_ne!(first.document_ref, other_project.document_ref);
        let mut hidden = first.clone();
        hidden.title = None;
        hidden.extraction = Some(TitleExtraction::default());
        references.attach(&mut hidden).expect("hidden fields");
        assert!(hidden.document_ref.is_none());
        assert_eq!(references.encodings, 2);
        let mut next_page = PageDocumentReferences::new(range, &versions);
        let mut repeated = segment();
        next_page
            .attach(&mut repeated)
            .expect("same identity on another page");
        assert_eq!(next_page.encodings, 1);
        assert_eq!(first.document_ref, repeated.document_ref);
        let mut changed_page =
            PageDocumentReferences::new(range, &QueryVersions("changed-version".into()));
        changed_page
            .attach(&mut repeated)
            .expect("changed input versions");
        assert_ne!(first.document_ref, repeated.document_ref);
    }
    #[test]
    fn identity_uses_app_case_nfc_and_distinct_optional_components() {
        let first = extraction("café.rs");
        let identity = DocumentIdentity::new(ApplicationId(1), &first).expect("identity");
        assert_eq!(
            Some(identity.clone()),
            DocumentIdentity::new(ApplicationId(1), &extraction("cafe\u{301}.rs"))
        );
        assert_ne!(
            Some(identity.clone()),
            DocumentIdentity::new(ApplicationId(2), &first)
        );
        assert_ne!(
            Some(identity.clone()),
            DocumentIdentity::new(ApplicationId(1), &extraction("Café.rs"))
        );
        for field in ["project", "profile"] {
            let mut empty = first.clone();
            let mut named = first.clone();
            if field == "project" {
                empty.project = Some(String::new());
                named.project = Some("Quartz".into());
            } else {
                empty.profile = Some(String::new());
                named.profile = Some("Work".into());
            }
            assert_ne!(
                Some(identity.clone()),
                DocumentIdentity::new(ApplicationId(1), &empty)
            );
            assert_ne!(
                DocumentIdentity::new(ApplicationId(1), &empty),
                DocumentIdentity::new(ApplicationId(1), &named)
            );
        }
        let mut display = first;
        display.dirty = Some(true);
        display.normalized_title = Some("changed display".into());
        display.extractor_rule = Some("user.extraction/custom@2".into());
        assert_eq!(
            Some(identity),
            DocumentIdentity::new(ApplicationId(1), &display)
        );
        assert!(DocumentIdentity::new(ApplicationId(1), &extraction("")).is_none());
        for status in [
            None,
            Some(ExtractionStatus::Empty),
            Some(ExtractionStatus::Unknown),
        ] {
            display.status = status;
            assert!(DocumentIdentity::new(ApplicationId(1), &display).is_none());
        }
    }
    #[test]
    fn references_roundtrip_bind_versions_range_and_identity_and_reject_tampering() {
        let identity =
            DocumentIdentity::new(ApplicationId(1), &extraction("main.rs")).expect("identity");
        let versions = QueryVersions("snapshot".into());
        let range = ReferenceRange {
            start_utc_ms: 10,
            end_utc_ms: 20,
        };
        let reference =
            DocumentReference::issue(identity.clone(), range, versions.clone()).expect("reference");
        assert_eq!(
            reference,
            DocumentReference::issue(identity.clone(), range, versions.clone())
                .expect("deterministic")
        );
        assert_eq!(
            DocumentReference::validate(&reference, range, &versions)
                .expect("decode")
                .identity,
            identity
        );
        assert_eq!(
            serde_json::to_value(&reference).expect("wire"),
            reference.as_str()
        );
        assert!(!format!("{reference:?}").contains("main.rs"));
        let tampered = DocumentRef::from_encoded(reference.as_str().replace("main.rs", "other.rs"));
        assert_eq!(
            DocumentReference::validate(&tampered, range, &versions)
                .expect_err("tamper")
                .kind(),
            quartz_protocol::ErrorKind::BadRequest
        );
        assert_eq!(
            DocumentReference::validate(&reference, range, &QueryVersions("changed".into()))
                .expect_err("stale")
                .kind(),
            quartz_protocol::ErrorKind::StaleDocumentRef
        );
        let different_range = ReferenceRange {
            start_utc_ms: 11,
            ..range
        };
        assert_eq!(
            DocumentReference::validate(&reference, different_range, &versions)
                .expect_err("range")
                .kind(),
            quartz_protocol::ErrorKind::BadRequest
        );
        for invalid in [
            "",
            "doc:opaque",
            "quartz.document.v2:abc",
            "quartz.document.v1:abc:{}",
        ] {
            assert!(
                DocumentReference::validate(
                    &DocumentRef::from_encoded(invalid.into()),
                    range,
                    &versions
                )
                .is_err()
            );
        }
        assert_ne!(
            reference,
            DocumentReference::issue(identity, different_range, versions).expect("range reference")
        );
    }
    #[test]
    fn maximum_escaped_document_detail_cursor_fits_the_named_limit() {
        use quartz_domain::capture::limits;
        use quartz_protocol::{CursorQuery, DetailEntity, MAX_CURSOR_BYTES, QueryCursor};
        /// A SHA-256 query digest uses two hexadecimal digits per byte.
        const QUERY_DIGEST_HEX_DIGITS: usize = 64;
        let maximum = "\0".repeat(limits::TITLE_UTF16_UNITS);
        let identity = DocumentIdentity {
            app: ApplicationId(i64::MAX),
            document: maximum.clone(),
            project: Some(maximum.clone()),
            profile: Some(maximum),
        };
        let versions = QueryVersions("f".repeat(QUERY_DIGEST_HEX_DIGITS));
        let range = ReferenceRange {
            start_utc_ms: 0,
            end_utc_ms: 1,
        };
        let reference =
            DocumentReference::issue(identity, range, versions.clone()).expect("maximum reference");
        let cursor = QueryCursor::new(
            versions,
            CursorQuery::EntityDetail {
                entity: DetailEntity::Document {
                    document_ref: reference,
                },
                local_date: "2025-01-02".parse().expect("date"),
                reference_range: Some(range),
            },
            usize::MAX,
        );
        let encoded = serde_json::to_vec(&cursor).expect("document cursor JSON");
        assert!(
            encoded.len() <= MAX_CURSOR_BYTES,
            "largest escaped document fields fit: {}",
            encoded.len()
        );
        assert!(cursor.decode().is_ok());
    }
    #[test]
    fn document_reference_payload_keeps_range_fields_strict() {
        let range = ReferenceRange {
            start_utc_ms: 0,
            end_utc_ms: 1,
        };
        let versions = QueryVersions("v".into());
        let payload =
            serde_json::json!({"identity":{"app":1,"document":"doc","project":null,"profile":null},
            "range":{"start_utc_ms":0,"end_utc_ms":1,"extra":true},"versions":"v"})
            .to_string();
        let reference = DocumentRef::from_encoded(format!(
            "{REFERENCE_PREFIX}{}:{payload}",
            DocumentReference::checksum(&payload)
        ));
        assert_eq!(
            DocumentReference::validate(&reference, range, &versions)
                .expect_err("strict embedded range")
                .kind(),
            quartz_protocol::ErrorKind::BadRequest
        );
    }
}
