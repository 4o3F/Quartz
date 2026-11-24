//! The database thread publishes digests for the dates registered by I/O.
use super::VersionState;
use crate::query::RunQueryError;
use crate::store::Store;
use crossbeam_channel::{Receiver, Sender};
use quartz_domain::rules::registry::SnapshotCell;
use quartz_protocol::{DayChanged, ErrorKind, LocalDate, ProtocolError};
use std::collections::{BTreeMap, BTreeSet};
use tokio::sync::{oneshot, watch};

/// Version requests are bounded independently of capture writes and query jobs.
const VERSION_CHANNEL_CAPACITY: usize = 8;
#[derive(Debug, Clone, Default)]
pub(crate) struct PublishedVersions {
    pub days: BTreeMap<LocalDate, DayChanged>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum VersionSubscriber {
    Registry,
    Tray,
}
pub(crate) struct VersionCommand {
    subscriber: VersionSubscriber,
    dates: BTreeSet<LocalDate>,
    reply: oneshot::Sender<Result<PublishedVersions, ProtocolError>>,
}
#[derive(Clone)]
pub(crate) struct VersionPort {
    requests: Sender<VersionCommand>,
    pub updates: watch::Receiver<PublishedVersions>,
}
impl VersionPort {
    pub(crate) async fn set_days(
        &self,
        dates: BTreeSet<LocalDate>,
    ) -> Result<PublishedVersions, ProtocolError> {
        self.subscribe(VersionSubscriber::Registry, dates).await
    }
    pub(crate) async fn set_summary_day(
        &self,
        date: LocalDate,
    ) -> Result<PublishedVersions, ProtocolError> {
        self.subscribe(VersionSubscriber::Tray, BTreeSet::from([date]))
            .await
    }
    async fn subscribe(
        &self,
        subscriber: VersionSubscriber,
        dates: BTreeSet<LocalDate>,
    ) -> Result<PublishedVersions, ProtocolError> {
        let (reply, received) = oneshot::channel();
        self.requests
            .try_send(VersionCommand {
                subscriber,
                dates,
                reply,
            })
            .map_err(|error| {
                ProtocolError::new(match error {
                    crossbeam_channel::TrySendError::Full(_) => ErrorKind::Busy,
                    crossbeam_channel::TrySendError::Disconnected(_) => ErrorKind::Internal,
                })
            })?;
        received
            .await
            .map_err(|_| ProtocolError::new(ErrorKind::Internal))?
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicationState {
    Current,
    Retry,
}
pub(crate) struct VersionPublisher {
    subscriptions: BTreeMap<VersionSubscriber, BTreeSet<LocalDate>>,
    state: VersionState,
    snapshots: SnapshotCell,
    publication: PublicationState,
    pub requests: Receiver<VersionCommand>,
    published: watch::Sender<PublishedVersions>,
}
impl VersionPublisher {
    pub(crate) fn open(
        store: &mut Store,
        snapshots: SnapshotCell,
    ) -> Result<VersionChannel, RunQueryError> {
        let state =
            VersionState::from_stored(store.version_state()?, snapshots.load().hashes().clone())?;
        let (requests, received) = crossbeam_channel::bounded(VERSION_CHANNEL_CAPACITY);
        let (published, updates) = watch::channel(PublishedVersions::default());
        let publisher = Self {
            subscriptions: BTreeMap::new(),
            state,
            snapshots,
            publication: PublicationState::Current,
            requests: received,
            published,
        };
        let port = VersionPort { requests, updates };
        Ok(VersionChannel { publisher, port })
    }
    pub(crate) fn receive(&mut self, command: VersionCommand) {
        self.receive_days(
            command,
            RunQueryError::system_time_zone(jiff::tz::TimeZone::try_system),
            VersionState::day_in_zone,
        );
    }
    fn receive_days(
        &mut self,
        command: VersionCommand,
        zone: Result<jiff::tz::TimeZone, RunQueryError>,
        day: impl Fn(&VersionState, LocalDate, &jiff::tz::TimeZone) -> Result<DayChanged, RunQueryError>,
    ) {
        let mut subscriptions = self.subscriptions.clone();
        subscriptions.insert(command.subscriber, command.dates);
        let dates: BTreeSet<_> = subscriptions.values().flatten().copied().collect();
        let result = zone
            .and_then(|zone| {
                let mut days = BTreeMap::new();
                for date in dates {
                    days.insert(date, day(&self.state, date, &zone)?);
                }
                self.subscriptions = subscriptions;
                self.state.time_zone = zone;
                let published = PublishedVersions { days };
                self.published.send_replace(published.clone());
                self.publication = PublicationState::Current;
                Ok(published)
            })
            .map_err(|error| ProtocolError::new(error.kind()));
        let _ = command.reply.send(result);
    }
    pub(crate) fn publish(
        &mut self,
        store: &mut Store,
    ) -> Result<Option<crate::query::calendar::DayStartOffset>, RunQueryError> {
        self.publish_days(store, VersionState::day)
    }
    fn publish_days(
        &mut self,
        store: &mut Store,
        day: impl Fn(&VersionState, LocalDate) -> Result<DayChanged, RunQueryError>,
    ) -> Result<Option<crate::query::calendar::DayStartOffset>, RunQueryError> {
        let hashes = self.snapshots.load().hashes().clone();
        let time_zone = RunQueryError::system_time_zone(jiff::tz::TimeZone::try_system)?;
        let settings = store.query_settings()?;
        let changes = store.take_committed_versions();
        if self.publication == PublicationState::Current
            && changes.facts.is_empty()
            && changes.metadata_version.is_none()
            && self.state.rule_hashes == hashes
            && self.state.time_zone == time_zone
            && self.state.settings.stats_policy_version == settings.stats_policy_version
            && self.state.settings.day_start_offset == settings.day_start_offset
        {
            return Ok(None);
        }
        let offset = settings.day_start_offset;
        let changed_offset = (offset != self.state.settings.day_start_offset).then_some(offset);
        self.state.settings.day_start_offset = offset;
        for fact in changes.facts {
            self.state.days.insert(fact.key, fact.version);
        }
        if let Some(version) = changes.metadata_version {
            self.state.settings.metadata_version = version;
        }
        self.state.rule_hashes = hashes;
        self.state.time_zone = time_zone;
        self.state.settings.stats_policy_version = settings.stats_policy_version;
        self.publication = PublicationState::Retry;
        let dates: Vec<_> = self.published.borrow().days.keys().copied().collect();
        let mut days = BTreeMap::new();
        for date in dates {
            match day(&self.state, date) {
                Ok(changed) => {
                    days.insert(date, changed);
                }
                Err(error) => {
                    tracing::warn!(?error, "could not refresh day subscriptions");
                    return Ok(changed_offset);
                }
            }
        }
        self.published.send_replace(PublishedVersions { days });
        self.publication = PublicationState::Current;
        Ok(changed_offset)
    }
}
pub(crate) struct VersionChannel {
    pub publisher: VersionPublisher,
    pub port: VersionPort,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        query::test_support::QueryFixture,
        versions::{UtcDay, VersionKey, VersionSource},
    };
    use quartz_domain::rules::registry::RuleRegistry;

    #[test]
    fn successful_subscription_clears_retry_without_republishing_unchanged_inputs() {
        let mut fixture = QueryFixture::new(0);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel {
            mut publisher,
            mut port,
        } = VersionPublisher::open(&mut fixture.store, snapshots).expect("publisher");
        let date = "2025-01-02".parse().expect("date");
        let first = set_days(&mut publisher, BTreeSet::from([date]));
        fixture.bump_fact_version();
        publisher
            .publish_days(&mut fixture.store, |_, _| Err(RunQueryError::InvalidRange))
            .expect("failed projection retains retry");
        let fresh = set_days(&mut publisher, BTreeSet::from([date]));
        assert_ne!(fresh.days[&date].versions, first.days[&date].versions);
        port.updates.borrow_and_update();
        let projected = std::cell::Cell::new(0);
        publisher
            .publish_days(&mut fixture.store, |state, date| {
                projected.set(projected.get() + 1);
                state.day(date)
            })
            .expect("unchanged publication");
        assert_eq!(
            projected.get(),
            0,
            "subscription already published every date"
        );
        assert!(!port.updates.has_changed().expect("watch open"));
    }
    #[test]
    fn failed_candidate_subscription_preserves_installed_dates_and_timezone() {
        let mut fixture = QueryFixture::new(0);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel {
            mut publisher,
            port,
        } = VersionPublisher::open(&mut fixture.store, snapshots).expect("publisher");
        let date: LocalDate = "2025-01-02".parse().expect("date");
        let first = set_days(&mut publisher, BTreeSet::from([date]));
        let subscriptions = publisher.subscriptions.clone();
        let zone = publisher.state.time_zone.clone();
        let (reply, received) = oneshot::channel();
        publisher.receive_days(
            VersionCommand {
                subscriber: VersionSubscriber::Registry,
                dates: BTreeSet::from([date.tomorrow().expect("tomorrow")]),
                reply,
            },
            Ok(jiff::tz::TimeZone::UTC),
            |_, _, _| Err(RunQueryError::InvalidRange),
        );
        assert!(received.blocking_recv().expect("reply").is_err());
        assert_eq!(publisher.subscriptions, subscriptions);
        assert_eq!(publisher.state.time_zone, zone);
        assert_eq!(port.updates.borrow().days, first.days);
    }
    #[test]
    fn subscription_digest_uses_one_captured_timezone_for_calendar_and_hash() {
        let mut fixture = QueryFixture::new(0);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel { mut publisher, .. } =
            VersionPublisher::open(&mut fixture.store, snapshots).expect("publisher");
        let date: LocalDate = "2025-01-02".parse().expect("date");
        for zone in [
            jiff::tz::TimeZone::UTC,
            jiff::tz::TimeZone::get("Asia/Shanghai").expect("zone"),
            jiff::tz::TimeZone::get("America/Los_Angeles").expect("zone"),
        ] {
            publisher.state.time_zone = zone.clone();
            let calendar = crate::query::details::EntityCalendar::in_zone(
                date,
                publisher.state.settings.day_start_offset,
                zone.clone(),
            )
            .expect("calendar");
            assert_eq!(
                publisher.state.day(date).expect("day").versions,
                publisher.state.digest(calendar.range).expect("digest")
            );
            let (reply, received) = oneshot::channel();
            publisher.receive_days(
                VersionCommand {
                    subscriber: VersionSubscriber::Registry,
                    dates: BTreeSet::from([date]),
                    reply,
                },
                Ok(zone.clone()),
                VersionState::day_in_zone,
            );
            let subscribed = received
                .blocking_recv()
                .expect("reply")
                .expect("subscription");
            assert_eq!(
                subscribed.days[&date].versions,
                publisher.state.digest(calendar.range).expect("digest")
            );
            assert_eq!(publisher.state.time_zone, zone);
        }
    }
    #[test]
    fn bounded_digest_matches_full_scan_and_visits_only_intersecting_entries() {
        let mut fixture = QueryFixture::new(0);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel { mut publisher, .. } =
            VersionPublisher::open(&mut fixture.store, snapshots).expect("publisher");
        const HISTORY_DAYS: i64 = 10_000;
        for source in VersionSource::all() {
            for day in 0..HISTORY_DAYS {
                publisher.state.days.insert(
                    VersionKey {
                        source,
                        day: UtcDay(day),
                    },
                    day,
                );
            }
        }
        let sources: Vec<_> = VersionSource::all().collect();
        assert!(
            sources.windows(2).all(|pair| pair[0] < pair[1]),
            "source order must match BTreeMap order"
        );
        for source in [VersionSource(1), VersionSource(3), VersionSource(5)] {
            publisher.state.days.insert(
                VersionKey {
                    source,
                    day: UtcDay(-1),
                },
                7,
            );
        }
        publisher.state.days.insert(
            VersionKey {
                source: VersionSource(1),
                day: UtcDay(-3),
            },
            8,
        );
        publisher.state.days.remove(&VersionKey {
            source: VersionSource(2),
            day: UtcDay(1),
        });
        publisher.state.days.remove(&VersionKey {
            source: VersionSource(4),
            day: UtcDay(2),
        });
        struct Case {
            start: i64,
            end: i64,
            entries: usize,
        }
        let day_ms = quartz_domain::capture::limits::UTC_DAY_MS;
        for case in [
            Case {
                start: 0,
                end: day_ms,
                entries: 5,
            },
            Case {
                start: -3 * day_ms + 1,
                end: 0,
                entries: 4,
            },
            Case {
                start: -2 * day_ms,
                end: 3 * day_ms,
                entries: 16,
            },
            Case {
                start: day_ms + 1,
                end: 2 * day_ms,
                entries: 4,
            },
            Case {
                start: -4 * day_ms,
                end: -3 * day_ms,
                entries: 0,
            },
            Case {
                start: HISTORY_DAYS * day_ms,
                end: (HISTORY_DAYS + 1) * day_ms,
                entries: 0,
            },
        ] {
            let range = crate::query::input::QueryRange {
                start: case.start,
                end: case.end,
            };
            let full = publisher
                .state
                .digest_entries(
                    range,
                    &publisher.state.time_zone,
                    publisher.state.days.iter(),
                )
                .expect("reference digest");
            let bounded = publisher
                .state
                .digest_entries(
                    range,
                    &publisher.state.time_zone,
                    publisher.state.entries(range),
                )
                .expect("bounded digest");
            assert_eq!(bounded.versions, full.versions, "range {range:?}");
            assert_eq!(bounded.visited_entries, case.entries, "range {range:?}");
            assert_eq!(full.visited_entries, publisher.state.days.len());
        }
    }
    #[test]
    fn failed_calendar_projection_keeps_subscriptions_and_delivers_committed_offset() {
        let mut fixture = QueryFixture::new(4);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel {
            mut publisher,
            port,
        } = VersionPublisher::open(&mut fixture.store, snapshots).expect("publisher");
        let date = "2025-01-02".parse().expect("date");
        let first = set_days(&mut publisher, BTreeSet::from([date]));
        fixture
            .store
            .test_day_offset_write(240)
            .expect("committed offset");
        let offset = publisher
            .publish_days(&mut fixture.store, |_, _| {
                Err(jiff::tz::TimeZone::get("Invalid/InjectedFailure")
                    .expect_err("injected zone failure")
                    .into())
            })
            .expect("calendar failure does not stop database publication");
        assert_eq!(
            offset,
            Some(crate::query::calendar::DayStartOffset::try_from(240).expect("offset"))
        );
        assert_eq!(
            port.updates.borrow().days[&date],
            first.days[&date],
            "keep the previous subscription snapshot"
        );
        publisher
            .publish(&mut fixture.store)
            .expect("retry without another input change");
        assert_ne!(
            port.updates.borrow().days[&date].versions,
            first.days[&date].versions
        );
    }

    fn set_days(publisher: &mut VersionPublisher, dates: BTreeSet<LocalDate>) -> PublishedVersions {
        let (reply, received) = oneshot::channel();
        publisher.receive(VersionCommand {
            subscriber: VersionSubscriber::Registry,
            dates,
            reply,
        });
        received
            .blocking_recv()
            .expect("version reply")
            .expect("valid dates")
    }
    #[test]
    fn publication_tracks_subscribed_days_and_invalidates_after_commits() {
        let mut fixture = QueryFixture::new(4);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel {
            mut publisher,
            port,
        } = VersionPublisher::open(&mut fixture.store, snapshots).expect("publisher");
        // Historical versions stay on the database thread even when I/O watches a single date.
        const HISTORICAL_DAYS: i64 = 1000;
        for day in 0..HISTORICAL_DAYS {
            publisher.state.days.insert(
                VersionKey {
                    source: VersionSource(1),
                    day: UtcDay(day),
                },
                1,
            );
        }
        assert!(port.updates.borrow().days.is_empty());
        let date = "2025-01-02".parse().expect("date");
        let first = set_days(&mut publisher, BTreeSet::from([date]));
        assert_eq!(first.days.len(), 1);
        assert_eq!(port.updates.borrow().days.len(), 1);
        fixture.bump_fact_version();
        publisher
            .publish(&mut fixture.store)
            .expect("publish versions");
        let fact = port.updates.borrow().days[&date].clone();
        assert_ne!(first.days[&date].versions, fact.versions);
        fixture.bump_metadata();
        publisher
            .publish(&mut fixture.store)
            .expect("publish versions");
        assert_ne!(fact.versions, port.updates.borrow().days[&date].versions);
        set_days(&mut publisher, BTreeSet::new());
        assert!(port.updates.borrow().days.is_empty());
        fixture.bump_fact_version();
        publisher
            .publish(&mut fixture.store)
            .expect("publish versions");
        assert!(port.updates.borrow().days.is_empty());
    }
    #[test]
    fn tray_and_registry_subscriptions_preserve_each_others_dates() {
        let mut fixture = QueryFixture::new(4);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel {
            mut publisher,
            port,
        } = VersionPublisher::open(&mut fixture.store, snapshots).expect("publisher");
        let today: LocalDate = "2025-01-02".parse().expect("today");
        let history = today.yesterday().expect("yesterday");
        let (reply, received) = oneshot::channel();
        publisher.receive(VersionCommand {
            subscriber: VersionSubscriber::Tray,
            dates: BTreeSet::from([today]),
            reply,
        });
        received
            .blocking_recv()
            .expect("tray subscription")
            .expect("valid date");
        set_days(&mut publisher, BTreeSet::from([history]));
        assert_eq!(
            port.updates
                .borrow()
                .days
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([history, today])
        );
        set_days(&mut publisher, BTreeSet::new());
        assert_eq!(
            port.updates
                .borrow()
                .days
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![today]
        );
        let before = port.updates.borrow().days[&today].clone();
        fixture.bump_fact_version();
        publisher
            .publish(&mut fixture.store)
            .expect("publish versions");
        assert_ne!(port.updates.borrow().days[&today].versions, before.versions);
    }
    #[test]
    fn classification_install_and_upgrade_invalidate_subscribed_days() {
        use crate::rules::registry::RegistryShell;
        use quartz_domain::{
            capture::UtcInstant,
            rules::{self, ParseContext, RuleOrigin, RuleSet, registry::RegistryCommand},
        };
        let mut fixture = QueryFixture::new(4);
        let now = UtcInstant::try_from(0).expect("UTC");
        let mut shell = RegistryShell::open(&mut fixture.store, now).expect("installed builtins");
        let VersionChannel {
            mut publisher,
            port,
        } = VersionPublisher::open(&mut fixture.store, shell.snapshots()).expect("publisher");
        publisher
            .publish(&mut fixture.store)
            .expect("publish versions");
        let date = "2025-01-02".parse().expect("date");
        let first = set_days(&mut publisher, BTreeSet::from([date]));
        let changed =
            rules::BUILTIN_CLASSIFICATION.replace("builtin:development", "builtin:learning");
        let set = RuleSet::parse(
            "builtin.toml",
            &changed,
            ParseContext {
                origin: RuleOrigin::Builtin,
                references: &shell.core().state().references,
            },
        )
        .expect("changed builtin");
        shell
            .run(
                &mut fixture.store,
                now,
                RegistryCommand::InstallBuiltin {
                    set,
                    revision: rules::BUILTIN_CLASSIFICATION_REVISION
                        .checked_add(1)
                        .expect("next revision"),
                },
            )
            .expect("builtin upgrade");
        publisher
            .publish(&mut fixture.store)
            .expect("publish versions");
        assert_ne!(
            first.days[&date].versions,
            port.updates.borrow().days[&date].versions
        );
        let upgraded = port.updates.borrow().days[&date].versions.clone();
        publisher
            .publish(&mut fixture.store)
            .expect("publish versions");
        assert_eq!(upgraded, port.updates.borrow().days[&date].versions);
    }
}
