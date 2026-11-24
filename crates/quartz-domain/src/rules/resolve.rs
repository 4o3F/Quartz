use super::{
    ContentHash, Diagnostic, DiagnosticCode, EntryPosition, Rule, RuleKind, RuleSet, RuleSetKey,
    RuleVersion,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Whether a set and its dependent subtree participate in evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enablement {
    /// Includes this set when its ancestors are also enabled.
    Enabled,
    /// Excludes this set and every descendant from the effective list.
    Disabled,
}
/// Current persisted revision and user tree ordering for a parsed rule set.
#[derive(Debug, Clone)]
pub struct InstalledSet {
    /// Parsed rule-set contents and diagnostic locations.
    pub set: RuleSet,
    /// Current persisted revision used in explanation identifiers.
    pub version: u32,
    /// Whether this set and its dependent subtree participate in evaluation.
    pub enabled: Enablement,
    /// User-defined sibling position, with smaller values traversed first.
    pub sibling_order: i64,
}
/// The rule-set revision supplying an ancestor replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideRef {
    /// Validated key of the replacing rule set.
    pub set: RuleSetKey,
    /// Revision of the replacing rule set.
    pub version: RuleVersion,
}
impl OverrideRef {
    const FIELD_COUNT: usize = 2;
    fn from_installed(installed: &InstalledSet) -> Self {
        Self {
            set: installed
                .set
                .header
                .key
                .parse()
                .expect("parsed rule sets have validated keys"),
            version: RuleVersion::new(installed.version),
        }
    }
}
// Preserve the existing two-element revision array on the serialized boundary.
impl Serialize for OverrideRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut fields = serializer.serialize_tuple(Self::FIELD_COUNT)?;
        fields.serialize_element(&self.set)?;
        fields.serialize_element(&self.version)?;
        fields.end()
    }
}
impl<'de> Deserialize<'de> for OverrideRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_tuple(Self::FIELD_COUNT, OverrideRefVisitor)
    }
}
struct OverrideRefVisitor;
impl<'de> serde::de::Visitor<'de> for OverrideRefVisitor {
    type Value = OverrideRef;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a rule-set key followed by its revision")
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut sequence: A,
    ) -> Result<Self::Value, A::Error> {
        use serde::de::Error;
        let set = sequence
            .next_element()?
            .ok_or_else(|| A::Error::invalid_length(0, &self))?;
        let version = sequence
            .next_element()?
            .ok_or_else(|| A::Error::invalid_length(1, &self))?;
        if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
            return Err(A::Error::invalid_length(
                OverrideRef::FIELD_COUNT + 1,
                &self,
            ));
        }
        Ok(OverrideRef { set, version })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Modification {
    set_index: usize,
    path: String,
}

/// Original definition and every replacement, in ancestor-to-descendant order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleProvenance {
    /// Key of the set that originally introduced the rule.
    pub defining_set: String,
    /// Revision of the original definition.
    pub defining_version: u32,
    /// Replacing set keys and revisions, ordered from the first ancestor replacement to the latest.
    pub overrides: Vec<OverrideRef>,
}
/// A rule after ancestor overlays have been applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveRule {
    /// Original definition name in `<set-key>/<rule-id>` form, retained across replacements.
    pub full_id: String,
    /// Current contents after applying ancestor overlays.
    pub rule: Rule,
    /// Original definition and the ordered history of replacements.
    pub provenance: RuleProvenance,
    /// Depth of the set supplying the current contents; roots have depth zero.
    pub depth: usize,
    /// Zero-based position of the supplying set in the enabled tree traversal.
    pub preorder: usize,
    /// Zero-based source position of the supplying rule or override across entry types.
    pub file_order: usize,
}
impl EffectiveRule {
    /// Returns the explanation ID for the revision supplying the current contents.
    pub fn explanation_id(&self) -> String {
        match self.provenance.overrides.last() {
            Some(replacement) => format!(
                "{}/{}@{}",
                replacement.set, self.rule.id, replacement.version
            ),
            None => format!(
                "{}/{}@{}",
                self.provenance.defining_set, self.rule.id, self.provenance.defining_version
            ),
        }
    }
}
/// Why a formerly effective rule no longer participates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuppressionReason {
    /// An override entry replaced these contents with a complete rule definition.
    Overridden {
        /// Key of the set supplying the replacement.
        by_set: String,
    },
    /// A disable entry removed the rule from the effective list.
    Disabled {
        /// Key of the set supplying the disable entry.
        by_set: String,
    },
}
/// Contents and source of a removed or replaced rule, retained for explanations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuppressedRule {
    /// Contents and provenance immediately before removal or replacement.
    pub rule: EffectiveRule,
    /// Overlay operation and source set that suppressed this version.
    pub reason: SuppressionReason,
}
/// Deterministic family-specific rules and their cache version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRules {
    /// Rule family represented by this list.
    pub kind: RuleKind,
    /// Effective rules sorted by descending priority, descending depth, traversal position, and file order.
    pub rules: Vec<EffectiveRule>,
    /// Removed and replaced versions retained for explanations.
    pub suppressed: Vec<SuppressedRule>,
    /// SHA-256 derived from enabled sets in traversal order and their canonical content hashes.
    pub version_hash: ContentHash,
}
/// Tree validation results, with invalid overlays excluded from the provisional effective list.
#[derive(Debug, Clone)]
#[must_use = "inspect diagnostics before using the provisional effective rules"]
pub struct Resolution {
    /// Provisional effective list with invalid overlays excluded; inspect diagnostics before using it.
    pub effective: EffectiveRules,
    /// All detected identity, ancestry, target, and conflict errors; empty for a valid tree.
    pub diagnostics: Vec<Diagnostic>,
    /// All nodes in tree preorder, including effectively disabled subtrees.
    pub nodes: Vec<ResolvedNode>,
    /// Every authored entry in tree and file order.
    pub entries: Vec<ResolvedEntry>,
    /// Rejected directives, including both sides of a sibling conflict.
    pub overlay_warnings: Vec<OverlayWarning>,
}

