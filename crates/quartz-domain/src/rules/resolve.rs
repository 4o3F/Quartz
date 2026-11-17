use super::{ContentHash, Diagnostic, DiagnosticCode, EntryPosition, Rule, RuleKind, RuleSet};
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
/// Original definition and every replacement, in ancestor-to-descendant order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleProvenance {
    /// Key of the set that originally introduced the rule.
    pub defining_set: String,
    /// Revision of the original definition.
    pub defining_version: u32,
    /// Replacing set keys and revisions, ordered from the first ancestor replacement to the latest.
    pub overrides: Vec<(String, u32)>,
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
        let (set, version) = self
            .provenance
            .overrides
            .last()
            .map(|(set, version)| (set.as_str(), *version))
            .unwrap_or((
                &self.provenance.defining_set,
                self.provenance.defining_version,
            ));
        format!("{set}/{}@{version}", self.rule.id)
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
}

impl Resolution {
    /// Validates ancestry and sibling modifications, then applies overlays in tree preorder.
    pub fn resolve(sets: &[InstalledSet], kind: RuleKind) -> Self {
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
        let mut modifications: BTreeMap<&str, Vec<(usize, String)>> = BTreeMap::new();
        let mut bad_targets = BTreeSet::new();
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
                    diagnostics.push(installed.set.diagnostic(
                        DiagnosticCode::Target,
                        &path,
                        "target does not name a rule in an ancestor set",
                    ));
                    bad_targets.insert((index, path.clone()));
                }
                let previous = modifications.entry(target).or_default();
                for (other, other_path) in previous.iter() {
                    if !ancestors[index].contains(other) && !ancestors[*other].contains(&index) {
                        diagnostics.push(installed.set.diagnostic(
                            DiagnosticCode::Conflict,
                            &path,
                            "target is also modified outside this ancestor chain",
                        ));
                        bad_targets.insert((index, path.clone()));
                        bad_targets.insert((*other, other_path.clone()));
                    }
                }
                previous.push((index, path));
            }
        }
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
        for (preorder, index) in traversal.into_iter().enumerate() {
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
                        .unwrap_or(file_order),
                });
            }
            for (i, entry) in set.content.r#override.iter().enumerate() {
                if bad_targets.contains(&(index, format!("override[{i}].target"))) {
                    continue;
                }
                if let Some(position) = rules.iter().position(|rule| rule.full_id == entry.target) {
                    let mut replacement = rules[position].clone();
                    replacement.rule = entry.rule.clone();
                    replacement
                        .provenance
                        .overrides
                        .push((set.header.key.clone(), installed.version));
                    replacement.depth = ancestors[index].len();
                    replacement.preorder = preorder;
                    replacement.file_order = set
                        .content
                        .order
                        .iter()
                        .position(|p| *p == EntryPosition::Override(i))
                        .unwrap_or(i);
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
                            .push((set.header.key.clone(), installed.version));
                        replacement.depth = ancestors[index].len();
                        replacement.preorder = preorder;
                        replacement.file_order = set
                            .content
                            .order
                            .iter()
                            .position(|p| *p == EntryPosition::Override(i))
                            .unwrap_or(i);
                        rules.push(replacement);
                    }
                }
            }
            for (i, entry) in set.content.disable.iter().enumerate() {
                if bad_targets.contains(&(index, format!("disable[{i}].target"))) {
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
        Resolution {
            effective: EffectiveRules {
                kind,
                rules,
                suppressed,
                version_hash,
            },
            diagnostics,
        }
    }
}
