//! Role authorization from the operation-prefix table in 04 §5.

use serde::{Deserialize, Serialize};
use snafu::Snafu;

use crate::{Op, OpFamily};

/// Connection role declared by a validated hello handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Interactive UI and its privileged management operations.
    Ui,
    /// Command-line diagnostics limited to the explicit read-only list.
    Cli,
    /// Native Messaging host forwarding browser extension state.
    Browser,
}

impl Role {
    /// Checks the role's request permission without performing I/O.
    ///
    /// Unknown operations must be answered with `unknown_op` before authorization.
    ///
    /// # Errors
    /// Returns the role and operation when access is not granted by 04 §5.
    pub fn authorize(self, op: Op) -> Result<(), AuthorizeOpError> {
        let allowed = match self {
            Self::Ui => match op.family() {
                OpFamily::Unknown | OpFamily::Browser => false,
                OpFamily::Hello
                | OpFamily::Subscribe
                | OpFamily::Core
                | OpFamily::Ui
                | OpFamily::Query
                | OpFamily::RuleSets
                | OpFamily::Rules
                | OpFamily::RulesText
                | OpFamily::Categories
                | OpFamily::Annotations
                | OpFamily::Settings
                | OpFamily::Tracking
                | OpFamily::PrivacyMode
                | OpFamily::Data
                | OpFamily::BrowserHosts => true,
            },
            Self::Cli => match op.family() {
                OpFamily::Hello | OpFamily::Query | OpFamily::Core | OpFamily::Settings => true,
                OpFamily::Ui | OpFamily::RuleSets | OpFamily::Rules | OpFamily::RulesText => {
                    op.cli_individual_permission()
                }
                OpFamily::Subscribe
                | OpFamily::Categories
                | OpFamily::Annotations
                | OpFamily::Tracking
                | OpFamily::PrivacyMode
                | OpFamily::Data
                | OpFamily::Browser
                | OpFamily::BrowserHosts
                | OpFamily::Unknown => false,
            },
            Self::Browser => matches!(op.family(), OpFamily::Hello | OpFamily::Browser),
        };
        if allowed {
            Ok(())
        } else {
            Err(AuthorizeOpError { role: self, op })
        }
    }
}

impl Op {
    fn cli_individual_permission(self) -> bool {
        match self {
            Self::UiOpen
            | Self::RuleSetsGet
            | Self::RuleSetsTree
            | Self::RulesTextValidate
            | Self::RulesTextExport => true,
            Self::Hello
            | Self::Subscribe
            | Self::Unsubscribe
            | Self::CoreStatus
            | Self::UiHidden
            | Self::UiFirstFrame
            | Self::QueryFacts
            | Self::QueryDay
            | Self::QueryTimeline
            | Self::QueryRange
            | Self::QuerySegmentDetail
            | Self::QueryEntityDetail
            | Self::QueryUnclassified
            | Self::QueryAnnotations
            | Self::QueryPendingAway
            | Self::RuleSetsSetEnabled
            | Self::RuleSetsDelete
            | Self::RuleSetsEdit
            | Self::RulesPreview
            | Self::RulesTextImport
            | Self::CategoriesList
            | Self::CategoriesCreate
            | Self::CategoriesUpdate
            | Self::CategoriesArchive
            | Self::CategoriesReorder
            | Self::AnnotationsCreate
            | Self::AnnotationsUpdate
            | Self::AnnotationsDelete
            | Self::SettingsGet
            | Self::SettingsSet
            | Self::TrackingPause
            | Self::TrackingResume
            | Self::TrackingKeepAlive
            | Self::PrivacyModeSet
            | Self::DataInfo
            | Self::DataBackup
            | Self::DataCompact
            | Self::DataDeleteRange
            | Self::BrowserState
            | Self::BrowserHostsStatus
            | Self::BrowserHostsEnable
            | Self::BrowserHostsDisable
            | Self::Unknown => false,
        }
    }
}

/// A request is outside the declared role's permitted operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(display("role {role:?} cannot request {op:?}"))]
pub struct AuthorizeOpError {
    /// Role that attempted the operation.
    pub role: Role,
    /// Denied operation.
    pub op: Op,
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::VariantArray;

    #[test]
    fn every_role_and_operation_matches_the_independent_permission_table() {
        // Written as wire-name lists, independently of the production family helper.
        let ui = "hello subscribe unsubscribe core.status ui.open ui.first_frame ui.hidden query.facts query.day query.timeline query.range query.segment_detail query.entity_detail query.unclassified query.pending_away query.annotations rule_sets.tree rule_sets.get rule_sets.set_enabled rule_sets.delete rule_sets.edit rules.preview rules_text.validate rules_text.import rules_text.export categories.list categories.create categories.update categories.archive categories.reorder annotations.create annotations.update annotations.delete settings.get settings.set tracking.pause tracking.resume tracking.keep_alive privacy_mode.set data.info data.backup data.compact data.delete_range browser_hosts.status browser_hosts.enable browser_hosts.disable";
        let cli = "hello settings.get settings.set ui.open query.facts query.day query.timeline query.range query.segment_detail query.entity_detail query.unclassified query.pending_away query.annotations core.status rules_text.validate rules_text.export rule_sets.tree rule_sets.get";
        let browser = "hello browser.state";
        for role in [Role::Ui, Role::Cli, Role::Browser] {
            let expected = match role {
                Role::Ui => ui,
                Role::Cli => cli,
                Role::Browser => browser,
            };
            let expected: std::collections::BTreeSet<_> = expected.split_whitespace().collect();
            let mut actual = std::collections::BTreeSet::new();
            for &op in Op::VARIANTS.iter().filter(|op| **op != Op::Unknown) {
                let name = serde_json::to_value(op).expect("known op");
                let name = name.as_str().expect("op wire string").to_owned();
                assert_eq!(
                    role.authorize(op).is_ok(),
                    expected.contains(name.as_str()),
                    "{role:?} {name}"
                );
                if role.authorize(op).is_ok() {
                    actual.insert(name.clone());
                }
                assert_eq!(
                    serde_json::from_value::<Op>(serde_json::json!(name)).expect("parse op"),
                    op
                );
                assert_eq!(
                    serde_json::to_value(op).expect("serialize op"),
                    serde_json::json!(name)
                );
            }
            assert_eq!(
                actual
                    .iter()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>(),
                expected,
                "no missing request operation for {role:?}"
            );
            assert!(role.authorize(Op::Unknown).is_err());
        }
    }
}
