//! Value-only subtraction of half-open ranges and deterministic remainder identities.

use quartz_domain::facts::QueryRange;
use quartz_protocol::AnnotationId;
use sha2::{Digest, Sha256};

/// Surviving portions of an older range, before and after the writing range.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::store) struct Remainders {
    pub left: Option<QueryRange>,
    pub right: Option<QueryRange>,
}
impl Remainders {
    /// Both input ranges must be nonempty and overlap.
    pub(in crate::store) fn subtract(old: QueryRange, writing: QueryRange) -> Self {
        Self {
            left: (old.start < writing.start).then_some(QueryRange {
                start: old.start,
                end: writing.start,
            }),
            right: (old.end > writing.end).then_some(QueryRange {
                start: writing.end,
                end: old.end,
            }),
        }
    }
}

/// UUIDv8 uses the first 128 SHA-256 bits, with RFC 9562 version and variant bits.
const UUID_BYTES: usize = 16;
/// Hexadecimal offsets of the 8-4-4-4-12 UUID text groups.
const UUID_GROUP_ENDS: [usize; 4] = [8, 12, 16, 20];
const VERSION_INDEX: usize = 6;
const VARIANT_INDEX: usize = 8;
const UUID_V8: u8 = 0x80;
const VERSION_PAYLOAD: u8 = 0x0f;
const RFC_VARIANT: u8 = 0x80;
const VARIANT_PAYLOAD: u8 = 0x3f;

fn format_uuid_v8(hash: &[u8]) -> AnnotationId {
    let mut bytes = [0; UUID_BYTES];
    bytes.copy_from_slice(&hash[..UUID_BYTES]);
    bytes[VERSION_INDEX] = (bytes[VERSION_INDEX] & VERSION_PAYLOAD) | UUID_V8;
    bytes[VARIANT_INDEX] = (bytes[VARIANT_INDEX] & VARIANT_PAYLOAD) | RFC_VARIANT;
    let hex = quartz_domain::text::encode_hex(&bytes);
    let [first, second, third, fourth] = UUID_GROUP_ENDS;
    let text = format!(
        "{}-{}-{}-{}-{}",
        &hex[..first],
        &hex[first..second],
        &hex[second..third],
        &hex[third..fourth],
        &hex[fourth..]
    );
    AnnotationId(text.parse().expect("formatted UUIDv8 is valid"))
}

#[derive(Debug)]
pub(super) struct SplitIdentity<'id> {
    pub old: &'id AnnotationId,
    pub writing: &'id AnnotationId,
    pub old_range: QueryRange,
    pub writing_range: QueryRange,
}
impl SplitIdentity<'_> {
    pub(super) fn derive(&self) -> AnnotationId {
        let mut digest = Sha256::new();
        digest.update(b"quartz.annotations.right.v1\0");
        digest.update(self.old.0.to_string().as_bytes());
        digest.update(self.writing.0.to_string().as_bytes());
        digest.update(self.old_range.start.to_le_bytes());
        digest.update(self.old_range.end.to_le_bytes());
        digest.update(self.writing_range.start.to_le_bytes());
        digest.update(self.writing_range.end.to_le_bytes());
        format_uuid_v8(&digest.finalize())
    }
}

/// Deterministic UUIDv8 for the right remainder when a date-range delete splits an annotation.
#[derive(Debug)]
pub(in crate::store) struct DeleteSplitIdentity<'id> {
    pub old: &'id AnnotationId,
    pub old_range: QueryRange,
    pub deleted_range: QueryRange,
}
impl DeleteSplitIdentity<'_> {
    pub(in crate::store) fn derive(&self) -> AnnotationId {
        let mut digest = Sha256::new();
        digest.update(b"quartz.annotations.delete_right.v1\0");
        digest.update(self.old.0.to_string().as_bytes());
        digest.update(self.old_range.start.to_le_bytes());
        digest.update(self.old_range.end.to_le_bytes());
        digest.update(self.deleted_range.start.to_le_bytes());
        digest.update(self.deleted_range.end.to_le_bytes());
        format_uuid_v8(&digest.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_identity_has_a_stable_sha256_uuidv8_vector() {
        let old = AnnotationId(
            "00000000-0000-4000-8000-000000000001"
                .parse()
                .expect("old UUID"),
        );
        let writing = AnnotationId(
            "00000000-0000-4000-8000-000000000002"
                .parse()
                .expect("writing UUID"),
        );
        assert_eq!(
            SplitIdentity {
                old: &old,
                writing: &writing,
                old_range: QueryRange { start: 0, end: 100 },
                writing_range: QueryRange { start: 40, end: 60 },
            }
            .derive()
            .0
            .to_string(),
            "c0a8826e-9b13-861c-9e6a-744206b50078"
        );
    }

    #[test]
    fn each_split_boundary_changes_the_remainder_identity() {
        let old = AnnotationId(
            "00000000-0000-4000-8000-000000000001"
                .parse()
                .expect("old UUID"),
        );
        let writing = AnnotationId(
            "00000000-0000-4000-8000-000000000002"
                .parse()
                .expect("writing UUID"),
        );
        let split = SplitIdentity {
            old: &old,
            writing: &writing,
            old_range: QueryRange { start: 0, end: 100 },
            writing_range: QueryRange { start: 40, end: 60 },
        };
        let expected = split.derive();
        for old_range in [
            QueryRange {
                start: -1,
                end: 100,
            },
            QueryRange { start: 0, end: 101 },
        ] {
            assert_ne!(SplitIdentity { old_range, ..split }.derive(), expected);
        }
        for writing_range in [
            QueryRange { start: 39, end: 60 },
            QueryRange { start: 40, end: 61 },
        ] {
            assert_ne!(
                SplitIdentity {
                    writing_range,
                    ..split
                }
                .derive(),
                expected
            );
        }
    }

    #[test]
    fn subtracts_each_overlap_shape_without_storage() {
        let old = QueryRange { start: 10, end: 50 };
        assert_eq!(
            Remainders::subtract(old, QueryRange { start: 0, end: 20 }),
            Remainders {
                left: None,
                right: Some(QueryRange { start: 20, end: 50 })
            }
        );
        assert_eq!(
            Remainders::subtract(old, QueryRange { start: 40, end: 60 }),
            Remainders {
                left: Some(QueryRange { start: 10, end: 40 }),
                right: None
            }
        );
        assert_eq!(
            Remainders::subtract(old, QueryRange { start: 0, end: 60 }),
            Remainders {
                left: None,
                right: None
            }
        );
        assert_eq!(
            Remainders::subtract(old, QueryRange { start: 20, end: 40 }),
            Remainders {
                left: Some(QueryRange { start: 10, end: 20 }),
                right: Some(QueryRange { start: 40, end: 50 })
            }
        );
    }

    #[test]
    fn delete_split_identity_is_deterministic_uuidv8_and_distinct_from_original() {
        let old = AnnotationId(
            "00000000-0000-4000-8000-000000000001"
                .parse()
                .expect("old UUID"),
        );
        let split = DeleteSplitIdentity {
            old: &old,
            old_range: QueryRange { start: 0, end: 100 },
            deleted_range: QueryRange { start: 40, end: 60 },
        };
        let derived = split.derive();
        assert_eq!(derived, split.derive());
        assert_ne!(derived, old);
        assert_ne!(
            DeleteSplitIdentity {
                deleted_range: QueryRange { start: 40, end: 61 },
                ..split
            }
            .derive(),
            derived
        );
    }
}
