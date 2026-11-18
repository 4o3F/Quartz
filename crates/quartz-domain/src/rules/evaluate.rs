use super::model::{MAX_TEXT_UTF16, REGEX_SIZE_LIMIT};
use super::{
    CaseSensitivity, Condition, EffectiveRules, Field, MatchMode, Operator, PrivacyAction, RuleBody,
};
use crate::{
    domain_name::{DomainName, NormalizeDomainError},
    text::{FoldedKey, NfcText},
};
use regex::{Regex, RegexBuilder};
use snafu::{ResultExt, Snafu};
use std::collections::BTreeMap;

/// An observation with typed field names, NFC text, and a validated IDNA domain.
#[derive(Debug, Clone, Default)]
pub struct FieldSnapshot {
    /// Present non-domain fields as NFC text; absent map entries represent unavailable fields.
    pub text: BTreeMap<Field, NfcText>,
    /// Validated IDNA domain used for `web.domain`, independently of the text map.
    pub domain: Option<DomainName>,
}
impl FieldSnapshot {
    fn value(&self, field: Field) -> Option<&str> {
        if field == Field::Domain {
            self.domain.as_ref().map(DomainName::as_ascii)
        } else {
            self.text.get(&field).map(NfcText::as_str)
        }
    }
}
/// Compilation failure retains the original regex or domain validation cause.
#[derive(Debug, Snafu)]
pub enum CompileRulesError {
    /// A pattern is invalid or exceeds its bounded compilation resources.
    #[snafu(display("could not compile rule expression"))]
    Regex {
        /// Original regex parser or compilation failure.
        source: regex::Error,
    },
    /// A domain condition contains a value that fails IDNA validation.
    #[snafu(display("could not normalize rule domain"))]
    Domain {
        /// Original domain normalization failure.
        source: NormalizeDomainError,
    },
}
#[derive(Debug, Clone)]
struct CompiledCondition {
    condition: Condition,
    nfc: NfcText,
    folded: FoldedKey,
    regex: Option<Regex>,
    domain: Option<DomainName>,
}
impl CompiledCondition {
    fn compile(condition: &Condition) -> Result<Self, CompileRulesError> {
        Ok(Self {
            condition: condition.clone(),
            nfc: NfcText::new(&condition.value),
            folded: FoldedKey::new(&condition.value),
            regex: if condition.op == Operator::Regex {
                Some(compile_regex(&condition.value)?)
            } else {
                None
            },
            domain: if condition.op == Operator::DomainSuffix {
                Some(DomainName::parse(&condition.value).context(DomainSnafu)?)
            } else {
                None
            },
        })
    }
    fn evaluate(&self, snapshot: &FieldSnapshot) -> ConditionOutcome {
        let value = snapshot.value(self.condition.field);
        let matched = value.is_some_and(|value| {
            let input = bounded(value);
            if let Some(regex) = &self.regex {
                return regex.is_match(NfcText::new(&input).as_str());
            }
            if let Some(domain) = &self.domain {
                return snapshot
                    .domain
                    .as_ref()
                    .is_some_and(|input| input.matches_suffix(domain));
            }
            let (input, pattern) = match self.condition.case {
                CaseSensitivity::Sensitive => {
                    (NfcText::new(&input).as_str().to_owned(), self.nfc.as_str())
                }
                CaseSensitivity::Folded => (
                    FoldedKey::new(&input).as_str().to_owned(),
                    self.folded.as_str(),
                ),
            };
            match self.condition.op {
                Operator::Equals => input == pattern,
                Operator::Contains => input.contains(pattern),
                Operator::StartsWith => input.starts_with(pattern),
                Operator::EndsWith => input.ends_with(pattern),
                Operator::Regex | Operator::DomainSuffix => {
                    unreachable!("compiled operators handled before literal comparison")
                }
            }
        });
        ConditionOutcome {
            field: self.condition.field,
            present: value.is_some(),
            matched: matched != self.condition.negate,
        }
    }
}
fn bounded(value: &str) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= MAX_TEXT_UTF16
        })
        .collect()
}
fn compile_regex(pattern: &str) -> Result<Regex, CompileRulesError> {
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .context(RegexSnafu)
}
/// One condition's evaluation without copying potentially private input values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionOutcome {
    /// Observation field tested by this condition.
    pub field: Field,
    /// Whether the observation supplied the field before evaluating the comparison.
    pub present: bool,
    /// Final condition result after applying optional negation.
    pub matched: bool,
}
/// The ordering component that selected an earlier matching rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowReason {
    /// The selected rule has a larger priority value.
    Priority,
    /// At equal priority, the selected rule comes from a deeper set.
    Depth,
    /// At equal priority and depth, traversal or file order places the selected rule first.
    Order,
}
/// Whether a rule contributed, failed, or was shadowed by an earlier match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleStatus {
    /// The combined conditions or the extraction pattern did not match.
    DidNotMatch,
    /// This rule supplies the classification or extraction result.
    Selected,
    /// This privacy rule matched and its actions participated in the field-wise merge.
    Merged,
    /// This rule matched after an earlier classification or extraction match had won.
    Shadowed {
        /// Explanation ID of the selected matching rule.
        by_rule: String,
        /// Ordering component that placed the selected rule first.
        reason: ShadowReason,
    },
}
/// Complete condition and extraction-pattern explanation for one effective rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOutcome {
    /// Explanation identifier in `<set>/<rule>@<version>` form.
    pub rule_id: String,
    /// Results of every condition in source order, including conditions after a decisive match.
    pub conditions: Vec<ConditionOutcome>,
    /// Extraction regex result, or `None` for rule families without an extraction pattern.
    pub pattern_matched: Option<bool>,
    /// Whether this rule failed, supplied an output, contributed privacy actions, or was shadowed.
    pub status: RuleStatus,
}
/// Named extraction outputs captured from a bounded NFC title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extraction {
    /// Captured document name, or the non-empty title produced by the generic fallback.
    pub document: Option<String>,
    /// Captured project name when the matching pattern supplies one.
    pub project: Option<String>,
    /// Captured browser or application profile suffix when supplied by the pattern.
    pub profile: Option<String>,
    /// Whether the matching pattern captured its `dirty` group.
    pub dirty: bool,
    /// NFC title span covering named outputs, or the generic suffix-stripped title; absent without input.
    pub normalized_title: Option<String>,
    /// Whether named extraction, fallback, empty text, or missing input produced this result.
    pub status: ExtractionStatus,
}
/// Whether an extractor matched, used the generic suffix fallback, or lacked input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionStatus {
    /// An application predicate and extraction pattern matched the title.
    Extracted,
    /// No extraction rule matched, so the generic application-suffix fallback supplied a title.
    Fallback,
    /// The fallback produced an empty title.
    Empty,
    /// No raw title was available for extraction or fallback.
    Unknown,
}
impl Extraction {
    fn fallback(snapshot: &FieldSnapshot) -> Self {
        let title = snapshot
            .value(Field::RawTitle)
            .map(|title| NfcText::new(&bounded(title)).as_str().to_owned());
        let title = title.map(|title| {
            let suffix = snapshot
                .value(Field::DisplayName)
                .map(|name| format!(" - {name}"));
            suffix
                .as_deref()
                .and_then(|suffix| title.strip_suffix(suffix))
                .unwrap_or(&title)
                .to_owned()
        });
        let status = match title.as_deref() {
            None => ExtractionStatus::Unknown,
            Some("") => ExtractionStatus::Empty,
            Some(_) => ExtractionStatus::Fallback,
        };
        Self {
            document: title.as_ref().filter(|title| !title.is_empty()).cloned(),
            project: None,
            profile: None,
            dirty: false,
            normalized_title: title,
            status,
        }
    }
}
/// Explanation IDs supplying the strictest action for each privacy field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivacySources {
    /// Rule supplying the strictest application retention, or `None` when identity is retained.
    pub app: Option<String>,
    /// Rule supplying title removal, or `None` when the title is retained.
    pub window_title: Option<String>,
    /// Rule supplying the strictest web restriction, or `None` for full web retention.
    pub web: Option<String>,
}
/// Evaluation result and ordered explanations; suppressed overlays remain in EffectiveRules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    /// First matching classification or extraction explanation ID; absent for fallback and privacy evaluation.
    pub selected: Option<String>,
    /// Category selected by the first matching classification rule, when any.
    pub category_uuid: Option<String>,
    /// Extraction or fallback output when evaluating the extraction family.
    pub extraction: Option<Extraction>,
    /// Merged privacy actions after applying field dependencies.
    pub privacy: PrivacyAction,
    /// Explanation IDs supplying each restricted privacy field, with user rules winning equal actions.
    pub privacy_sources: PrivacySources,
    /// Per-rule explanations in effective evaluation order.
    pub rules: Vec<RuleOutcome>,
}
/// Compiled expressions and normalized values, reusable across observation snapshots.
#[derive(Debug, Clone)]
pub struct CompiledRules {
    effective: EffectiveRules,
    conditions: Vec<Vec<CompiledCondition>>,
    patterns: Vec<Option<Regex>>,
}
impl EffectiveRules {
    /// Compiles patterns with one MiB NFA and DFA limits and caches normalized values.
    ///
    /// # Errors
    /// Returns the original compilation cause if constructed contents bypassed validation.
    pub fn compile(&self) -> Result<CompiledRules, CompileRulesError> {
        let mut conditions = Vec::new();
        let mut patterns = Vec::new();
        for entry in &self.rules {
            let (compiled, pattern) = entry.rule.compile_expressions()?;
            conditions.push(compiled);
            patterns.push(pattern);
        }
        Ok(CompiledRules {
            effective: self.clone(),
            conditions,
            patterns,
        })
    }
}
impl CompiledRules {
    /// Returns the label snapshot for a revision-qualified privacy explanation.
    pub fn privacy_label(&self, explanation_id: &ExplanationId) -> Option<PrivacyLabelSnapshot> {
        self.effective
            .rules
            .iter()
            .find(|entry| entry.explanation_id() == explanation_id.as_str())
            .map(|entry| PrivacyLabelSnapshot {
                key: entry
                    .rule
                    .builtin
                    .as_ref()
                    .and_then(|metadata| metadata.label_key.clone()),
                text: entry.rule.label.clone(),
            })
    }

