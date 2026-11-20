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
pub(crate) struct VersionCommand {
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
        let (reply, received) = oneshot::channel();
        self.requests
            .try_send(VersionCommand { dates, reply })
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
        let state = VersionState::from_stored(store.version_state()?, String::new());
        let (requests, received) = crossbeam_channel::bounded(VERSION_CHANNEL_CAPACITY);
        let (published, updates) = watch::channel(PublishedVersions::default());
        let mut publisher = Self {
            state,
            snapshots,
            requests: received,
            published,
        };
        publisher.state.classification_hash = publisher.classification_hash();
        let port = VersionPort { requests, updates };
        Ok(VersionChannel { publisher, port })
    }
    pub(crate) fn classification_hash(&self) -> String {
        self.snapshots.load().hashes().classification.to_string()
    }
    pub(crate) fn receive(&mut self, command: VersionCommand) {
        let result = command
            .dates
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
    pub(crate) fn publish(&mut self, store: &mut Store) {
        let changes = store.take_committed_versions();
        let hash = self.classification_hash();
        if changes.facts.is_empty()
            && changes.metadata_version.is_none()
            && self.state.classification_hash == hash
        {
            return;
        }
        for fact in changes.facts {
            self.state.days.insert(fact.key, fact.version);
        }
        if let Some(version) = changes.metadata_version {
            self.state.settings.metadata_version = version;
        }
        self.state.classification_hash = hash;
        let dates: Vec<_> = self.published.borrow().days.keys().copied().collect();
        let mut days = BTreeMap::new();
        for date in dates {
            match self.state.day(date) {
                Ok(changed) => {
                    days.insert(date, changed);
                }
                Err(error) => {
                    tracing::error!(%error, "could not update day subscription");
                    return;
                }
            }
        }
        self.published.send_replace(PublishedVersions { days });
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

    fn set_days(publisher: &mut VersionPublisher, dates: BTreeSet<LocalDate>) -> PublishedVersions {
        let (reply, received) = oneshot::channel();
        publisher.receive(VersionCommand { dates, reply });
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
        publisher.publish(&mut fixture.store);
        let fact = port.updates.borrow().days[&date].clone();
        assert_ne!(first.days[&date].versions, fact.versions);
        fixture.bump_metadata();
        publisher.publish(&mut fixture.store);
        assert_ne!(fact.versions, port.updates.borrow().days[&date].versions);
        set_days(&mut publisher, BTreeSet::new());
        assert!(port.updates.borrow().days.is_empty());
        fixture.bump_fact_version();
        publisher.publish(&mut fixture.store);
        assert!(port.updates.borrow().days.is_empty());
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
        publisher.publish(&mut fixture.store);
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
                RegistryCommand::InstallBuiltin { set, revision: 2 },
            )
            .expect("builtin upgrade");
        publisher.publish(&mut fixture.store);
        assert_ne!(
            first.days[&date].versions,
            port.updates.borrow().days[&date].versions
        );
        let upgraded = port.updates.borrow().days[&date].versions.clone();
        publisher.publish(&mut fixture.store);
        assert_eq!(upgraded, port.updates.borrow().days[&date].versions);
    }
}
