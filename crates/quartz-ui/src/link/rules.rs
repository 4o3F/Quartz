//! Rule-set and category reads and mutations over the existing UI pipe link.

use super::{Link, LoadDataError};
use quartz_protocol::{
    AcceptedBody, CategoriesArchiveRequest, CategoriesCreateRequest, CategoriesListRequest,
    CategoriesListResponse, CategoriesReorderRequest, CategoriesUpdateRequest, CategoryResponse,
    RuleKind, RuleSetsDeleteRequest, RuleSetsEditRequest, RuleSetsGetRequest, RuleSetsGetResponse,
    RuleSetsSetEnabledRequest, RuleSetsTreeRequest, RuleSetsTreeResponse, RuleSetsWriteResponse,
    RuleView, RulesPreviewRequest, RulesPreviewResponse, RulesTextExportRequest,
    RulesTextExportResponse, RulesTextValidateRequest, RulesTextValidateResponse,
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

    /// Loads all categories (active and archived) in server hierarchy preorder.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn list_categories(&self) -> Result<CategoriesListResponse, LoadDataError> {
        self.request(CategoriesListRequest {
            include_archived: true,
        })
        .await
        .map_err(LoadDataError::from)
    }

    /// Creates a new user category with a client-supplied UUID.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn create_category(
        &self,
        request: CategoriesCreateRequest,
    ) -> Result<CategoryResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }

    /// Updates an existing category's editable metadata.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn update_category(
        &self,
        request: CategoriesUpdateRequest,
    ) -> Result<CategoryResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }

    /// Archives or restores a category without deleting historical records.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn archive_category(
        &self,
        request: CategoriesArchiveRequest,
    ) -> Result<CategoryResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }

    /// Reorders all siblings under one parent category.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn reorder_categories(
        &self,
        request: CategoriesReorderRequest,
    ) -> Result<AcceptedBody, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }

    /// Exports the canonical `quartz-rules/1` text of an active rule set.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn export_rule_set_text(
        &self,
        key: String,
    ) -> Result<RulesTextExportResponse, LoadDataError> {
        self.request(RulesTextExportRequest { key })
            .await
            .map_err(LoadDataError::from)
    }

    /// Validates rule-set text against an observed installed version without saving it.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure (such as `rule_set_stale`).
    pub(crate) async fn validate_rule_set_text(
        &self,
        request: RulesTextValidateRequest,
    ) -> Result<RulesTextValidateResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }

    /// Previews the impact of a rule-set mutation over one to seven logical days.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure.
    pub(crate) async fn preview_rules(
        &self,
        request: RulesPreviewRequest,
    ) -> Result<RulesPreviewResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }

    /// Creates or saves a user rule set with optimistic concurrency via `base_version`.
    ///
    /// # Errors
    /// Returns any transport, decoding, or protocol failure (such as `rule_set_stale` or
    /// `rule_validation_failed`).
    pub(crate) async fn edit_rule_set(
        &self,
        request: RuleSetsEditRequest,
    ) -> Result<RuleSetsWriteResponse, LoadDataError> {
        self.request(request).await.map_err(LoadDataError::from)
    }
}
