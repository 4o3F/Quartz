//! Rules page view model: rule-set tree, selected set detail, load states, and mutation state
//! machine without GPUI dependencies so behavior is unit-tested on every platform.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
};

use quartz_protocol::{self as wire, ErrorKind};

use crate::{
    link::{LoadDataError, LoadDataErrorCode},
    model::{BuiltinCategory, CategoryColor, CategoryName, PaletteSlot, Tendency},
};

/// Maximum category name length in Unicode scalar values (04 §6.3.5).
const MAX_CATEGORY_NAME_SCALARS: usize = 256;
/// Reserved prefix for canonical built-in category identifiers.
const BUILTIN_IDENTIFIER_PREFIX: &str = "builtin:";
/// Length of a canonical 8-4-4-4-12 UUID string.
const UUID_TEXT_LEN: usize = 36;
/// Byte positions of hyphens in a canonical UUID string.
const UUID_HYPHENS: [usize; 4] = [8, 13, 18, 23];

/// Monotonic generation counter for dropping late rule-set responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct RulesGeneration(u64);

impl RulesGeneration {
    /// Returns the next monotonic generation.
    #[must_use]
    pub(crate) const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Returns whether `incoming` matches the active generation.
    #[must_use]
    pub(crate) const fn accepts(self, incoming: Self) -> bool {
        self.0 == incoming.0
    }
}

/// Stored enablement flag of a rule set itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuleSetEnablement {
    Enabled,
    Disabled,
}

impl RuleSetEnablement {
    /// Maps the wire boolean flag into a typed enablement state.
    #[must_use]
    pub(crate) const fn from_wire(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }

    /// Returns the opposite enablement state.
    #[must_use]
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Enabled => Self::Disabled,
            Self::Disabled => Self::Enabled,
        }
    }

    /// Converts to the wire boolean expected by `rule_sets.set_enabled`.
    #[must_use]
    pub(crate) const fn into_wire(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Effective enablement of a node within the rule-set hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeEnablement {
    /// Both this set and all of its ancestors are enabled.
    Enabled,
    /// This set is directly disabled while its ancestors are enabled.
    Disabled,
    /// An ancestor rule set is disabled, suppressing this entire subtree.
    DisabledByParent,
}

impl NodeEnablement {
    /// Whether this node participates in rule resolution.
    #[must_use]
    pub(crate) const fn is_effective(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Whether a rule set authors overrides/disables or is overridden by active descendants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverrideMarker {
    None,
    OverridesAncestor,
    OverriddenByChild,
    Both,
}

impl OverrideMarker {
    /// Whether an active descendant overrides or disables entries in this set's subtree.
    #[must_use]
    pub(crate) const fn is_overridden(self) -> bool {
        matches!(self, Self::OverriddenByChild | Self::Both)
    }

    /// Whether this set itself authors `[[override]]` or `[[disable]]` entries.
    #[must_use]
    pub(crate) const fn authors_overrides(self) -> bool {
        matches!(self, Self::OverridesAncestor | Self::Both)
    }
}

/// Failure when mapping one or more `rule_sets.tree` responses into a [`RuleTree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
pub(crate) enum MapRuleTreeError {
    #[snafu(display("rule-set tree responses came from different registry versions"))]
    VersionMismatch,
    #[snafu(display("rule-set tree response contained a duplicate key"))]
    DuplicateKey,
    #[snafu(display("rule-set tree response contained a parent cycle"))]
    Cycle,
}

/// One node in the rule-set tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleTreeNode {
    pub(crate) key: String,
    pub(crate) parent: Option<String>,
    pub(crate) kind: wire::RuleKind,
    pub(crate) source: wire::RuleSetSource,
    pub(crate) version: u32,
    pub(crate) self_enablement: RuleSetEnablement,
    pub(crate) enablement: NodeEnablement,
    pub(crate) override_marker: OverrideMarker,
    pub(crate) counts: wire::RuleSetCounts,
    pub(crate) warnings: Vec<wire::RuleSetWarning>,
    pub(crate) children: Vec<RuleTreeNode>,
}

impl RuleTreeNode {
    fn find(&self, key: &str) -> Option<&Self> {
        if self.key == key {
            return Some(self);
        }
        for child in &self.children {
            if let Some(found) = child.find(key) {
                return Some(found);
            }
        }
        None
    }

    fn flatten_into<'a>(&'a self, depth: usize, output: &mut Vec<FlatTreeRow<'a>>) {
        output.push(FlatTreeRow { node: self, depth });
        for child in &self.children {
            child.flatten_into(depth.saturating_add(1), output);
        }
    }

    fn subtree_count(&self) -> usize {
        self.children.iter().fold(1_usize, |acc, child| {
            acc.saturating_add(child.subtree_count())
        })
    }
}

/// One row of the flattened tree in preorder traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FlatTreeRow<'a> {
    pub(crate) node: &'a RuleTreeNode,
    pub(crate) depth: usize,
}

/// Mapped forest of rule sets across loaded rule families.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RuleTree {
    pub(crate) roots: Vec<RuleTreeNode>,
    pub(crate) versions: Option<wire::QueryVersions>,
    pub(crate) truncated: bool,
}

impl RuleTree {
    /// Builds a rule-set tree from one or more `rule_sets.tree` family responses.
    ///
    /// # Errors
    /// Returns [`MapRuleTreeError`] if responses disagree on registry version, duplicate a key,
    /// or form a parent cycle.
    pub(crate) fn from_responses(
        responses: &[wire::RuleSetsTreeResponse],
    ) -> Result<Self, MapRuleTreeError> {
        let mut versions: Option<wire::QueryVersions> = None;
        let mut truncated = false;
        let mut roots = Vec::new();
        let mut seen_keys = BTreeSet::new();

        for response in responses {
            if let Some(expected) = &versions {
                if expected != &response.versions {
                    return Err(MapRuleTreeError::VersionMismatch);
                }
            } else {
                versions = Some(response.versions.clone());
            }
            if response.truncated {
                truncated = true;
            }
            let mut by_key: BTreeMap<&str, &wire::RuleSetNode> = BTreeMap::new();
            for node in &response.nodes {
                if !seen_keys.insert(node.key.as_str()) {
                    return Err(MapRuleTreeError::DuplicateKey);
                }
                by_key.insert(node.key.as_str(), node);
            }
            let mut root_keys = Vec::new();
            let mut children_by_parent: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for node in &response.nodes {
                match node.parent.as_deref() {
                    Some(parent) if by_key.contains_key(parent) => {
                        children_by_parent
                            .entry(parent)
                            .or_default()
                            .push(node.key.as_str());
                    }
                    _ => root_keys.push(node.key.as_str()),
                }
            }
            let mut built_keys = BTreeSet::new();
            for root_key in root_keys {
                let mut visiting = BTreeSet::new();
                let tree_node = build_tree_node(
                    root_key,
                    response.kind,
                    NodeEnablement::Enabled,
                    &by_key,
                    &children_by_parent,
                    &mut visiting,
                    &mut built_keys,
                )?;
                roots.push(tree_node);
            }
            if built_keys.len() != by_key.len() {
                return Err(MapRuleTreeError::Cycle);
            }
        }

        Ok(Self {
            roots,
            versions,
            truncated,
        })
    }

    /// Looks up a rule-set node by its key.
    #[must_use]
    pub(crate) fn find(&self, key: &str) -> Option<&RuleTreeNode> {
        for root in &self.roots {
            if let Some(found) = root.find(key) {
                return Some(found);
            }
        }
        None
    }

    /// Returns all nodes in preorder traversal with their indentation depth.
    #[must_use]
    pub(crate) fn flatten(&self) -> Vec<FlatTreeRow<'_>> {
        let mut rows = Vec::new();
        for root in &self.roots {
            root.flatten_into(0, &mut rows);
        }
        rows
    }

    /// Total number of nodes across all families in this tree.
    #[must_use]
    pub(crate) fn node_count(&self) -> usize {
        self.roots.iter().fold(0_usize, |acc, root| {
            acc.saturating_add(root.subtree_count())
        })
    }
}

fn build_tree_node<'a>(
    key: &'a str,
    kind: wire::RuleKind,
    parent_enablement: NodeEnablement,
    by_key: &BTreeMap<&'a str, &'a wire::RuleSetNode>,
    children_by_parent: &BTreeMap<&'a str, Vec<&'a str>>,
    visiting: &mut BTreeSet<&'a str>,
    built_keys: &mut BTreeSet<&'a str>,
) -> Result<RuleTreeNode, MapRuleTreeError> {
    if !visiting.insert(key) {
        return Err(MapRuleTreeError::Cycle);
    }
    let Some(wire_node) = by_key.get(key).copied() else {
        return Err(MapRuleTreeError::Cycle);
    };
    let self_enablement = RuleSetEnablement::from_wire(wire_node.enabled);
    let enablement = match (parent_enablement, self_enablement) {
        (NodeEnablement::Enabled, RuleSetEnablement::Enabled) => NodeEnablement::Enabled,
        (NodeEnablement::Enabled, RuleSetEnablement::Disabled) => NodeEnablement::Disabled,
        (NodeEnablement::Disabled | NodeEnablement::DisabledByParent, _) => {
            NodeEnablement::DisabledByParent
        }
    };
    let mut children = Vec::new();
    if let Some(child_keys) = children_by_parent.get(key) {
        for child_key in child_keys {
            children.push(build_tree_node(
                child_key,
                kind,
                enablement,
                by_key,
                children_by_parent,
                visiting,
                built_keys,
            )?);
        }
    }
    visiting.remove(key);
    built_keys.insert(key);

    let authors_overrides = wire_node.counts.overridden > 0 || wire_node.counts.disabled > 0;
    let overridden_by_child = children.iter().any(|child| {
        child.enablement.is_effective()
            && (child.override_marker.authors_overrides() || child.override_marker.is_overridden())
    });
    let override_marker = match (authors_overrides, overridden_by_child) {
        (false, false) => OverrideMarker::None,
        (true, false) => OverrideMarker::OverridesAncestor,
        (false, true) => OverrideMarker::OverriddenByChild,
        (true, true) => OverrideMarker::Both,
    };

    Ok(RuleTreeNode {
        key: wire_node.key.clone(),
        parent: wire_node.parent.clone(),
        kind,
        source: wire_node.source,
        version: wire_node.version,
        self_enablement,
        enablement,
        override_marker,
        counts: wire_node.counts.clone(),
        warnings: wire_node.warnings.clone(),
        children,
    })
}

/// Display label of a rule entry, kept language-independent until formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuleEntryLabel {
    Text(String),
    Key(String),
    Identifier(String),
    Unnamed,
}

/// Polarity of a single rule condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConditionPolarity {
    Positive,
    Negated,
}