/// Participation of an authored entry after the shared resolver applies overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedEntryState {
    Effective,
    Disabled,
    Overridden,
    TargetMissing,
}
/// Source entry and its resolved provenance, retained for rule editors.
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    /// Key of the authoring set.
    pub key: String,
    /// Entry identity in canonical contents.
    pub position: EntryPosition,
    /// Final state of this authored entry.
    pub state: ResolvedEntryState,
    /// Definition and replacement chain up to this entry.
    pub provenance: Vec<RuleEntrySource>,
    /// Position in the effective evaluation list, present for active rule contents.
    pub effective_order: Option<usize>,
}
/// One definition, replacement, or disable author in an editor entry's chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleEntrySource {
    /// Authoring rule-set key.
    pub key: String,
    /// Authoring revision.
    pub version: u32,
    /// Original target's local rule identifier.
    pub id: String,
}
impl RuleEntrySource {
    fn from_installed(installed: &InstalledSet, id: &str) -> Self {
        Self {
            key: installed.set.header.key.clone(),
            version: installed.version,
            id: id.into(),
        }
    }
}
/// Tree traversal includes disabled descendants with their effective enablement.
#[derive(Debug, Clone)]
pub struct ResolvedNode {
    /// Key of this tree node.
    pub key: String,
    /// Enablement after ancestors and compilation exclusions are applied.
    pub enabled: Enablement,
}
/// Structured overlay rejection produced during target and conflict validation.
#[derive(Debug, Clone)]
pub struct OverlayWarning {
    /// Set containing the rejected directive.
    pub key: String,
    /// Qualified target rule reference.
    pub target: String,
    /// Typed validation cause; Target or Conflict.
    pub code: DiagnosticCode,
}

impl Resolution {
    /// Validates the whole tree, then resolves the selected rule family.
    pub fn resolve(sets: &[InstalledSet], kind: RuleKind) -> Self {
        RuleTree::new(sets).resolve(kind).into_resolution()
    }
}

/// Family evaluation and diagnostics produced independently of shared tree validation.
#[derive(Debug)]
pub(super) struct FamilyResolution {
    resolution: Resolution,
    diagnostics: Vec<Diagnostic>,
}
impl FamilyResolution {
    pub(super) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub(super) fn into_resolution(self) -> Resolution {
        self.resolution
    }
}

