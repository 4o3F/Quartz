//! Version inputs shared by snapshot cursors and day-topic invalidations.

use super::{RunQueryError, details::EntityCalendar, input::QueryRange};
use crate::store::versions::{QuerySettings, StoredVersions, UtcDays, VersionKey, VersionSource};
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
struct VersionDigest {
    versions: QueryVersions,
    #[cfg(test)]
    visited_entries: usize,
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
            time_zone: RunQueryError::system_time_zone(jiff::tz::TimeZone::try_system)?,
        })
    }

    pub(crate) fn digest(&self, range: QueryRange) -> Result<QueryVersions, RunQueryError> {
        self.digest_in_zone(range, &self.time_zone)
    }
    fn digest_in_zone(
        &self,
        range: QueryRange,
        zone: &jiff::tz::TimeZone,
    ) -> Result<QueryVersions, RunQueryError> {
        self.digest_entries(range, zone, self.entries(range))
            .map(|digest| digest.versions)
    }
    fn entries(&self, range: QueryRange) -> impl Iterator<Item = (&VersionKey, &i64)> {
        let days = UtcDays::covering(range);
        VersionSource::all().flat_map(move |source| {
            self.days.range(
                VersionKey {
                    source,
                    day: *days.start(),
                }..=VersionKey {
                    source,
                    day: *days.end(),
                },
            )
        })
    }
    fn digest_entries<'a>(
        &self,
        range: QueryRange,
        zone: &jiff::tz::TimeZone,
        entries: impl Iterator<Item = (&'a VersionKey, &'a i64)>,
    ) -> Result<VersionDigest, RunQueryError> {
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
        let zone = jiff::fmt::temporal::DateTimePrinter::new()
            .time_zone_to_string(zone)
            .map_err(|source| RunQueryError::EncodeTimeZoneError { source })?;
        digest.update(zone.as_bytes());
        let days = UtcDays::covering(range);
        #[cfg(test)]
        let mut visited_entries = 0;
        for (key, version) in entries {
            #[cfg(test)]
            {
                visited_entries += 1;
            }
            if !days.contains(&key.day) {
                continue;
            }
            digest.update(key.source.0.to_le_bytes());
            digest.update(key.day.0.to_le_bytes());
            digest.update(version.to_le_bytes());
        }
        Ok(VersionDigest {
            versions: QueryVersions(quartz_domain::text::encode_hex(&digest.finalize())),
            #[cfg(test)]
            visited_entries,
        })
    }
    pub(crate) fn day(&self, date: LocalDate) -> Result<DayChanged, RunQueryError> {
        self.day_in_zone(date, &self.time_zone)
    }
    fn day_in_zone(
        &self,
        date: LocalDate,
        zone: &jiff::tz::TimeZone,
    ) -> Result<DayChanged, RunQueryError> {
        let calendar = EntityCalendar::in_zone(date, self.settings.day_start_offset, zone.clone())?;
        Ok(DayChanged {
            date,
            versions: self.digest_in_zone(calendar.range, zone)?,
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
        let initial = state.digest(range).expect("digest");
        let mut changed = state.clone();
        changed.rule_hashes.extraction = ContentHash::of("changed");
        assert_ne!(initial, changed.digest(range).expect("digest"));
        changed = state.clone();
        changed.rule_hashes.privacy = ContentHash::of("changed");
        assert_ne!(initial, changed.digest(range).expect("digest"));
        changed = state.clone();
        changed.rule_hashes.classification = ContentHash::of("changed");
        assert_ne!(initial, changed.digest(range).expect("digest"));
        changed = state.clone();
        changed.settings.metadata_version += 1;
        assert_ne!(initial, changed.digest(range).expect("digest"));
        changed = state.clone();
        changed.settings.stats_policy_version += 1;
        assert_ne!(initial, changed.digest(range).expect("digest"));
        changed = state.clone();
        changed.settings.day_start_offset =
            super::super::calendar::DayStartOffset::try_from(60).expect("offset");
        assert_ne!(initial, changed.digest(range).expect("digest"));
        changed = state.clone();
        changed.time_zone = jiff::tz::TimeZone::get("Asia/Shanghai").expect("zone");
        assert_ne!(initial, changed.digest(range).expect("digest"));
        assert_ne!(
            initial,
            state
                .digest(QueryRange {
                    end: 60_001,
                    ..range
                })
                .expect("digest")
        );
        for source in VersionSource::all() {
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
                changed.digest(range).expect("digest"),
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
                changed.digest(range).expect("digest"),
                "every intersecting source invalidates references"
            );
        }
    }
}
