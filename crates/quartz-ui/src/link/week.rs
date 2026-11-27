//! Week range summary queries over the existing UI pipe link.

use super::{Link, LoadDataError, QueryProgress};
use quartz_protocol::{QueryRangeRequest, QueryRangeResponse};
use tokio::sync::watch;

impl Link {
    /// Queries per-day category summaries over a half-open logical-date range, forwarding
    /// per-day core progress updates to `progress`.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn query_range(
        &self,
        request: QueryRangeRequest,
        progress: watch::Sender<Option<QueryProgress>>,
    ) -> Result<QueryRangeResponse, LoadDataError> {
        self.request_with_progress(request, progress)
            .await
            .map_err(LoadDataError::from)
    }
}
