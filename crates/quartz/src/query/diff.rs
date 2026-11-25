//! Comparison sources and byte-bounded pages for the shared two-snapshot evaluator.

use super::{
    RunQueryError,
    classification::QueryCategoryRef,
    control::QueryControl,
    paging::{PageBudget, PagePosition},
    pipeline::QUERY_BATCH_ROWS,
};
use crate::rules::{registry::RegistryShell, write::RuleMutation};
use quartz_domain::rules::{
    self as domain,
    registry::{RegistryState, RuleRegistry, RuleSetDeletion, RuleSnapshot},
};
use quartz_protocol::{self as wire, ErrorKind, ProtocolError};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Debug)]
pub(super) enum ComparisonSource {
    Preview(RuleMutation),
    Diff(wire::RuleComparison),
}
pub(super) struct ComparedSnapshots {
    pub before: Arc<RuleSnapshot>,
    pub after: Arc<RuleSnapshot>,
    pub kind: domain::RuleKind,
    pub comparison: Option<ComparisonData>,
}
#[derive(Debug)]
pub(super) struct ComparisonData {
    pub changes: Vec<wire::RuleDiffChange>,
    pub sides: Option<wire::RuleDiffSides>,
}
enum ChangeScope<'key> {
    Registry,
    Own(&'key domain::RuleSetKey),
}
impl ComparisonSource {
    pub(super) fn prepare(
        &self,
        registry: &RegistryShell,
        control: &QueryControl,
    ) -> Result<ComparedSnapshots, ProtocolError> {
        control
            .check()
            .map_err(|error| ProtocolError::new(error.kind()))?;
        let current = registry.snapshots().load();
        match self {
            Self::Preview(mutation) => Self::draft(mutation, registry),
            Self::Diff(wire::RuleComparison::Draft { mutation }) => {
                let mutation = match mutation {
                    wire::DiffMutation::Editor(mutation) => RuleMutation::from(mutation.clone()),
                    wire::DiffMutation::Text(mutation) => RuleMutation::from(mutation.clone()),
                };
                let mut snapshots = Self::draft(&mutation, registry)?;
                snapshots.comparison = Some(ComparisonData::new(
                    &snapshots.before,
                    &snapshots.after,
                    None,
                    ChangeScope::Registry,
                    control,
                )?);
                Ok(snapshots)
            }
            Self::Diff(wire::RuleComparison::Versions {
                key,
                before_version,
                after_version,
            }) => {
                let key = Self::key(key)?;
                let before =
                    HistoryChain::saved(current.state(), &key, before_version.get(), control)?;
                let after =
                    HistoryChain::saved(current.state(), &key, after_version.get(), control)?;
                Self::historical(
                    &current,
                    before,
                    after,
                    None,
                    ChangeScope::Own(&key),
                    control,
                )
            }
            Self::Diff(wire::RuleComparison::Upstream { key, version }) => {
                let key = Self::key(key)?;
                let selected = current.state().sets.get(&key).ok_or_else(|| {
                    ProtocolError::new(ErrorKind::RuleSetNotFound {
                        key: key.to_string(),
                        version: Some(version.get()),
                    })
                })?;
                if selected.origin != domain::RuleOrigin::User
                    || selected.current.set.header.parent.is_none()
                {
                    return Err(ProtocolError::new(ErrorKind::BadRequest));
                }
                let mut before =
                    HistoryChain::saved(current.state(), &key, version.get(), control)?;
                let overlay = before.sets.last_mut().ok_or_else(Self::internal)?;
                overlay.enabled = selected.current.enabled;
                overlay.sibling_order = selected.current.sibling_order;
                let parent = Self::key(
                    overlay
                        .set
                        .header
                        .parent
                        .as_deref()
                        .ok_or_else(Self::internal)?,
                )?;
                let mut after = HistoryChain::current(current.state(), &parent, control)?;
                after.sets.push(overlay.clone());
                let (saved_overlay, saved_parents) =
                    before.sets.split_last().ok_or_else(Self::internal)?;
                let (_, current_parents) = after.sets.split_last().ok_or_else(Self::internal)?;
                let sides = wire::RuleDiffSides {
                    saved_parent: saved_parents
                        .iter()
                        .map(HistoryChain::side)
                        .collect::<Result<_, _>>()?,
                    current_parent: current_parents
                        .iter()
                        .map(HistoryChain::side)
                        .collect::<Result<_, _>>()?,
                    overlay: vec![HistoryChain::side(saved_overlay)?],
                };
                Self::historical(
                    &current,
                    before,
                    after,
                    Some(sides),
                    ChangeScope::Registry,
                    control,
                )
            }
        }
    }
    fn draft(
        mutation: &RuleMutation,
        registry: &RegistryShell,
    ) -> Result<ComparedSnapshots, ProtocolError> {
        let key = Self::key(mutation.target_key()?)?;
        let after = mutation.clone().draft(registry)?;
        let kind = after
            .state()
            .sets
            .get(&key)
            .ok_or_else(Self::internal)?
            .current
            .set
            .header
            .kind;
        Ok(ComparedSnapshots {
            before: registry.snapshots().load(),
            after,
            kind,
            comparison: None,
        })
    }
    fn historical(
        current: &Arc<RuleSnapshot>,
        before: HistoryChain,
        after: HistoryChain,
        sides: Option<wire::RuleDiffSides>,
        scope: ChangeScope<'_>,
        control: &QueryControl,
    ) -> Result<ComparedSnapshots, ProtocolError> {
        let kind = before
            .sets
            .last()
            .ok_or_else(Self::internal)?
            .set
            .header
            .kind;
        let before = before.snapshot(current)?;
        control
            .check()
            .map_err(|error| ProtocolError::new(error.kind()))?;
        let after = after.snapshot(current)?;
        let comparison = Some(ComparisonData::new(&before, &after, sides, scope, control)?);
        Ok(ComparedSnapshots {
            before,
            after,
            kind,
            comparison,
        })
    }
    fn key(key: &str) -> Result<domain::RuleSetKey, ProtocolError> {
        key.parse()
            .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))
    }
    fn internal() -> ProtocolError {
        ProtocolError::new(ErrorKind::Internal)
    }
}
impl ComparisonData {
    fn new(
        before: &RuleSnapshot,
        after: &RuleSnapshot,
        sides: Option<wire::RuleDiffSides>,
        scope: ChangeScope<'_>,
        control: &QueryControl,
    ) -> Result<Self, ProtocolError> {
        let mut changes = Vec::new();
        let keys: BTreeSet<_> = before
            .state()
            .sets
            .keys()
            .chain(after.state().sets.keys())
            .collect();
        let empty = domain::Content::default();
        let before_enabled = Self::enabled_sets(before);
        let after_enabled = Self::enabled_sets(after);
        let changed_orders = Self::changed_evaluation_orders(before, after)?;
        for key in keys {
            if matches!(scope, ChangeScope::Own(selected) if key != selected) {
                continue;
            }
            control
                .check()
                .map_err(|error| ProtocolError::new(error.kind()))?;
            let old = before
                .state()
                .sets
                .get(key)
                .filter(|set| {
                    set.deletion == RuleSetDeletion::Active
                        && (matches!(scope, ChangeScope::Own(_))
                            || before_enabled.contains_key(key.as_str()))
                })
                .map_or(&empty, |set| &set.current.set.content);
            let new = after
                .state()
                .sets
                .get(key)
                .filter(|set| {
                    set.deletion == RuleSetDeletion::Active
                        && (matches!(scope, ChangeScope::Own(_))
                            || after_enabled.contains_key(key.as_str()))
                })
                .map_or(&empty, |set| &set.current.set.content);
            let mut entry_changes = old.diff(new, key);
            if matches!(scope, ChangeScope::Registry)
                && let Some(old_order) = before_enabled.get(key.as_str())
                && let Some(new_order) = after_enabled.get(key.as_str())
                && old_order != new_order
                && let Some(old_set) = before.state().sets.get(key)
                && let Some(new_set) = after.state().sets.get(key)
                && old_set.current.sibling_order != new_set.current.sibling_order
                && changed_orders.contains(&old_set.current.set.header.kind)
            {
                for entry in new.diff(&empty, key) {
                    if let Some(change) = entry_changes
                        .iter_mut()
                        .find(|change| change.rule == entry.rule)
                    {
                        if change.change == domain::EntryChangeKind::Changed {
                            change.paths.push("set_order".into());
                            change.paths.sort();
                        }
                    } else {
                        entry_changes.push(domain::EntryChange {
                            rule: entry.rule,
                            change: domain::EntryChangeKind::Changed,
                            paths: vec!["set_order".into()],
                        });
                    }
                }
            }
            changes.extend(
                entry_changes
                    .into_iter()
                    .map(|change| wire::RuleDiffChange {
                        rule: change.rule,
                        change: match change.change {
                            domain::EntryChangeKind::Added => wire::RuleDiffChangeKind::Added,
                            domain::EntryChangeKind::Removed => wire::RuleDiffChangeKind::Removed,
                            domain::EntryChangeKind::Changed => wire::RuleDiffChangeKind::Changed,
                        },
                        paths: change.paths,
                    }),
            );
        }
        changes.sort_by(|left, right| {
            left.rule
                .cmp(&right.rule)
                .then(left.paths.cmp(&right.paths))
        });
        Ok(Self { changes, sides })
    }
    fn changed_evaluation_orders(
        before: &RuleSnapshot,
        after: &RuleSnapshot,
    ) -> Result<BTreeSet<domain::RuleKind>, ProtocolError> {
        let mut changed = BTreeSet::new();
        for kind in [
            domain::RuleKind::Classification,
            domain::RuleKind::Extraction,
            domain::RuleKind::Privacy,
        ] {
            let before = before
                .resolution(kind)
                .ok_or_else(ComparisonSource::internal)?;
            let after = after
                .resolution(kind)
                .ok_or_else(ComparisonSource::internal)?;
            if !before
                .effective
                .rules
                .iter()
                .map(|rule| &rule.full_id)
                .eq(after.effective.rules.iter().map(|rule| &rule.full_id))
            {
                changed.insert(kind);
            }
        }
        Ok(changed)
    }
    fn enabled_sets(snapshot: &RuleSnapshot) -> BTreeMap<String, usize> {
        let mut enabled = BTreeMap::new();
        for kind in [
            domain::RuleKind::Classification,
            domain::RuleKind::Extraction,
            domain::RuleKind::Privacy,
        ] {
            if let Some(resolution) = snapshot.resolution(kind) {
                for (order, node) in resolution
                    .nodes
                    .iter()
                    .filter(|node| node.enabled == domain::Enablement::Enabled)
                    .enumerate()
                {
                    enabled.insert(node.key.clone(), order);
                }
            }
        }
        enabled
    }
}
struct HistoryChain {
    sets: Vec<domain::InstalledSet>,
}
impl HistoryChain {
    fn saved(
        state: &RegistryState,
        key: &domain::RuleSetKey,
        version: u32,
        control: &QueryControl,
    ) -> Result<Self, ProtocolError> {
        Self::walk(state, key, ChainRevision::Saved(version), control)
    }
    fn current(
        state: &RegistryState,
        key: &domain::RuleSetKey,
        control: &QueryControl,
    ) -> Result<Self, ProtocolError> {
        Self::walk(state, key, ChainRevision::Current, control)
    }
    fn walk(
        state: &RegistryState,
        key: &domain::RuleSetKey,
        mut selection: ChainRevision,
        control: &QueryControl,
    ) -> Result<Self, ProtocolError> {
        let mut sets = Vec::new();
        let mut key = key.clone();
        let mut visited = BTreeSet::new();
        loop {
            control
                .check()
                .map_err(|error| ProtocolError::new(error.kind()))?;
            if !visited.insert(key.clone()) {
                return Err(ComparisonSource::internal());
            }
            let missing = || {
                ProtocolError::new(ErrorKind::RuleSetNotFound {
                    key: key.to_string(),
                    version: match selection {
                        ChainRevision::Saved(version) => Some(version),
                        ChainRevision::Current => None,
                    },
                })
            };
            let entry = state.sets.get(&key).ok_or_else(missing)?;
            if matches!(selection, ChainRevision::Current)
                && entry.deletion == RuleSetDeletion::Deleted
            {
                return Err(missing());
            }
            let version = match selection {
                ChainRevision::Saved(version) => version,
                ChainRevision::Current => entry.current.version,
            };
            let revision = entry.versions.get(&version).ok_or_else(missing)?;
            let mut installed = entry.current.clone();
            installed.version = version;
            installed.set.header = revision.header_or_legacy(&entry.current.set.header);
            installed.set.content = revision.content.clone();
            if matches!(selection, ChainRevision::Saved(_)) {
                installed.enabled = domain::Enablement::Enabled;
                installed.sibling_order = HISTORICAL_SIBLING_ORDER;
            }
            let parent = installed.set.header.parent.clone();
            sets.push(installed);
            match parent {
                None if revision.parent_version.is_none() => break,
                Some(parent) => {
                    key = parent.parse().map_err(|_| ComparisonSource::internal())?;
                    selection = match selection {
                        ChainRevision::Current => ChainRevision::Current,
                        ChainRevision::Saved(_) => ChainRevision::Saved(
                            revision
                                .parent_version
                                .ok_or_else(ComparisonSource::internal)?,
                        ),
                    };
                }
                None => return Err(ComparisonSource::internal()),
            }
        }
        sets.reverse();
        Ok(Self { sets })
    }
    fn snapshot(self, current: &RuleSnapshot) -> Result<Arc<RuleSnapshot>, ProtocolError> {
        let mut state = current.state().clone();
        for installed in self.sets {
            let key = installed
                .set
                .header
                .key
                .parse()
                .map_err(|_| ComparisonSource::internal())?;
            let entry = state
                .sets
                .get_mut(&key)
                .ok_or_else(ComparisonSource::internal)?;
            entry.current = installed;
            entry.deletion = RuleSetDeletion::Active;
        }
        Ok(RuleRegistry::from_state(state)
            .map_err(|_| ComparisonSource::internal())?
            .snapshots()
            .load())
    }
    fn side(set: &domain::InstalledSet) -> Result<wire::RuleDiffSet, ProtocolError> {
        let entries = set.set.content.order.iter().map(|position| {
            serde_json::from_value(serde_json::json!({"entry_kind": position.kind(), "content": set.set.content.entry_fields(*position)})).map_err(|_| ComparisonSource::internal())
        }).collect::<Result<_, _>>()?;
        Ok(wire::RuleDiffSet {
            key: set.set.header.key.clone(),
            version: set.version,
            entries,
        })
    }
}
/// History stores content and direct parent revisions, but no sibling-order snapshots.
const HISTORICAL_SIBLING_ORDER: i64 = 0;
#[derive(Clone, Copy)]
enum ChainRevision {
    Saved(u32),
    Current,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DiffPageProgress {
    More,
    Complete,
}

#[derive(Debug)]
pub(super) struct DiffPage {
    response: wire::RulesTextDiffResponse,
    budget: PageBudget,
    parameters: wire::CursorQuery,
    position: PagePosition,
    items: VecDeque<DiffItem>,
    index: usize,
    admitted: usize,
}
#[derive(Debug, Clone)]
enum SideKind {
    SavedParent,
    CurrentParent,
    Overlay,
}
#[derive(Debug)]
enum DiffItem {
    Change(wire::RuleDiffChange),
    Category(wire::RuleDiffCategoryDelta),
    Sample(Box<wire::RulePreviewSample>),
    Side {
        side: SideKind,
        set: wire::RuleDiffSet,
    },
}
impl DiffPage {
    pub(super) fn new(
        mut response: wire::RulesTextDiffResponse,
        parameters: wire::CursorQuery,
        position: PagePosition,
    ) -> Result<Self, RunQueryError> {
        let mut items = VecDeque::new();
        items.extend(
            std::mem::take(&mut response.changes)
                .into_iter()
                .map(DiffItem::Change),
        );
        items.extend(
            std::mem::take(&mut response.impact.category_deltas)
                .into_iter()
                .map(DiffItem::Category),
        );
        items.extend(
            std::mem::take(&mut response.samples)
                .into_iter()
                .map(Box::new)
                .map(DiffItem::Sample),
        );
        if let Some(sides) = &mut response.sides {
            Self::side_items(
                &mut items,
                SideKind::SavedParent,
                std::mem::take(&mut sides.saved_parent),
            );
            Self::side_items(
                &mut items,
                SideKind::CurrentParent,
                std::mem::take(&mut sides.current_parent),
            );
            Self::side_items(
                &mut items,
                SideKind::Overlay,
                std::mem::take(&mut sides.overlay),
            );
        }
        let budget = PageBudget::new(&response)?.with_cursor(&wire::QueryCursor::new(
            response.versions.clone(),
            parameters.clone(),
            usize::MAX,
        ))?;
        Ok(Self {
            response,
            budget,
            parameters,
            position,
            items,
            index: 0,
            admitted: 0,
        })
    }
    fn side_items(items: &mut VecDeque<DiffItem>, side: SideKind, sets: Vec<wire::RuleDiffSet>) {
        for mut set in sets {
            let entries = std::mem::take(&mut set.entries);
            if entries.is_empty() {
                items.push_back(DiffItem::Side {
                    side: side.clone(),
                    set,
                });
            } else {
                for entry in entries {
                    items.push_back(DiffItem::Side {
                        side: side.clone(),
                        set: wire::RuleDiffSet {
                            key: set.key.clone(),
                            version: set.version,
                            entries: vec![entry],
                        },
                    });
                }
            }
        }
    }
    pub(super) fn advance(
        &mut self,
        control: &QueryControl,
    ) -> Result<DiffPageProgress, RunQueryError> {
        for _ in 0..QUERY_BATCH_ROWS {
            control.check()?;
            let Some(item) = self.items.pop_front() else {
                if !self.position.is_first() && self.index <= self.position.index() {
                    return Err(RunQueryError::InvalidCursor);
                }
                return Ok(DiffPageProgress::Complete);
            };
            if self.index < self.position.index() {
                self.index += 1;
                continue;
            }
            let admitted = match &item {
                DiffItem::Change(row) => self.budget.admit(row)?,
                DiffItem::Category(row) => self.budget.admit(row)?,
                DiffItem::Sample(row) => self.budget.admit(row)?,
                DiffItem::Side { set, .. } => self.budget.admit(set)?,
            };
            if !admitted {
                if self.admitted == 0 {
                    return Err(RunQueryError::PageTooLarge);
                }
                self.response.next_cursor = Some(wire::QueryCursor::new(
                    self.response.versions.clone(),
                    self.parameters.clone(),
                    self.index,
                ));
                return Ok(DiffPageProgress::Complete);
            }
            match item {
                DiffItem::Change(row) => self.response.changes.push(row),
                DiffItem::Category(row) => self.response.impact.category_deltas.push(row),
                DiffItem::Sample(row) => self.response.samples.push(*row),
                DiffItem::Side { side, mut set } => {
                    let sides = self.response.sides.as_mut().expect("upstream sides");
                    let sets = match side {
                        SideKind::SavedParent => &mut sides.saved_parent,
                        SideKind::CurrentParent => &mut sides.current_parent,
                        SideKind::Overlay => &mut sides.overlay,
                    };
                    if let Some(previous) =
                        sets.last_mut().filter(|previous| previous.key == set.key)
                    {
                        previous.entries.append(&mut set.entries);
                    } else {
                        sets.push(set);
                    }
                }
            }
            self.index += 1;
            self.admitted += 1;
        }
        Ok(DiffPageProgress::More)
    }
    pub(super) fn finish(self) -> wire::RulesTextDiffResponse {
        self.response
    }
}

pub(super) struct CategoryDeltas {
    rows: BTreeMap<Option<wire::CategoryId>, wire::RuleDiffCategoryDelta>,
}
impl CategoryDeltas {
    pub(super) fn new(
        before: Vec<wire::PreviewCategoryRank>,
        after: Vec<wire::PreviewCategoryRank>,
    ) -> Self {
        let mut rows = BTreeMap::new();
        for row in before {
            rows.insert(
                row.category.id(),
                wire::RuleDiffCategoryDelta {
                    category: row.category,
                    before_active_ms: row.active_ms,
                    after_active_ms: 0,
                },
            );
        }
        for row in after {
            rows.entry(row.category.id())
                .or_insert_with(|| wire::RuleDiffCategoryDelta {
                    category: row.category,
                    before_active_ms: 0,
                    after_active_ms: 0,
                })
                .after_active_ms = row.active_ms;
        }
        Self { rows }
    }
    pub(super) fn finish(self) -> Vec<wire::RuleDiffCategoryDelta> {
        self.rows
            .into_values()
            .filter(|row| row.before_active_ms != row.after_active_ms)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parameters() -> wire::CursorQuery {
        wire::CursorQuery::RulesTextDiff {
            comparison_digest: "comparison".into(),
            start_date: "2025-01-02".parse().expect("date"),
            end_date: "2025-01-03".parse().expect("date"),
            expected_versions: wire::QueryVersions("inputs".into()),
            deadline_ms: Default::default(),
            sample_limit: Default::default(),
        }
    }
    fn response() -> wire::RulesTextDiffResponse {
        wire::RulesTextDiffResponse {
            changes: Vec::new(),
            impact: wire::RuleDiffImpact {
                category_deltas: Vec::new(),
                document_changes: Some(50),
                further_hidden: Some(Default::default()),
            },
            samples: (0..50)
                .map(|index| wire::RulePreviewSample {
                    start_utc_ms: i64::from(index),
                    end_utc_ms: i64::from(index + 1),
                    clock_segment_id: wire::ClockSegmentId(1),
                    rule: "user.extract/document@2".into(),
                    before: wire::RulePreviewOutput::Extraction {
                        extraction: wire::TitleExtraction {
                            document: Some("a".repeat(3000)),
                            ..Default::default()
                        },
                    },
                    after: wire::RulePreviewOutput::Extraction {
                        extraction: wire::TitleExtraction {
                            document: Some("b".repeat(3000)),
                            ..Default::default()
                        },
                    },
                })
                .collect(),
            sides: None,
            versions: wire::QueryVersions("versions".into()),
            next_cursor: None,
        }
    }
    #[test]
    fn samples_page_completely_in_global_order_within_the_shared_byte_budget() {
        let original = response();
        let mut position = PagePosition::First;
        let mut samples = Vec::new();
        let mut pages = 0;
        loop {
            let mut response = original.clone();
            if !position.is_first() {
                response.impact.document_changes = None;
                response.impact.further_hidden = None;
            }
            let mut page = DiffPage::new(response, parameters(), position).expect("page");
            while page.advance(&QueryControl::default()).expect("turn") == DiffPageProgress::More {}
            let response = page.finish();
            assert!(
                serde_json::to_vec(&response).expect("encoded page").len()
                    <= wire::QUERY_PAGE_BYTES
            );
            samples.extend(response.samples);
            pages += 1;
            let Some(cursor) = response.next_cursor else {
                break;
            };
            position = PagePosition::from_cursor(Some(&cursor), &original.versions, &parameters())
                .expect("position");
        }
        assert!(pages > 1);
        assert_eq!(samples, original.samples);
    }
    #[test]
    fn page_work_checks_cancellation_and_deadlines_while_skipping_continuations() {
        let mut page =
            DiffPage::new(response(), parameters(), PagePosition::Cursor(40)).expect("page");
        let cancelled = QueryControl::default();
        cancelled.cancel();
        assert_eq!(
            page.advance(&cancelled).expect_err("cancelled").kind(),
            ErrorKind::Cancelled
        );
        assert_eq!(
            page.advance(&QueryControl::expired())
                .expect_err("expired")
                .kind(),
            ErrorKind::DeadlineExceeded
        );
    }
    #[test]
    fn oversized_single_items_fail_instead_of_returning_a_repeating_cursor() {
        let mut response = response();
        response.samples.truncate(1);
        response.samples[0].rule = "x".repeat(wire::QUERY_PAGE_BYTES);
        let mut page = DiffPage::new(response, parameters(), PagePosition::First).expect("page");
        assert!(matches!(
            page.advance(&QueryControl::default()),
            Err(RunQueryError::PageTooLarge)
        ));
    }
    #[test]
    fn out_of_range_continuations_fail_after_skipping_the_available_items() {
        let mut page = DiffPage::new(response(), parameters(), PagePosition::Cursor(usize::MAX))
            .expect("page");
        assert!(matches!(
            page.advance(&QueryControl::default()),
            Err(RunQueryError::InvalidCursor)
        ));
    }
}