    /// Reports whether missing browser context requires withholding foreground titles.
    pub fn requires_web_privacy(&self) -> bool {
        self.effective
            .rules
            .iter()
            .any(|entry| match &entry.rule.body {
                RuleBody::Privacy { action, when, .. } => {
                    (action.app == super::Action::Drop
                        || action.window_title == super::Action::Drop
                        || action.web != super::WebAction::Full)
                        && when.iter().any(|condition| {
                            matches!(
                                condition.field,
                                Field::Domain | Field::TabTitle | Field::BrowserProfile
                            )
                        })
                }
                _ => false,
            })
    }

    /// Evaluates every rule for explanation, choosing the first classification or extraction match.
    pub fn evaluate(&self, snapshot: &FieldSnapshot) -> Evaluation {
        let mut result = Evaluation {
            selected: None,
            category_uuid: None,
            extraction: None,
            privacy: PrivacyAction::default(),
            privacy_sources: PrivacySources::default(),
            rules: Vec::new(),
        };
        let mut winner: Option<usize> = None;
        let mut privacy_matches = Vec::new();
        for (index, entry) in self.effective.rules.iter().enumerate() {
            let conditions: Vec<_> = self.conditions[index]
                .iter()
                .map(|condition| condition.evaluate(snapshot))
                .collect();
            let mode = match entry.rule.body {
                RuleBody::Classification { mode, .. } | RuleBody::Privacy { mode, .. } => mode,
                RuleBody::Extraction { .. } => MatchMode::All,
            };
            let matches = match mode {
                MatchMode::All => conditions.iter().all(|c| c.matched),
                MatchMode::Any => conditions.iter().any(|c| c.matched),
            };
            let captures = self.patterns[index].as_ref().and_then(|pattern| {
                snapshot.value(Field::RawTitle).and_then(|title| {
                    let title = NfcText::new(&bounded(title));
                    pattern.captures(title.as_str()).map(|captures| Extraction {
                        document: captures.name("document").map(|m| m.as_str().into()),
                        project: captures.name("project").map(|m| m.as_str().into()),
                        profile: captures.name("profile").map(|m| m.as_str().into()),
                        dirty: captures.name("dirty").is_some(),
                        normalized_title: {
                            let spans: Vec<_> = ["document", "project", "profile"]
                                .into_iter()
                                .filter_map(|name| captures.name(name))
                                .collect();
                            let start = spans
                                .iter()
                                .map(|capture| capture.start())
                                .min()
                                .unwrap_or(0);
                            let end = spans
                                .iter()
                                .map(|capture| capture.end())
                                .max()
                                .unwrap_or(title.as_str().len());
                            Some(title.as_str()[start..end].to_owned())
                        },
                        status: ExtractionStatus::Extracted,
                    })
                })
            });
            let pattern_matched = self.patterns[index].as_ref().map(|_| captures.is_some());
            let matched = matches && pattern_matched.unwrap_or(true);
            let status = if !matched {
                RuleStatus::DidNotMatch
            } else if let RuleBody::Privacy { action, .. } = entry.rule.body {
                let mut action = action;
                if let RuleBody::Privacy { when, .. } = &entry.rule.body
                    && action.window_title == super::Action::Drop
                    && when.iter().any(|condition| {
                        matches!(
                            condition.field,
                            Field::Domain | Field::TabTitle | Field::BrowserProfile
                        )
                    })
                {
                    action.web = action.web.max(super::WebAction::DomainOnly);
                }
                action = PrivacyAction::default().merge(action);
                privacy_matches.push((index, action));
                result.privacy = result.privacy.merge(action);
                RuleStatus::Merged
            } else if let Some(winner) = winner {
                let selected = &self.effective.rules[winner];
                let reason = if selected.rule.priority != entry.rule.priority {
                    ShadowReason::Priority
                } else if selected.depth != entry.depth {
                    ShadowReason::Depth
                } else {
                    ShadowReason::Order
                };
                RuleStatus::Shadowed {
                    by_rule: selected.explanation_id(),
                    reason,
                }
            } else {
                winner = Some(index);
                result.selected = Some(entry.explanation_id());
                result.extraction = captures;
                if let RuleBody::Classification { category, .. } = &entry.rule.body {
                    result.category_uuid = Some(category.uuid.clone());
                }
                RuleStatus::Selected
            };
            result.rules.push(RuleOutcome {
                rule_id: entry.explanation_id(),
                conditions,
                pattern_matched,
                status,
            });
        }
        if self.effective.kind == super::RuleKind::Extraction && result.extraction.is_none() {
            result.extraction = Some(Extraction::fallback(snapshot));
        }
        result.privacy_sources = PrivacySources {
            app: self.privacy_source(&privacy_matches, |action| action.app, super::Action::Keep),
            window_title: self.privacy_source(
                &privacy_matches,
                |action| action.window_title,
                super::Action::Keep,
            ),
            web: self.privacy_source(
                &privacy_matches,
                |action| action.web,
                super::WebAction::Full,
            ),
        };
        result
    }
    fn privacy_source<T: Ord + Copy>(
        &self,
        matches: &[(usize, PrivacyAction)],
        field: impl Fn(PrivacyAction) -> T,
        baseline: T,
    ) -> Option<String> {
        matches
            .iter()
            .filter(|(_, action)| field(*action) > baseline)
            .max_by_key(|(index, action)| {
                let rule = &self.effective.rules[*index];
                (
                    field(*action),
                    rule.rule.builtin.is_none(),
                    std::cmp::Reverse(rule.preorder),
                    std::cmp::Reverse(rule.file_order),
                )
            })
            .map(|(index, _)| self.effective.rules[*index].explanation_id())
    }
}

