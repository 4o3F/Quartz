//! Rules page view model: rule-set tree, selected set detail, load states, and mutation state
//! machine without GPUI dependencies so behavior is unit-tested on every platform.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
};

use quartz_protocol::{self as wire, ErrorKind};

use crate::link::{LoadDataError, LoadDataErrorCode};

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
    pub(crate) label: RuleEntryLabel,
    pub(crate) target: Option<String>,
    pub(crate) priority: Option<u16>,
    pub(crate) summary: EntryConditionSummary,
    pub(crate) provenance: Vec<wire::RuleProvenance>,
}

impl RuleEntryDetail {
    /// Maps a single protocol rule-set entry into a typed detail row.
    #[must_use]
    pub(crate) fn from_protocol(entry: wire::RuleSetEntry) -> Self {
        let content = entry.content;
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
            label,
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

/// State machine for the Rules page: rule-set tree, selection, detail, and mutations.
#[derive(Debug)]
pub(crate) struct RulesPage {
    tree_state: LoadState<RuleTree>,
    selected_key: Option<String>,
    detail_state: LoadState<RuleSetDetail>,
    tree_generation: RulesGeneration,
    detail_generation: RulesGeneration,
    mutation_generation: RulesGeneration,
    delete_confirm: DeleteConfirmation,
    mutating: MutationActivity,
    mutation_notice: Option<MutationNotice>,
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
            tree_state: LoadState::Loading,
            selected_key: None,
            detail_state: LoadState::Loading,
            tree_generation: RulesGeneration::default(),
            detail_generation: RulesGeneration::default(),
            mutation_generation: RulesGeneration::default(),
            delete_confirm: DeleteConfirmation::Idle,
            mutating: MutationActivity::Idle,
            mutation_notice: None,
        }
    }

    /// Starts loading the rule-set tree when opening the Rules page.
    #[must_use]
    pub(crate) fn open(&mut self) -> RulesEffect {
        self.reload_tree()
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

    /// Whether a mutation is currently in flight.
    #[must_use]
    pub(crate) const fn mutating(&self) -> MutationActivity {
        self.mutating
    }

    /// Inline mutation failure notice, if any.
    #[must_use]
    pub(crate) const fn mutation_notice(&self) -> Option<&MutationNotice> {
        self.mutation_notice.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::{
        MutatedRuleSet, QueryVersions, RuleCondition, RuleEntryContent, RuleEntryKind,
        RuleEntryState, RuleField, RuleKind, RuleMatch, RuleOperator, RuleProvenance,
        RuleSetCounts, RuleSetEntry, RuleSetNode, RuleSetSource, RuleSetWarning,
        RuleSetsGetResponse, RuleSetsTreeResponse, RuleSetsWriteResponse, RuleWarningCode,
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
        let RulesEffect::LoadTree { generation } = page.open() else {
            panic!("opening the page must load the tree");
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
}
