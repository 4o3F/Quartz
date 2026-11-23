//! Version inputs shared by snapshot cursors and day-topic invalidations.

use super::{RunQueryError, details::EntityCalendar, input::QueryRange};
use crate::store::versions::{QuerySettings, StoredVersions, UtcDays, VersionKey};
use quartz_domain::rules::registry::SnapshotHashes;
use quartz_protocol::{DayChanged, LocalDate, QueryVersions};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub(crate) struct VersionState {
    pub days: BTreeMap<VersionKey, i64>,
    pub settings: QuerySettings,
    pub rule_hashes: SnapshotHashes,
    pub time_zone: jiff::tz::TimeZone,
}
impl VersionState {
    pub(crate) fn from_stored(
        stored: StoredVersions,
        rule_hashes: SnapshotHashes,
    ) -> Result<Self, RunQueryError> {
        Ok(Self {
            days: stored.days,
            settings: stored.settings,
            rule_hashes,
            time_zone: jiff::tz::TimeZone::try_system()?,
        })
    }

    pub(crate) fn digest(&self, range: QueryRange) -> QueryVersions {
        let mut digest = Sha256::new();
        digest.update(super::QUERY_SEMANTICS_VERSION.to_le_bytes());
        digest.update(self.settings.metadata_version.to_le_bytes());
        digest.update(self.settings.day_start_offset.minutes().to_le_bytes());
        digest.update(range.start.to_le_bytes());
        digest.update(range.end.to_le_bytes());
        digest.update(self.rule_hashes.classification.as_str().as_bytes());
        digest.update(self.rule_hashes.extraction.as_str().as_bytes());
        digest.update(self.rule_hashes.privacy.as_str().as_bytes());
        digest.update(self.settings.stats_policy_version.to_le_bytes());
        digest.update(format!("{:?}", self.time_zone).as_bytes());
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
        let calendar = EntityCalendar::new(date, self.settings.day_start_offset)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::versions::{UtcDay, VersionSource};
    use quartz_domain::rules::ContentHash;

    #[test]
    fn document_versions_cover_rules_metadata_policy_zone_range_and_intersecting_fact_sources() {
        let range = QueryRange {
            start: 0,
            end: 60_000,
        };
        let state = VersionState {
            days: BTreeMap::new(),
            settings: QuerySettings {
                day_start_offset: super::super::calendar::DayStartOffset::MIDNIGHT,
                metadata_version: 0,
                stats_policy_version: 0,
            },
            rule_hashes: SnapshotHashes {
                classification: ContentHash::of("classification"),
                extraction: ContentHash::of("extraction"),
                privacy: ContentHash::of("privacy"),
            },
            time_zone: jiff::tz::TimeZone::UTC,
        };
        let initial = state.digest(range);
        let mut changed = state.clone();
        changed.rule_hashes.extraction = ContentHash::of("changed");
        assert_ne!(initial, changed.digest(range));
        changed = state.clone();
        changed.rule_hashes.privacy = ContentHash::of("changed");
        assert_ne!(initial, changed.digest(range));
        changed = state.clone();
        changed.rule_hashes.classification = ContentHash::of("changed");
        assert_ne!(initial, changed.digest(range));
        changed = state.clone();
        changed.settings.metadata_version += 1;
        assert_ne!(initial, changed.digest(range));
        changed = state.clone();
        changed.settings.stats_policy_version += 1;
        assert_ne!(initial, changed.digest(range));
        changed = state.clone();
        changed.settings.day_start_offset =
            super::super::calendar::DayStartOffset::try_from(60).expect("offset");
        assert_ne!(initial, changed.digest(range));
        changed = state.clone();
        changed.time_zone = jiff::tz::TimeZone::get("Asia/Shanghai").expect("zone");
        assert_ne!(initial, changed.digest(range));
        assert_ne!(
            initial,
            state.digest(QueryRange {
                end: 60_001,
                ..range
            })
        );
        for source in [VersionSource(1), VersionSource(2), VersionSource(3)] {
            changed = state.clone();
            changed.days.insert(
                VersionKey {
                    source,
                    day: UtcDay(1),
                },
                1,
            );
            assert_eq!(
                initial,
                changed.digest(range),
                "unrelated dates retain references"
            );
            changed.days.insert(
                VersionKey {
                    source,
                    day: UtcDay(0),
                },
                1,
            );
            assert_ne!(
                initial,
                changed.digest(range),
                "every intersecting source invalidates references"
            );
        }
    }
}