impl super::Rule {
    fn compile_expressions(
        &self,
    ) -> Result<(Vec<CompiledCondition>, Option<Regex>), CompileRulesError> {
        let (when, pattern) = match &self.body {
            RuleBody::Classification { when, .. } | RuleBody::Privacy { when, .. } => (when, None),
            RuleBody::Extraction { app, pattern } => (app, Some(compile_regex(pattern)?)),
        };
        Ok((
            when.iter()
                .map(CompiledCondition::compile)
                .collect::<Result<_, _>>()?,
            pattern,
        ))
    }
}
impl super::RuleSet {
    /// Compiles every local definition and override, including currently disabled or shadowed entries.
    ///
    /// # Errors
    /// Returns the original bounded-expression or domain compilation failure.
    pub fn validate_compilation(&self) -> Result<(), CompileRulesError> {
        for rule in self
            .content
            .rule
            .iter()
            .chain(self.content.r#override.iter().map(|entry| &entry.rule))
        {
            rule.compile_expressions()?;
        }
        Ok(())
    }
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CompiledRules>();
};

/// Revision-qualified explanation identity used by privacy metadata lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplanationId(String);
impl ExplanationId {
    /// Borrows its revision-qualified representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Label metadata retained from the exact effective rule revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyLabelSnapshot {
    /// Builtin translation key.
    pub key: Option<String>,
    /// Historical user label.
    pub text: Option<String>,
}

/// A revision-qualified explanation has an invalid target or revision.
#[derive(Debug, Snafu)]
#[snafu(module(explanation_id_error))]
pub enum ParseExplanationIdError {
    /// A revision separator is required after the qualified rule target.
    #[snafu(display("rule explanation is missing its revision"))]
    MissingRevision,
    /// The set key or local rule identifier is invalid.
    #[snafu(display("rule explanation has an invalid target"))]
    Target {
        /// Original target validation error.
        source: crate::rules::ParseRuleTargetError,
    },
    /// The revision must be a positive 32-bit integer.
    #[snafu(display("rule explanation has an invalid revision"))]
    Revision {
        /// Original integer validation error.
        source: std::num::ParseIntError,
    },
}
impl std::str::FromStr for ExplanationId {
    type Err = ParseExplanationIdError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (target, revision) = value
            .rsplit_once('@')
            .ok_or(ParseExplanationIdError::MissingRevision)?;
        target
            .parse::<super::RuleTarget>()
            .map_err(|source| ParseExplanationIdError::Target { source })?;
        revision
            .parse::<std::num::NonZeroU32>()
            .map_err(|source| ParseExplanationIdError::Revision { source })?;
        Ok(Self(value.to_owned()))
    }
}
