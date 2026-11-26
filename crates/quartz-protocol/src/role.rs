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
    /// Native Messaging host reporting one registered browser stream.
    Host,
    /// Command-line diagnostics limited to the explicit read-only list.
    Cli,
}

impl Role {
    /// Checks the role's request permission without performing I/O.
    ///
    /// A host may operate only on the browser stream registered by its handshake;
    /// the pipe server must check that ownership at runtime.
    /// Unknown operations must be answered with `unknown_op` before authorization.
    ///
    /// # Errors
    /// Returns the role and operation when access is not granted by 04 §5.
    pub fn authorize(self, op: Op) -> Result<(), AuthorizeOpError> {
        let allowed = match self {
            Self::Ui => match op.family() {
                OpFamily::Browser | OpFamily::Unknown => false,
                OpFamily::Requests
                | OpFamily::Hello
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
                | OpFamily::Data
                | OpFamily::Tracking
                | OpFamily::Apps
                | OpFamily::BrowserHosts
                | OpFamily::Maintenance
                | OpFamily::PrivacyMode => true,
            },
            Self::Host => match op.family() {
                OpFamily::Hello | OpFamily::Browser => true,
                OpFamily::Requests
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
                | OpFamily::Data
                | OpFamily::Tracking
                | OpFamily::Apps
                | OpFamily::BrowserHosts
                | OpFamily::Maintenance
                | OpFamily::PrivacyMode
                | OpFamily::Unknown => false,
            },
            Self::Cli => match op.family() {
                OpFamily::Requests
                | OpFamily::Hello
                | OpFamily::Query
                | OpFamily::Core
                | OpFamily::Apps
                | OpFamily::Settings => true,
                OpFamily::Ui
                | OpFamily::RuleSets
                | OpFamily::Rules
                | OpFamily::RulesText
                | OpFamily::BrowserHosts => op.cli_individual_permission(),
                OpFamily::Subscribe
                | OpFamily::Categories
                | OpFamily::Annotations
                | OpFamily::Data
                | OpFamily::Tracking
                | OpFamily::Browser
                | OpFamily::Maintenance
                | OpFamily::PrivacyMode
                | OpFamily::Unknown => false,
            },
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
            Self::RequestsCancel
            | Self::UiOpen
            | Self::RuleSetsGet
            | Self::RuleSetsTree
            | Self::RulesTextValidate
            | Self::RulesTextPreview
            | Self::RulesTextExport
            | Self::BrowserHostsStatus => true,
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
            | Self::DataDelete
            | Self::TrackingPause
            | Self::TrackingResume
            | Self::TrackingKeepAlive
            | Self::BrowserHello
            | Self::BrowserState
            | Self::BrowserGap
            | Self::BrowserPurgeAck
            | Self::BrowserQuerySiteToday
            | Self::AppsIcon
            | Self::BrowserHostsEnable
            | Self::BrowserHostsDisable
            | Self::MaintenanceBackup
            | Self::MaintenanceCompact
            | Self::PrivacyModeSet
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
        let ui = "requests.cancel hello subscribe unsubscribe core.status ui.open ui.first_frame ui.hidden query.facts query.day query.timeline query.range query.segment_detail query.entity_detail query.unclassified query.pending_away query.annotations rule_sets.tree rule_sets.get rule_sets.set_enabled rule_sets.delete rule_sets.edit rules.preview rules_text.validate rules_text.preview rules_text.import rules_text.export categories.list categories.create categories.update categories.archive categories.reorder annotations.create annotations.update annotations.delete settings.get settings.set data.delete tracking.pause tracking.resume tracking.keep_alive apps.icon browser_hosts.status browser_hosts.enable browser_hosts.disable maintenance.backup maintenance.compact privacy_mode.set";
        let host = "hello browser.hello browser.state browser.gap browser.purge_ack browser.query_site_today";
        let cli = "requests.cancel hello settings.get settings.set ui.open query.facts query.day query.timeline query.range query.segment_detail query.entity_detail query.unclassified query.pending_away query.annotations apps.icon browser_hosts.status core.status rules_text.validate rules_text.preview rules_text.export rule_sets.tree rule_sets.get";
        for role in [Role::Ui, Role::Host, Role::Cli] {
            let expected = match role {
                Role::Ui => ui,
                Role::Host => host,
                Role::Cli => cli,
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

    #[test]
    fn sensitive_permission_boundaries() {
        assert!(Role::Ui.authorize(Op::UiOpen).is_ok());
        assert!(Role::Cli.authorize(Op::UiOpen).is_ok());
        assert!(Role::Host.authorize(Op::UiOpen).is_err());
        assert!(Role::Host.authorize(Op::DataDelete).is_err());
        assert!(Role::Cli.authorize(Op::AnnotationsCreate).is_err());
        assert!(Role::Cli.authorize(Op::Subscribe).is_err());
        assert!(Role::Host.authorize(Op::Subscribe).is_err());
        assert!(Role::Ui.authorize(Op::Subscribe).is_ok());
        assert!(Role::Cli.authorize(Op::RulesTextImport).is_err());
        assert!(Role::Cli.authorize(Op::RulesTextValidate).is_ok());
        assert!(Role::Ui.authorize(Op::BrowserState).is_err());
        assert!(Role::Ui.authorize(Op::BrowserHostsEnable).is_ok());
        assert!(Role::Host.authorize(Op::UiFirstFrame).is_err());
        assert!(Role::Cli.authorize(Op::UiFirstFrame).is_err());
        assert!(Role::Ui.authorize(Op::UiFirstFrame).is_ok());
        assert!(Role::Ui.authorize(Op::UiHidden).is_ok());
        assert!(Role::Cli.authorize(Op::UiHidden).is_err());
        assert!(Role::Host.authorize(Op::UiHidden).is_err());
    }
}