/// Typed condition line mapped from a protocol [`wire::RuleCondition`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConditionItem {
    pub(crate) field: wire::RuleField,
    pub(crate) op: wire::RuleOperator,
    pub(crate) value: String,
    pub(crate) polarity: ConditionPolarity,
}

impl From<&wire::RuleCondition> for ConditionItem {
    fn from(condition: &wire::RuleCondition) -> Self {
        Self {
            field: condition.field,
            op: condition.op,
            value: condition.value.clone(),
            polarity: if condition.negate {
                ConditionPolarity::Negated
            } else {
                ConditionPolarity::Positive
            },
        }
    }
}

/// Privacy retention actions summarized on a privacy entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrivacyEffectSummary {
    pub(crate) app: Option<wire::RuleAction>,
    pub(crate) window_title: Option<wire::RuleAction>,
    pub(crate) web: Option<wire::RuleWebAction>,
}

/// Structured summary of an entry's matching conditions and effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EntryConditionSummary {
    DisableTarget {
        target: String,
        reason: Option<String>,
    },
    Conditions {
        combinator: wire::RuleMatch,
        items: Vec<ConditionItem>,
        privacy: Option<PrivacyEffectSummary>,
    },
    Extraction {
        app_conditions: Vec<ConditionItem>,
        pattern: Option<String>,
    },
    Empty,
}

/// One entry inside the selected rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleEntryDetail {
    pub(crate) entry_kind: wire::RuleEntryKind,
    pub(crate) state: wire::RuleEntryState,
    pub(crate) rule_id: Option<String>,
    pub(crate) label: RuleEntryLabel,
    pub(crate) category: Option<wire::Uuid>,
    pub(crate) target: Option<String>,
    pub(crate) priority: Option<u16>,
    pub(crate) summary: EntryConditionSummary,
    pub(crate) provenance: Vec<wire::RuleProvenance>,
}

impl RuleEntryDetail {
    /// Returns the qualified `<set-key>/<rule-id>` target for overriding or disabling this rule.
    #[cfg(windows)]
    #[must_use]
    pub(crate) fn qualified_target(&self, set_key: &str) -> Option<String> {
        if let Some(first) = self.provenance.first() {
            return Some(format!("{}/{}", first.key, first.id));
        }
        self.rule_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| format!("{set_key}/{id}"))
    }

    /// Maps a single protocol rule-set entry into a typed detail row.
    #[must_use]
    pub(crate) fn from_protocol(entry: wire::RuleSetEntry) -> Self {
        let content = entry.content;
        let rule_id = content
            .id
            .clone()
            .filter(|id| !id.is_empty())
            .or_else(|| entry.provenance.first().map(|step| step.id.clone()));
        let label = if let Some(text) = content.label.filter(|text| !text.is_empty()) {
            RuleEntryLabel::Text(text)
        } else if let Some(key) = content.label_key.filter(|key| !key.is_empty()) {
            RuleEntryLabel::Key(key)
        } else if let Some(id) = content.id.filter(|id| !id.is_empty()) {
            RuleEntryLabel::Identifier(id)
        } else if let Some(target) = content.target.as_ref().filter(|target| !target.is_empty()) {
            RuleEntryLabel::Identifier(target.clone())
        } else if let Some(last) = entry.provenance.last() {
            RuleEntryLabel::Identifier(format!("{}/{}", last.key, last.id))
        } else {
            RuleEntryLabel::Unnamed
        };

        let summary = if entry.entry_kind == wire::RuleEntryKind::Disable {
            EntryConditionSummary::DisableTarget {
                target: content.target.clone().unwrap_or_default(),
                reason: content.reason.filter(|reason| !reason.is_empty()),
            }
        } else if let Some(wire::RuleApp::Conditions(conditions)) = &content.app {
            EntryConditionSummary::Extraction {
                app_conditions: conditions.iter().map(ConditionItem::from).collect(),
                pattern: content.pattern.filter(|pattern| !pattern.is_empty()),
            }
        } else if content.pattern.is_some() {
            EntryConditionSummary::Extraction {
                app_conditions: Vec::new(),
                pattern: content.pattern.filter(|pattern| !pattern.is_empty()),
            }
        } else {
            let privacy_app = match content.app {
                Some(wire::RuleApp::Action(action)) => Some(action),
                _ => None,
            };
            let privacy =
                if privacy_app.is_some() || content.window_title.is_some() || content.web.is_some()
                {
                    Some(PrivacyEffectSummary {
                        app: privacy_app,
                        window_title: content.window_title,
                        web: content.web,
                    })
                } else {
                    None
                };
            let items: Vec<ConditionItem> = content
                .when
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(ConditionItem::from)
                .collect();
            if items.is_empty() && privacy.is_none() {
                EntryConditionSummary::Empty
            } else {
                EntryConditionSummary::Conditions {
                    combinator: content.r#match.unwrap_or(wire::RuleMatch::All),
                    items,
                    privacy,
                }
            }
        };

        Self {
            entry_kind: entry.entry_kind,
            state: entry.state,
            rule_id,
            label,
            category: content.category,
            target: content.target,
            priority: content.priority,
            summary,
            provenance: entry.provenance,
        }
    }
}

/// Loaded detail of the selected rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleSetDetail {
    pub(crate) key: String,
    pub(crate) version: u32,
    pub(crate) parent_version: Option<u32>,
    pub(crate) entries: Vec<RuleEntryDetail>,
    pub(crate) truncated: bool,
}

impl RuleSetDetail {
    /// Maps a `rule_sets.get` response into a typed detail model.
    #[must_use]
    pub(crate) fn from_protocol(response: wire::RuleSetsGetResponse) -> Self {
        Self {
            key: response.key,
            version: response.version,
            parent_version: response.parent_version,
            entries: response
                .entries
                .into_iter()
                .map(RuleEntryDetail::from_protocol)
                .collect(),
            truncated: response.truncated,
        }
    }
}

/// Active section inside the Rules page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RulesTab {
    #[default]
    RuleSets,
    Categories,
}

/// Whether archived categories are visible in the categories list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ArchivedVisibility {
    #[default]
    Hidden,
    Visible,
}

impl ArchivedVisibility {
    /// Returns the opposite visibility state.
    #[must_use]
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Hidden => Self::Visible,
            Self::Visible => Self::Hidden,
        }
    }
}

/// Archive status of a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CategoryArchiveState {
    Active,
    Archived,
}

impl CategoryArchiveState {
    /// Maps the wire boolean flag into a typed archive state.
    #[must_use]
    pub(crate) const fn from_wire(archived: bool) -> Self {
        if archived {
            Self::Archived
        } else {
            Self::Active
        }
    }
}

/// Origin of a category: built-in or user-created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CategoryOrigin {
    Builtin(BuiltinCategory),
    User,
}

/// Direction of sibling reordering inside the categories list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReorderDirection {
    Up,
    Down,
}

/// Failure when mapping `categories.list` into a [`CategoryCatalog`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
pub(crate) enum MapCategoriesError {
    #[snafu(display("category list contained a duplicate UUID"))]
    DuplicateUuid,
    #[snafu(display("user category is missing a display name"))]
    MissingName,
    #[snafu(display("category color is outside the supported palette"))]
    InvalidColor,
    #[snafu(display("category list contained a parent cycle"))]
    ParentCycle,
}

/// Client-side validation failure for a category name input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, snafu::Snafu)]
pub(crate) enum ValidateCategoryNameError {
    #[snafu(display("category name cannot be blank"))]
    Blank,
    #[snafu(display("category name exceeds the maximum length"))]
    TooLong,
    #[snafu(display("category name uses a reserved identifier"))]
    Reserved,
}

/// Outcome of resolving a rule entry's category UUID against the loaded catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedCategory {
    Active {
        name: CategoryName,
        color: CategoryColor,
    },
    Missing,
}

/// One category row mapped from `categories.list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CategoryItem {
    pub(crate) uuid: wire::Uuid,
    pub(crate) parent_uuid: Option<wire::Uuid>,
    pub(crate) origin: CategoryOrigin,
    pub(crate) custom_name: Option<String>,
    pub(crate) display_name: CategoryName,
    pub(crate) color: CategoryColor,
    pub(crate) tendency: Tendency,
    pub(crate) archive_state: CategoryArchiveState,
    pub(crate) depth: usize,
}

/// Loaded category catalog in server hierarchy preorder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CategoryCatalog {
    pub(crate) items: Vec<CategoryItem>,
    pub(crate) truncated: bool,
}

impl CategoryCatalog {
    /// Maps a `categories.list` response into a typed catalog.
    ///
    /// # Errors
    /// Returns [`MapCategoriesError`] if UUIDs repeat, colors are invalid, a user category lacks
    /// a name, or parent pointers form a cycle.
    pub(crate) fn from_protocol(
        response: wire::CategoriesListResponse,
    ) -> Result<Self, MapCategoriesError> {
        let mut parent_by_uuid: BTreeMap<wire::Uuid, Option<wire::Uuid>> = BTreeMap::new();
        for entity in &response.entities {
            if parent_by_uuid
                .insert(entity.uuid.clone(), entity.parent_uuid.clone())
                .is_some()
            {
                return Err(MapCategoriesError::DuplicateUuid);
            }
        }

        let mut items = Vec::with_capacity(response.entities.len());
        for entity in response.entities {
            let depth = compute_category_depth(&entity.uuid, &parent_by_uuid)?;
            let origin = match entity.builtin_key {
                Some(builtin) => CategoryOrigin::Builtin(builtin_from_wire(builtin)),
                None => CategoryOrigin::User,
            };
            let custom_name = entity
                .name
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty());
            let display_name = match (origin, &custom_name) {
                (_, Some(name)) => CategoryName::Custom(name.clone()),
                (CategoryOrigin::Builtin(builtin), None) => CategoryName::Builtin(builtin),
                (CategoryOrigin::User, None) => return Err(MapCategoriesError::MissingName),
            };
            let color =
                CategoryColor::parse_wire(&entity.color).ok_or(MapCategoriesError::InvalidColor)?;
            items.push(CategoryItem {
                uuid: entity.uuid,
                parent_uuid: entity.parent_uuid,
                origin,
                custom_name,
                display_name,
                color,
                tendency: tendency_from_wire(entity.tendency),
                archive_state: CategoryArchiveState::from_wire(entity.archived),
                depth,
            });
        }

