//! Incremental ordering and wire conversion of raw facts.

use super::RunQueryError;
use crate::store::facts::Fact;
use quartz_protocol::{QueryVersions, facts::FactRecord};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io};

/// Keeps conversion and hashing bounded independently of the SQL read batch.
pub(crate) const FINALIZE_BATCH_RECORDS: usize = 32;
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FinalizeProgress {
    More,
    Complete,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FactOrder {
    start_utc_ms: i64,
    kind: &'static str,
    id: i64,
}
#[derive(Debug, Default)]
pub(super) struct OrderedFacts(BTreeMap<FactOrder, Fact>);
impl OrderedFacts {
    pub(super) fn extend(&mut self, facts: Vec<Fact>) {
        for fact in facts {
            self.0.insert(
                FactOrder {
                    start_utc_ms: fact.start_utc_ms,
                    kind: fact.kind.label(),
                    id: fact.id,
                },
                fact,
            );
        }
    }
    pub(super) fn finalize(self, versions: &QueryVersions) -> FinalizeFacts {
        let mut digest = Sha256::new();
        digest.update(versions.0.as_bytes());
        FinalizeFacts {
            remaining: self.0.into_values(),
            records: Vec::new(),
            digest: HashJson(digest),
        }
    }
}
#[derive(Debug)]
pub(super) struct FinalizeFacts {
    remaining: std::collections::btree_map::IntoValues<FactOrder, Fact>,
    records: Vec<FactRecord>,
    digest: HashJson,
}
impl FinalizeFacts {
    pub(super) fn advance(&mut self) -> Result<FinalizeProgress, RunQueryError> {
        for _ in 0..FINALIZE_BATCH_RECORDS {
            let Some(fact) = self.remaining.next() else {
                return Ok(FinalizeProgress::Complete);
            };
            // Drop the stored representation before retaining its wire replacement.
            let record = fact.record()?;
            drop(fact);
            serde_json::to_writer(&mut self.digest, &record)?;
            self.records.push(record);
        }
        Ok(if self.remaining.len() == 0 {
            FinalizeProgress::Complete
        } else {
            FinalizeProgress::More
        })
    }
    pub(super) fn finish(self) -> FinalizedFacts {
        FinalizedFacts {
            records: self.records,
            versions: QueryVersions(quartz_domain::text::encode_hex(&self.digest.0.finalize())),
        }
    }
}
#[derive(Debug)]
pub(super) struct FinalizedFacts {
    pub(super) records: Vec<FactRecord>,
    pub(super) versions: QueryVersions,
}
#[derive(Debug)]
struct HashJson(Sha256);
impl io::Write for HashJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
