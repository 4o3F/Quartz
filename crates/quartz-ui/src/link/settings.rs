//! Settings read/update requests over the existing UI pipe link.

use super::{Link, LoadDataError};
use quartz_protocol::{SettingsGetRequest, SettingsResponse, SettingsSetRequest};

impl Link {
    /// Reads stored/effective UI settings and live Windows autostart state.
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
}
