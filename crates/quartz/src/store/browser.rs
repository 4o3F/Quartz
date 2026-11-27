//! Browser stream registration persistence (`browser_streams`, 05 §3.1).

use super::{Store, rows::BrowserStreamsRow, schema::browser_streams};
use diesel::{ExpressionMethods, NullableExpressionMethods, RunQueryDsl};
use quartz_domain::capture::UtcInstant;
use quartz_protocol::{BrowserKind, Uuid};
use snafu::Snafu;

/// Fixed Chromium (Chrome/Edge) extension ID placeholder, to be filled in M5-B3.
pub(crate) const CHROMIUM_EXTENSION_ID: &str = "TODO_CHROMIUM_EXTENSION_ID_M5_B3";

/// Fixed Firefox add-on ID (05 §1.1).
pub(crate) const FIREFOX_EXTENSION_ID: &str = "quartz@4o3f.dev";

/// Returns the fixed extension identifier for a browser family (05 §1.1).
#[must_use]
pub(crate) const fn extension_id_for_browser(browser: BrowserKind) -> &'static str {
    match browser {
        BrowserKind::Chrome | BrowserKind::Edge => CHROMIUM_EXTENSION_ID,
        BrowserKind::Firefox => FIREFOX_EXTENSION_ID,
    }
}

/// Persisted primary key for a row in `browser_streams`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct BrowserStreamRowId(i64);

impl BrowserStreamRowId {
    /// Wraps a persisted SQLite `browser_streams.id` value.
    #[must_use]
    pub(crate) const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Returns the underlying SQLite primary key.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn get(self) -> i64 {
        self.0
    }
}

/// Failure upserting a `browser_streams` row during browser handshake.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum UpsertBrowserStreamError {
    /// SQLite rejected the insert or update.
    #[snafu(display("could not upsert browser stream"))]
    Database { source: diesel::result::Error },
}

impl From<diesel::result::Error> for UpsertBrowserStreamError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}

impl Store {
    /// Inserts a `browser_streams` row on first sight or updates `last_seen_utc_ms` on reconnect.
    ///
    /// Preserves `id`, `first_seen_utc_ms`, and user-assigned `label` across reconnects.
    pub(crate) fn upsert_browser_stream(
        &mut self,
        browser: BrowserKind,
        profile_instance_id: &Uuid,
        now: UtcInstant,
    ) -> Result<BrowserStreamRowId, UpsertBrowserStreamError> {
        let browser_name: &'static str = browser.into();
        let extension_id = extension_id_for_browser(browser);
        let now_ms = now.get().as_millisecond();
        let row = BrowserStreamsRow {
            id: None,
            browser: browser_name.to_owned(),
            profile_instance_id: profile_instance_id.to_string(),
            extension_id: extension_id.to_owned(),
            label: None,
            first_seen_utc_ms: now_ms,
            last_seen_utc_ms: now_ms,
        };
        self.write(|tx| {
            let id: i64 = diesel::insert_into(browser_streams::table)
                .values(&row)
                .on_conflict(browser_streams::profile_instance_id)
                .do_update()
                .set((
                    browser_streams::browser.eq(&row.browser),
                    browser_streams::extension_id.eq(&row.extension_id),
                    browser_streams::last_seen_utc_ms.eq(row.last_seen_utc_ms),
                ))
                .returning(browser_streams::id.assume_not_null())
                .get_result(tx.connection)?;
            Ok(BrowserStreamRowId::new(id))
        })
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredBrowserStream {
    pub id: BrowserStreamRowId,
    pub browser: String,
    pub profile_instance_id: String,
    pub extension_id: String,
    pub label: Option<String>,
    pub first_seen_utc_ms: i64,
    pub last_seen_utc_ms: i64,
}

#[cfg(test)]
impl super::ReadOnlyStore {
    pub(crate) fn browser_stream(
        &mut self,
        profile_instance_id: &Uuid,
    ) -> Result<Option<StoredBrowserStream>, diesel::result::Error> {
        use diesel::{OptionalExtension, QueryDsl, SelectableHelper};
        let row = browser_streams::table
            .filter(browser_streams::profile_instance_id.eq(profile_instance_id.to_string()))
            .select(BrowserStreamsRow::as_select())
            .first(&mut self.connection)
            .optional()?;
        Ok(row.map(|row| StoredBrowserStream {
            id: BrowserStreamRowId::new(row.id.expect("persisted browser_streams id")),
            browser: row.browser,
            profile_instance_id: row.profile_instance_id,
            extension_id: row.extension_id,
            label: row.label,
            first_seen_utc_ms: row.first_seen_utc_ms,
            last_seen_utc_ms: row.last_seen_utc_ms,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_directory::DataDirectory;

    #[test]
    fn browser_stream_upsert_creates_then_updates_last_seen_without_changing_first_seen() {
        let dir = tempfile::tempdir().expect("isolated browser store");
        let directory = DataDirectory::new(dir.path());
        let mut store = Store::open(&directory).expect("open store");
        let chrome_profile: Uuid = "a286737e-3855-43cb-98ee-c366344372d0"
            .parse()
            .expect("chrome uuid");
        let firefox_profile: Uuid = "b286737e-3855-43cb-98ee-c366344372d1"
            .parse()
            .expect("firefox uuid");

        let first_at = UtcInstant::try_from(1_000).expect("t1");
        let second_at = UtcInstant::try_from(5_000).expect("t2");

        let first_id = store
            .upsert_browser_stream(BrowserKind::Chrome, &chrome_profile, first_at)
            .expect("insert chrome stream");
        let firefox_id = store
            .upsert_browser_stream(BrowserKind::Firefox, &firefox_profile, first_at)
            .expect("insert firefox stream");
        assert_ne!(first_id, firefox_id);
        assert!(first_id.get() > 0);
        assert!(firefox_id.get() > 0);

        let updated_id = store
            .upsert_browser_stream(BrowserKind::Chrome, &chrome_profile, second_at)
            .expect("update chrome stream");
        assert_eq!(updated_id, first_id);

        let mut reader = super::super::ReadOnlyStore::open(&directory.database()).expect("reader");
        let chrome = reader
            .browser_stream(&chrome_profile)
            .expect("read chrome")
            .expect("chrome row exists");
        assert_eq!(chrome.id, first_id);
        assert_eq!(chrome.browser, "chrome");
        assert_eq!(chrome.extension_id, CHROMIUM_EXTENSION_ID);
        assert_eq!(chrome.first_seen_utc_ms, 1_000);
        assert_eq!(chrome.last_seen_utc_ms, 5_000);
        assert_eq!(chrome.label, None);

        let firefox = reader
            .browser_stream(&firefox_profile)
            .expect("read firefox")
            .expect("firefox row exists");
        assert_eq!(firefox.id, firefox_id);
        assert_eq!(firefox.browser, "firefox");
        assert_eq!(firefox.extension_id, FIREFOX_EXTENSION_ID);
        assert_eq!(firefox.first_seen_utc_ms, 1_000);
        assert_eq!(firefox.last_seen_utc_ms, 1_000);
    }
}
