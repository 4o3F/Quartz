//! Request operations, prefix families, and pushed control operations.

use serde::{Deserialize, Serialize};

/// Permission families matching operation prefixes in 04 §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpFamily {
    /// The `hello` operation family.
    Hello,
    /// The `subscribe` operation family.
    Subscribe,
    /// The `core` operation family.
    Core,
    /// The `ui` operation family.
    Ui,
    /// The `query` operation family.
    Query,
    /// The `rule_sets` operation family.
    RuleSets,
    /// The `rules` operation family.
    Rules,
    /// The `rules_text` operation family.
    RulesText,
    /// The `categories` operation family.
    Categories,
    /// The `annotations` operation family.
    Annotations,
    /// The `settings` operation family.
    Settings,
    /// The `tracking` operation family.
    Tracking,
    /// The `privacy_mode` operation family.
    PrivacyMode,
    /// The `data` operation family.
    Data,
    /// The `browser` operation family.
    Browser,
    /// The `browser_hosts` operation family.
    BrowserHosts,
    /// An unrecognized operation; no role may authorize it.
    Unknown,
}

/// Client request operations listed in docs/design/04-ipc.md §4–§7.
///
/// Unknown wire names decode to `Unknown`, preserving the enclosing request ID.
/// Core-to-client pushes are represented separately by [`ControlOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::VariantArray)]
pub enum Op {
    /// The `hello` request.
    #[serde(rename = "hello")]
    Hello,
    /// The `subscribe` request.
    #[serde(rename = "subscribe")]
    Subscribe,
    /// Removes topics from this connection.
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
    /// The `core.status` request.
    #[serde(rename = "core.status")]
    CoreStatus,
    /// Opens or activates the UI through the core.
    #[serde(rename = "ui.open")]
    UiOpen,
    /// The `ui.first_frame` request.
    #[serde(rename = "ui.first_frame")]
    UiFirstFrame,
    /// UI reports that its last window has closed.
    #[serde(rename = "ui.hidden")]
    UiHidden,
    /// The `query.facts` request.
    #[serde(rename = "query.facts")]
    QueryFacts,
    /// The `query.day` request.
    #[serde(rename = "query.day")]
    QueryDay,
    /// The `query.timeline` request.
    #[serde(rename = "query.timeline")]
    QueryTimeline,
    /// The `query.range` request.
    #[serde(rename = "query.range")]
    QueryRange,
    /// The `query.segment_detail` request.
    #[serde(rename = "query.segment_detail")]
    QuerySegmentDetail,
    /// The `query.entity_detail` request.
    #[serde(rename = "query.entity_detail")]
    QueryEntityDetail,
    /// The `query.unclassified` request.
    #[serde(rename = "query.unclassified")]
    QueryUnclassified,
    /// The `query.annotations` request.
    #[serde(rename = "query.annotations")]
    QueryAnnotations,
    /// The `query.pending_away` request.
    #[serde(rename = "query.pending_away")]
    QueryPendingAway,
    /// The `rule_sets.tree` request.
    #[serde(rename = "rule_sets.tree")]
    RuleSetsTree,
    /// The `rule_sets.get` request.
    #[serde(rename = "rule_sets.get")]
    RuleSetsGet,
    /// The `rule_sets.set_enabled` request.
    #[serde(rename = "rule_sets.set_enabled")]
    RuleSetsSetEnabled,
    /// The `rule_sets.delete` request.
    #[serde(rename = "rule_sets.delete")]
    RuleSetsDelete,
    /// The `rule_sets.edit` request.
    #[serde(rename = "rule_sets.edit")]
    RuleSetsEdit,
    /// The `rules.preview` request.
    #[serde(rename = "rules.preview")]
    RulesPreview,
    /// The `rules_text.validate` request.
    #[serde(rename = "rules_text.validate")]
    RulesTextValidate,
    /// The `rules_text.import` request.
    #[serde(rename = "rules_text.import")]
    RulesTextImport,
    /// The `rules_text.export` request.
    #[serde(rename = "rules_text.export")]
    RulesTextExport,
    /// The `categories.list` request.
    #[serde(rename = "categories.list")]
    CategoriesList,
    /// The `categories.create` request.
    #[serde(rename = "categories.create")]
    CategoriesCreate,
    /// The `categories.update` request.
    #[serde(rename = "categories.update")]
    CategoriesUpdate,
    /// The `categories.archive` request.
    #[serde(rename = "categories.archive")]
    CategoriesArchive,
    /// The `categories.reorder` request.
    #[serde(rename = "categories.reorder")]
    CategoriesReorder,
    /// The `annotations.create` request.
    #[serde(rename = "annotations.create")]
    AnnotationsCreate,
    /// The `annotations.update` request.
    #[serde(rename = "annotations.update")]
    AnnotationsUpdate,
    /// The `annotations.delete` request.
    #[serde(rename = "annotations.delete")]
    AnnotationsDelete,
    /// The `settings.get` request.
    #[serde(rename = "settings.get")]
    SettingsGet,
    /// The `settings.set` request.
    #[serde(rename = "settings.set")]
    SettingsSet,
    /// The `tracking.pause` request.
    #[serde(rename = "tracking.pause")]
    TrackingPause,
    /// The `tracking.resume` request.
    #[serde(rename = "tracking.resume")]
    TrackingResume,
    /// The `tracking.keep_alive` request.
    #[serde(rename = "tracking.keep_alive")]
    TrackingKeepAlive,
    /// The `privacy_mode.set` request.
    #[serde(rename = "privacy_mode.set")]
    PrivacyModeSet,
    /// The `data.info` request.
    #[serde(rename = "data.info")]
    DataInfo,
    /// The `data.backup` request.
    #[serde(rename = "data.backup")]
    DataBackup,
    /// The `data.compact` request.
    #[serde(rename = "data.compact")]
    DataCompact,
    /// The `data.delete_range` request.
    #[serde(rename = "data.delete_range")]
    DataDeleteRange,
    /// The `browser.state` request.
    #[serde(rename = "browser.state")]
    BrowserState,
    /// The `browser_hosts.status` request.
    #[serde(rename = "browser_hosts.status")]
    BrowserHostsStatus,
    /// A future or invalid request name; respond with `unknown_op`.
    #[serde(other, skip_serializing)]
    Unknown,
}

