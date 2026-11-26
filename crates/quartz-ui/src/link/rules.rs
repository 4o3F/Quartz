//! Rule-set tree, detail, and mutation requests over the existing UI pipe link.

use super::{Link, LoadDataError};
use quartz_protocol::{
    RuleKind, RuleSetsDeleteRequest, RuleSetsGetRequest, RuleSetsGetResponse,
    RuleSetsSetEnabledRequest, RuleSetsTreeRequest, RuleSetsTreeResponse, RuleSetsWriteResponse,
    RuleView,
};

/// All rule families queried when building the Rules page tree, in display order.
const RULE_KINDS: [RuleKind; 3] = [
    RuleKind::Classification,
    RuleKind::Extraction,
    RuleKind::Privacy,
];

impl Link {
    /// Loads the current rule-set tree across every rule family from one consistent registry
    /// version.
    ///
    /// # Errors
    /// Returns [`LoadDataError::Stale`] if the registry digest changed between family reads, or
    /// any underlying transport or protocol failure.
    pub(crate) async fn load_rule_trees(&self) -> Result<Vec<RuleSetsTreeResponse>, LoadDataError> {
        let mut responses: Vec<RuleSetsTreeResponse> = Vec::with_capacity(RULE_KINDS.len());
        for kind in RULE_KINDS {
            let response = self.request(RuleSetsTreeRequest { kind }).await?;
            if let Some(first) = responses.first()
                && first.versions != response.versions
            {
                return Err(LoadDataError::Stale);
            }
            responses.push(response);
        }
        Ok(responses)
    }

    /// Loads entries of the rule set identified by `key` in the requested `view`.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn get_rule_set(
        &self,
        key: String,
        view: RuleView,
    ) -> Result<RuleSetsGetResponse, LoadDataError> {
        self.request(RuleSetsGetRequest { key, view })
            .await
            .map_err(LoadDataError::from)
    }

    /// Enables or disables a user rule set.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure (such as `rule_set_stale` or
    /// `rule_set_read_only`).
    pub(crate) async fn set_rule_set_enabled(
        &self,
        request: RuleSetsSetEnabledRequest,
    ) -> Result<RuleSetsWriteResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }

    /// Deletes a childless user rule set.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure (such as `rule_set_stale` or
    /// `rule_set_has_children`).
    pub(crate) async fn delete_rule_set(
        &self,
        request: RuleSetsDeleteRequest,
    ) -> Result<RuleSetsWriteResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }
}
