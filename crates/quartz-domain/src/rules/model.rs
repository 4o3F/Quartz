use super::{ContentHash, SourceLocation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

macro_rules! text_enum {
    ($(#[$meta:meta])* pub enum $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum $name { $($(#[$variant_meta])* #[serde(rename = $text)] $variant),+ }
        impl $name {
            /// Every value in the rule format's fixed vocabulary.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            /// Parses a value from the rule format's fixed vocabulary.
            pub fn parse(value: &str) -> Option<Self> { match value { $($text => Some(Self::$variant),)+ _ => None } }
            /// Returns the spelling used in rule files.
            pub const fn as_str(self) -> &'static str { match self { $(Self::$variant => $text),+ } }
        }
    };
}
text_enum! {
    /// Rule family; M7 families are recognized and rejected by the M1 parser.
    pub enum RuleKind {
        /// Selects the category of the first matching rule.
        Classification => "classification",
        /// Extracts document, project, profile, and modification information from a title.
        Extraction => "extraction",
        /// Combines all matching rules using the strictest retention for each field.
        Privacy => "privacy",
        /// Append-only allowances for a task chain, recognized but unsupported until M7.
        Precedent => "precedent",
        /// Automatic policy checks, recognized but unsupported until M7.
        PolicyCheck => "policy_check",
    }
}
text_enum! {
    /// Fields available in an observation snapshot.
    pub enum Field {
        /// Executable file name, such as `code.exe`.
        ExeName => "app.exe_name",
        /// Stable application identity, such as an executable path or AUMID.
        IdentityKey => "app.identity_key",
        /// Application display name supplied by the operating system or user.
        DisplayName => "app.display_name",
        /// Operating system window class name.
        WindowClass => "window.class",
        /// Observed window title before application-specific extraction.
        RawTitle => "title.raw",
        /// Title produced by extraction or the generic suffix fallback.
        NormalizedTitle => "title.normalized",
        /// Document name obtained from extraction.
        Document => "doc.document",
        /// Project name obtained from extraction.
        Project => "doc.project",
        /// Profile suffix obtained from extraction.
        Profile => "doc.profile",
        /// Validated IDNA ASCII domain of the associated web page.
        Domain => "web.domain",
        /// Title of the associated browser tab.
        TabTitle => "web.tab_title",
        /// User-assigned browser profile label.
        BrowserProfile => "browser.profile_label",
        /// Category reference of an already classified activity.
        Category => "category",
        /// Category tendency: focus, neutral, or distraction.
        Tendency => "category.tendency",
        /// Observed activity state, such as active, idle, or locked.
        Activity => "activity",
    }
}
impl Field {
    pub(super) const fn is_app(self) -> bool {
        matches!(self, Self::ExeName | Self::IdentityKey | Self::DisplayName)
    }
    pub(super) const fn allows(self, kind: RuleKind) -> bool {
        match kind {
            RuleKind::Extraction => self.is_app(),
            RuleKind::Classification => self.is_app() || matches!(self, Self::RawTitle),
            RuleKind::Privacy => {
                self.is_app()
                    || matches!(
                        self,
                        Self::WindowClass
                            | Self::RawTitle
                            | Self::Domain
                            | Self::TabTitle
                            | Self::BrowserProfile
                    )
            }
            RuleKind::Precedent | RuleKind::PolicyCheck => true,
        }
    }
}
text_enum! {
    /// Comparison operation, validated for its field.
    pub enum Operator {
        /// Matches the entire normalized value.
        Equals => "equals",
        /// Matches a contiguous substring of the normalized value.
        Contains => "contains",
        /// Matches a prefix of the normalized value.
        StartsWith => "starts_with",
        /// Matches a suffix of the normalized value.
        EndsWith => "ends_with",
        /// Matches NFC input with Rust regex syntax and flags supplied by the pattern.
        Regex => "regex",
        /// Matches a domain or its subdomains at a complete label boundary.
        DomainSuffix => "domain_suffix",
    }
}
text_enum! {
    /// How a rule combines its conditions.
    pub enum MatchMode {
        /// Requires every condition to match.
        All => "all",
        /// Requires at least one condition to match.
        Any => "any",
    }
}
text_enum! {
    /// Application and title retention, ordered from least to most restrictive.
    pub enum Action {
        /// Retains the field unless another rule requires removal.
        Keep => "keep",
        /// Removes the field before persistence and publication.
        Drop => "drop",
    }
}
text_enum! {
    /// Web retention, ordered from least to most restrictive.
    pub enum WebAction {
        /// Retains both domain and tab title.
        Full => "full",
        /// Retains the domain and removes tab and associated window titles.
        DomainOnly => "domain_only",
        /// Removes all web information.
        None => "none",
    }
}

/// Whether literal comparisons use NFC or compatibility and case folding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseSensitivity {
    /// Compares NFC text while preserving case and compatibility distinctions.
    Sensitive,
    /// Compares compatibility-normalized text using Unicode full case folding.
    Folded,
}
/// Distinguishes embedded data from user imports without trusting their key prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleOrigin {
    /// Trusted data embedded in the executable, permitted to use builtin-only keys.
    Builtin,
    /// User-supplied data subject to reserved-key and metadata restrictions.
    User,
}
/// Stable category UUID resolved from a name or builtin key at import time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryReference {
    /// Durable category identifier stored instead of a display name.
    pub uuid: crate::categories::CategoryUuid,
}
/// One validated field comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    /// Observation field to read.
    pub field: Field,
    /// Comparison operation applied before optional negation.
    pub op: Operator,
    /// Validated comparison value; domain literals use IDNA ASCII and category references use UUIDs.
    pub value: String,
    /// Normalization and case-folding policy for literal text comparisons; regex uses its own flags.
    pub case: CaseSensitivity,
    /// Whether to invert the comparison, including treating an absent field as a match.
    pub negate: bool,
}
/// Privacy retention applied independently per field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyAction {
    /// Retention of application identity; dropping it also drops titles and web information.
    pub app: Action,
    /// Retention of the observed foreground window title.
    pub window_title: Action,
    /// Retention of web domain and tab title information.
    pub web: WebAction,
}
impl Default for PrivacyAction {
    fn default() -> Self {
        Self {
            app: Action::Keep,
            window_title: Action::Keep,
            web: WebAction::Full,
        }
    }
}
impl PrivacyAction {
    /// Takes the most restrictive retention for each field.
    pub fn merge(self, other: Self) -> Self {
        let mut merged = Self {
            app: self.app.max(other.app),
            window_title: self.window_title.max(other.window_title),
            web: self.web.max(other.web),
        };
        if merged.app == Action::Drop {
            merged.window_title = Action::Drop;
            merged.web = WebAction::None;
        }
        if merged.web == WebAction::DomainOnly {
            merged.window_title = Action::Drop;
        }
        merged
    }
}
/// Metadata permitted only in embedded rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinMetadata {
    /// Stable identifier of the builtin detector that owns this entry, when specified.
    pub detector: Option<String>,
    /// Application interface languages covered by the pattern; `*` denotes all languages.
    pub languages: Vec<String>,
    /// Whether samples have verified this entry; absent when verification metadata was omitted.
    pub verified: Option<bool>,
    /// Translation key for the builtin rule label, replacing a user-provided label.
    pub label_key: Option<String>,
}
/// Family-specific rule data; unsupported families cannot enter an effective list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleBody {
    /// Conditions selecting a durable category reference.
    Classification {
        /// Category assigned when this rule is the first match.
        category: CategoryReference,
        /// How the conditions are combined.
        mode: MatchMode,
        /// Conditions restricted to fields available before classification.
        when: Vec<Condition>,
    },
    /// Application predicates and a title regex with named output groups.
    Extraction {
        /// Application-field conditions, all of which must match.
        app: Vec<Condition>,
        /// Regex over the raw NFC title with document, project, profile, or dirty groups.
        pattern: String,
    },
    /// Conditions contributing retention actions to the merged privacy result.
    Privacy {
        /// Per-field retention requested by this rule.
        action: PrivacyAction,
        /// How the conditions are combined.
        mode: MatchMode,
        /// Conditions restricted to fields available before persistence.
        when: Vec<Condition>,
    },
}
/// Complete rule contents, including an inherited identifier for overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Identifier unique within the defining set; overrides inherit the target identifier.
    pub id: String,
    /// User-provided description; builtin entries may provide a translation key instead.
    pub label: Option<String>,
    /// Ranking from 0 to 1000 for classification and extraction, with larger values first.
    pub priority: u16,
    /// Metadata accepted only when parsing with builtin import authority.
    pub builtin: Option<BuiltinMetadata>,
    /// Validated conditions and outputs specific to this rule family.
    pub body: RuleBody,
}
/// A complete replacement of an ancestor's rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Override {
    /// Ancestor rule name in `<set-key>/<rule-id>` form.
    pub target: String,
    /// Complete replacement contents, including the inherited target identifier.
    pub rule: Rule,
}
/// A request to remove an ancestor's rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disable {
    /// Ancestor rule name in `<set-key>/<rule-id>` form.
    pub target: String,
    /// Optional user explanation for disabling the target.
    pub reason: Option<String>,
}
/// Canonical version contents; JSON field order and entry array order are stable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Content {
    /// New definitions in their rule-array order.
    pub rule: Vec<Rule>,
    /// Complete ancestor replacements in their override-array order.
    pub r#override: Vec<Override>,
    /// Ancestor removals in their disable-array order.
    pub disable: Vec<Disable>,
    /// Source order across all three arrays, retained in canonical content and rule ordering.
    pub order: Vec<EntryPosition>,
}
/// Entry identity preserving source order across the three TOML table arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryPosition {
    /// Identifies a new definition in the rule array.
    Rule(
        /// Zero-based index into [`Content::rule`].
        usize,
    ),
    /// Identifies a replacement in the override array.
    Override(
        /// Zero-based index into the override array in [`Content`].
        usize,
    ),
    /// Identifies a removal in the disable array.
    Disable(
        /// Zero-based index into [`Content::disable`].
        usize,
    ),
}
impl Content {
    /// Returns compact deterministic JSON with category UUID references.
    pub fn canonical_json(&self) -> String {
        serde_json::to_string(self).expect("rule content contains only JSON-safe values")
    }
    /// SHA-256 of the exact canonical JSON bytes.
    pub fn content_hash(&self) -> ContentHash {
        ContentHash::of(&self.canonical_json())
    }
}
/// Rule-set identity and tree metadata from the file header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetHeader {
    /// Globally unique stable key; the `builtin.` prefix is reserved.
    pub key: String,
    /// Rule family shared with every parent and descendant.
    pub kind: RuleKind,
    /// Display name supplied by the rule author.
    pub name: String,
    /// Optional parent key defining the ancestor chain.
    pub parent: Option<String>,
    /// Optional description of the set as a whole.
    pub description: Option<String>,
    /// Precedent chain name, recognized for M7 rule sets that M1 rejects.
    pub chain: Option<String>,
}
/// A validated source file with locations retained for tree diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSet {
    /// Validated identity and tree metadata from the set table.
    pub header: SetHeader,
    /// Validated definitions, replacements, removals, and source order.
    pub content: Content,
    /// Source spans indexed by logical paths; the empty path identifies the whole file and `#key` suffixes identify key tokens.
    pub locations: BTreeMap<String, SourceLocation>,
}
impl RuleSet {
    pub(super) fn diagnostic(
        &self,
        code: super::DiagnosticCode,
        path: &str,
        message: impl Into<String>,
    ) -> super::Diagnostic {
        super::Diagnostic {
            code,
            set_key: self.header.key.clone(),
            path: path.into(),
            location: self
                .locations
                .get(path)
                .or_else(|| self.locations.get(""))
                .expect("parsed rule set has a source location")
                .clone(),
            message: message.into(),
        }
    }
}

/// Maximum text length shared by rule values and expression inputs (UTF-16 units).
pub(super) const MAX_TEXT_UTF16: usize = 4096;
/// Per-expression NFA and DFA memory limits specified by data model section 5.5.
pub(super) const REGEX_SIZE_LIMIT: usize = 1024 * 1024;
