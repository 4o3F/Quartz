//! Version inputs shared by snapshot cursors and day-topic invalidations.

use super::{RunQueryError, calendar::DayCalendar, input::QueryRange};
use crate::store::versions::{QuerySettings, StoredVersions, UtcDays, VersionKey};
use quartz_domain::rules::ContentHash;
use quartz_protocol::{DayChanged, LocalDate, QueryVersions};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub(crate) struct VersionState {
    pub days: BTreeMap<VersionKey, i64>,
    pub settings: QuerySettings,
    pub classification_hash: ContentHash,
}
impl VersionState {
    pub(crate) fn from_stored(stored: StoredVersions, classification_hash: ContentHash) -> Self {
        Self {
            days: stored.days,
            settings: stored.settings,
            classification_hash,
        }
    }

    pub(crate) fn digest(&self, range: QueryRange) -> QueryVersions {
        let mut digest = Sha256::new();
        digest.update(super::QUERY_SEMANTICS_VERSION.to_le_bytes());
        digest.update(self.settings.metadata_version.to_le_bytes());
        digest.update(self.settings.day_start_offset_min.to_le_bytes());
        digest.update(range.start.to_le_bytes());
        digest.update(range.end.to_le_bytes());
        digest.update(self.classification_hash.as_str().as_bytes());
        let days = UtcDays::covering(range);
        for (key, version) in &self.days {
            if days.contains(&key.day) {
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

#[cfg(any(windows, test))]
mod publisher;
#[cfg(any(windows, test))]
pub(crate) use publisher::{VersionChannel, VersionCommand, VersionPort, VersionPublisher};
