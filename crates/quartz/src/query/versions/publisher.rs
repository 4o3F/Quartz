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
pub(crate) struct VersionPublisher {
    subscriptions: BTreeMap<VersionSubscriber, BTreeSet<LocalDate>>,
    state: VersionState,
    snapshots: SnapshotCell,
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
            requests: received,
            published,
        };
        let port = VersionPort { requests, updates };
        Ok(VersionChannel { publisher, port })
    }
    pub(crate) fn receive(&mut self, command: VersionCommand) {
        self.subscriptions.insert(command.subscriber, command.dates);
        let dates: BTreeSet<_> = self.subscriptions.values().flatten().copied().collect();
        let result = dates
            .into_iter()
            .map(|date| self.state.day(date).map(|changed| (date, changed)))
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map(|days| PublishedVersions { days })
            .map_err(|error| ProtocolError::new(error.kind()));
        if let Ok(state) = &result {
            self.published.send_replace(state.clone());
        }
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
        let time_zone = jiff::tz::TimeZone::try_system()?;
        let settings = store.query_settings()?;
        let changes = store.take_committed_versions();
        if changes.facts.is_empty()
            && changes.metadata_version.is_none()
            && self.state.rule_hashes == hashes
            && self.state.time_zone == time_zone
            && self.state.settings.stats_policy_version == settings.stats_policy_version
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
        fixture.bump_fact_version();
        publisher
            .publish(&mut fixture.store)
            .expect("retry on next commit");
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
