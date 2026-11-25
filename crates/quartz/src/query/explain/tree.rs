//! Tree projection from the shared resolver and evaluator, with retained values only.
use super::{
    CategorySource, Classifier, DerivedSegment, QueryControl, RuleSnapshot, RunQueryError,
    SegmentFacts, domain, wire,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EntryKey {
    stage: domain::RuleKind,
    set: String,
    file_order: usize,
}
/// Compiles inactive contents once, retaining compilation failures as unavailable evidence.
#[derive(Debug, Default)]
pub(super) struct ExplainRules {
    entries: BTreeMap<EntryKey, Option<domain::CompiledRules>>,
}
impl ExplainRules {
    pub(super) fn prepare(
        rules: &RuleSnapshot,
        control: &QueryControl,
    ) -> Result<Self, RunQueryError> {
        let mut entries = BTreeMap::new();
        for stage in [
            domain::RuleKind::Extraction,
            domain::RuleKind::Classification,
        ] {
            let resolution = rules
                .resolution(stage)
                .ok_or(RunQueryError::InvalidRuleEntry)?;
            for entry in &resolution.entries {
                control.check()?;
                if entry.effective_order.is_some() {
                    continue;
                }
                let installed = &rules
                    .state()
                    .sets
                    .get(
                        &entry
                            .key
                            .parse()
                            .map_err(|_| RunQueryError::InvalidRuleEntry)?,
                    )
                    .ok_or(RunQueryError::InvalidRuleEntry)?
                    .current;
                let Some(rule) = Self::rule(&installed.set.content, entry.position) else {
                    continue;
                };
                let file_order = Self::file_order(&installed.set.content, entry.position)?;
                let single = domain::EffectiveRules {
                    kind: stage,
                    rules: vec![domain::EffectiveRule {
                        full_id: format!("{}/{}", entry.key, rule.id),
                        rule: rule.clone(),
                        provenance: domain::RuleProvenance {
                            defining_set: entry.key.clone(),
                            defining_version: installed.version,
                            overrides: Vec::new(),
                        },
                        depth: 0,
                        preorder: 0,
                        file_order,
                    }],
                    suppressed: Vec::new(),
                    version_hash: resolution.effective.version_hash.clone(),
                };
                // Invalid startup contents remain visible with unknown condition results.
                let compiled = single.compile().ok();
                entries.insert(
                    EntryKey {
                        stage,
                        set: entry.key.clone(),
                        file_order,
                    },
                    compiled,
                );
            }
        }
        Ok(Self { entries })
    }
    fn rule(content: &domain::Content, position: domain::EntryPosition) -> Option<&domain::Rule> {
        match position {
            domain::EntryPosition::Rule(index) => Some(&content.rule[index]),
            domain::EntryPosition::Override(index) => Some(&content.r#override[index].rule),
            domain::EntryPosition::Disable(_) => None,
        }
    }
    fn conditions(rule: &domain::Rule) -> &[domain::Condition] {
        match &rule.body {
            domain::RuleBody::Classification { when, .. }
            | domain::RuleBody::Privacy { when, .. } => when,
            domain::RuleBody::Extraction { app, .. } => app,
        }
    }
    fn file_order(
        content: &domain::Content,
        position: domain::EntryPosition,
    ) -> Result<usize, RunQueryError> {
        content
            .order
            .iter()
            .position(|candidate| *candidate == position)
            .ok_or(RunQueryError::InvalidRuleEntry)
    }
}

pub(super) struct FragmentExplanation<'rules> {
    rules: &'rules RuleSnapshot,
    prepared: &'rules ExplainRules,
    control: &'rules QueryControl,
}
impl<'rules> FragmentExplanation<'rules> {
    pub(super) fn new(
        rules: &'rules RuleSnapshot,
        prepared: &'rules ExplainRules,
        control: &'rules QueryControl,
    ) -> Self {
        Self {
            rules,
            prepared,
            control,
        }
    }
    pub(super) fn fragment(
        &self,
        segment: &DerivedSegment,
        facts: &SegmentFacts,
    ) -> Result<wire::ExplainFragment, RunQueryError> {
        let row = facts.explain_foreground(segment);
        let otherwise_rule = facts.explain_classification_rule(segment);
        let category_source = match &segment.category_source {
            CategorySource::Rule(rule) => wire::ExplainCategorySource::Rule {
                rule: rule.as_str().into(),
            },
            CategorySource::Annotation(annotation) => wire::ExplainCategorySource::Annotation {
                annotation: annotation.clone(),
                otherwise_rule,
            },
            CategorySource::Unclassified => wire::ExplainCategorySource::Unclassified,
        };
        let privacy = row
            .and_then(|row| row.privacy())
            .map(|privacy| wire::ExplainPrivacy {
                rule: privacy.rule.clone(),
                label_key: privacy.label_key.clone(),
                label: privacy.label.clone(),
            });
        let mut nodes = Vec::new();
        for stage in [
            domain::RuleKind::Extraction,
            domain::RuleKind::Classification,
        ] {
            self.stage_nodes(segment, facts, stage, &mut nodes)?;
        }
        nodes.sort_by(|left, right| left.path.cmp(&right.path));
        let extraction = facts.extraction(segment);
        Ok(wire::ExplainFragment {
            start_utc_ms: segment.span.start_utc_ms,
            end_utc_ms: segment.span.end_utc_ms,
            clock_segment_id: segment
                .clock_segment_id
                .map(|id| wire::ClockSegmentId(id.0)),
            category: facts.category(segment).clone(),
            category_source,
            extraction: extraction.extractor_rule.is_some().then_some(extraction),
            privacy,
            nodes,
        })
    }
    fn valid_parent_chain(&self, key: &str) -> bool {
        let mut seen = BTreeSet::new();
        let mut current = Some(key);
        while let Some(key) = current {
            if !seen.insert(key) {
                return false;
            }
            let Ok(key) = key.parse() else {
                return false;
            };
            let Some(set) = self.rules.state().sets.get(&key) else {
                return false;
            };
            current = set.current.set.header.parent.as_deref();
        }
        true
    }
    fn stage_nodes(
        &self,
        segment: &DerivedSegment,
        facts: &SegmentFacts,
        stage: domain::RuleKind,
        nodes: &mut Vec<wire::ExplainNode>,
    ) -> Result<(), RunQueryError> {
        let resolution = self
            .rules
            .resolution(stage)
            .ok_or(RunQueryError::InvalidRuleEntry)?;
        let compiled = match stage {
            domain::RuleKind::Extraction => self.rules.extraction(),
            domain::RuleKind::Classification => self.rules.classification(),
            _ => return Err(RunQueryError::InvalidRuleEntry),
        };
        let fields = facts.explain_fields(segment);
        let evaluation = compiled.evaluate(&fields);
        // CompiledRules::evaluate appends exactly one outcome for each effective rule, in order.
        if evaluation.rules.len() != resolution.effective.rules.len() {
            return Err(RunQueryError::AdvanceQueryError);
        }
        let available = facts
            .explain_foreground(segment)
            .is_some_and(|row| match stage {
                domain::RuleKind::Classification => row.is_retained_window(),
                domain::RuleKind::Extraction => Classifier::accepts_extraction(row),
                _ => unreachable!("explain stages are classification and extraction"),
            });
        let mut paths: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut child_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut root_index = nodes.iter().filter(|node| node.path.len() == 1).count();
        for set_node in &resolution.nodes {
            self.control.check()?;
            let key = set_node
                .key
                .parse()
                .map_err(|_| RunQueryError::InvalidRuleEntry)?;
            let installed = &self
                .rules
                .state()
                .sets
                .get(&key)
                .ok_or(RunQueryError::InvalidRuleEntry)?
                .current;
            let parent_path = installed
                .set
                .header
                .parent
                .as_ref()
                .filter(|_| self.valid_parent_chain(&set_node.key))
                .and_then(|parent| paths.get(parent));
            let path = if let Some(parent_path) = parent_path {
                let parent = installed
                    .set
                    .header
                    .parent
                    .as_ref()
                    .expect("parent path has a parent");
                let count = child_counts.entry(parent.clone()).or_insert_with(|| {
                    resolution
                        .entries
                        .iter()
                        .filter(|entry| {
                            &entry.key == parent
                                && !matches!(entry.position, domain::EntryPosition::Disable(_))
                        })
                        .count()
                });
                let mut path = parent_path.clone();
                path.push(*count);
                *count += 1;
                path
            } else {
                let path = vec![root_index];
                root_index += 1;
                path
            };
            paths.insert(set_node.key.clone(), path.clone());
            let enabled = set_node.enabled == domain::Enablement::Enabled;
            let mut set = Self::node(
                path.clone(),
                wire::ExplainNodeKind::Set,
                None,
                stage,
                if enabled {
                    wire::ExplainState::Effective
                } else {
                    wire::ExplainState::Disabled
                },
            );
            set.set = Some(wire::ExplainSet {
                key: set_node.key.clone(),
                version: installed.version,
            });
            if !enabled {
                set.reason = Some(wire::ExplainReason::SetDisabled);
            }
            nodes.push(set);
            for (entry_index, entry) in resolution
                .entries
                .iter()
                .filter(|entry| {
                    entry.key == set_node.key
                        && !matches!(entry.position, domain::EntryPosition::Disable(_))
                })
                .enumerate()
            {
                self.control.check()?;
                let content = &installed.set.content;
                let rule = ExplainRules::rule(content, entry.position)
                    .ok_or(RunQueryError::InvalidRuleEntry)?;
                let file_order = ExplainRules::file_order(content, entry.position)?;
                let outcome = match entry.effective_order {
                    Some(order) => Some(
                        evaluation
                            .rules
                            .get(order)
                            .ok_or(RunQueryError::AdvanceQueryError)?
                            .clone(),
                    ),
                    None => {
                        let compiled = self
                            .prepared
                            .entries
                            .get(&EntryKey {
                                stage,
                                set: entry.key.clone(),
                                file_order,
                            })
                            .ok_or(RunQueryError::AdvanceQueryError)?;
                        match compiled {
                            Some(compiled) => Some(
                                compiled
                                    .evaluate(&fields)
                                    .rules
                                    .into_iter()
                                    .next()
                                    .ok_or(RunQueryError::AdvanceQueryError)?,
                            ),
                            None => None,
                        }
                    }
                };
                let mut rule_path = path.clone();
                rule_path.push(entry_index);
                let mut node = Self::node(
                    rule_path.clone(),
                    wire::ExplainNodeKind::Rule,
                    Some(format!("{}/{}@{}", entry.key, rule.id, installed.version)),
                    stage,
                    wire::ExplainState::Effective,
                );
                node.provenance = entry
                    .provenance
                    .iter()
                    .map(|source| wire::RuleProvenance {
                        key: source.key.clone(),
                        version: source.version,
                        id: source.id.clone(),
                    })
                    .collect();
                node.pattern_matched = outcome.as_ref().and_then(|outcome| outcome.pattern_matched);
                node.reason = Some(match outcome.as_ref().map(|outcome| &outcome.status) {
                    None => wire::ExplainReason::SetDisabled,
                    Some(domain::RuleStatus::DidNotMatch) => wire::ExplainReason::DidNotMatch,
                    Some(domain::RuleStatus::Selected | domain::RuleStatus::Merged) => {
                        wire::ExplainReason::Selected
                    }
                    Some(domain::RuleStatus::Shadowed { by_rule, reason }) => {
                        node.state = wire::ExplainState::Shadowed;
                        wire::ExplainReason::Shadowed {
                            by_rule: by_rule.clone(),
                            order: match reason {
                                domain::ShadowReason::Priority => {
                                    wire::ExplainShadowOrder::Priority
                                }
                                domain::ShadowReason::Depth => wire::ExplainShadowOrder::Depth,
                                domain::ShadowReason::Order => wire::ExplainShadowOrder::Order,
                            },
                        }
                    }
                });
                match entry.state {
                    domain::ResolvedEntryState::Disabled => {
                        node.state = wire::ExplainState::Disabled;
                        node.reason = Some(wire::ExplainReason::SetDisabled);
                    }
                    domain::ResolvedEntryState::Overridden => {
                        node.state = wire::ExplainState::Overridden
                    }
                    domain::ResolvedEntryState::TargetMissing => {
                        node.state = wire::ExplainState::Disabled;
                        node.reason = Some(wire::ExplainReason::TargetMissing);
                    }
                    domain::ResolvedEntryState::Effective => {}
                }
                if matches!(
                    entry.state,
                    domain::ResolvedEntryState::Disabled | domain::ResolvedEntryState::Overridden
                ) {
                    let target = match entry.position {
                        domain::EntryPosition::Rule(_) => format!("{}/{}", entry.key, rule.id),
                        domain::EntryPosition::Override(index) => {
                            content.r#override[index].target.clone()
                        }
                        domain::EntryPosition::Disable(_) => {
                            unreachable!("disable directives have no rule node")
                        }
                    };
                    if let Some(suppressed) =
                        resolution.effective.suppressed.iter().find(|old| {
                            let supplied = &old.rule;
                            supplied.full_id == target
                                && supplied.file_order == file_order
                                && supplied.provenance.overrides.last().map_or(
                                    supplied.provenance.defining_set.as_str(),
                                    |replacement| replacement.set.as_str(),
                                ) == entry.key
                        })
                    {
                        node.reason = Some(match &suppressed.reason {
                            domain::SuppressionReason::Disabled { by_set } => {
                                wire::ExplainReason::Disabled {
                                    by_set: by_set.clone(),
                                }
                            }
                            domain::SuppressionReason::Overridden { by_set } => {
                                wire::ExplainReason::Overridden {
                                    by_set: by_set.clone(),
                                }
                            }
                        });
                    }
                }
                if node.state == wire::ExplainState::Effective
                    && outcome.as_ref().is_some_and(|outcome| {
                        matches!(outcome.status, domain::RuleStatus::Selected)
                    })
                    && let domain::RuleBody::Classification { category, .. } = &rule.body
                {
                    if !facts.accepts_category(&category.uuid) {
                        node.state = wire::ExplainState::Disabled;
                        node.reason = Some(wire::ExplainReason::CategoryUnavailable);
                    } else if let CategorySource::Annotation(annotation) = &segment.category_source
                    {
                        node.state = wire::ExplainState::Shadowed;
                        node.reason = Some(wire::ExplainReason::Annotation {
                            annotation: annotation.clone(),
                        });
                    }
                }
                if !available {
                    node.state = wire::ExplainState::Disabled;
                    node.reason = Some(wire::ExplainReason::StageUnavailable);
                }
                let state = node.state;
                nodes.push(node);
                let conditions = ExplainRules::conditions(rule);
                for (index, condition) in conditions.iter().enumerate() {
                    self.control.check()?;
                    let mut path = rule_path.clone();
                    path.push(index);
                    let mut node =
                        Self::node(path, wire::ExplainNodeKind::Condition, None, stage, state);
                    node.condition_index = Some(index);
                    node.field = Some(Self::field(condition.field));
                    node.value = fields.retained_value(condition.field);
                    node.missing = Some(node.value.is_none());
                    node.matched = outcome
                        .as_ref()
                        .and_then(|outcome| outcome.conditions.get(index))
                        .map(|condition| condition.matched);
                    nodes.push(node);
                }
                if stage == domain::RuleKind::Extraction {
                    let mut path = rule_path;
                    path.push(conditions.len());
                    let mut node =
                        Self::node(path, wire::ExplainNodeKind::Condition, None, stage, state);
                    node.field = Some(wire::RuleField::RawTitle);
                    node.value = fields.retained_value(domain::Field::RawTitle);
                    node.missing = Some(node.value.is_none());
                    node.matched = outcome.as_ref().and_then(|outcome| outcome.pattern_matched);
                    nodes.push(node);
                }
            }
        }
        Ok(())
    }
    fn field(field: domain::Field) -> wire::RuleField {
        match field {
            domain::Field::ExeName => wire::RuleField::ExeName,
            domain::Field::IdentityKey => wire::RuleField::IdentityKey,
            domain::Field::DisplayName => wire::RuleField::DisplayName,
            domain::Field::WindowClass => wire::RuleField::WindowClass,
            domain::Field::RawTitle => wire::RuleField::RawTitle,
            domain::Field::NormalizedTitle => wire::RuleField::NormalizedTitle,
            domain::Field::Document => wire::RuleField::Document,
            domain::Field::Project => wire::RuleField::Project,
            domain::Field::Profile => wire::RuleField::Profile,
            domain::Field::Domain => wire::RuleField::Domain,
            domain::Field::TabTitle => wire::RuleField::TabTitle,
            domain::Field::BrowserProfile => wire::RuleField::BrowserProfile,
            domain::Field::Category => wire::RuleField::Category,
            domain::Field::Tendency => wire::RuleField::Tendency,
            domain::Field::Activity => wire::RuleField::Activity,
        }
    }
    fn node(
        path: Vec<usize>,
        kind: wire::ExplainNodeKind,
        rule: Option<String>,
        stage: domain::RuleKind,
        state: wire::ExplainState,
    ) -> wire::ExplainNode {
        wire::ExplainNode {
            path,
            set: None,
            kind,
            rule,
            condition_index: None,
            field: None,
            value: None,
            missing: None,
            matched: None,
            state,
            stage: match stage {
                domain::RuleKind::Extraction => wire::RuleKind::Extraction,
                domain::RuleKind::Classification => wire::RuleKind::Classification,
                _ => unreachable!("explain stages are classification and extraction"),
            },
            reason: None,
            provenance: Vec::new(),
            pattern_matched: None,
        }
    }
}
