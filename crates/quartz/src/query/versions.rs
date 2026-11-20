//! Version inputs shared by snapshot cursors and day-topic invalidations.

use super::{RunQueryError, calendar::DayCalendar, input::QueryRange};
use crate::store::{facts::UTC_DAY_MS, query::QuerySettings};
use quartz_protocol::{DayChanged, LocalDate, QueryVersions};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct UtcDay(pub i64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VersionSource(pub i32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VersionKey {
    pub source: VersionSource,
    pub day: UtcDay,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct FactVersion {
    pub key: VersionKey,
    pub version: i64,
}
#[derive(Debug, Clone)]
pub(crate) struct VersionState {
    pub days: BTreeMap<VersionKey, i64>,
    pub settings: QuerySettings,
    pub classification_hash: String,
}
impl VersionState {
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            days: BTreeMap::new(),
            settings: QuerySettings {
                day_start_offset_min: super::calendar::DEFAULT_DAY_START_OFFSET_MIN,
                metadata_version: 0,
            },
            classification_hash: String::new(),
        }
    }

    pub(crate) fn digest(&self, range: QueryRange) -> QueryVersions {
        let mut digest = Sha256::new();
        digest.update(super::QUERY_SEMANTICS_VERSION.to_le_bytes());
        digest.update(self.settings.metadata_version.to_le_bytes());
        digest.update(self.settings.day_start_offset_min.to_le_bytes());
        digest.update(range.start.to_le_bytes());
        digest.update(range.end.to_le_bytes());
        digest.update(self.classification_hash.as_bytes());
        let first = range.start.div_euclid(UTC_DAY_MS);
        let last = (range.end - 1).div_euclid(UTC_DAY_MS);
        for (key, version) in &self.days {
            if (first..=last).contains(&key.day.0) {
                digest.update(key.source.0.to_le_bytes());
                digest.update(key.day.0.to_le_bytes());
                digest.update(version.to_le_bytes());
            }
        }
        QueryVersions(quartz_domain::text::encode_hex(&digest.finalize()))
    }
    pub(crate) fn day(&self, date: LocalDate) -> Result<DayChanged, RunQueryError> {
        let calendar = DayCalendar::new(date, self.settings.day_start_offset_min)?;
        Ok(DayChanged {
            date,
            versions: self.digest(calendar.range),
        })
    }
}