        Ok(Self {
            items,
            truncated: response.truncated,
        })
    }

    /// Looks up a category by UUID.
    #[must_use]
    pub(crate) fn find(&self, uuid: &wire::Uuid) -> Option<&CategoryItem> {
        self.items.iter().find(|item| &item.uuid == uuid)
    }

    /// Active (non-archived) categories in server preorder.
    #[must_use]
    pub(crate) fn active_items(&self) -> Vec<&CategoryItem> {
        self.items
            .iter()
            .filter(|item| item.archive_state == CategoryArchiveState::Active)
            .collect()
    }

    /// Archived categories in server preorder.
    #[must_use]
    pub(crate) fn archived_items(&self) -> Vec<&CategoryItem> {
        self.items
            .iter()
            .filter(|item| item.archive_state == CategoryArchiveState::Archived)
            .collect()
    }

    /// Resolves a rule entry's category UUID to its active name and color, or marks it missing
    /// when unknown or archived.
    #[must_use]
    pub(crate) fn resolve(&self, uuid: &wire::Uuid) -> ResolvedCategory {
        match self.find(uuid) {
            Some(item) if item.archive_state == CategoryArchiveState::Active => {
                ResolvedCategory::Active {
                    name: item.display_name.clone(),
                    color: item.color,
                }
            }
            Some(_) | None => ResolvedCategory::Missing,
        }
    }

    /// Whether an active category can move in `direction` among its active siblings.
    #[must_use]
    pub(crate) fn can_reorder(&self, uuid: &wire::Uuid, direction: ReorderDirection) -> bool {
        self.reorder_request(uuid, direction).is_some()
    }

    /// Builds a `categories.reorder` request swapping `uuid` with its adjacent active sibling
    /// while preserving every sibling (including archived ones) under the same parent.
    #[must_use]
    pub(crate) fn reorder_request(
        &self,
        uuid: &wire::Uuid,
        direction: ReorderDirection,
    ) -> Option<wire::CategoriesReorderRequest> {
        let item = self.find(uuid)?;
        if item.archive_state != CategoryArchiveState::Active {
            return None;
        }
        let parent_uuid = item.parent_uuid.clone();
        let mut all_siblings: Vec<wire::Uuid> = Vec::new();
        let mut active_positions: Vec<usize> = Vec::new();
        for candidate in &self.items {
            if candidate.parent_uuid == parent_uuid {
                let index = all_siblings.len();
                all_siblings.push(candidate.uuid.clone());
                if candidate.archive_state == CategoryArchiveState::Active {
                    active_positions.push(index);
                }
            }
        }
        let active_index = active_positions
            .iter()
            .position(|&idx| all_siblings.get(idx).is_some_and(|sibling| sibling == uuid))?;
        let target_active_index = match direction {
            ReorderDirection::Up => active_index.checked_sub(1)?,
            ReorderDirection::Down => {
                let next = active_index.checked_add(1)?;
                (next < active_positions.len()).then_some(next)?
            }
        };
        let from_pos = *active_positions.get(active_index)?;
        let to_pos = *active_positions.get(target_active_index)?;
        all_siblings.swap(from_pos, to_pos);
        Some(wire::CategoriesReorderRequest {
            parent_uuid,
            uuids: all_siblings,
        })
    }

    /// Next recommended palette color for a newly created category.
    #[must_use]
    pub(crate) fn next_default_color(&self) -> CategoryColor {
        let used: Vec<CategoryColor> = self
            .active_items()
            .into_iter()
            .map(|item| item.color)
            .collect();
        CategoryColor::Palette(PaletteSlot::first_unused(&used).unwrap_or(PaletteSlot::numbered(1)))
    }
}

fn compute_category_depth(
    uuid: &wire::Uuid,
    parent_by_uuid: &BTreeMap<wire::Uuid, Option<wire::Uuid>>,
) -> Result<usize, MapCategoriesError> {
    let mut visited = BTreeSet::new();
    visited.insert(uuid);
    let mut depth = 0_usize;
    let mut current = uuid;
    while let Some(Some(parent)) = parent_by_uuid.get(current) {
        if !parent_by_uuid.contains_key(parent) {
            break;
        }
        if !visited.insert(parent) {
            return Err(MapCategoriesError::ParentCycle);
        }
        depth = depth.saturating_add(1);
        current = parent;
    }
    Ok(depth)
}

const fn builtin_from_wire(builtin: wire::BuiltinCategory) -> BuiltinCategory {
    match builtin {
        wire::BuiltinCategory::Development => BuiltinCategory::Development,
        wire::BuiltinCategory::Learning => BuiltinCategory::Learning,
        wire::BuiltinCategory::Communication => BuiltinCategory::Communication,
        wire::BuiltinCategory::Entertainment => BuiltinCategory::Entertainment,
        wire::BuiltinCategory::System => BuiltinCategory::System,
    }
}

const fn tendency_from_wire(tendency: wire::CategoryTendency) -> Tendency {
    match tendency {
        wire::CategoryTendency::Focus => Tendency::Focus,
        wire::CategoryTendency::Neutral => Tendency::Neutral,
        wire::CategoryTendency::Distraction => Tendency::Distraction,
    }
}

const fn tendency_into_wire(tendency: Tendency) -> wire::CategoryTendency {
    match tendency {
        Tendency::Focus => wire::CategoryTendency::Focus,
        Tendency::Neutral => wire::CategoryTendency::Neutral,
        Tendency::Distraction => wire::CategoryTendency::Distraction,
    }
}

fn is_reserved_category_name(value: &str) -> bool {
    if value.starts_with(BUILTIN_IDENTIFIER_PREFIX) {
        return true;
    }
    value.len() == UUID_TEXT_LEN
        && value.bytes().enumerate().all(|(index, byte)| {
            if UUID_HYPHENS.contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn validate_category_name(raw: &str) -> Result<String, ValidateCategoryNameError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ValidateCategoryNameError::Blank);
    }
    if trimmed.chars().count() > MAX_CATEGORY_NAME_SCALARS {
        return Err(ValidateCategoryNameError::TooLong);
    }
    if is_reserved_category_name(trimmed) {
        return Err(ValidateCategoryNameError::Reserved);
    }
    Ok(trimmed.to_owned())
}

/// Whether the category editor is creating a new category or updating an existing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CategoryEditorMode {
    Create {
        uuid: wire::Uuid,
    },
    Edit {
        uuid: wire::Uuid,
        origin: CategoryOrigin,
        parent_uuid: Option<wire::Uuid>,
    },
}

/// Mutable form state of the category editor card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CategoryEditorDraft {
    pub(crate) mode: CategoryEditorMode,
    pub(crate) name_input: String,
    pub(crate) color: CategoryColor,
    pub(crate) tendency: Tendency,
    pub(crate) validation_error: Option<ValidateCategoryNameError>,
}

/// Inline confirmation state for archiving a category.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum ArchiveConfirmation {
    #[default]
    Idle,
    Pending {
        uuid: wire::Uuid,
    },
}

/// Typed category mutation emitted by [`RulesPage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CategoryMutation {
    Create(wire::CategoriesCreateRequest),
    Update(wire::CategoriesUpdateRequest),
    Archive(wire::CategoriesArchiveRequest),
    Reorder(wire::CategoriesReorderRequest),
}

/// Load lifecycle for the rule-set tree or selected rule-set detail.
#[derive(Debug)]
pub(crate) enum LoadState<T> {
    Loading,
    Ready(T),
    Failed(LoadDataError),
    Stale,
}

/// Inline confirmation state for deleting a user rule set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum DeleteConfirmation {
    #[default]
    Idle,
    Pending {
        key: String,
    },
}

/// Whether a rule-set mutation is currently in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MutationActivity {
    #[default]
    Idle,
    Busy,
}

/// Typed mutation request emitted by [`RulesPage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RulesMutation {
    SetEnabled(wire::RuleSetsSetEnabledRequest),
    Delete(wire::RuleSetsDeleteRequest),
}

/// Inline failure from a rule-set mutation, preserving loaded content.
#[derive(Debug)]
pub(crate) struct MutationNotice {
    pub(crate) error: LoadDataError,
}

/// Side-effect requested by a [`RulesPage`] transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RulesEffect {
    None,
    LoadPage {
        tree_generation: RulesGeneration,
        categories_generation: RulesGeneration,
    },
    LoadTree {
        generation: RulesGeneration,
    },
    LoadDetail {
        key: String,
        generation: RulesGeneration,
    },
    Mutate {
        mutation: RulesMutation,
        generation: RulesGeneration,
    },
    LoadCategories {
        generation: RulesGeneration,
    },
    MutateCategory {
        mutation: CategoryMutation,
        generation: RulesGeneration,
    },
}

/// Direction of keyboard movement in the rule-set tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeDirection {
    Up,
    Down,
}

/// Keyboard actions supported by the Rules page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RulesKeyAction {
    Move(TreeDirection),
    Open,
    ToggleEnabled,
    Cancel,
}

/// State machine for the Rules page: rule-set tree, selection, detail, categories, and mutations.
#[derive(Debug)]
pub(crate) struct RulesPage {
    active_tab: RulesTab,
    tree_state: LoadState<RuleTree>,
    selected_key: Option<String>,
    detail_state: LoadState<RuleSetDetail>,
    tree_generation: RulesGeneration,
    detail_generation: RulesGeneration,
    mutation_generation: RulesGeneration,
    delete_confirm: DeleteConfirmation,
    mutating: MutationActivity,
    mutation_notice: Option<MutationNotice>,
    categories_state: LoadState<CategoryCatalog>,
    categories_generation: RulesGeneration,
    category_mutation_generation: RulesGeneration,
    archived_visibility: ArchivedVisibility,
    category_editor: Option<CategoryEditorDraft>,
    archive_confirm: ArchiveConfirmation,
    category_mutating: MutationActivity,
    category_mutation_notice: Option<MutationNotice>,
}

impl Default for RulesPage {
    fn default() -> Self {
        Self::new()
    }
}

