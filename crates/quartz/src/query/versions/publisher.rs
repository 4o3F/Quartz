//! The database thread publishes digests for the dates registered by I/O.
use super::VersionState;
use crate::query::RunQueryError;
use crate::store::Store;
use crossbeam_channel::{Receiver, Sender};
use quartz_domain::rules::registry::SnapshotCell;
use quartz_protocol::{DayChanged, ErrorKind, LocalDate, ProtocolError};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{oneshot, watch};

/// Version requests are bounded independently of capture writes and query jobs.
const VERSION_CHANNEL_CAPACITY: usize = 8;
#[derive(Debug, Clone)]
pub(crate) struct PublishedVersions {
    pub days: BTreeMap<LocalDate, DayChanged>,
    pub day_start_offset: crate::query::calendar::DayStartOffset,
    pub time_zone: jiff::tz::TimeZone,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum VersionSubscriber {
    Registry,
    Tray,
}
pub(crate) struct SubscribeCommand {
    subscriber: VersionSubscriber,
    dates: BTreeSet<LocalDate>,
    reply: oneshot::Sender<Result<PublishedVersions, ProtocolError>>,
}
pub(crate) enum VersionCommand {
    Subscribe(SubscribeCommand),
    Refresh,
}
#[derive(Clone)]
pub(crate) struct VersionPort {
    requests: Sender<VersionCommand>,
    refresh_queued: Arc<AtomicBool>,
    pub updates: watch::Receiver<PublishedVersions>,
}
impl std::fmt::Debug for VersionPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VersionPort").finish_non_exhaustive()
    }
}
impl VersionPort {
    /// Coalesces nonblocking refresh notifications after system time-zone changes.
    pub(crate) fn refresh(&self) {
        if !self.refresh_queued.swap(true, Ordering::AcqRel)
            && let Err(error) = self.requests.try_send(VersionCommand::Refresh)
        {
            self.refresh_queued.store(false, Ordering::Release);
            tracing::warn!(%error, "could not request version refresh");
        }
    }
    #[cfg(test)]
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
    pub(crate) fn submit_days(
        &self,
        dates: BTreeSet<LocalDate>,
    ) -> Result<oneshot::Receiver<Result<PublishedVersions, ProtocolError>>, ProtocolError> {
        self.submit(VersionSubscriber::Registry, dates)
    }
    async fn subscribe(
        &self,
        subscriber: VersionSubscriber,
        dates: BTreeSet<LocalDate>,
    ) -> Result<PublishedVersions, ProtocolError> {
        self.submit(subscriber, dates)?
            .await
            .map_err(|_| ProtocolError::new(ErrorKind::Internal))?
    }
    fn submit(
        &self,
        subscriber: VersionSubscriber,
        dates: BTreeSet<LocalDate>,
    ) -> Result<oneshot::Receiver<Result<PublishedVersions, ProtocolError>>, ProtocolError> {
        let (reply, received) = oneshot::channel();
        self.requests
            .try_send(VersionCommand::Subscribe(SubscribeCommand {
                subscriber,
                dates,
                reply,
            }))
            .map_err(|error| {
                ProtocolError::new(match error {
                    crossbeam_channel::TrySendError::Full(_) => ErrorKind::Busy,
                    crossbeam_channel::TrySendError::Disconnected(_) => ErrorKind::Internal,
                })
            })?;
        Ok(received)
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
    refresh_queued: Arc<AtomicBool>,
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
        let day_start_offset = state.settings.day_start_offset;
        let time_zone = state.time_zone.clone();
        let (requests, received) = crossbeam_channel::bounded(VERSION_CHANNEL_CAPACITY);
        let (published, updates) = watch::channel(PublishedVersions {
            days: BTreeMap::new(),
            day_start_offset,
            time_zone,
        });
        let refresh_queued = Arc::new(AtomicBool::new(false));
        let publisher = Self {
            subscriptions: BTreeMap::new(),
            state,
            snapshots,
            publication: PublicationState::Current,
            refresh_queued: Arc::clone(&refresh_queued),
            requests: received,
            published,
        };
        let port = VersionPort {
            requests,
            refresh_queued,
            updates,
        };
        Ok(VersionChannel { publisher, port })
    }
    pub(crate) fn receive(
        &mut self,
        store: &mut Store,
        command: VersionCommand,
    ) -> Result<Option<crate::query::calendar::DayStartOffset>, RunQueryError> {
        match command {
            VersionCommand::Subscribe(command) => {
                self.receive_days(
                    command,
                    RunQueryError::system_time_zone(jiff::tz::TimeZone::try_system),
                    VersionState::day_in_zone,
                );
                Ok(None)
            }
            VersionCommand::Refresh => {
                self.refresh_queued.store(false, Ordering::Release);
                self.publish(store)
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn receive_in_zone(
        &mut self,
        store: &mut Store,
        command: VersionCommand,
        time_zone: jiff::tz::TimeZone,
    ) -> Result<Option<crate::query::calendar::DayStartOffset>, RunQueryError> {
        match command {
            VersionCommand::Subscribe(command) => {
                self.receive_days(command, Ok(time_zone), VersionState::day_in_zone);
                Ok(None)
            }
            VersionCommand::Refresh => {
                self.refresh_queued.store(false, Ordering::Release);
                self.publish_days_in_zone(store, time_zone, VersionState::day)
            }
        }
    }
    fn receive_days(
        &mut self,
        command: SubscribeCommand,
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
                let published = PublishedVersions {
                    days,
                    day_start_offset: self.state.settings.day_start_offset,
                    time_zone: self.state.time_zone.clone(),
                };
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
        let time_zone = RunQueryError::system_time_zone(jiff::tz::TimeZone::try_system)?;
        self.publish_days_in_zone(store, time_zone, day)
    }
    fn publish_days_in_zone(
        &mut self,
        store: &mut Store,
        time_zone: jiff::tz::TimeZone,
        day: impl Fn(&VersionState, LocalDate) -> Result<DayChanged, RunQueryError>,
    ) -> Result<Option<crate::query::calendar::DayStartOffset>, RunQueryError> {
        let hashes = self.snapshots.load().hashes().clone();
        let settings = store.query_settings()?;
        let changes = store.take_committed_versions();
        if self.publication == PublicationState::Current
            && changes.facts.is_empty()
            && changes.metadata_version.is_none()
            && self.state.settings.metadata_version == settings.metadata_version
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
        self.state.settings.metadata_version = changes
            .metadata_version
            .unwrap_or(settings.metadata_version);
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
        self.published.send_replace(PublishedVersions {
            days,
            day_start_offset: self.state.settings.day_start_offset,
            time_zone: self.state.time_zone.clone(),
        });
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
        let first = set_days(&mut publisher, &mut fixture.store, BTreeSet::from([date]));
        fixture.bump_fact_version();
        publisher
            .publish_days(&mut fixture.store, |_, _| Err(RunQueryError::InvalidRange))
            .expect("failed projection retains retry");
        let fresh = set_days(&mut publisher, &mut fixture.store, BTreeSet::from([date]));
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
        let first = set_days(&mut publisher, &mut fixture.store, BTreeSet::from([date]));
        let subscriptions = publisher.subscriptions.clone();
        let zone = publisher.state.time_zone.clone();
        let (reply, received) = oneshot::channel();
        publisher.receive_days(
            SubscribeCommand {
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
                SubscribeCommand {
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
        let first = set_days(&mut publisher, &mut fixture.store, BTreeSet::from([date]));
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

    fn set_days(
        publisher: &mut VersionPublisher,
        store: &mut Store,
        dates: BTreeSet<LocalDate>,
    ) -> PublishedVersions {
        let (reply, received) = oneshot::channel();
        publisher
            .receive(
                store,
                VersionCommand::Subscribe(SubscribeCommand {
                    subscriber: VersionSubscriber::Registry,
                    dates,
                    reply,
                }),
            )
            .expect("subscribe command");
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
        let first = set_days(&mut publisher, &mut fixture.store, BTreeSet::from([date]));
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
        set_days(&mut publisher, &mut fixture.store, BTreeSet::new());
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
        publisher
            .receive(
                &mut fixture.store,
                VersionCommand::Subscribe(SubscribeCommand {
                    subscriber: VersionSubscriber::Tray,
                    dates: BTreeSet::from([today]),
                    reply,
                }),
            )
            .expect("tray subscribe");
        received
            .blocking_recv()
            .expect("tray subscription")
            .expect("valid date");
        set_days(
            &mut publisher,
            &mut fixture.store,
            BTreeSet::from([history]),
        );
        assert_eq!(
            port.updates
                .borrow()
                .days
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([history, today])
        );
        set_days(&mut publisher, &mut fixture.store, BTreeSet::new());
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
        let first = set_days(&mut publisher, &mut fixture.store, BTreeSet::from([date]));
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

    #[test]
    fn timezone_refresh_republishes_subscribed_days_and_updates_published_zone() {
        let mut fixture = QueryFixture::new(0);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel {
            mut publisher,
            mut port,
        } = VersionPublisher::open(&mut fixture.store, snapshots).expect("publisher");
        let date: LocalDate = "2025-01-02".parse().expect("date");
        let (reply, received) = oneshot::channel();
        publisher
            .receive_in_zone(
                &mut fixture.store,
                VersionCommand::Subscribe(SubscribeCommand {
                    subscriber: VersionSubscriber::Registry,
                    dates: BTreeSet::from([date]),
                    reply,
                }),
                jiff::tz::TimeZone::UTC,
            )
            .expect("subscribe UTC");
        let initial = received.blocking_recv().expect("reply").expect("UTC");
        port.updates.borrow_and_update();
        port.refresh();
        port.refresh();
        let command = publisher.requests.try_recv().expect("refresh command");
        assert!(
            publisher.requests.try_recv().is_err(),
            "concurrent refreshes coalesce"
        );
        let tokyo = jiff::tz::TimeZone::get("Asia/Tokyo").expect("tokyo");
        publisher
            .receive_in_zone(&mut fixture.store, command, tokyo.clone())
            .expect("refresh Tokyo");
        assert!(port.updates.has_changed().expect("watch open"));
        let updated = port.updates.borrow_and_update().clone();
        assert_eq!(updated.time_zone, tokyo);
        assert_ne!(updated.days[&date].versions, initial.days[&date].versions);
        port.refresh();
        let repeat = publisher.requests.try_recv().expect("repeat refresh");
        publisher
            .receive_in_zone(&mut fixture.store, repeat, tokyo)
            .expect("unchanged zone");
        assert!(!port.updates.has_changed().expect("watch open"));
    }

    #[test]
    fn extraction_rule_change_republishes_subscribed_days() {
        use crate::rules::registry::RegistryShell;
        use quartz_domain::{capture::UtcInstant, rules::registry::RegistryCommand};
        let mut fixture = QueryFixture::new(0);
        let now = UtcInstant::try_from(0).expect("UTC");
        let mut shell = RegistryShell::open(&mut fixture.store, now).expect("registry");
        let VersionChannel {
            mut publisher,
            port,
        } = VersionPublisher::open(&mut fixture.store, shell.snapshots()).expect("publisher");
        let date: LocalDate = "2025-01-02".parse().expect("date");
        let before = set_days(&mut publisher, &mut fixture.store, BTreeSet::from([date]));
        shell
            .run(
                &mut fixture.store,
                now,
                RegistryCommand::Apply {
                    key: "user.local.extraction".parse().expect("key"),
                    file: "overlay.toml".into(),
                    text: r"format='quartz-rules/1'
[set]
key='user.local.extraction'
kind='extraction'
name='Extraction'
parent='builtin.extraction'
[[rule]]
id='notes'
label='Notes'
app=[{field='app.exe_name',op='equals',value='notes.exe'}]
pattern='\ANote: (?P<document>[^\r\n]+)\z'
"
                    .into(),
                    base_version: 0,
                },
            )
            .expect("save extraction rule");
        publisher
            .publish(&mut fixture.store)
            .expect("publish extraction change");
        assert_ne!(
            before.days[&date].versions,
            port.updates.borrow().days[&date].versions
        );
    }

    #[test]
    fn date_scoped_fact_and_annotation_changes_leave_unaffected_dates_unchanged() {
        let mut fixture = QueryFixture::new(1);
        let snapshots = RuleRegistry::from_state(Default::default())
            .expect("rules")
            .snapshots();
        let VersionChannel {
            mut publisher,
            port,
        } = VersionPublisher::open(&mut fixture.store, snapshots).expect("publisher");
        let prior: LocalDate = "2025-01-01".parse().expect("prior");
        let affected: LocalDate = "2025-01-02".parse().expect("affected");
        let next: LocalDate = "2025-01-03".parse().expect("next");
        let unaffected: LocalDate = "2025-01-20".parse().expect("unaffected");
        let (reply, received) = oneshot::channel();
        publisher
            .receive_in_zone(
                &mut fixture.store,
                VersionCommand::Subscribe(SubscribeCommand {
                    subscriber: VersionSubscriber::Registry,
                    dates: BTreeSet::from([prior, affected, next, unaffected]),
                    reply,
                }),
                jiff::tz::TimeZone::UTC,
            )
            .expect("subscribe");
        let initial = received.blocking_recv().expect("reply").expect("dates");
        fixture.bump_fact_version();
        publisher
            .publish_days_in_zone(
                &mut fixture.store,
                jiff::tz::TimeZone::UTC,
                VersionState::day,
            )
            .expect("publish fact");
        let after_fact = port.updates.borrow().days.clone();
        assert_ne!(
            after_fact[&affected].versions,
            initial.days[&affected].versions
        );
        assert_eq!(
            after_fact[&unaffected].versions,
            initial.days[&unaffected].versions
        );
        let midnight = "2025-01-03T00:00:00Z"
            .parse::<jiff::Timestamp>()
            .expect("midnight")
            .as_millisecond();
        fixture
            .store
            .create_annotations(
                quartz_protocol::AnnotationsCreateRequest {
                    entities: vec![quartz_protocol::CreateAnnotation {
                        uuid: quartz_protocol::AnnotationId(
                            "00000000-0000-4000-8000-000000000099"
                                .parse()
                                .expect("uuid"),
                        ),
                        kind: quartz_protocol::AnnotationKind::Note,
                        values: quartz_protocol::AnnotationValues {
                            start_utc_ms: midnight - 1000,
                            end_utc_ms: midnight + 1000,
                            category: None,
                            label_preset: Some(quartz_protocol::AnnotationLabelPreset::Rest),
                            label: None,
                            note: None,
                        },
                    }],
                },
                quartz_domain::capture::UtcInstant::try_from(midnight).expect("UTC"),
            )
            .expect("midnight annotation");
        publisher
            .publish_days_in_zone(
                &mut fixture.store,
                jiff::tz::TimeZone::UTC,
                VersionState::day,
            )
            .expect("publish annotation");
        let after_ann = port.updates.borrow().days.clone();
        assert_ne!(
            after_ann[&affected].versions,
            after_fact[&affected].versions
        );
        assert_ne!(after_ann[&next].versions, after_fact[&next].versions);
        assert_eq!(after_ann[&prior].versions, after_fact[&prior].versions);
        assert_eq!(
            after_ann[&unaffected].versions,
            after_fact[&unaffected].versions
        );
    }
}
