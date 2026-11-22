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
struct CompiledExpressions {
    conditions: Vec<CompiledCondition>,
    pattern: Option<Regex>,
}
struct PrivacyMatch {
    index: usize,
    action: PrivacyAction,
}

/// Both fast classification and explanations cache each field's bounded comparison forms.
struct FieldValues<'snapshot> {
    snapshot: &'snapshot FieldSnapshot,
    values: BTreeMap<Field, FieldValue>,
}
struct FieldValue {
    // A character-boundary prefix of NFC text remains NFC; FieldSnapshot already normalizes it.
    nfc: String,
    folded: FoldedKey,
}
impl FieldValue {
    fn new(value: &str) -> Self {
        let nfc = bounded(value);
        let folded = FoldedKey::new(&nfc);
        Self { nfc, folded }
    }
}
impl<'snapshot> FieldValues<'snapshot> {
    fn new(snapshot: &'snapshot FieldSnapshot) -> Self {
        Self {
            snapshot,
            values: BTreeMap::new(),
        }
    }
    fn evaluate(&mut self, condition: &CompiledCondition) -> ConditionOutcome {
        let raw = self.snapshot.value(condition.condition.field);
        let matched = raw.is_some_and(|raw| {
            let value = self
                .values
                .entry(condition.condition.field)
                .or_insert_with(|| FieldValue::new(raw));
            condition.matches_value(value, self.snapshot.domain.as_ref())
        });
        ConditionOutcome {
            field: condition.condition.field,
            present: raw.is_some(),
            matched: matched != condition.condition.negate,
        }
    }
}
#[derive(Debug, Clone, Copy)]
enum LiteralOperation {
    Equals,
    Contains,
    StartsWith,
    EndsWith,
}
#[derive(Debug, Clone)]
enum ConditionMatcher {
    Literal {
        operation: LiteralOperation,
        nfc: NfcText,
        folded: FoldedKey,
    },
    Regex(Regex),
    DomainSuffix(DomainName),
}
#[derive(Debug, Clone)]
struct CompiledCondition {
    condition: Condition,
    matcher: ConditionMatcher,
}
impl CompiledCondition {
    fn matches_value(&self, value: &FieldValue, domain: Option<&DomainName>) -> bool {
        match &self.matcher {
            ConditionMatcher::Regex(regex) => regex.is_match(&value.nfc),
            ConditionMatcher::DomainSuffix(suffix) => {
                domain.is_some_and(|input| input.matches_suffix(suffix))
            }
            ConditionMatcher::Literal {
                operation,
                nfc,
                folded,
            } => {
                let input = match self.condition.case {
                    CaseSensitivity::Sensitive => value.nfc.as_str(),
                    CaseSensitivity::Folded => value.folded.as_str(),
                };
                let pattern = match self.condition.case {
                    CaseSensitivity::Sensitive => nfc.as_str(),
                    CaseSensitivity::Folded => folded.as_str(),
                };
                match operation {
                    LiteralOperation::Equals => input == pattern,
                    LiteralOperation::Contains => input.contains(pattern),
                    LiteralOperation::StartsWith => input.starts_with(pattern),
                    LiteralOperation::EndsWith => input.ends_with(pattern),
                }
            }
        }
    }
    fn compile(condition: &Condition) -> Result<Self, CompileRulesError> {
        let literal = |operation| ConditionMatcher::Literal {
            operation,
            nfc: NfcText::new(&condition.value),
            folded: FoldedKey::new(&condition.value),
        };
        let matcher = match condition.op {
            Operator::Equals => literal(LiteralOperation::Equals),
            Operator::Contains => literal(LiteralOperation::Contains),
            Operator::StartsWith => literal(LiteralOperation::StartsWith),
            Operator::EndsWith => literal(LiteralOperation::EndsWith),
            Operator::Regex => ConditionMatcher::Regex(compile_regex(&condition.value)?),
            Operator::DomainSuffix => ConditionMatcher::DomainSuffix(
                DomainName::parse(&condition.value).context(DomainSnafu)?,
            ),
        };
        Ok(Self {
            condition: condition.clone(),
            matcher,
        })
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
            // Titles without the application's suffix remain unchanged.
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
            let compiled = entry.rule.compile_expressions()?;
            conditions.push(compiled.conditions);
            patterns.push(compiled.pattern);
        }
        Ok(CompiledRules {
            effective: self.clone(),
            conditions,
            patterns,
        })
    }
}
/// First matching classification rule, without allocating an explanation for every rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationMatch {
    /// UUID resolved at import time.
    pub category: crate::categories::CategoryUuid,
    /// Exact defining rule revision.
    pub rule: ExplanationId,
}
impl CompiledRules {
    /// Lists fields read by compiled conditions for deriving cache dependencies.
    pub fn fields(&self) -> impl Iterator<Item = Field> + '_ {
        self.conditions
            .iter()
            .flatten()
            .map(|condition| condition.condition.field)
    }

    /// Evaluates classification in effective priority, depth, and file order.
    pub fn classify(&self, snapshot: &FieldSnapshot) -> Option<ClassificationMatch> {
        let mut fields = FieldValues::new(snapshot);
        for index in 0..self.effective.rules.len() {
            let entry = &self.effective.rules[index];
            let RuleBody::Classification { category, mode, .. } = &entry.rule.body else {
                continue;
            };
            let conditions = &self.conditions[index];
            let matched = match mode {
                MatchMode::All => conditions
                    .iter()
                    .all(|condition| fields.evaluate(condition).matched),
                MatchMode::Any => conditions
                    .iter()
                    .any(|condition| fields.evaluate(condition).matched),
            };
            if matched {
                return Some(ClassificationMatch {
                    category: category.uuid.clone(),
                    rule: ExplanationId(entry.explanation_id().into()),
                });
            }
        }
        None
    }

    /// Borrows effective contents by the revision-qualified explanation identity.
    pub fn rule(&self, explanation_id: &ExplanationId) -> Option<&super::Rule> {
        self.effective
            .rules
            .iter()
            .find(|entry| entry.explanation_id() == explanation_id.as_str())
            .map(|entry| &entry.rule)
    }

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
        let mut fields = FieldValues::new(snapshot);
        for (index, entry) in self.effective.rules.iter().enumerate() {
            let conditions: Vec<_> = self.conditions[index]
                .iter()
                .map(|condition| fields.evaluate(condition))
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
                            // Only fields allowed by privacy policy reach extraction (06 §6).
                            // A match without named captures retains that allowed title in full.
                            if spans.is_empty() {
                                Some(title.as_str().to_owned())
                            } else {
                                let start = spans
                                    .iter()
                                    .map(|capture| capture.start())
                                    .min()
                                    .expect("capture spans are nonempty");
                                let end = spans
                                    .iter()
                                    .map(|capture| capture.end())
                                    .max()
                                    .expect("capture spans are nonempty");
                                Some(title.as_str()[start..end].to_owned())
                            }
                        },
                        status: ExtractionStatus::Extracted,
                    })
                })
            });
            let pattern_matched = self.patterns[index].as_ref().map(|_| captures.is_some());
            // Rules without an extraction pattern are decided by their conditions alone.
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
                privacy_matches.push(PrivacyMatch { index, action });
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
                    result.category_uuid = Some(category.uuid.0.clone());
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
        matches: &[PrivacyMatch],
        field: impl Fn(PrivacyAction) -> T,
        baseline: T,
    ) -> Option<String> {
        matches
            .iter()
            .filter(|matched| field(matched.action) > baseline)
            .max_by_key(|matched| {
                let rule = &self.effective.rules[matched.index];
                (
                    field(matched.action),
                    rule.rule.builtin.is_none(),
                    std::cmp::Reverse(rule.preorder),
                    std::cmp::Reverse(rule.file_order),
                )
            })
            .map(|matched| self.effective.rules[matched.index].explanation_id())
    }
}

impl super::Rule {
    fn compile_expressions(&self) -> Result<CompiledExpressions, CompileRulesError> {
        let when = match &self.body {
            RuleBody::Classification { when, .. } | RuleBody::Privacy { when, .. } => when,
            RuleBody::Extraction { app, .. } => app,
        };
        let pattern = match &self.body {
            RuleBody::Extraction { pattern, .. } => Some(compile_regex(pattern)?),
            RuleBody::Classification { .. } | RuleBody::Privacy { .. } => None,
        };
        Ok(CompiledExpressions {
            conditions: when
                .iter()
                .map(CompiledCondition::compile)
                .collect::<Result<_, _>>()?,
            pattern,
        })
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExplanationId(std::sync::Arc<str>);
impl TryFrom<String> for ExplanationId {
    type Error = ParseExplanationIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<ExplanationId> for String {
    fn from(value: ExplanationId) -> Self {
        value.as_str().to_owned()
    }
}
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
        Ok(Self(value.into()))
    }
}