impl RulesPage {
    /// Creates an unloaded Rules page state.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            active_tab: RulesTab::default(),
            tree_state: LoadState::Loading,
            selected_key: None,
            detail_state: LoadState::Loading,
            tree_generation: RulesGeneration::default(),
            detail_generation: RulesGeneration::default(),
            mutation_generation: RulesGeneration::default(),
            delete_confirm: DeleteConfirmation::Idle,
            mutating: MutationActivity::Idle,
            mutation_notice: None,
            categories_state: LoadState::Loading,
            categories_generation: RulesGeneration::default(),
            category_mutation_generation: RulesGeneration::default(),
            archived_visibility: ArchivedVisibility::default(),
            category_editor: None,
            archive_confirm: ArchiveConfirmation::Idle,
            category_mutating: MutationActivity::Idle,
            category_mutation_notice: None,
        }
    }

    /// Starts loading the rule-set tree and category catalog when opening the Rules page.
    #[must_use]
    pub(crate) fn open(&mut self) -> RulesEffect {
        self.tree_generation = self.tree_generation.next();
        self.detail_generation = self.detail_generation.next();
        self.categories_generation = self.categories_generation.next();
        self.tree_state = LoadState::Loading;
        self.detail_state = LoadState::Loading;
        self.categories_state = LoadState::Loading;
        self.delete_confirm = DeleteConfirmation::Idle;
        self.mutation_notice = None;
        self.archive_confirm = ArchiveConfirmation::Idle;
        self.category_mutation_notice = None;
        self.category_editor = None;
        RulesEffect::LoadPage {
            tree_generation: self.tree_generation,
            categories_generation: self.categories_generation,
        }
    }

    /// Switches between the Rule sets and Categories sections of the Rules page.
    pub(crate) fn set_active_tab(&mut self, tab: RulesTab) {
        self.active_tab = tab;
    }

    /// Forces a fresh load of the rule-set tree, invalidating older in-flight responses.
    #[must_use]
    pub(crate) fn reload_tree(&mut self) -> RulesEffect {
        self.tree_generation = self.tree_generation.next();
        self.detail_generation = self.detail_generation.next();
        self.tree_state = LoadState::Loading;
        self.detail_state = LoadState::Loading;
        self.delete_confirm = DeleteConfirmation::Idle;
        self.mutation_notice = None;
        RulesEffect::LoadTree {
            generation: self.tree_generation,
        }
    }

    /// Reloads the rule-set tree and preserves `key` as the active selection once loaded.
    #[cfg(windows)]
    #[must_use]
    pub(crate) fn reload_tree_selecting(&mut self, key: String) -> RulesEffect {
        self.selected_key = Some(key);
        self.reload_tree()
    }

    /// Applies the result of a `rule_sets.tree` load, ignoring stale generations.
    #[must_use]
    pub(crate) fn apply_tree_loaded(
        &mut self,
        generation: RulesGeneration,
        result: Result<Vec<wire::RuleSetsTreeResponse>, LoadDataError>,
    ) -> RulesEffect {
        if !self.tree_generation.accepts(generation) {
            return RulesEffect::None;
        }
        match result {
            Ok(responses) => match RuleTree::from_responses(&responses) {
                Ok(tree) => {
                    let preserved = self
                        .selected_key
                        .as_deref()
                        .filter(|key| tree.find(key).is_some())
                        .map(str::to_owned);
                    let next_selected = preserved
                        .or_else(|| tree.flatten().first().map(|row| row.node.key.clone()));
                    self.tree_state = LoadState::Ready(tree);
                    self.selected_key = next_selected;
                    if let DeleteConfirmation::Pending { key } = &self.delete_confirm
                        && self.selected_key.as_deref() != Some(key.as_str())
                    {
                        self.delete_confirm = DeleteConfirmation::Idle;
                    }
                    if let Some(key) = self.selected_key.clone() {
                        self.detail_generation = self.detail_generation.next();
                        self.detail_state = LoadState::Loading;
                        RulesEffect::LoadDetail {
                            key,
                            generation: self.detail_generation,
                        }
                    } else {
                        self.detail_state = LoadState::Loading;
                        RulesEffect::None
                    }
                }
                Err(MapRuleTreeError::VersionMismatch) => {
                    self.tree_state = LoadState::Stale;
                    RulesEffect::None
                }
                Err(MapRuleTreeError::DuplicateKey | MapRuleTreeError::Cycle) => {
                    self.tree_state = LoadState::Failed(LoadDataError::InvalidData);
                    RulesEffect::None
                }
            },
            Err(LoadDataError::Stale) => {
                self.tree_state = LoadState::Stale;
                RulesEffect::None
            }
            Err(error) => {
                self.tree_state = LoadState::Failed(error);
                RulesEffect::None
            }
        }
    }

    /// Selects a rule set by key and requests its entries.
    #[must_use]
    pub(crate) fn select(&mut self, key: String) -> RulesEffect {
        self.delete_confirm = DeleteConfirmation::Idle;
        self.mutation_notice = None;
        self.selected_key = Some(key.clone());
        self.detail_generation = self.detail_generation.next();
        self.detail_state = LoadState::Loading;
        RulesEffect::LoadDetail {
            key,
            generation: self.detail_generation,
        }
    }

    /// Opens or reloads the currently selected rule set (or the first set if none is selected).
    #[must_use]
    pub(crate) fn open_selected(&mut self) -> RulesEffect {
        let key = self
            .selected_key
            .clone()
            .or_else(|| self.flat_rows().first().map(|row| row.node.key.clone()));
        match key {
            Some(key) => self.select(key),
            None => RulesEffect::None,
        }
    }

    /// Applies the result of a `rule_sets.get` request, dropping responses for older selections.
    pub(crate) fn apply_detail_loaded(
        &mut self,
        generation: RulesGeneration,
        result: Result<wire::RuleSetsGetResponse, LoadDataError>,
    ) {
        if !self.detail_generation.accepts(generation) {
            return;
        }
        match result {
            Ok(response) => {
                if self.selected_key.as_deref() != Some(response.key.as_str()) {
                    return;
                }
                let tree_version_mismatch = self
                    .tree()
                    .and_then(|tree| tree.versions.as_ref())
                    .is_some_and(|tree_versions| *tree_versions != response.versions);
                if tree_version_mismatch {
                    self.detail_state = LoadState::Stale;
                    return;
                }
                self.detail_state = LoadState::Ready(RuleSetDetail::from_protocol(response));
            }
            Err(error) => {
                if matches!(error, LoadDataError::Stale)
                    || matches!(
                        error.code(),
                        LoadDataErrorCode::Protocol(ErrorKind::RuleSetStale { .. })
                    )
                {
                    self.detail_state = LoadState::Stale;
                } else {
                    self.detail_state = LoadState::Failed(error);
                }
            }
        }
    }

    /// Requests enabling or disabling a user rule set. Built-in rule sets are read-only in the
    /// protocol and return [`RulesEffect::None`].
    #[must_use]
    pub(crate) fn toggle_enabled(&mut self, key: &str) -> RulesEffect {
        if self.mutating == MutationActivity::Busy {
            return RulesEffect::None;
        }
        let Some(node) = self.tree().and_then(|tree| tree.find(key)) else {
            return RulesEffect::None;
        };
        if node.source == wire::RuleSetSource::Builtin {
            return RulesEffect::None;
        }
        let Some(base_version) = NonZeroU32::new(node.version) else {
            return RulesEffect::None;
        };
        let request = wire::RuleSetsSetEnabledRequest {
            key: node.key.clone(),
            base_version,
            enabled: node.self_enablement.toggled().into_wire(),
        };
        self.delete_confirm = DeleteConfirmation::Idle;
        self.mutation_notice = None;
        self.mutating = MutationActivity::Busy;
        self.mutation_generation = self.mutation_generation.next();
        RulesEffect::Mutate {
            mutation: RulesMutation::SetEnabled(request),
            generation: self.mutation_generation,
        }
    }

    /// Opens the inline delete confirmation for a childless user rule set.
    pub(crate) fn request_delete(&mut self, key: &str) {
        if self.mutating == MutationActivity::Busy {
            return;
        }
        let Some(node) = self.tree().and_then(|tree| tree.find(key)) else {
            return;
        };
        if node.source != wire::RuleSetSource::User || !node.children.is_empty() {
            return;
        }
        let key = node.key.clone();
        self.mutation_notice = None;
        self.delete_confirm = DeleteConfirmation::Pending { key };
    }

    /// Dismisses any pending inline delete confirmation.
    pub(crate) fn cancel_delete(&mut self) {
        self.delete_confirm = DeleteConfirmation::Idle;
    }

    /// Executes `rule_sets.delete` only when inline confirmation is pending for `key`.
    #[must_use]
    pub(crate) fn confirm_delete(&mut self, key: &str) -> RulesEffect {
        if self.mutating == MutationActivity::Busy {
            return RulesEffect::None;
        }
        let DeleteConfirmation::Pending { key: pending } = &self.delete_confirm else {
            return RulesEffect::None;
        };
        if pending != key {
            return RulesEffect::None;
        }
        let Some(node) = self.tree().and_then(|tree| tree.find(key)) else {
            self.delete_confirm = DeleteConfirmation::Idle;
            return RulesEffect::None;
        };
        if node.source != wire::RuleSetSource::User || !node.children.is_empty() {
            self.delete_confirm = DeleteConfirmation::Idle;
            return RulesEffect::None;
        }
        let Some(base_version) = NonZeroU32::new(node.version) else {
            self.delete_confirm = DeleteConfirmation::Idle;
            return RulesEffect::None;
        };
        let request = wire::RuleSetsDeleteRequest {
            key: node.key.clone(),
            base_version,
        };
        self.delete_confirm = DeleteConfirmation::Idle;
        self.mutation_notice = None;
        self.mutating = MutationActivity::Busy;
        self.mutation_generation = self.mutation_generation.next();
        RulesEffect::Mutate {
            mutation: RulesMutation::Delete(request),
            generation: self.mutation_generation,
        }
    }

    /// Applies the outcome of a mutation; success reloads the rule-set tree.
    #[must_use]
    pub(crate) fn apply_mutation_finished(
        &mut self,
        generation: RulesGeneration,
        result: Result<wire::RuleSetsWriteResponse, LoadDataError>,
    ) -> RulesEffect {
        if !self.mutation_generation.accepts(generation) {
            return RulesEffect::None;
        }
        self.mutating = MutationActivity::Idle;
        match result {
            Ok(_) => self.reload_tree(),
            Err(error) => {
                if matches!(
                    error.code(),
                    LoadDataErrorCode::Protocol(ErrorKind::RuleSetStale { .. })
                ) {
                    self.tree_state = LoadState::Stale;
                } else {
                    self.mutation_notice = Some(MutationNotice { error });
                }
                RulesEffect::None
            }
        }
    }

    /// Handles keyboard navigation and shortcuts on the Rules page.
    ///
    /// Selection movement clamps at the first and last rows of the tree.
    #[must_use]
    pub(crate) fn handle_key(&mut self, action: RulesKeyAction) -> RulesEffect {
        if self.active_tab == RulesTab::Categories {
            if matches!(action, RulesKeyAction::Cancel) {
                if matches!(self.archive_confirm, ArchiveConfirmation::Pending { .. }) {
                    self.cancel_archive_category();
                } else {
                    self.close_category_editor();
                }
            }
            return RulesEffect::None;
        }
        match action {
            RulesKeyAction::Move(direction) => {
                let target_key = {
                    let rows = self.flat_rows();
                    if rows.is_empty() {
                        return RulesEffect::None;
                    }
                    let current = self
                        .selected_key
                        .as_deref()
                        .and_then(|key| rows.iter().position(|row| row.node.key == key));
                    let next_index = match (current, direction) {
                        (None, _) => 0,
                        (Some(index), TreeDirection::Up) => index.saturating_sub(1),
                        (Some(index), TreeDirection::Down) => {
                            index.saturating_add(1).min(rows.len().saturating_sub(1))
                        }
                    };
                    rows.get(next_index).map(|row| row.node.key.clone())
                };
                match target_key {
                    Some(key) if self.selected_key.as_deref() != Some(key.as_str()) => {
                        self.select(key)
                    }
                    _ => RulesEffect::None,
                }
            }
            RulesKeyAction::Open => self.open_selected(),
            RulesKeyAction::ToggleEnabled => match self.selected_key.clone() {
                Some(key) => self.toggle_enabled(&key),
                None => RulesEffect::None,
            },
            RulesKeyAction::Cancel => {
                self.cancel_delete();
                RulesEffect::None
            }
        }
    }

    /// Forces a fresh load of the category list, invalidating older in-flight responses.
    #[must_use]
    pub(crate) fn reload_categories(&mut self) -> RulesEffect {
        self.categories_generation = self.categories_generation.next();
        self.categories_state = LoadState::Loading;
        self.archive_confirm = ArchiveConfirmation::Idle;
        self.category_mutation_notice = None;
        RulesEffect::LoadCategories {
            generation: self.categories_generation,
        }
    }

    /// Applies the result of a `categories.list` load, ignoring stale generations.
    pub(crate) fn apply_categories_loaded(
        &mut self,
        generation: RulesGeneration,
        result: Result<wire::CategoriesListResponse, LoadDataError>,
    ) {
        if !self.categories_generation.accepts(generation) {
            return;
        }
        match result {
            Ok(response) => match CategoryCatalog::from_protocol(response) {
                Ok(catalog) => {
                    if let ArchiveConfirmation::Pending { uuid } = &self.archive_confirm
                        && catalog
                            .find(uuid)
                            .is_none_or(|item| item.archive_state != CategoryArchiveState::Active)
                    {
                        self.archive_confirm = ArchiveConfirmation::Idle;
                    }
                    self.categories_state = LoadState::Ready(catalog);
                }
                Err(_) => {
                    self.categories_state = LoadState::Failed(LoadDataError::InvalidData);
                }
            },
            Err(LoadDataError::Stale) => {
                self.categories_state = LoadState::Stale;
            }
            Err(error) => {
                self.categories_state = LoadState::Failed(error);
            }
        }
    }

    /// Toggles whether archived categories are shown below the active category list.
    pub(crate) fn toggle_archived_visibility(&mut self) {
        self.archived_visibility = self.archived_visibility.toggled();
    }

    /// Opens the category editor in create mode with a client-generated UUID.
    pub(crate) fn begin_create_category(&mut self, uuid: wire::Uuid) {
        if self.category_mutating == MutationActivity::Busy {
            return;
        }
        let color = self.categories().map_or(
            CategoryColor::Palette(PaletteSlot::numbered(1)),
            CategoryCatalog::next_default_color,
        );
        self.archive_confirm = ArchiveConfirmation::Idle;
        self.category_mutation_notice = None;
        self.category_editor = Some(CategoryEditorDraft {
            mode: CategoryEditorMode::Create { uuid },
            name_input: String::new(),
            color,
            tendency: Tendency::Neutral,
            validation_error: None,
        });
    }

    /// Opens the category editor for an existing category.
    pub(crate) fn begin_edit_category(&mut self, uuid: &wire::Uuid) {
        if self.category_mutating == MutationActivity::Busy {
            return;
        }
        let Some(item) = self.categories().and_then(|catalog| catalog.find(uuid)) else {
            return;
        };
        let draft = CategoryEditorDraft {
            mode: CategoryEditorMode::Edit {
                uuid: item.uuid.clone(),
                origin: item.origin,
                parent_uuid: item.parent_uuid.clone(),
            },
            name_input: item.custom_name.clone().unwrap_or_default(),
            color: item.color,
            tendency: item.tendency,
            validation_error: None,
        };
        self.archive_confirm = ArchiveConfirmation::Idle;
        self.category_mutation_notice = None;
        self.category_editor = Some(draft);
    }

    /// Closes the category editor without saving.
    pub(crate) fn close_category_editor(&mut self) {
        if self.category_mutating == MutationActivity::Busy {
            return;
        }
        self.category_editor = None;
    }

    /// Updates the draft name text and clears any prior validation notice.
    pub(crate) fn set_category_editor_name(&mut self, name: String) {
        if let Some(draft) = &mut self.category_editor {
            draft.name_input = name;
            draft.validation_error = None;
        }
    }

    /// Clears a renamed built-in category's custom name so saving restores the localized default.
    pub(crate) fn reset_builtin_editor_name(&mut self) {
        if let Some(draft) = &mut self.category_editor
            && matches!(
                draft.mode,
                CategoryEditorMode::Edit {
                    origin: CategoryOrigin::Builtin(_),
                    ..
                }
            )
        {
            draft.name_input.clear();
            draft.validation_error = None;
        }
    }

    /// Selects a palette color inside the category editor.
    pub(crate) fn set_category_editor_color(&mut self, color: CategoryColor) {
        if let Some(draft) = &mut self.category_editor {
            draft.color = color;
        }
    }

    /// Selects a focus tendency inside the category editor.
    pub(crate) fn set_category_editor_tendency(&mut self, tendency: Tendency) {
        if let Some(draft) = &mut self.category_editor {
            draft.tendency = tendency;
        }
    }

    /// Validates the category editor draft client-side and emits a create or update mutation.
    #[must_use]
    pub(crate) fn submit_category_editor(&mut self) -> RulesEffect {
        if self.category_mutating == MutationActivity::Busy {
            return RulesEffect::None;
        }
        let Some(draft) = &mut self.category_editor else {
            return RulesEffect::None;
        };
        let mutation = match &draft.mode {
            CategoryEditorMode::Create { uuid } => {
                let name = match validate_category_name(&draft.name_input) {
                    Ok(name) => name,
                    Err(error) => {
                        draft.validation_error = Some(error);
                        return RulesEffect::None;
                    }
                };
                CategoryMutation::Create(wire::CategoriesCreateRequest {
                    entity: wire::NewCategory {
                        uuid: uuid.clone(),
                        parent_uuid: None,
                        name: Some(name),
                        color: draft.color.token_key().to_owned(),
                        tendency: tendency_into_wire(draft.tendency),
                    },
                })
            }
            CategoryEditorMode::Edit {
                uuid,
                origin,
                parent_uuid,
            } => {
                let name = match origin {
                    CategoryOrigin::User => match validate_category_name(&draft.name_input) {
                        Ok(name) => Some(name),
                        Err(error) => {
                            draft.validation_error = Some(error);
                            return RulesEffect::None;
                        }
                    },
                    CategoryOrigin::Builtin(_) => {
                        if draft.name_input.trim().is_empty() {
                            None
                        } else {
                            match validate_category_name(&draft.name_input) {
                                Ok(name) => Some(name),
                                Err(error) => {
                                    draft.validation_error = Some(error);
                                    return RulesEffect::None;
                                }
                            }
                        }
                    }
                };
                CategoryMutation::Update(wire::CategoriesUpdateRequest {
                    entity: wire::CategoryEdit {
                        uuid: uuid.clone(),
                        parent_uuid: parent_uuid.clone(),
                        name,
                        color: draft.color.token_key().to_owned(),
                        tendency: tendency_into_wire(draft.tendency),
                    },
                })
            }
        };
        self.category_mutation_notice = None;
        self.category_mutating = MutationActivity::Busy;
        self.category_mutation_generation = self.category_mutation_generation.next();
        RulesEffect::MutateCategory {
            mutation,
            generation: self.category_mutation_generation,
        }
    }

    /// Opens the inline archive confirmation for an active category.
    pub(crate) fn request_archive_category(&mut self, uuid: &wire::Uuid) {
        if self.category_mutating == MutationActivity::Busy {
            return;
        }
        let Some(item) = self.categories().and_then(|catalog| catalog.find(uuid)) else {
            return;
        };
        if item.archive_state != CategoryArchiveState::Active {
            return;
        }
        let uuid = item.uuid.clone();
        self.category_mutation_notice = None;
        self.archive_confirm = ArchiveConfirmation::Pending { uuid };
    }

    /// Dismisses any pending inline archive confirmation.
    pub(crate) fn cancel_archive_category(&mut self) {
        self.archive_confirm = ArchiveConfirmation::Idle;
    }

    /// Executes `categories.archive` only when inline confirmation is pending for `uuid`.
    #[must_use]
    pub(crate) fn confirm_archive_category(&mut self, uuid: &wire::Uuid) -> RulesEffect {
        if self.category_mutating == MutationActivity::Busy {
            return RulesEffect::None;
        }
        let ArchiveConfirmation::Pending { uuid: pending } = &self.archive_confirm else {
            return RulesEffect::None;
        };
        if pending != uuid {
            return RulesEffect::None;
        }
        let Some(item) = self.categories().and_then(|catalog| catalog.find(uuid)) else {
            self.archive_confirm = ArchiveConfirmation::Idle;
            return RulesEffect::None;
        };
        if item.archive_state != CategoryArchiveState::Active {
            self.archive_confirm = ArchiveConfirmation::Idle;
            return RulesEffect::None;
        }
        let uuid = item.uuid.clone();
        self.archive_confirm = ArchiveConfirmation::Idle;
        self.category_mutation_notice = None;
        self.category_mutating = MutationActivity::Busy;
        self.category_mutation_generation = self.category_mutation_generation.next();
        RulesEffect::MutateCategory {
            mutation: CategoryMutation::Archive(wire::CategoriesArchiveRequest {
                uuid,
                archived: true,
            }),
            generation: self.category_mutation_generation,
        }
    }

    /// Restores an archived category back to active status.
    #[must_use]
    pub(crate) fn restore_category(&mut self, uuid: &wire::Uuid) -> RulesEffect {
        if self.category_mutating == MutationActivity::Busy {
            return RulesEffect::None;
        }
        let Some(item) = self.categories().and_then(|catalog| catalog.find(uuid)) else {
            return RulesEffect::None;
        };
        if item.archive_state != CategoryArchiveState::Archived {
            return RulesEffect::None;
        }
        let uuid = item.uuid.clone();
        self.archive_confirm = ArchiveConfirmation::Idle;
        self.category_mutation_notice = None;
        self.category_mutating = MutationActivity::Busy;
        self.category_mutation_generation = self.category_mutation_generation.next();
        RulesEffect::MutateCategory {
            mutation: CategoryMutation::Archive(wire::CategoriesArchiveRequest {
                uuid,
                archived: false,
            }),
            generation: self.category_mutation_generation,
        }
    }

    /// Moves an active category up or down among its siblings; no-op at sibling bounds.
    #[must_use]
    pub(crate) fn reorder_category(
        &mut self,
        uuid: &wire::Uuid,
        direction: ReorderDirection,
    ) -> RulesEffect {
        if self.category_mutating == MutationActivity::Busy {
            return RulesEffect::None;
        }
        let Some(request) = self
            .categories()
            .and_then(|catalog| catalog.reorder_request(uuid, direction))
        else {
            return RulesEffect::None;
        };
        self.archive_confirm = ArchiveConfirmation::Idle;
        self.category_mutation_notice = None;
        self.category_mutating = MutationActivity::Busy;
        self.category_mutation_generation = self.category_mutation_generation.next();
        RulesEffect::MutateCategory {
            mutation: CategoryMutation::Reorder(request),
            generation: self.category_mutation_generation,
        }
    }

    /// Applies the outcome of a category mutation; success closes the editor and reloads the list.
    #[must_use]
    pub(crate) fn apply_category_mutation_finished(
        &mut self,
        generation: RulesGeneration,
        result: Result<(), LoadDataError>,
    ) -> RulesEffect {
        if !self.category_mutation_generation.accepts(generation) {
            return RulesEffect::None;
        }
        self.category_mutating = MutationActivity::Idle;
        match result {
            Ok(()) => {
                self.category_editor = None;
                self.reload_categories()
            }
            Err(error) => {
                if matches!(error, LoadDataError::Stale)
                    || matches!(
                        error.code(),
                        LoadDataErrorCode::Protocol(
                            ErrorKind::RuleSetStale { .. } | ErrorKind::StaleDocumentRef
                        )
                    )
                {
                    self.category_editor = None;
                    self.categories_state = LoadState::Stale;
                } else {
                    self.category_mutation_notice = Some(MutationNotice { error });
                }
                RulesEffect::None
            }
        }
    }

    /// Resolves a rule entry's category UUID against the loaded category catalog.
    #[must_use]
    pub(crate) fn resolve_entry_category(&self, uuid: &wire::Uuid) -> ResolvedCategory {
        match self.categories() {
            Some(catalog) => catalog.resolve(uuid),
            None => ResolvedCategory::Missing,
        }
    }

    /// Active tab on the Rules page.
    #[must_use]
    pub(crate) const fn active_tab(&self) -> RulesTab {
        self.active_tab
    }

    /// Current tree when loaded.
    #[must_use]
    pub(crate) fn tree(&self) -> Option<&RuleTree> {
        match &self.tree_state {
            LoadState::Ready(tree) => Some(tree),
            LoadState::Loading | LoadState::Failed(_) | LoadState::Stale => None,
        }
    }

    /// Current tree load state.
    #[must_use]
    pub(crate) const fn tree_state(&self) -> &LoadState<RuleTree> {
        &self.tree_state
    }

    /// Current selected set detail load state.
    #[must_use]
    pub(crate) const fn detail_state(&self) -> &LoadState<RuleSetDetail> {
        &self.detail_state
    }

    /// Key of the currently selected rule set, if any.
    #[must_use]
    pub(crate) fn selected_key(&self) -> Option<&str> {
        self.selected_key.as_deref()
    }

    /// Node of the currently selected rule set when the tree is loaded.
    #[must_use]
    pub(crate) fn selected_node(&self) -> Option<&RuleTreeNode> {
        let key = self.selected_key.as_deref()?;
        self.tree()?.find(key)
    }

    /// Flattened preorder view of the currently loaded tree.
    #[must_use]
    pub(crate) fn flat_rows(&self) -> Vec<FlatTreeRow<'_>> {
        self.tree().map_or_else(Vec::new, RuleTree::flatten)
    }

    /// Active inline delete confirmation state.
    #[must_use]
    pub(crate) const fn delete_confirmation(&self) -> &DeleteConfirmation {
        &self.delete_confirm
    }

    /// Whether a rule-set mutation is currently in flight.
    #[must_use]
    pub(crate) const fn mutating(&self) -> MutationActivity {
        self.mutating
    }

    /// Inline rule-set mutation failure notice, if any.
    #[must_use]
    pub(crate) const fn mutation_notice(&self) -> Option<&MutationNotice> {
        self.mutation_notice.as_ref()
    }

    /// Loaded category catalog, if ready.
    #[must_use]
    pub(crate) fn categories(&self) -> Option<&CategoryCatalog> {
        match &self.categories_state {
            LoadState::Ready(catalog) => Some(catalog),
            LoadState::Loading | LoadState::Failed(_) | LoadState::Stale => None,
        }
    }

    /// Current category list load state.
    #[must_use]
    pub(crate) const fn categories_state(&self) -> &LoadState<CategoryCatalog> {
        &self.categories_state
    }

    /// Active (non-archived) categories in server preorder.
    #[must_use]
    pub(crate) fn active_categories(&self) -> Vec<&CategoryItem> {
        self.categories()
            .map_or_else(Vec::new, CategoryCatalog::active_items)
    }

    /// Archived categories currently visible according to the archived visibility toggle.
    #[must_use]
    pub(crate) fn visible_archived_categories(&self) -> Vec<&CategoryItem> {
        match self.archived_visibility {
            ArchivedVisibility::Hidden => Vec::new(),
            ArchivedVisibility::Visible => self
                .categories()
                .map_or_else(Vec::new, CategoryCatalog::archived_items),
        }
    }

    /// Total number of archived categories in the loaded catalog.
    #[must_use]
    pub(crate) fn archived_category_count(&self) -> usize {
        self.categories()
            .map_or(0, |catalog| catalog.archived_items().len())
    }

    /// Whether archived categories are currently expanded.
    #[must_use]
    pub(crate) const fn archived_visibility(&self) -> ArchivedVisibility {
        self.archived_visibility
    }

    /// Open category editor draft, if any.
    #[must_use]
    pub(crate) const fn category_editor(&self) -> Option<&CategoryEditorDraft> {
        self.category_editor.as_ref()
    }

    /// Active inline archive confirmation state.
    #[must_use]
    pub(crate) const fn archive_confirmation(&self) -> &ArchiveConfirmation {
        &self.archive_confirm
    }

    /// Whether a category mutation is currently in flight.
    #[must_use]
    pub(crate) const fn category_mutating(&self) -> MutationActivity {
        self.category_mutating
    }

    /// Inline category mutation failure notice, if any.
    #[must_use]
    pub(crate) const fn category_mutation_notice(&self) -> Option<&MutationNotice> {
        self.category_mutation_notice.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::{
        BuiltinCategory as WireBuiltinCategory, CategoriesListResponse, CategoryTendency,
        EditableCategory, MutatedRuleSet, QueryVersions, RuleCondition, RuleEntryContent,
        RuleEntryKind, RuleEntryState, RuleField, RuleKind, RuleMatch, RuleOperator,
        RuleProvenance, RuleSetCounts, RuleSetEntry, RuleSetNode, RuleSetSource, RuleSetWarning,
        RuleSetsGetResponse, RuleSetsTreeResponse, RuleSetsWriteResponse, RuleWarningCode, Uuid,
    };

    fn node(
        key: &str,
        parent: Option<&str>,
        source: RuleSetSource,
        enabled: bool,
        counts: RuleSetCounts,
    ) -> RuleSetNode {
        RuleSetNode {
            key: key.to_owned(),
            parent: parent.map(str::to_owned),
            version: 2,
            enabled,
            sibling_order: 0,
            source,
            counts,
            warnings: Vec::new(),
        }
    }

    fn tree_response(nodes: Vec<RuleSetNode>) -> RuleSetsTreeResponse {
        RuleSetsTreeResponse {
            kind: RuleKind::Classification,
            effective_version: "eff-v1".to_owned(),
            nodes,
            versions: QueryVersions("v1".to_owned()),
            truncated: false,
        }
    }

    fn get_response(key: &str, label: &str) -> RuleSetsGetResponse {
        RuleSetsGetResponse {
            key: key.to_owned(),
            version: 2,
            parent_version: Some(1),
            entries: vec![RuleSetEntry {
                entry_kind: RuleEntryKind::Rule,
                content: RuleEntryContent {
                    id: Some("editor".to_owned()),
                    target: None,
                    reason: None,
                    label: Some(label.to_owned()),
                    priority: Some(50),
                    detector: None,
                    languages: None,
                    verified: None,
                    label_key: None,
                    category: None,
                    r#match: Some(RuleMatch::All),
                    when: Some(vec![RuleCondition {
                        field: RuleField::ExeName,
                        op: RuleOperator::Equals,
                        value: "code.exe".to_owned(),
                        case_sensitive: false,
                        negate: false,
                    }]),
                    app: None,
                    pattern: None,
                    window_title: None,
                    web: None,
                },
                provenance: vec![RuleProvenance {
                    key: key.to_owned(),
                    version: 2,
                    id: "editor".to_owned(),
                }],
                state: RuleEntryState::Effective,
            }],
            versions: QueryVersions("v1".to_owned()),
            truncated: false,
        }
    }

    fn loaded_page() -> RulesPage {
        let mut page = RulesPage::new();
        let RulesEffect::LoadPage {
            tree_generation: generation,
            ..
        } = page.open()
        else {
            panic!("opening the page must load both the tree and categories");
        };
        let mut parent = node(
            "builtin.classification",
            None,
            RuleSetSource::Builtin,
            true,
            RuleSetCounts {
                added: 10,
                overridden: 0,
                disabled: 0,
            },
        );
        parent.warnings.push(RuleSetWarning {
            code: RuleWarningCode::Conflict,
            target: "builtin.classification/x".to_owned(),
        });
        let child = node(
            "user.tweaks",
            Some("builtin.classification"),
            RuleSetSource::User,
            true,
            RuleSetCounts {
                added: 2,
                overridden: 1,
                disabled: 0,
            },
        );
        let disabled_branch = node(
            "user.exam",
            Some("builtin.classification"),
            RuleSetSource::User,
            false,
            RuleSetCounts {
                added: 1,
                overridden: 0,
                disabled: 1,
            },
        );
        let disabled_leaf = node(
            "user.exam.leaf",
            Some("user.exam"),
            RuleSetSource::User,
            true,
            RuleSetCounts {
                added: 1,
                overridden: 0,
                disabled: 0,
            },
        );
        let effect = page.apply_tree_loaded(
            generation,
            Ok(vec![tree_response(vec![
                parent,
                child,
                disabled_branch,
                disabled_leaf,
            ])]),
        );
        assert!(matches!(effect, RulesEffect::LoadDetail { .. }));
        page
    }

    #[test]
    fn tree_mapping_marks_builtin_user_disabled_subtree_and_overrides() {
        let page = loaded_page();
        let tree = page.tree().expect("tree loaded");
        assert_eq!(tree.node_count(), 4);
        let rows = page.flat_rows();
        assert_eq!(rows.len(), 4);

        let builtin = rows.first().expect("builtin root");
        assert_eq!(builtin.node.key, "builtin.classification");
        assert_eq!(builtin.node.parent, None);
        assert_eq!(builtin.node.kind, RuleKind::Classification);
        assert_eq!(builtin.depth, 0);
        assert_eq!(builtin.node.source, RuleSetSource::Builtin);
        assert_eq!(builtin.node.enablement, NodeEnablement::Enabled);
        assert_eq!(
            builtin.node.override_marker,
            OverrideMarker::OverriddenByChild
        );
        assert_eq!(builtin.node.counts.added, 10);
        assert_eq!(builtin.node.warnings.len(), 1);
        assert!(!tree.truncated);

        let tweaks = rows.get(1).expect("user tweaks");
        assert_eq!(tweaks.node.key, "user.tweaks");
        assert_eq!(
            tweaks.node.parent.as_deref(),
            Some("builtin.classification")
        );
        assert_eq!(tweaks.depth, 1);
        assert_eq!(tweaks.node.source, RuleSetSource::User);
        assert_eq!(tweaks.node.enablement, NodeEnablement::Enabled);
        assert_eq!(
            tweaks.node.override_marker,
            OverrideMarker::OverridesAncestor
        );

        let exam = rows.get(2).expect("disabled parent");
        assert_eq!(exam.node.key, "user.exam");
        assert_eq!(exam.depth, 1);
        assert_eq!(exam.node.enablement, NodeEnablement::Disabled);
        assert!(!exam.node.enablement.is_effective());

        let leaf = rows.get(3).expect("disabled subtree child");
        assert_eq!(leaf.node.key, "user.exam.leaf");
        assert_eq!(leaf.depth, 2);
        assert_eq!(leaf.node.enablement, NodeEnablement::DisabledByParent);
        assert!(!leaf.node.enablement.is_effective());
    }

    #[test]
    fn late_get_response_for_previous_selection_is_ignored() {
        let mut page = loaded_page();
        let RulesEffect::LoadDetail {
            generation: first_gen,
            ..
        } = page.select("builtin.classification".to_owned())
        else {
            panic!("selecting first set requests detail");
        };
        let RulesEffect::LoadDetail {
            generation: second_gen,
            ..
        } = page.select("user.tweaks".to_owned())
        else {
            panic!("selecting second set requests detail");
        };
        assert_ne!(first_gen, second_gen);

        page.apply_detail_loaded(
            first_gen,
            Ok(get_response("builtin.classification", "Stale builtin")),
        );
        assert!(
            matches!(page.detail_state(), LoadState::Loading),
            "late response for old generation must be ignored"
        );

        page.apply_detail_loaded(second_gen, Ok(get_response("user.tweaks", "Fresh user")));
        let LoadState::Ready(detail) = page.detail_state() else {
            panic!("current selection response must populate detail");
        };
        assert_eq!(detail.key, "user.tweaks");
        assert_eq!(detail.version, 2);
        assert_eq!(detail.parent_version, Some(1));
        assert!(!detail.truncated);
        let entry = detail.entries.first().expect("first entry");
        assert_eq!(entry.label, RuleEntryLabel::Text("Fresh user".to_owned()));
        assert_eq!(entry.entry_kind, RuleEntryKind::Rule);
        assert_eq!(entry.state, RuleEntryState::Effective);
        assert_eq!(entry.target, None);
        assert_eq!(entry.priority, Some(50));
        assert_eq!(entry.provenance.len(), 1);
        assert!(matches!(
            entry.summary,
            EntryConditionSummary::Conditions { .. }
        ));
    }

    #[test]
    fn successful_mutation_triggers_tree_reload() {
        let mut page = loaded_page();
        assert_eq!(
            page.toggle_enabled("builtin.classification"),
            RulesEffect::None,
            "builtin sets are read-only"
        );

        let RulesEffect::Mutate {
            mutation,
            generation,
        } = page.toggle_enabled("user.tweaks")
        else {
            panic!("toggling user set emits mutation");
        };
        assert!(matches!(
            mutation,
            RulesMutation::SetEnabled(wire::RuleSetsSetEnabledRequest {
                ref key,
                enabled: false,
                ..
            }) if key == "user.tweaks"
        ));
        assert_eq!(page.mutating(), MutationActivity::Busy);

        let reload = page.apply_mutation_finished(
            generation,
            Ok(RuleSetsWriteResponse {
                sets: vec![MutatedRuleSet {
                    key: "user.tweaks".to_owned(),
                    version: 3,
                    deleted: false,
                }],
                versions: QueryVersions("v2".to_owned()),
            }),
        );
        assert_eq!(page.mutating(), MutationActivity::Idle);
        assert!(
            matches!(reload, RulesEffect::LoadTree { .. }),
            "mutation success must trigger tree reload"
        );
        assert!(matches!(page.tree_state(), LoadState::Loading));
    }

    #[test]
    fn delete_without_inline_confirm_does_nothing_until_confirmed() {
        let mut page = loaded_page();
        assert_eq!(
            page.confirm_delete("user.tweaks"),
            RulesEffect::None,
            "delete without prior confirm request must do nothing"
        );
        assert_eq!(page.delete_confirmation(), &DeleteConfirmation::Idle);

        page.request_delete("user.exam");
        assert_eq!(
            page.delete_confirmation(),
            &DeleteConfirmation::Idle,
            "user set with children cannot enter delete confirmation"
        );

        page.request_delete("user.tweaks");
        assert_eq!(
            page.delete_confirmation(),
            &DeleteConfirmation::Pending {
                key: "user.tweaks".to_owned()
            }
        );
        assert_eq!(
            page.confirm_delete("user.exam.leaf"),
            RulesEffect::None,
            "confirming a different key must do nothing"
        );

        let effect = page.confirm_delete("user.tweaks");
        assert!(matches!(
            effect,
            RulesEffect::Mutate {
                mutation: RulesMutation::Delete(wire::RuleSetsDeleteRequest { ref key, .. }),
                ..
            } if key == "user.tweaks"
        ));
        assert_eq!(page.delete_confirmation(), &DeleteConfirmation::Idle);
    }

    #[test]
    fn keyboard_selection_movement_clamps_at_tree_bounds() {
        let mut page = loaded_page();
        assert_eq!(page.selected_key(), Some("builtin.classification"));

        assert_eq!(
            page.handle_key(RulesKeyAction::Move(TreeDirection::Up)),
            RulesEffect::None,
            "moving up at the top row clamps without reloading"
        );
        assert_eq!(page.selected_key(), Some("builtin.classification"));

        for expected in ["user.tweaks", "user.exam", "user.exam.leaf"] {
            let effect = page.handle_key(RulesKeyAction::Move(TreeDirection::Down));
            assert!(matches!(effect, RulesEffect::LoadDetail { ref key, .. } if key == expected));
            assert_eq!(page.selected_key(), Some(expected));
        }

        assert_eq!(
            page.handle_key(RulesKeyAction::Move(TreeDirection::Down)),
            RulesEffect::None,
            "moving down at the last row clamps without wrapping"
        );
        assert_eq!(page.selected_key(), Some("user.exam.leaf"));

        assert!(matches!(
            page.handle_key(RulesKeyAction::Open),
            RulesEffect::LoadDetail { ref key, .. } if key == "user.exam.leaf"
        ));

        assert!(matches!(
            page.handle_key(RulesKeyAction::ToggleEnabled),
            RulesEffect::Mutate {
                mutation: RulesMutation::SetEnabled(wire::RuleSetsSetEnabledRequest {
                    ref key,
                    enabled: false,
                    ..
                }),
                ..
            } if key == "user.exam.leaf"
        ));
    }

    #[test]
    fn stale_version_and_cancel_delete_update_page_state() {
        let mut page = loaded_page();
        let RulesEffect::LoadDetail { generation, .. } = page.select("user.tweaks".to_owned())
        else {
            panic!("detail load");
        };
        let mut stale = get_response("user.tweaks", "Stale");
        stale.versions = QueryVersions("v99".to_owned());
        page.apply_detail_loaded(generation, Ok(stale));
        assert!(matches!(page.detail_state(), LoadState::Stale));

        page.request_delete("user.tweaks");
        assert!(matches!(
            page.delete_confirmation(),
            DeleteConfirmation::Pending { .. }
        ));
        assert_eq!(page.handle_key(RulesKeyAction::Cancel), RulesEffect::None);
        assert_eq!(page.delete_confirmation(), &DeleteConfirmation::Idle);
        assert_eq!(
            page.selected_node().map(|node| node.key.as_str()),
            Some("user.tweaks")
        );
        assert!(page.mutation_notice().is_none());

        let RulesEffect::Mutate { generation, .. } = page.toggle_enabled("user.tweaks") else {
            panic!("toggle mutation");
        };
        assert_eq!(
            page.apply_mutation_finished(generation, Err(LoadDataError::InvalidData)),
            RulesEffect::None
        );
        assert_eq!(
            page.mutation_notice()
                .expect("mutation notice")
                .error
                .code()
                .as_str(),
            "invalid_response"
        );

        let RulesEffect::LoadTree { generation } = page.reload_tree() else {
            panic!("reload tree");
        };
        assert_eq!(
            page.apply_tree_loaded(generation, Err(LoadDataError::InvalidData)),
            RulesEffect::None
        );
        assert!(matches!(page.tree_state(), LoadState::Failed(_)));
    }

    fn uuid(suffix: u16) -> Uuid {
        format!("00000000-0000-4000-8000-{suffix:012}")
            .parse()
            .expect("valid test UUID")
    }

    fn editable_category(
        suffix: u16,
        parent: Option<u16>,
        builtin_key: Option<WireBuiltinCategory>,
        name: Option<&str>,
        color: &str,
        tendency: CategoryTendency,
        archived: bool,
    ) -> EditableCategory {
        EditableCategory {
            uuid: uuid(suffix),
            builtin_key,
            parent_uuid: parent.map(uuid),
            name: name.map(str::to_owned),
            color: color.to_owned(),
            tendency,
            sort_order: i64::from(suffix),
            archived,
        }
    }

    fn loaded_categories_page() -> RulesPage {
        let mut page = RulesPage::new();
        let RulesEffect::LoadPage {
            categories_generation,
            ..
        } = page.open()
        else {
            panic!("opening the page must request categories");
        };
        page.set_active_tab(RulesTab::Categories);
        let response = CategoriesListResponse {
            entities: vec![
                editable_category(
                    1,
                    None,
                    Some(WireBuiltinCategory::Development),
                    None,
                    "cat.8",
                    CategoryTendency::Focus,
                    false,
                ),
                editable_category(
                    2,
                    Some(1),
                    None,
                    Some("Backend"),
                    "cat.2",
                    CategoryTendency::Focus,
                    false,
                ),
                editable_category(
                    3,
                    None,
                    None,
                    Some("Old client"),
                    "cat.9",
                    CategoryTendency::Neutral,
                    true,
                ),
                editable_category(
                    4,
                    None,
                    Some(WireBuiltinCategory::Learning),
                    Some("Deep study"),
                    "cat.1",
                    CategoryTendency::Focus,
                    false,
                ),
            ],
            versions: QueryVersions("categories.v1:4".to_owned()),
            truncated: false,
        };
        page.apply_categories_loaded(categories_generation, Ok(response));
        page
    }

    #[test]
    fn category_list_mapping_and_archived_filtering() {
        let mut page = loaded_categories_page();
        assert_eq!(page.active_tab(), RulesTab::Categories);
        assert_eq!(page.archived_visibility(), ArchivedVisibility::Hidden);
        assert_eq!(page.archived_category_count(), 1);

        let active = page.active_categories();
        assert_eq!(active.len(), 3);
        let dev = active.first().expect("development");
        assert_eq!(
            dev.origin,
            CategoryOrigin::Builtin(BuiltinCategory::Development)
        );
        assert_eq!(
            dev.display_name,
            CategoryName::Builtin(BuiltinCategory::Development)
        );
        assert_eq!(dev.color, CategoryColor::Palette(PaletteSlot::numbered(8)));
        assert_eq!(dev.tendency, Tendency::Focus);
        assert_eq!(dev.depth, 0);

        let backend = active.get(1).expect("backend child");
        assert_eq!(backend.origin, CategoryOrigin::User);
        assert_eq!(
            backend.display_name,
            CategoryName::Custom("Backend".to_owned())
        );
        assert_eq!(backend.depth, 1);

        let learning = active.get(2).expect("renamed builtin");
        assert_eq!(
            learning.origin,
            CategoryOrigin::Builtin(BuiltinCategory::Learning)
        );
        assert_eq!(
            learning.display_name,
            CategoryName::Custom("Deep study".to_owned())
        );
        assert_eq!(learning.custom_name.as_deref(), Some("Deep study"));
        assert!(!page.categories().expect("catalog").truncated);

        assert!(
            page.visible_archived_categories().is_empty(),
            "archived categories stay hidden until toggled visible"
        );
        page.toggle_archived_visibility();
        assert_eq!(page.archived_visibility(), ArchivedVisibility::Visible);
        let archived = page.visible_archived_categories();
        assert_eq!(archived.len(), 1);
        assert_eq!(
            archived.first().expect("archived").display_name,
            CategoryName::Custom("Old client".to_owned())
        );
    }

    #[test]
    fn create_category_rejects_blank_and_reserved_names_without_request() {
        let mut page = loaded_categories_page();
        let new_id = uuid(99);
        page.begin_create_category(new_id.clone());
        assert_eq!(
            page.category_editor().expect("editor").color,
            CategoryColor::Palette(PaletteSlot::numbered(3)),
            "default color picks the first unused palette slot"
        );

        page.set_category_editor_name("   \t ".to_owned());
        assert_eq!(
            page.submit_category_editor(),
            RulesEffect::None,
            "blank name must not emit a create request"
        );
        assert_eq!(
            page.category_editor().expect("editor").validation_error,
            Some(ValidateCategoryNameError::Blank)
        );

        page.set_category_editor_name("builtin:development".to_owned());
        assert_eq!(page.submit_category_editor(), RulesEffect::None);
        assert_eq!(
            page.category_editor().expect("editor").validation_error,
            Some(ValidateCategoryNameError::Reserved)
        );

        page.set_category_editor_name("x".repeat(MAX_CATEGORY_NAME_SCALARS + 1));
        assert_eq!(page.submit_category_editor(), RulesEffect::None);
        assert_eq!(
            page.category_editor().expect("editor").validation_error,
            Some(ValidateCategoryNameError::TooLong)
        );

        page.set_category_editor_name("  Research  ".to_owned());
        page.set_category_editor_color(CategoryColor::Palette(PaletteSlot::numbered(5)));
        page.set_category_editor_tendency(Tendency::Focus);
        let effect = page.submit_category_editor();
        assert!(matches!(
            effect,
            RulesEffect::MutateCategory {
                mutation: CategoryMutation::Create(wire::CategoriesCreateRequest {
                    entity: wire::NewCategory {
                        ref uuid,
                        parent_uuid: None,
                        ref name,
                        ref color,
                        tendency: CategoryTendency::Focus,
                    }
                }),
                ..
            } if *uuid == new_id && name.as_deref() == Some("Research") && color == "cat.5"
        ));
    }

    #[test]
    fn archive_category_requires_inline_confirmation() {
        let mut page = loaded_categories_page();
        let target = uuid(2);

        assert_eq!(
            page.confirm_archive_category(&target),
            RulesEffect::None,
            "archive without prior confirmation request must do nothing"
        );
        assert_eq!(page.archive_confirmation(), &ArchiveConfirmation::Idle);

        page.request_archive_category(&target);
        assert_eq!(
            page.archive_confirmation(),
            &ArchiveConfirmation::Pending {
                uuid: target.clone()
            }
        );
        assert_eq!(
            page.confirm_archive_category(&uuid(1)),
            RulesEffect::None,
            "confirming a different category must do nothing"
        );

        let effect = page.confirm_archive_category(&target);
        assert!(matches!(
            effect,
            RulesEffect::MutateCategory {
                mutation: CategoryMutation::Archive(wire::CategoriesArchiveRequest {
                    ref uuid,
                    archived: true,
                }),
                ..
            } if *uuid == target
        ));
        assert_eq!(page.archive_confirmation(), &ArchiveConfirmation::Idle);
    }

    #[test]
    fn reorder_up_at_top_is_a_noop_and_preserves_archived_siblings() {
        let mut page = loaded_categories_page();
        let top_root = uuid(1);
        let second_active_root = uuid(4);
        let archived_root = uuid(3);
        let only_child = uuid(2);

        let catalog = page.categories().expect("catalog");
        assert!(!catalog.can_reorder(&top_root, ReorderDirection::Up));
        assert!(catalog.can_reorder(&top_root, ReorderDirection::Down));
        assert!(catalog.can_reorder(&second_active_root, ReorderDirection::Up));
        assert!(!catalog.can_reorder(&second_active_root, ReorderDirection::Down));

        assert_eq!(
            page.reorder_category(&top_root, ReorderDirection::Up),
            RulesEffect::None,
            "moving up at the top of a sibling group is a no-op"
        );
        assert_eq!(
            page.reorder_category(&second_active_root, ReorderDirection::Down),
            RulesEffect::None,
            "moving down at the bottom of a sibling group is a no-op"
        );
        assert_eq!(
            page.reorder_category(&only_child, ReorderDirection::Up),
            RulesEffect::None
        );

        let effect = page.reorder_category(&second_active_root, ReorderDirection::Up);
        assert!(matches!(
            effect,
            RulesEffect::MutateCategory {
                mutation: CategoryMutation::Reorder(wire::CategoriesReorderRequest {
                    parent_uuid: None,
                    ref uuids,
                }),
                ..
            } if *uuids == vec![second_active_root.clone(), archived_root.clone(), top_root.clone()]
        ));
    }

    #[test]
    fn category_mutation_success_reloads_list_and_errors_preserve_state() {
        let mut page = loaded_categories_page();
        page.begin_edit_category(&uuid(4));
        page.reset_builtin_editor_name();
        let RulesEffect::MutateCategory {
            mutation,
            generation,
        } = page.submit_category_editor()
        else {
            panic!("resetting builtin name and saving emits update mutation");
        };
        assert!(matches!(
            mutation,
            CategoryMutation::Update(wire::CategoriesUpdateRequest {
                entity: wire::CategoryEdit { name: None, .. }
            })
        ));
        assert_eq!(page.category_mutating(), MutationActivity::Busy);

        let reload = page.apply_category_mutation_finished(generation, Ok(()));
        assert_eq!(page.category_mutating(), MutationActivity::Idle);
        assert!(page.category_editor().is_none());
        assert!(
            matches!(reload, RulesEffect::LoadCategories { .. }),
            "successful category mutation must reload the category list"
        );
        assert!(matches!(page.categories_state(), LoadState::Loading));

        // Failed mutation surfaces an inline notice without wiping loaded categories.
        let mut failing_page = loaded_categories_page();
        let RulesEffect::MutateCategory { generation, .. } =
            failing_page.restore_category(&uuid(3))
        else {
            panic!("restoring archived category emits mutation");
        };
        assert_eq!(
            failing_page
                .apply_category_mutation_finished(generation, Err(LoadDataError::InvalidData)),
            RulesEffect::None
        );
        assert!(failing_page.category_mutation_notice().is_some());
        assert!(failing_page.categories().is_some());
    }

    #[test]
    fn rule_entry_category_resolution_handles_active_archived_and_unknown() {
        let mut page = loaded_categories_page();
        let mut response = get_response("user.tweaks", "Dev rule");
        if let Some(entry) = response.entries.first_mut() {
            entry.content.category = Some(uuid(1));
        }
        let detail = RuleSetDetail::from_protocol(response);
        let entry_category = detail
            .entries
            .first()
            .expect("entry")
            .category
            .as_ref()
            .expect("entry category UUID");
        assert_eq!(
            page.resolve_entry_category(entry_category),
            ResolvedCategory::Active {
                name: CategoryName::Builtin(BuiltinCategory::Development),
                color: CategoryColor::Palette(PaletteSlot::numbered(8)),
            }
        );
        assert_eq!(
            page.resolve_entry_category(&uuid(2)),
            ResolvedCategory::Active {
                name: CategoryName::Custom("Backend".to_owned()),
                color: CategoryColor::Palette(PaletteSlot::numbered(2)),
            }
        );
        assert_eq!(
            page.resolve_entry_category(&uuid(3)),
            ResolvedCategory::Missing,
            "archived categories resolve as missing on rule entries"
        );
        assert_eq!(
            page.resolve_entry_category(&uuid(999)),
            ResolvedCategory::Missing,
            "unknown category UUIDs resolve as missing without raw UUID fallback"
        );

        page.begin_edit_category(&uuid(1));
        assert!(page.category_editor().is_some());
        assert_eq!(page.handle_key(RulesKeyAction::Cancel), RulesEffect::None);
        assert!(page.category_editor().is_none());
    }
}
