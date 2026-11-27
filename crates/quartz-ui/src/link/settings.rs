//! Settings, local data maintenance, and core status requests over the existing UI pipe link.

use super::{Link, LoadDataError};
use quartz_protocol::{
    BackupEntry, BrowserHostsDisableRequest, BrowserHostsEnableRequest, BrowserHostsStatusRequest,
    BrowserHostsStatusResponse, BrowserKind, CoreStatusRequest, CoreStatusResponse,
    DataBackupRequest, DataCompactRequest, DataCompactResponse, DataDeleteRangeRequest,
    DataDeleteRangeResponse, DataInfoRequest, DataInfoResponse, SettingsGetRequest,
    SettingsResponse, SettingsSetRequest,
};

impl Link {
    /// Reads stored/effective UI settings, title retention, and live Windows autostart state.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn get_settings(&self) -> Result<SettingsResponse, LoadDataError> {
        self.request(SettingsGetRequest {})
            .await
            .map_err(LoadDataError::from)
    }

    /// Applies a partial `settings.set` update and returns the committed settings snapshot.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn set_settings(
        &self,
        request: SettingsSetRequest,
    ) -> Result<SettingsResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }

    /// Reads local database path, size, logs directory, and backup metadata.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn get_data_info(&self) -> Result<DataInfoResponse, LoadDataError> {
        self.request(DataInfoRequest {})
            .await
            .map_err(LoadDataError::from)
    }

    /// Triggers a manual database backup (`VACUUM INTO`).
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure (such as `backup_exists`).
    pub(crate) async fn create_backup(&self) -> Result<BackupEntry, LoadDataError> {
        self.request(DataBackupRequest {})
            .await
            .map_err(LoadDataError::from)
    }

    /// Compacts the SQLite database (`VACUUM` + WAL truncation).
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn compact_database(&self) -> Result<DataCompactResponse, LoadDataError> {
        self.request(DataCompactRequest {})
            .await
            .map_err(LoadDataError::from)
    }

    /// Deletes all recorded data across a half-open range of finished logical days.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure (such as `bad_request`).
    pub(crate) async fn delete_data_range(
        &self,
        request: DataDeleteRangeRequest,
    ) -> Result<DataDeleteRangeResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }

    /// Reads the core version and current recording status without touching the database.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn get_core_status(&self) -> Result<CoreStatusResponse, LoadDataError> {
        self.request(CoreStatusRequest {})
            .await
            .map_err(LoadDataError::from)
    }

    /// Reads Native Messaging host registration and live connection status for all browsers.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn get_browser_hosts(
        &self,
    ) -> Result<BrowserHostsStatusResponse, LoadDataError> {
        self.request(BrowserHostsStatusRequest {})
            .await
            .map_err(LoadDataError::from)
    }

    /// Enables or re-enables Native Messaging host registration for `browser`.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure (such as `host_executable_missing`).
    pub(crate) async fn enable_browser_host(
        &self,
        browser: BrowserKind,
    ) -> Result<BrowserHostsStatusResponse, LoadDataError> {
        self.request(BrowserHostsEnableRequest { browser })
            .await
            .map_err(LoadDataError::from)
    }

    /// Disables Native Messaging host registration for `browser`.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn disable_browser_host(
        &self,
        browser: BrowserKind,
    ) -> Result<BrowserHostsStatusResponse, LoadDataError> {
        self.request(BrowserHostsDisableRequest { browser })
            .await
            .map_err(LoadDataError::from)
    }
}