/// Shared ancestry and directive validation for every family in one installed tree.
pub(super) struct RuleTree<'sets> {
    sets: &'sets [InstalledSet],
    by_key: BTreeMap<&'sets str, usize>,
    ancestors: Vec<Vec<usize>>,
    invalid: BTreeSet<usize>,
    bad_targets: BTreeSet<Modification>,
    diagnostics: Vec<Diagnostic>,
    overlay_warnings: Vec<OverlayWarning>,
}
impl<'sets> RuleTree<'sets> {
    pub(super) fn new(sets: &'sets [InstalledSet]) -> Self {
        let mut diagnostics = Vec::new();
        let mut by_key = BTreeMap::new();
        for (index, installed) in sets.iter().enumerate() {
            if by_key
                .insert(installed.set.header.key.as_str(), index)
                .is_some()
            {
                diagnostics.push(installed.set.diagnostic(
                    DiagnosticCode::Set,
                    "set.key",
                    "duplicate rule-set key",
                ));
            }
        }
        let mut ancestors = vec![Vec::new(); sets.len()];
        let mut invalid = BTreeSet::new();
        for (index, installed) in sets.iter().enumerate() {
            let mut visited = BTreeSet::from([index]);
            let mut cursor = index;
            while let Some(parent) = &sets[cursor].set.header.parent {
                let Some(&parent_index) = by_key.get(parent.as_str()) else {
                    diagnostics.push(installed.set.diagnostic(
                        DiagnosticCode::Parent,
                        "set.parent",
                        "parent rule set does not exist",
                    ));
                    invalid.insert(index);
                    break;
                };
                if !visited.insert(parent_index) {
                    diagnostics.push(installed.set.diagnostic(
                        DiagnosticCode::Parent,
                        "set.parent",
                        "rule-set tree contains a cycle",
                    ));
                    invalid.insert(index);
                    break;
                }
                if sets[parent_index].set.header.kind != installed.set.header.kind {
                    diagnostics.push(installed.set.diagnostic(
                        DiagnosticCode::Set,
                        "set.parent",
                        "parent and child must have the same kind",
                    ));
                    invalid.insert(index);
                }
                ancestors[index].push(parent_index);
                cursor = parent_index;
            }
        }
        let mut modifications: BTreeMap<&str, Vec<Modification>> = BTreeMap::new();
        let mut bad_targets = BTreeSet::new();
        let mut overlay_warnings = Vec::new();
        for (index, installed) in sets.iter().enumerate() {
            let targets = installed
                .set
                .content
                .r#override
                .iter()
                .enumerate()
                .map(|(i, entry)| (entry.target.as_str(), format!("override[{i}].target")))
                .chain(
                    installed
                        .set
                        .content
                        .disable
                        .iter()
                        .enumerate()
                        .map(|(i, entry)| (entry.target.as_str(), format!("disable[{i}].target"))),
                );
            for (target, path) in targets {
                let exists = target.split_once('/').is_some_and(|(key, id)| {
                    ancestors[index].iter().any(|&ancestor| {
                        sets[ancestor].set.header.key == key
                            && sets[ancestor]
                                .set
                                .content
                                .rule
                                .iter()
                                .any(|rule| rule.id == id)
                    })
                });
                if !exists {
                    overlay_warnings.push(OverlayWarning {
                        key: installed.set.header.key.clone(),
                        target: target.into(),
                        code: DiagnosticCode::Target,
                    });
                    diagnostics.push(installed.set.diagnostic(
                        DiagnosticCode::Target,
                        &path,
                        "target does not name a rule in an ancestor set",
                    ));
                    bad_targets.insert(Modification {
                        set_index: index,
                        path: path.clone(),
                    });
                }
                let previous = modifications.entry(target).or_default();
                for Modification {
                    set_index: other,
                    path: other_path,
                } in previous.iter()
                {
                    if !ancestors[index].contains(other) && !ancestors[*other].contains(&index) {
                        for set_index in [index, *other] {
                            overlay_warnings.push(OverlayWarning {
                                key: sets[set_index].set.header.key.clone(),
                                target: target.into(),
                                code: DiagnosticCode::Conflict,
                            });
                        }
                        diagnostics.push(installed.set.diagnostic(
                            DiagnosticCode::Conflict,
                            &path,
                            "target is also modified outside this ancestor chain",
                        ));
                        bad_targets.insert(Modification {
                            set_index: index,
                            path: path.clone(),
                        });
                        bad_targets.insert(Modification {
                            set_index: *other,
                            path: other_path.clone(),
                        });
                    }
                }
                previous.push(Modification {
                    set_index: index,
                    path,
                });
            }
        }
        Self {
            sets,
            by_key,
            ancestors,
            invalid,
            bad_targets,
            diagnostics,
            overlay_warnings,
        }
    }
    pub(super) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub(super) fn resolve(&self, kind: RuleKind) -> FamilyResolution {
        let sets = self.sets;
        let by_key = &self.by_key;
        let ancestors = &self.ancestors;
        let invalid = &self.invalid;
        let bad_targets = &self.bad_targets;
        let overlay_warnings = &self.overlay_warnings;
        let mut traversal: Vec<_> = (0..sets.len())
            .filter(|&i| {
                sets[i].set.header.kind == kind
                    && !invalid.contains(&i)
                    && sets[i].enabled == Enablement::Enabled
                    && ancestors[i]
                        .iter()
                        .all(|a| !invalid.contains(a) && sets[*a].enabled == Enablement::Enabled)
            })
            .collect();
        // Lexicographic ancestry paths produce preorder and preserve user sibling order.
        traversal.sort_by_key(|&index| {
            let mut path: Vec<_> = ancestors[index].iter().rev().copied().collect();
            path.push(index);
            path.into_iter()
                .map(|i| (sets[i].sibling_order, sets[i].set.header.key.as_str()))
                .collect::<Vec<_>>()
        });
        let version_inputs: Vec<_> = traversal
            .iter()
            .map(|&i| {
                (
                    &sets[i].set.header.key,
                    ancestors[i].len(),
                    sets[i].set.content.content_hash(),
                )
            })
            .collect();
        let version_hash = ContentHash::of(
            &serde_json::to_string(&version_inputs).expect("hash inputs are JSON-safe"),
        );
        let mut rules: Vec<EffectiveRule> = Vec::new();
        let mut suppressed = Vec::new();
        for (preorder, index) in traversal.iter().copied().enumerate() {
            let installed = &sets[index];
            let set = &installed.set;
            for (file_order, rule) in set.content.rule.iter().enumerate() {
                rules.push(EffectiveRule {
                    full_id: format!("{}/{}", set.header.key, rule.id),
                    rule: rule.clone(),
                    provenance: RuleProvenance {
                        defining_set: set.header.key.clone(),
                        defining_version: installed.version,
                        overrides: Vec::new(),
                    },
                    depth: ancestors[index].len(),
                    preorder,
                    file_order: set
                        .content
                        .order
                        .iter()
                        .position(|p| *p == EntryPosition::Rule(file_order))
                        .expect("canonical content order includes every rule"),
                });
            }
            for (i, entry) in set.content.r#override.iter().enumerate() {
                if bad_targets.contains(&Modification {
                    set_index: index,
                    path: format!("override[{i}].target"),
                }) {
                    continue;
                }
                if let Some(position) = rules.iter().position(|rule| rule.full_id == entry.target) {
                    let mut replacement = rules[position].clone();
                    replacement.rule = entry.rule.clone();
                    replacement
                        .provenance
                        .overrides
                        .push(OverrideRef::from_installed(installed));
                    replacement.depth = ancestors[index].len();
                    replacement.preorder = preorder;
                    replacement.file_order = set
                        .content
                        .order
                        .iter()
                        .position(|p| *p == EntryPosition::Override(i))
                        .expect("canonical content order includes every override");
                    let old = std::mem::replace(&mut rules[position], replacement);
                    suppressed.push(SuppressedRule {
                        rule: old,
                        reason: SuppressionReason::Overridden {
                            by_set: set.header.key.clone(),
                        },
                    });
                } else {
                    // An ancestor disable can be followed by a descendant override of the original definition.
                    if let Some(old) = suppressed
                        .iter()
                        .rev()
                        .find(|old| old.rule.full_id == entry.target)
                    {
                        let mut replacement = old.rule.clone();
                        replacement.rule = entry.rule.clone();
                        replacement
                            .provenance
                            .overrides
                            .push(OverrideRef::from_installed(installed));
                        replacement.depth = ancestors[index].len();
                        replacement.preorder = preorder;
                        replacement.file_order = set
                            .content
                            .order
                            .iter()
                            .position(|p| *p == EntryPosition::Override(i))
                            .expect("canonical content order includes every override");
                        rules.push(replacement);
                    }
                }
            }
            for (i, entry) in set.content.disable.iter().enumerate() {
                if bad_targets.contains(&Modification {
                    set_index: index,
                    path: format!("disable[{i}].target"),
                }) {
                    continue;
                }
                if let Some(position) = rules.iter().position(|rule| rule.full_id == entry.target) {
                    suppressed.push(SuppressedRule {
                        rule: rules.remove(position),
                        reason: SuppressionReason::Disabled {
                            by_set: set.header.key.clone(),
                        },
                    });
                }
            }
        }
        rules.sort_by_key(|r| {
            (
                std::cmp::Reverse(r.rule.priority),
                std::cmp::Reverse(r.depth),
                r.preorder,
                r.file_order,
            )
        });
        let mut all_nodes: Vec<_> = (0..sets.len())
            .filter(|&index| sets[index].set.header.kind == kind)
            .collect();
        all_nodes.sort_by_key(|&index| {
            let mut path: Vec<_> = ancestors[index].iter().rev().copied().collect();
            path.push(index);
            path.into_iter()
                .map(|i| (sets[i].sibling_order, sets[i].set.header.key.as_str()))
                .collect::<Vec<_>>()
        });
        let mut nodes = Vec::new();
        let mut entries = Vec::new();
        for index in all_nodes {
            let installed = &sets[index];
            let key = &installed.set.header.key;
            let enabled = traversal.contains(&index);
            nodes.push(ResolvedNode {
                key: key.clone(),
                enabled: if enabled {
                    Enablement::Enabled
                } else {
                    Enablement::Disabled
                },
            });
            for (file_order, position) in installed.set.content.order.iter().copied().enumerate() {
                let target = match position {
                    EntryPosition::Rule(i) => format!("{key}/{}", installed.set.content.rule[i].id),
                    EntryPosition::Override(i) => {
                        installed.set.content.r#override[i].target.clone()
                    }
                    EntryPosition::Disable(i) => installed.set.content.disable[i].target.clone(),
                };
                let supplies = |rule: &EffectiveRule| {
                    rule.full_id == target
                        && rule.file_order == file_order
                        && rule
                            .provenance
                            .overrides
                            .last()
                            .map_or(rule.provenance.defining_set.as_str(), |replacement| {
                                replacement.set.as_str()
                            })
                            == key
                };
                let effective_order = rules.iter().position(supplies);
                let removed = suppressed.iter().find(|old| supplies(&old.rule));
                let missing = overlay_warnings.iter().any(|warning| {
                    warning.key == *key
                        && warning.target == target
                        && warning.code == DiagnosticCode::Target
                });
                let state = if missing {
                    ResolvedEntryState::TargetMissing
                } else if !enabled {
                    ResolvedEntryState::Disabled
                } else if effective_order.is_some() {
                    ResolvedEntryState::Effective
                } else if removed
                    .is_some_and(|old| matches!(old.reason, SuppressionReason::Overridden { .. }))
                {
                    ResolvedEntryState::Overridden
                } else {
                    ResolvedEntryState::Disabled
                };
                let source = effective_order
                    .map(|i| &rules[i])
                    .or_else(|| removed.map(|old| &old.rule));
                let mut provenance = Vec::new();
                if let Some(definition) = target.split_once('/') {
                    let id = definition.1;
                    if let Some(source) = source {
                        provenance.push(RuleEntrySource {
                            key: source.provenance.defining_set.clone(),
                            version: source.provenance.defining_version,
                            id: id.into(),
                        });
                        provenance.extend(source.provenance.overrides.iter().map(|replacement| {
                            RuleEntrySource {
                                key: replacement.set.to_string(),
                                version: replacement.version.get(),
                                id: id.into(),
                            }
                        }));
                    } else {
                        if let Some(definition_index) = by_key.get(definition.0) {
                            provenance.push(RuleEntrySource::from_installed(
                                &sets[*definition_index],
                                id,
                            ));
                        }
                        if !matches!(position, EntryPosition::Rule(_)) {
                            for ancestor in ancestors[index].iter().rev() {
                                let ancestor = &sets[*ancestor];
                                if ancestor
                                    .set
                                    .content
                                    .r#override
                                    .iter()
                                    .any(|entry| entry.target == target)
                                {
                                    provenance.push(RuleEntrySource::from_installed(ancestor, id));
                                }
                            }
                            provenance.push(RuleEntrySource::from_installed(installed, id));
                        }
                    }
                }
                entries.push(ResolvedEntry {
                    key: key.clone(),
                    position,
                    state,
                    provenance,
                    effective_order,
                });
            }
        }
        let mut diagnostics = Vec::new();
        for entry in &entries {
            let set = &sets[by_key[entry.key.as_str()]].set;
            if entry.format_entry(&set.content).to_string().len() > super::MAX_ENTRY_BYTES {
                diagnostics.push(set.diagnostic(
                    DiagnosticCode::Value,
                    "",
                    "encoded rule entry exceeds 128 KiB",
                ));
            }
        }
        let all_diagnostics = self
            .diagnostics
            .iter()
            .chain(&diagnostics)
            .cloned()
            .collect();
        FamilyResolution {
            diagnostics,
            resolution: Resolution {
                nodes,
                entries,
                overlay_warnings: overlay_warnings.clone(),
                effective: EffectiveRules {
                    kind,
                    rules,
                    suppressed,
                    version_hash,
                },
                diagnostics: all_diagnostics,
            },
        }
    }
}