impl Op {
    /// Returns the prefix family used for authorization.
    #[must_use]
    pub const fn family(self) -> OpFamily {
        match self {
            Self::Hello => OpFamily::Hello,
            Self::Subscribe | Self::Unsubscribe => OpFamily::Subscribe,
            Self::CoreStatus => OpFamily::Core,
            Self::UiOpen | Self::UiFirstFrame | Self::UiHidden => OpFamily::Ui,
            Self::QueryFacts
            | Self::QueryDay
            | Self::QueryTimeline
            | Self::QueryRange
            | Self::QuerySegmentDetail
            | Self::QueryEntityDetail
            | Self::QueryUnclassified
            | Self::QueryPendingAway
            | Self::QueryAnnotations => OpFamily::Query,
            Self::RuleSetsTree
            | Self::RuleSetsGet
            | Self::RuleSetsSetEnabled
            | Self::RuleSetsDelete
            | Self::RuleSetsEdit => OpFamily::RuleSets,
            Self::RulesPreview => OpFamily::Rules,
            Self::RulesTextValidate | Self::RulesTextImport | Self::RulesTextExport => {
                OpFamily::RulesText
            }
            Self::CategoriesList
            | Self::CategoriesCreate
            | Self::CategoriesUpdate
            | Self::CategoriesArchive
            | Self::CategoriesReorder => OpFamily::Categories,
            Self::AnnotationsCreate | Self::AnnotationsUpdate | Self::AnnotationsDelete => {
                OpFamily::Annotations
            }
            Self::SettingsGet | Self::SettingsSet => OpFamily::Settings,
            Self::TrackingPause | Self::TrackingResume | Self::TrackingKeepAlive => {
                OpFamily::Tracking
            }
            Self::PrivacyModeSet => OpFamily::PrivacyMode,
            Self::DataInfo | Self::DataBackup | Self::DataCompact | Self::DataDeleteRange => {
                OpFamily::Data
            }
            Self::BrowserState => OpFamily::Browser,
            Self::BrowserHostsStatus => OpFamily::BrowserHosts,
            Self::Unknown => OpFamily::Unknown,
        }
    }
}

/// Operations pushed in event bodies on the `control` topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ControlOp {
    /// The `ui.show` control notification.
    #[serde(rename = "ui.show")]
    UiShow,
    /// The `ui.activate` control notification.
    #[serde(rename = "ui.activate")]
    UiActivate,
    /// The `ui.shutdown` control notification.
    #[serde(rename = "ui.shutdown")]
    UiShutdown,
}

/// A typed control operation with raw, operation-specific JSON fields.
///
/// Use as the body of an [`crate::Event`] with [`crate::Topic::Control`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlBody {
    /// Control operation, never a request operation.
    pub op: ControlOp,
    /// Operation-specific fields at the same object level as `op`.
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}
