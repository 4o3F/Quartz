//! Rule-set text editor view model: create/edit/override templates, key and parent validation,
//! debounced validation generations, 7-day preview gating, stale-version recovery, and dirty-leave
//! confirmation without GPUI dependencies.

use std::{fmt, time::Duration};

use jiff::ToSpan as _;
use quartz_protocol::{self as wire, ErrorKind, LocalDate};
use snafu::Snafu;

use crate::{
    link::{LoadDataError, LoadDataErrorCode},
    model::rules::{MutationActivity, RuleTree, RulesTab},
};

/// Delay before firing `rules_text.validate` after the user stops typing.
pub(crate) const VALIDATE_DEBOUNCE_DELAY: Duration = Duration::from_millis(400);
/// Required prefix for all user-authored rule-set keys (`docs/rules/rule-format.md` §1).
const USER_KEY_PREFIX: &str = "user.";
/// Published format identifier written at the top of generated rule-set templates.
const RULE_FORMAT_VERSION: &str = "quartz-rules/1";
/// Initial editable key prefix shown when creating a brand-new user rule set.
const DEFAULT_NEW_SET_KEY: &str = "user.";
/// Prefix used when proposing a child rule-set key to override or disable a built-in rule.
const DEFAULT_OVERRIDE_KEY_PREFIX: &str = "user.override-";
/// Prefix used when proposing a classification rule-set key from an unclassified entity.
const DEFAULT_CLASSIFY_KEY_PREFIX: &str = "user.classify-";
/// Fallback slug if a rule or entity identifier has no lowercase ASCII characters.
const FALLBACK_OVERRIDE_SLUG: &str = "rule";
/// Fallback slug if an unclassified entity has no lowercase ASCII characters.
const FALLBACK_CLASSIFY_SLUG: &str = "item";
/// Fallback display name in generated `[set]` headers when no slug is available yet.
const DEFAULT_RULE_SET_NAME: &str = "Custom rules";
/// Upper bound on numeric suffix attempts when deduplicating a suggested child key.
const MAX_KEY_DEDUP_ATTEMPTS: u32 = 999;

/// Monotonic generation counter for dropping late export, validation, preview, or save responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct EditorGeneration(u64);

impl EditorGeneration {
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

/// Client-side validation failure for a newly created user rule-set key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub(crate) enum ValidateRuleSetKeyError {
    #[snafu(display("rule-set key cannot be empty"))]
    Empty,
    #[snafu(display("user rule-set key must start with user."))]
    MissingUserPrefix,
    #[snafu(display("user rule-set key must include a non-empty slug after user."))]
    MissingSlug,
    #[snafu(display("rule-set key contains unsupported characters"))]
    InvalidCharacters,
    #[snafu(display("a rule set with this key already exists"))]
    AlreadyExists,
}

/// Validates a new user rule-set key against `quartz-rules/1` naming rules and the loaded tree.
///
/// # Errors
/// Returns [`ValidateRuleSetKeyError`] if the key is empty, lacks the `user.` prefix or slug,
/// contains unsupported characters, or collides with an active key in `tree`.
pub(crate) fn validate_new_set_key(
    raw: &str,
    tree: Option<&RuleTree>,
) -> Result<String, ValidateRuleSetKeyError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ValidateRuleSetKeyError::Empty);
    }
    let Some(slug) = trimmed.strip_prefix(USER_KEY_PREFIX) else {
        return Err(ValidateRuleSetKeyError::MissingUserPrefix);
    };
    if slug.is_empty() {
        return Err(ValidateRuleSetKeyError::MissingSlug);
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte))
    {
        return Err(ValidateRuleSetKeyError::InvalidCharacters);
    }
    if tree.is_some_and(|loaded| loaded.find(trimmed).is_some()) {
        return Err(ValidateRuleSetKeyError::AlreadyExists);
    }
    Ok(trimmed.to_owned())
}

/// One valid parent candidate offered in the "New rule set" parent picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParentOption {
    pub(crate) key: String,
    pub(crate) depth: usize,
    pub(crate) source: wire::RuleSetSource,
}

/// Returns every node in `tree` that belongs to `kind` in preorder traversal.
#[must_use]
pub(crate) fn valid_parents_for_kind(tree: &RuleTree, kind: wire::RuleKind) -> Vec<ParentOption> {
    tree.flatten()
        .into_iter()
        .filter(|row| row.node.kind == kind)
        .map(|row| ParentOption {
            key: row.node.key.clone(),
            depth: row.depth,
            source: row.node.source,
        })
        .collect()
}

/// Chooses the default parent for a new set of `kind`: the first built-in set of that family.
#[must_use]
fn default_parent_for_kind(tree: Option<&RuleTree>, kind: wire::RuleKind) -> Option<String> {
    let loaded = tree?;
    let options = valid_parents_for_kind(loaded, kind);
    options
        .iter()
        .find(|option| option.source == wire::RuleSetSource::Builtin)
        .or_else(|| options.first())
        .map(|option| option.key.clone())
}

/// One `when` condition prefilled into a classification rule template from an unclassified entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrefilledCondition {
    pub(crate) field: &'static str,
    pub(crate) op: &'static str,
    pub(crate) value: String,
}

/// Entity-derived fields used to prefill a new classification rule set from the unclassified queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClassificationRulePrefill {
    pub(crate) slug_hint: String,
    pub(crate) rule_label: String,
    pub(crate) conditions: Vec<PrefilledCondition>,
}

/// Source used to regenerate the template in create mode while [`TemplateSync::Auto`] is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CreateTemplateSource {
    Default,
    OverrideTarget(String),
    PrefilledClassification(ClassificationRulePrefill),
}

/// Suggests a unique `user.*` key for a child set overriding or disabling `rule_id`.
#[must_use]
pub(crate) fn suggested_child_key(rule_id: &str, tree: Option<&RuleTree>) -> String {
    suggested_user_key(
        DEFAULT_OVERRIDE_KEY_PREFIX,
        rule_id,
        FALLBACK_OVERRIDE_SLUG,
        tree,
    )
}

/// Suggests a unique `user.*` key for a new classification set created from an unclassified entity.
#[must_use]
pub(crate) fn suggested_classification_key(slug_hint: &str, tree: Option<&RuleTree>) -> String {
    suggested_user_key(
        DEFAULT_CLASSIFY_KEY_PREFIX,
        slug_hint,
        FALLBACK_CLASSIFY_SLUG,
        tree,
    )
}

fn suggested_user_key(
    prefix: &str,
    hint: &str,
    fallback_slug: &str,
    tree: Option<&RuleTree>,
) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in hint.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() || lower == '_' || lower == '-' {
            slug.push(lower);
            last_was_dash = lower == '-';
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let trimmed_slug = slug.trim_matches('-');
    let base_slug = if trimmed_slug.is_empty() {
        fallback_slug
    } else {
        trimmed_slug
    };
    let candidate = format!("{prefix}{base_slug}");
    if tree.is_none_or(|loaded| loaded.find(&candidate).is_none()) {
        return candidate;
    }
    for suffix in 2..=MAX_KEY_DEDUP_ATTEMPTS {
        let numbered = format!("{candidate}-{suffix}");
        if tree.is_none_or(|loaded| loaded.find(&numbered).is_none()) {
            return numbered;
        }
    }
    candidate
}

/// Builds a `quartz-rules/1` classification template matching an unclassified entity with
/// `category` left empty for the user to pick.
#[must_use]
pub(crate) fn prefilled_classification_template(
    key: &str,
    parent: Option<&str>,
    prefill: &ClassificationRulePrefill,
) -> String {
    let display_name = display_name_for_key(key);
    let mut text = format_set_header(key, wire::RuleKind::Classification, &display_name, parent);
    let escaped_label = toml_basic_string(&prefill.rule_label);
    text.push_str(&format!(
        "\n[[rule]]\n\
         id = \"match-entity\"\n\
         label = {escaped_label}\n\
         # Pick a category (e.g. builtin:development, builtin:learning, builtin:communication, builtin:entertainment, builtin:system, or a custom category name)\n\
         category = \"\"\n\
         priority = 50\n\
         match = \"all\"\n\
         when = [\n"
    ));
    for condition in &prefill.conditions {
        let escaped_field = toml_basic_string(condition.field);
        let escaped_op = toml_basic_string(condition.op);
        let escaped_value = toml_basic_string(&condition.value);
        text.push_str(&format!(
            "  {{ field = {escaped_field}, op = {escaped_op}, value = {escaped_value} }},\n"
        ));
    }
    text.push_str("]\n");
    text
}

/// Builds a minimal valid `quartz-rules/1` template for a new user rule set of `kind`.
#[must_use]
pub(crate) fn template_for_kind(key: &str, kind: wire::RuleKind, parent: Option<&str>) -> String {
    let display_name = display_name_for_key(key);
    let mut text = format_set_header(key, kind, &display_name, parent);
    match kind {
        wire::RuleKind::Classification => {
            text.push_str(
                "\n[[rule]]\n\
                 id = \"example-rule\"\n\
                 label = \"Example classification rule\"\n\
                 category = \"builtin:development\"\n\
                 priority = 50\n\
                 match = \"all\"\n\
                 when = [\n\
                   { field = \"app.exe_name\", op = \"equals\", value = \"example.exe\" },\n\
                 ]\n",
            );
        }
        wire::RuleKind::Extraction => {
            text.push_str(
                "\n[[rule]]\n\
                 id = \"example-extraction\"\n\
                 label = \"Example extraction rule\"\n\
                 priority = 50\n\
                 app = [\n\
                   { field = \"app.exe_name\", op = \"equals\", value = \"example.exe\" },\n\
                 ]\n\
                 pattern = '^(?P<document>.+?) - Example$'\n",
            );
        }
        wire::RuleKind::Privacy => {
            text.push_str(
                "\n[[rule]]\n\
                 id = \"example-privacy\"\n\
                 label = \"Example privacy rule\"\n\
                 match = \"all\"\n\
                 when = [\n\
                   { field = \"app.exe_name\", op = \"equals\", value = \"example.exe\" },\n\
                 ]\n\
                 preset = \"title_hidden\"\n",
            );
        }
    }
    text
}

/// Builds a valid `quartz-rules/1` child-set template that disables a built-in rule and includes a
/// commented `[[override]]` skeleton tailored to `kind`.
#[must_use]
pub(crate) fn override_child_template(
    key: &str,
    kind: wire::RuleKind,
    parent: &str,
    target: &str,
) -> String {
    let display_name = display_name_for_key(key);
    let mut text = format_set_header(key, kind, &display_name, Some(parent));
    let escaped_target = toml_basic_string(target);
    text.push_str(&format!(
        "\n[[disable]]\n\
         target = {escaped_target}\n\
         reason = \"Disabled in user child rule set\"\n\n\
         # To replace the built-in rule instead of disabling it, remove [[disable]] above\n\
         # and uncomment the [[override]] block below:\n\
         # [[override]]\n\
         # target = {escaped_target}\n"
    ));
    match kind {
        wire::RuleKind::Classification => {
            text.push_str(
                "# label = \"Custom override\"\n\
                 # category = \"builtin:development\"\n\
                 # priority = 50\n\
                 # match = \"all\"\n\
                 # when = [\n\
                 #   { field = \"app.exe_name\", op = \"equals\", value = \"example.exe\" },\n\
                 # ]\n",
            );
        }
        wire::RuleKind::Extraction => {
            text.push_str(
                "# label = \"Custom extraction override\"\n\
                 # priority = 50\n\
                 # app = [\n\
                 #   { field = \"app.exe_name\", op = \"equals\", value = \"example.exe\" },\n\
                 # ]\n\
                 # pattern = '^(?P<document>.+?) - Example$'\n",
            );
        }
        wire::RuleKind::Privacy => {
            text.push_str(
                "# label = \"Custom privacy override\"\n\
                 # match = \"all\"\n\
                 # when = [\n\
                 #   { field = \"app.exe_name\", op = \"equals\", value = \"example.exe\" },\n\
                 # ]\n\
                 # preset = \"title_hidden\"\n",
            );
        }
    }
    text
}

/// Computes the half-open `[start_date, end_date)` range covering the last 7 logical days up to
/// and including `today`.
#[must_use]
pub(crate) fn preview_last_seven_days(today: LocalDate) -> Option<(LocalDate, LocalDate)> {
    let span = (wire::PREVIEW_MAX_DAYS - 1).days();
    let start_date = today.checked_sub(span).ok()?;
    let end_date = today.tomorrow().ok()?;
    Some((start_date, end_date))
}

/// Classification of wire validation diagnostic codes defined in `docs/rules/rule-format.md` §4.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagnosticCodeKind {
    Format,
    Key,
    Set,
    Parent,
    Target,
    Conflict,
    Field,
    Op,
    Value,
    Regex,
    Ref,
    Dup,
    Unknown,
}

impl DiagnosticCodeKind {
    /// Parses a server diagnostic code string; unrecognized future codes map to [`Self::Unknown`].
    #[must_use]
    pub(crate) fn parse(code: &str) -> Self {
        match code {
            "E-RULE-FORMAT" => Self::Format,
            "E-RULE-KEY" => Self::Key,
            "E-RULE-SET" => Self::Set,
            "E-RULE-PARENT" => Self::Parent,
            "E-RULE-TARGET" => Self::Target,
            "E-RULE-CONFLICT" => Self::Conflict,
            "E-RULE-FIELD" => Self::Field,
            "E-RULE-OP" => Self::Op,
            "E-RULE-VALUE" => Self::Value,
            "E-RULE-REGEX" => Self::Regex,
            "E-RULE-REF" => Self::Ref,
            "E-RULE-DUP" => Self::Dup,
            _ => Self::Unknown,
        }
    }
}

/// Whether the text buffer has unsaved edits relative to its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirtyState {
    Clean,
    Dirty,
}

/// Whether metadata picker changes in create mode keep regenerating the template text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateSync {
    Auto,
    Manual,
}

/// Whether the editor is editing an existing user rule set or creating a new user rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuleEditorMode {
    EditExisting {
        key: String,
        kind: wire::RuleKind,
        base_version: u32,
    },
    CreateNew {
        key: String,
        kind: wire::RuleKind,
        parent: Option<String>,
        key_error: Option<ValidateRuleSetKeyError>,
        template_sync: TemplateSync,
        template_source: CreateTemplateSource,
    },
}

/// Lifecycle of loading the initial text into the editor.
#[derive(Debug)]
pub(crate) enum EditorLoadState {
    Loading,
    Ready,
    Failed(LoadDataError),
}

/// Current server validation state of the active text buffer.
#[derive(Debug)]
pub(crate) enum ValidationState {
    Idle,
    Debouncing,
    Validating,
    Valid,
    Invalid {
        diagnostics: Vec<wire::RuleDiagnostic>,
        truncated: bool,
    },
    Failed(LoadDataError),
}

/// Lifecycle of the 7-day rule preview panel.
#[derive(Debug)]
pub(crate) enum PreviewState {
    Idle,
    Loading,
    Ready(wire::RulesPreviewResponse),
    Failed(LoadDataError),
}

/// Inline stale-version conflict notice returned when `base_version` is outdated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StaleNotice {
    pub(crate) current_version: u32,
}

/// Destination the user tried to navigate to while leaving the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeaveTarget {
    CloseEditor,
    OpenImport,
    SelectRuleSet(String),
    SwitchTab(RulesTab),
    LeavePage,
}

/// Inline unsaved-change confirmation state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum LeaveConfirmation {
    #[default]
    Idle,
    Pending {
        target: LeaveTarget,
    },
}

/// Immediate outcome of requesting to leave the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeaveDecision {
    Proceed(LeaveTarget),
    NeedsConfirm,
    Busy,
}

/// Outcome of finishing a `rule_sets.edit` save request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SaveOutcome {
    Ignored,
    Saved { key: String },
    Failed,
}

/// Side effect emitted by [`RuleEditor`] transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuleEditorEffect {
    None,
    ExportText {
        key: String,
        generation: EditorGeneration,
    },
    ReloadStale {
        key: String,
        generation: EditorGeneration,
    },
    ScheduleValidate {
        generation: EditorGeneration,
    },
    Validate {
        request: wire::RulesTextValidateRequest,
        generation: EditorGeneration,
    },
    Preview {
        request: wire::RulesPreviewRequest,
        buffer_generation: EditorGeneration,
        preview_generation: EditorGeneration,
    },
    Save {
        request: wire::RuleSetsEditRequest,
        generation: EditorGeneration,
    },
}

/// Active rule-set editor session; redacts rule text in [`fmt::Debug`] so logs never leak rules.
pub(crate) struct RuleEditorSession {
    pub(crate) mode: RuleEditorMode,
    pub(crate) load_state: EditorLoadState,
    text: String,
    saved_text: String,
    pub(crate) dirty: DirtyState,
    pub(crate) validation: ValidationState,
    pub(crate) preview: PreviewState,
    pub(crate) preview_range: Option<(LocalDate, LocalDate)>,
    pub(crate) saving: MutationActivity,
    pub(crate) stale_notice: Option<StaleNotice>,
    pub(crate) save_error: Option<LoadDataError>,
    pub(crate) leave_confirm: LeaveConfirmation,
}

impl fmt::Debug for RuleEditorSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuleEditorSession")
            .field("mode", &self.mode)
            .field("load_state", &self.load_state)
            .field("text", &"[redacted]")
            .field("dirty", &self.dirty)
            .field("validation", &self.validation)
            .field("preview", &self.preview)
            .field("preview_range", &self.preview_range)
            .field("saving", &self.saving)
            .field("stale_notice", &self.stale_notice)
            .field("leave_confirm", &self.leave_confirm)
            .finish_non_exhaustive()
    }
}

impl RuleEditorSession {
    /// Current editable TOML buffer.
    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Current rule-set key targeted by this editing session.
    #[must_use]
    pub(crate) fn key(&self) -> &str {
        match &self.mode {
            RuleEditorMode::EditExisting { key, .. } | RuleEditorMode::CreateNew { key, .. } => {
                key.as_str()
            }
        }
    }

    /// Expected base version sent in validate, preview, and edit requests (`0` for new sets).
    #[must_use]
    pub(crate) const fn base_version(&self) -> u32 {
        match &self.mode {
            RuleEditorMode::EditExisting { base_version, .. } => *base_version,
            RuleEditorMode::CreateNew { .. } => 0,
        }
    }

    /// Whether the session's key is valid for issuing server validate/preview/save calls.
    #[must_use]
    fn has_valid_key(&self) -> bool {
        match &self.mode {
            RuleEditorMode::EditExisting { .. } => true,
            RuleEditorMode::CreateNew { key, key_error, .. } => {
                key_error.is_none() && validate_new_set_key(key, None).is_ok()
            }
        }
    }
}

/// State machine for the rule-set text editor.
#[derive(Debug, Default)]
pub(crate) struct RuleEditor {
    session: Option<RuleEditorSession>,
    load_generation: EditorGeneration,
    buffer_generation: EditorGeneration,
    preview_generation: EditorGeneration,
    save_generation: EditorGeneration,
}

impl RuleEditor {
    /// Creates a closed rule-set editor.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Active editing session, if the editor is open.
    #[must_use]
    pub(crate) const fn session(&self) -> Option<&RuleEditorSession> {
        self.session.as_ref()
    }

    /// Whether the editor is currently open.
    #[must_use]
    pub(crate) const fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// Whether the editor is open with unsaved modifications.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_dirty(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.dirty == DirtyState::Dirty)
    }

    /// Opens an existing user rule set for text editing and emits an export request.
    #[must_use]
    pub(crate) fn open_edit(
        &mut self,
        key: String,
        kind: wire::RuleKind,
        base_version: u32,
    ) -> RuleEditorEffect {
        self.load_generation = self.load_generation.next();
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        self.save_generation = self.save_generation.next();
        let request_key = key.clone();
        self.session = Some(RuleEditorSession {
            mode: RuleEditorMode::EditExisting {
                key,
                kind,
                base_version,
            },
            load_state: EditorLoadState::Loading,
            text: String::new(),
            saved_text: String::new(),
            dirty: DirtyState::Clean,
            validation: ValidationState::Idle,
            preview: PreviewState::Idle,
            preview_range: None,
            saving: MutationActivity::Idle,
            stale_notice: None,
            save_error: None,
            leave_confirm: LeaveConfirmation::Idle,
        });
        RuleEditorEffect::ExportText {
            key: request_key,
            generation: self.load_generation,
        }
    }

    /// Opens the editor for creating a new user rule set prefilled with a minimal template.
    pub(crate) fn open_new(&mut self, kind: wire::RuleKind, tree: Option<&RuleTree>) {
        self.load_generation = self.load_generation.next();
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        self.save_generation = self.save_generation.next();
        let parent = default_parent_for_kind(tree, kind);
        let key = DEFAULT_NEW_SET_KEY.to_owned();
        let template = template_for_kind(&key, kind, parent.as_deref());
        self.session = Some(RuleEditorSession {
            mode: RuleEditorMode::CreateNew {
                key,
                kind,
                parent,
                key_error: None,
                template_sync: TemplateSync::Auto,
                template_source: CreateTemplateSource::Default,
            },
            load_state: EditorLoadState::Ready,
            text: template.clone(),
            saved_text: template,
            dirty: DirtyState::Clean,
            validation: ValidationState::Idle,
            preview: PreviewState::Idle,
            preview_range: None,
            saving: MutationActivity::Idle,
            stale_notice: None,
            save_error: None,
            leave_confirm: LeaveConfirmation::Idle,
        });
    }

    /// Opens the editor to create a new user child set overriding/disabling `target` from a
    /// built-in parent rule set, and triggers immediate validation of the prefilled template.
    #[must_use]
    pub(crate) fn open_override_child(
        &mut self,
        parent: String,
        kind: wire::RuleKind,
        rule_id: &str,
        target: String,
        tree: Option<&RuleTree>,
    ) -> RuleEditorEffect {
        self.load_generation = self.load_generation.next();
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        self.save_generation = self.save_generation.next();
        let key = suggested_child_key(rule_id, tree);
        let key_error = validate_new_set_key(&key, tree).err();
        let template = override_child_template(&key, kind, &parent, &target);
        let request = key_error.is_none().then(|| wire::RulesTextValidateRequest {
            key: key.clone(),
            base_version: 0,
            text: template.clone(),
        });
        let validation = if request.is_some() {
            ValidationState::Validating
        } else {
            ValidationState::Idle
        };
        self.session = Some(RuleEditorSession {
            mode: RuleEditorMode::CreateNew {
                key,
                kind,
                parent: Some(parent),
                key_error,
                template_sync: TemplateSync::Auto,
                template_source: CreateTemplateSource::OverrideTarget(target),
            },
            load_state: EditorLoadState::Ready,
            text: template.clone(),
            saved_text: template,
            dirty: DirtyState::Clean,
            validation,
            preview: PreviewState::Idle,
            preview_range: None,
            saving: MutationActivity::Idle,
            stale_notice: None,
            save_error: None,
            leave_confirm: LeaveConfirmation::Idle,
        });
        match request {
            Some(request) => RuleEditorEffect::Validate {
                request,
                generation: self.buffer_generation,
            },
            None => RuleEditorEffect::None,
        }
    }

    /// Opens the editor in create mode for a classification rule set prefilled with conditions
    /// matching an unclassified entity, leaving `category` empty for the user to choose.
    #[must_use]
    pub(crate) fn open_prefilled_classification(
        &mut self,
        prefill: ClassificationRulePrefill,
        tree: Option<&RuleTree>,
    ) -> RuleEditorEffect {
        self.load_generation = self.load_generation.next();
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        self.save_generation = self.save_generation.next();
        let kind = wire::RuleKind::Classification;
        let parent = default_parent_for_kind(tree, kind);
        let key = suggested_classification_key(&prefill.slug_hint, tree);
        let key_error = validate_new_set_key(&key, tree).err();
        let template = prefilled_classification_template(&key, parent.as_deref(), &prefill);
        let request = key_error.is_none().then(|| wire::RulesTextValidateRequest {
            key: key.clone(),
            base_version: 0,
            text: template.clone(),
        });
        let validation = if request.is_some() {
            ValidationState::Validating
        } else {
            ValidationState::Idle
        };
        self.session = Some(RuleEditorSession {
            mode: RuleEditorMode::CreateNew {
                key,
                kind,
                parent,
                key_error,
                template_sync: TemplateSync::Auto,
                template_source: CreateTemplateSource::PrefilledClassification(prefill),
            },
            load_state: EditorLoadState::Ready,
            text: template.clone(),
            saved_text: template,
            dirty: DirtyState::Clean,
            validation,
            preview: PreviewState::Idle,
            preview_range: None,
            saving: MutationActivity::Idle,
            stale_notice: None,
            save_error: None,
            leave_confirm: LeaveConfirmation::Idle,
        });
        match request {
            Some(request) => RuleEditorEffect::Validate {
                request,
                generation: self.buffer_generation,
            },
            None => RuleEditorEffect::None,
        }
    }

    /// Applies the exported canonical rule text for an existing user set and validates it.
    #[must_use]
    pub(crate) fn apply_export_loaded(
        &mut self,
        generation: EditorGeneration,
        result: Result<wire::RulesTextExportResponse, LoadDataError>,
    ) -> RuleEditorEffect {
        if !self.load_generation.accepts(generation) {
            return RuleEditorEffect::None;
        }
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        match result {
            Ok(response) => {
                session.text = response.text.clone();
                session.saved_text = response.text;
                session.dirty = DirtyState::Clean;
                session.load_state = EditorLoadState::Ready;
                session.stale_notice = None;
                session.save_error = None;
                session.preview = PreviewState::Idle;
                session.preview_range = None;
                self.buffer_generation = self.buffer_generation.next();
                session.validation = ValidationState::Validating;
                RuleEditorEffect::Validate {
                    request: wire::RulesTextValidateRequest {
                        key: session.key().to_owned(),
                        base_version: session.base_version(),
                        text: session.text.clone(),
                    },
                    generation: self.buffer_generation,
                }
            }
            Err(error) => {
                session.load_state = EditorLoadState::Failed(error);
                RuleEditorEffect::None
            }
        }
    }

    /// Retries loading text when opening an existing user set failed.
    #[must_use]
    pub(crate) fn retry_load(&mut self) -> RuleEditorEffect {
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        let RuleEditorMode::EditExisting { key, .. } = &session.mode else {
            return RuleEditorEffect::None;
        };
        let key = key.clone();
        self.load_generation = self.load_generation.next();
        session.load_state = EditorLoadState::Loading;
        RuleEditorEffect::ExportText {
            key,
            generation: self.load_generation,
        }
    }

    /// Updates the new rule-set key in create mode, validates it, refreshes auto-synced templates,
    /// and schedules debounced server validation when valid.
    #[must_use]
    pub(crate) fn set_new_key(
        &mut self,
        raw_key: String,
        tree: Option<&RuleTree>,
    ) -> RuleEditorEffect {
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        if session.saving == MutationActivity::Busy {
            return RuleEditorEffect::None;
        }
        let RuleEditorMode::CreateNew {
            key,
            kind,
            parent,
            key_error,
            template_sync,
            template_source,
        } = &mut session.mode
        else {
            return RuleEditorEffect::None;
        };
        if *key == raw_key {
            return RuleEditorEffect::None;
        }
        *key = raw_key;
        *key_error = validate_new_set_key(key, tree).err();
        if *template_sync == TemplateSync::Auto {
            session.text = rebuild_create_template(key, *kind, parent.as_deref(), template_source);
        }
        session.dirty = DirtyState::Dirty;
        session.stale_notice = None;
        session.save_error = None;
        session.preview = PreviewState::Idle;
        session.preview_range = None;
        session.leave_confirm = LeaveConfirmation::Idle;
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        if key_error.is_none() {
            session.validation = ValidationState::Debouncing;
            RuleEditorEffect::ScheduleValidate {
                generation: self.buffer_generation,
            }
        } else {
            session.validation = ValidationState::Idle;
            RuleEditorEffect::None
        }
    }

    /// Changes the rule family in create mode, updating the parent selection and auto-synced
    /// template to match `kind`.
    #[must_use]
    pub(crate) fn set_new_kind(
        &mut self,
        new_kind: wire::RuleKind,
        tree: Option<&RuleTree>,
    ) -> RuleEditorEffect {
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        if session.saving == MutationActivity::Busy {
            return RuleEditorEffect::None;
        }
        let RuleEditorMode::CreateNew {
            key,
            kind,
            parent,
            key_error,
            template_sync,
            template_source,
        } = &mut session.mode
        else {
            return RuleEditorEffect::None;
        };
        if *kind == new_kind {
            return RuleEditorEffect::None;
        }
        *kind = new_kind;
        *template_source = CreateTemplateSource::Default;
        let parent_still_valid = parent.as_deref().is_some_and(|parent_key| {
            tree.is_some_and(|loaded| {
                loaded
                    .find(parent_key)
                    .is_some_and(|node| node.kind == new_kind)
            })
        });
        if !parent_still_valid {
            *parent = default_parent_for_kind(tree, new_kind);
        }
        if *template_sync == TemplateSync::Auto {
            session.text = template_for_kind(key, *kind, parent.as_deref());
        }
        session.dirty = DirtyState::Dirty;
        session.stale_notice = None;
        session.save_error = None;
        session.preview = PreviewState::Idle;
        session.preview_range = None;
        session.leave_confirm = LeaveConfirmation::Idle;
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        if key_error.is_none() && validate_new_set_key(key, tree).is_ok() {
            session.validation = ValidationState::Debouncing;
            RuleEditorEffect::ScheduleValidate {
                generation: self.buffer_generation,
            }
        } else {
            session.validation = ValidationState::Idle;
            RuleEditorEffect::None
        }
    }

    /// Selects a parent rule set (or `None` for a root set) in create mode, limited to valid
    /// parents of the same rule family.
    #[must_use]
    pub(crate) fn set_new_parent(
        &mut self,
        new_parent: Option<String>,
        tree: Option<&RuleTree>,
    ) -> RuleEditorEffect {
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        if session.saving == MutationActivity::Busy {
            return RuleEditorEffect::None;
        }
        let RuleEditorMode::CreateNew {
            key,
            kind,
            parent,
            key_error,
            template_sync,
            template_source,
        } = &mut session.mode
        else {
            return RuleEditorEffect::None;
        };
        if let Some(candidate) = new_parent.as_deref()
            && !tree.is_some_and(|loaded| {
                loaded
                    .find(candidate)
                    .is_some_and(|node| node.kind == *kind)
            })
        {
            return RuleEditorEffect::None;
        }
        if *parent == new_parent {
            return RuleEditorEffect::None;
        }
        *parent = new_parent;
        if *template_sync == TemplateSync::Auto {
            session.text = rebuild_create_template(key, *kind, parent.as_deref(), template_source);
        }
        session.dirty = DirtyState::Dirty;
        session.stale_notice = None;
        session.save_error = None;
        session.preview = PreviewState::Idle;
        session.preview_range = None;
        session.leave_confirm = LeaveConfirmation::Idle;
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        if key_error.is_none() && validate_new_set_key(key, tree).is_ok() {
            session.validation = ValidationState::Debouncing;
            RuleEditorEffect::ScheduleValidate {
                generation: self.buffer_generation,
            }
        } else {
            session.validation = ValidationState::Idle;
            RuleEditorEffect::None
        }
    }

    /// Updates the editable rule-set text buffer and schedules a debounced validation.
    #[must_use]
    pub(crate) fn set_text(&mut self, text: String) -> RuleEditorEffect {
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        if !matches!(session.load_state, EditorLoadState::Ready)
            || session.saving == MutationActivity::Busy
        {
            return RuleEditorEffect::None;
        }
        if session.text == text {
            return RuleEditorEffect::None;
        }
        session.text = text;
        match &mut session.mode {
            RuleEditorMode::EditExisting { .. } => {
                session.dirty = if session.text == session.saved_text {
                    DirtyState::Clean
                } else {
                    DirtyState::Dirty
                };
            }
            RuleEditorMode::CreateNew { template_sync, .. } => {
                *template_sync = TemplateSync::Manual;
                session.dirty = DirtyState::Dirty;
            }
        }
        session.save_error = None;
        session.preview = PreviewState::Idle;
        session.preview_range = None;
        session.leave_confirm = LeaveConfirmation::Idle;
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        if session.stale_notice.is_some() || !session.has_valid_key() {
            session.validation = ValidationState::Idle;
            return RuleEditorEffect::None;
        }
        session.validation = ValidationState::Debouncing;
        RuleEditorEffect::ScheduleValidate {
            generation: self.buffer_generation,
        }
    }

    /// Fires `rules_text.validate` when the debounce timer for `generation` matures.
    #[must_use]
    pub(crate) fn trigger_validate(&mut self, generation: EditorGeneration) -> RuleEditorEffect {
        if !self.buffer_generation.accepts(generation) {
            return RuleEditorEffect::None;
        }
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        if !matches!(session.load_state, EditorLoadState::Ready)
            || session.stale_notice.is_some()
            || !session.has_valid_key()
        {
            return RuleEditorEffect::None;
        }
        session.validation = ValidationState::Validating;
        RuleEditorEffect::Validate {
            request: wire::RulesTextValidateRequest {
                key: session.key().to_owned(),
                base_version: session.base_version(),
                text: session.text.clone(),
            },
            generation,
        }
    }

    /// Applies the result of `rules_text.validate`, ignoring responses for older buffer generations.
    pub(crate) fn apply_validation_finished(
        &mut self,
        generation: EditorGeneration,
        result: Result<wire::RulesTextValidateResponse, LoadDataError>,
    ) {
        if !self.buffer_generation.accepts(generation) {
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        match result {
            Ok(response) => {
                if response.valid && response.diagnostics.is_empty() {
                    session.validation = ValidationState::Valid;
                } else {
                    session.validation = ValidationState::Invalid {
                        diagnostics: response.diagnostics,
                        truncated: response.truncated,
                    };
                }
            }
            Err(error) => match error.code() {
                LoadDataErrorCode::Protocol(ErrorKind::RuleSetStale {
                    current_version, ..
                }) => {
                    session.stale_notice = Some(StaleNotice {
                        current_version: current_version.0,
                    });
                    session.validation = ValidationState::Failed(error);
                }
                LoadDataErrorCode::Protocol(ErrorKind::RuleValidationFailed {
                    diagnostics,
                    truncated,
                }) => {
                    session.validation = ValidationState::Invalid {
                        diagnostics,
                        truncated,
                    };
                }
                _ => {
                    session.validation = ValidationState::Failed(error);
                }
            },
        }
    }

    /// Whether the 7-day preview action is currently enabled (only after a clean validate).
    #[must_use]
    pub(crate) fn can_preview(&self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        matches!(session.load_state, EditorLoadState::Ready)
            && matches!(session.validation, ValidationState::Valid)
            && session.stale_notice.is_none()
            && session.saving == MutationActivity::Idle
            && !matches!(session.preview, PreviewState::Loading)
            && session.has_valid_key()
    }

    /// Requests a 7-day `rules.preview` for the current validated draft.
    #[must_use]
    pub(crate) fn request_preview(&mut self, today: LocalDate) -> RuleEditorEffect {
        if !self.can_preview() {
            return RuleEditorEffect::None;
        }
        let Some((start_date, end_date)) = preview_last_seven_days(today) else {
            return RuleEditorEffect::None;
        };
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        self.preview_generation = self.preview_generation.next();
        session.preview = PreviewState::Loading;
        session.preview_range = Some((start_date, end_date));
        let edit = wire::RuleSetsEditRequest {
            key: session.key().to_owned(),
            base_version: session.base_version(),
            text: session.text.clone(),
        };
        RuleEditorEffect::Preview {
            request: wire::RulesPreviewRequest {
                mutation: wire::RulePreviewMutation::Edit(edit),
                start_date,
                end_date,
            },
            buffer_generation: self.buffer_generation,
            preview_generation: self.preview_generation,
        }
    }

    /// Applies the result of `rules.preview`, dropping responses from older buffers or requests.
    pub(crate) fn apply_preview_finished(
        &mut self,
        buffer_generation: EditorGeneration,
        preview_generation: EditorGeneration,
        result: Result<wire::RulesPreviewResponse, LoadDataError>,
    ) {
        if !self.buffer_generation.accepts(buffer_generation)
            || !self.preview_generation.accepts(preview_generation)
        {
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        match result {
            Ok(response) => {
                session.preview = PreviewState::Ready(response);
            }
            Err(error) => {
                if let LoadDataErrorCode::Protocol(ErrorKind::RuleSetStale {
                    current_version,
                    ..
                }) = error.code()
                {
                    session.stale_notice = Some(StaleNotice {
                        current_version: current_version.0,
                    });
                }
                session.preview = PreviewState::Failed(error);
            }
        }
    }

    /// Whether the save button should be enabled.
    #[must_use]
    pub(crate) fn can_save(&self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        matches!(session.load_state, EditorLoadState::Ready)
            && session.saving == MutationActivity::Idle
            && session.stale_notice.is_none()
            && !matches!(session.validation, ValidationState::Invalid { .. })
            && session.has_valid_key()
    }

    /// Validates the key (in create mode) and emits `rule_sets.edit`.
    #[must_use]
    pub(crate) fn submit_save(&mut self, tree: Option<&RuleTree>) -> RuleEditorEffect {
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        if !matches!(session.load_state, EditorLoadState::Ready)
            || session.saving == MutationActivity::Busy
            || session.stale_notice.is_some()
            || matches!(session.validation, ValidationState::Invalid { .. })
        {
            return RuleEditorEffect::None;
        }
        if let RuleEditorMode::CreateNew { key, key_error, .. } = &mut session.mode {
            match validate_new_set_key(key, tree) {
                Ok(validated) => {
                    *key = validated;
                    *key_error = None;
                }
                Err(error) => {
                    *key_error = Some(error);
                    return RuleEditorEffect::None;
                }
            }
        }
        self.save_generation = self.save_generation.next();
        session.saving = MutationActivity::Busy;
        session.save_error = None;
        session.leave_confirm = LeaveConfirmation::Idle;
        RuleEditorEffect::Save {
            request: wire::RuleSetsEditRequest {
                key: session.key().to_owned(),
                base_version: session.base_version(),
                text: session.text.clone(),
            },
            generation: self.save_generation,
        }
    }

    /// Applies the outcome of `rule_sets.edit`. Success closes the editor and returns the saved
    /// key so the page can reload the tree and select it; stale maps to an inline reload notice.
    #[must_use]
    pub(crate) fn apply_save_finished(
        &mut self,
        generation: EditorGeneration,
        result: Result<wire::RuleSetsWriteResponse, LoadDataError>,
    ) -> SaveOutcome {
        if !self.save_generation.accepts(generation) {
            return SaveOutcome::Ignored;
        }
        let Some(session) = &mut self.session else {
            return SaveOutcome::Ignored;
        };
        session.saving = MutationActivity::Idle;
        match result {
            Ok(_) => {
                let saved_key = session.key().to_owned();
                self.session = None;
                SaveOutcome::Saved { key: saved_key }
            }
            Err(error) => {
                match error.code() {
                    LoadDataErrorCode::Protocol(ErrorKind::RuleSetStale {
                        current_version,
                        ..
                    }) => {
                        session.stale_notice = Some(StaleNotice {
                            current_version: current_version.0,
                        });
                        session.save_error = None;
                    }
                    LoadDataErrorCode::Protocol(ErrorKind::RuleValidationFailed {
                        diagnostics,
                        truncated,
                    }) => {
                        session.validation = ValidationState::Invalid {
                            diagnostics,
                            truncated,
                        };
                        session.save_error = None;
                    }
                    _ => {
                        session.save_error = Some(error);
                    }
                }
                SaveOutcome::Failed
            }
        }
    }

    /// Reloads the latest version and canonical text after a stale conflict notice.
    #[must_use]
    pub(crate) fn reload_after_stale(&mut self) -> RuleEditorEffect {
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        if session.saving == MutationActivity::Busy {
            return RuleEditorEffect::None;
        }
        match &session.mode {
            RuleEditorMode::EditExisting { key, .. } => {
                let key = key.clone();
                self.load_generation = self.load_generation.next();
                self.buffer_generation = self.buffer_generation.next();
                self.preview_generation = self.preview_generation.next();
                session.load_state = EditorLoadState::Loading;
                session.stale_notice = None;
                session.save_error = None;
                session.validation = ValidationState::Idle;
                session.preview = PreviewState::Idle;
                session.preview_range = None;
                session.leave_confirm = LeaveConfirmation::Idle;
                RuleEditorEffect::ReloadStale {
                    key,
                    generation: self.load_generation,
                }
            }
            RuleEditorMode::CreateNew { .. } => {
                session.stale_notice = None;
                session.save_error = None;
                RuleEditorEffect::None
            }
        }
    }

    /// Applies a consistent `(version, exported_text)` pair after clicking "Reload" on a stale
    /// conflict notice.
    #[must_use]
    pub(crate) fn apply_stale_reloaded(
        &mut self,
        generation: EditorGeneration,
        result: Result<(u32, wire::RulesTextExportResponse), LoadDataError>,
    ) -> RuleEditorEffect {
        if !self.load_generation.accepts(generation) {
            return RuleEditorEffect::None;
        }
        let Some(session) = &mut self.session else {
            return RuleEditorEffect::None;
        };
        match result {
            Ok((version, response)) => {
                if let RuleEditorMode::EditExisting { base_version, .. } = &mut session.mode {
                    *base_version = version;
                }
                session.text = response.text.clone();
                session.saved_text = response.text;
                session.dirty = DirtyState::Clean;
                session.load_state = EditorLoadState::Ready;
                session.stale_notice = None;
                session.save_error = None;
                session.preview = PreviewState::Idle;
                session.preview_range = None;
                session.leave_confirm = LeaveConfirmation::Idle;
                self.buffer_generation = self.buffer_generation.next();
                session.validation = ValidationState::Validating;
                RuleEditorEffect::Validate {
                    request: wire::RulesTextValidateRequest {
                        key: session.key().to_owned(),
                        base_version: session.base_version(),
                        text: session.text.clone(),
                    },
                    generation: self.buffer_generation,
                }
            }
            Err(error) => {
                session.load_state = EditorLoadState::Failed(error);
                RuleEditorEffect::None
            }
        }
    }

    /// Requests leaving the editor; clean buffers close immediately, dirty buffers ask for inline
    /// confirmation first.
    #[must_use]
    pub(crate) fn request_leave(&mut self, target: LeaveTarget) -> LeaveDecision {
        let Some(session) = &mut self.session else {
            return LeaveDecision::Proceed(target);
        };
        if session.saving == MutationActivity::Busy {
            return LeaveDecision::Busy;
        }
        match session.dirty {
            DirtyState::Clean => {
                self.session = None;
                LeaveDecision::Proceed(target)
            }
            DirtyState::Dirty => {
                session.leave_confirm = LeaveConfirmation::Pending { target };
                LeaveDecision::NeedsConfirm
            }
        }
    }

    /// Dismisses a pending unsaved-change confirmation and stays in the editor.
    pub(crate) fn cancel_leave(&mut self) {
        if let Some(session) = &mut self.session {
            session.leave_confirm = LeaveConfirmation::Idle;
        }
    }

    /// Confirms discarding unsaved changes, closes the editor, and returns the deferred target.
    #[must_use]
    pub(crate) fn confirm_leave(&mut self) -> Option<LeaveTarget> {
        let target = {
            let session = self.session.as_mut()?;
            if session.saving == MutationActivity::Busy {
                return None;
            }
            match std::mem::take(&mut session.leave_confirm) {
                LeaveConfirmation::Pending { target } => target,
                LeaveConfirmation::Idle => return None,
            }
        };
        self.session = None;
        Some(target)
    }
}

fn rebuild_create_template(
    key: &str,
    kind: wire::RuleKind,
    parent: Option<&str>,
    source: &CreateTemplateSource,
) -> String {
    match source {
        CreateTemplateSource::OverrideTarget(target) => {
            if let Some(parent_key) = parent {
                override_child_template(key, kind, parent_key, target)
            } else {
                template_for_kind(key, kind, parent)
            }
        }
        CreateTemplateSource::PrefilledClassification(prefill)
            if kind == wire::RuleKind::Classification =>
        {
            prefilled_classification_template(key, parent, prefill)
        }
        CreateTemplateSource::Default | CreateTemplateSource::PrefilledClassification(_) => {
            template_for_kind(key, kind, parent)
        }
    }
}

fn format_set_header(key: &str, kind: wire::RuleKind, name: &str, parent: Option<&str>) -> String {
    let mut header = format!(
        "format = {format}\n\n\
         [set]\n\
         key = {key}\n\
         kind = {kind}\n\
         name = {name}\n",
        format = toml_basic_string(RULE_FORMAT_VERSION),
        key = toml_basic_string(key),
        kind = toml_basic_string(rule_kind_wire_str(kind)),
        name = toml_basic_string(name),
    );
    if let Some(parent_key) = parent {
        header.push_str(&format!("parent = {}\n", toml_basic_string(parent_key)));
    }
    header
}

fn display_name_for_key(key: &str) -> String {
    match key.strip_prefix(USER_KEY_PREFIX).map(str::trim) {
        Some(slug) if !slug.is_empty() => slug.to_owned(),
        _ => DEFAULT_RULE_SET_NAME.to_owned(),
    }
}

const fn rule_kind_wire_str(kind: wire::RuleKind) -> &'static str {
    match kind {
        wire::RuleKind::Classification => "classification",
        wire::RuleKind::Extraction => "extraction",
        wire::RuleKind::Privacy => "privacy",
    }
}

fn toml_basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_pipe::CallCoreError;
    use quartz_protocol::{
        QueryVersions, RuleDiagnostic, RuleDiagnosticLocation, RuleKind, RuleSetCounts,
        RuleSetNode, RuleSetSource, RuleSetVersion, RuleSetsTreeResponse, RulesPreviewResponse,
        RulesTextExportResponse, RulesTextValidateResponse,
    };

    use crate::link::RequestCoreError;

    fn sample_tree() -> RuleTree {
        let make_node = |key: &str, parent: Option<&str>, source: RuleSetSource| RuleSetNode {
            key: key.to_owned(),
            parent: parent.map(str::to_owned),
            version: 1,
            enabled: true,
            sibling_order: 0,
            source,
            counts: RuleSetCounts {
                added: 1,
                overridden: 0,
                disabled: 0,
            },
            warnings: Vec::new(),
        };
        let responses = vec![
            RuleSetsTreeResponse {
                kind: RuleKind::Classification,
                effective_version: "c1".to_owned(),
                nodes: vec![
                    make_node("builtin.classification", None, RuleSetSource::Builtin),
                    make_node(
                        "user.local.classification",
                        Some("builtin.classification"),
                        RuleSetSource::User,
                    ),
                ],
                versions: QueryVersions("v1".to_owned()),
                truncated: false,
            },
            RuleSetsTreeResponse {
                kind: RuleKind::Extraction,
                effective_version: "e1".to_owned(),
                nodes: vec![make_node(
                    "builtin.extraction",
                    None,
                    RuleSetSource::Builtin,
                )],
                versions: QueryVersions("v1".to_owned()),
                truncated: false,
            },
            RuleSetsTreeResponse {
                kind: RuleKind::Privacy,
                effective_version: "p1".to_owned(),
                nodes: vec![make_node("builtin.privacy", None, RuleSetSource::Builtin)],
                versions: QueryVersions("v1".to_owned()),
                truncated: false,
            },
        ];
        RuleTree::from_responses(&responses).expect("valid tree")
    }

    fn protocol_error(kind: ErrorKind) -> LoadDataError {
        LoadDataError::from(RequestCoreError::Call {
            source: CallCoreError::Protocol { kind },
        })
    }

    #[test]
    fn templates_for_every_rule_kind_include_set_header_key_kind_and_parent() {
        let cases = [
            (
                RuleKind::Classification,
                "classification",
                Some("builtin.classification"),
                "category = \"builtin:development\"",
            ),
            (
                RuleKind::Extraction,
                "extraction",
                Some("builtin.extraction"),
                "pattern =",
            ),
            (
                RuleKind::Privacy,
                "privacy",
                Some("builtin.privacy"),
                "preset = \"title_hidden\"",
            ),
            (RuleKind::Classification, "classification", None, "[[rule]]"),
        ];
        for (kind, kind_str, parent, body_marker) in cases {
            let text = template_for_kind("user.sample", kind, parent);
            assert!(text.contains("format = \"quartz-rules/1\""));
            assert!(text.contains("[set]"));
            assert!(text.contains("key = \"user.sample\""));
            assert!(text.contains(&format!("kind = \"{kind_str}\"")));
            match parent {
                Some(parent_key) => assert!(text.contains(&format!("parent = \"{parent_key}\""))),
                None => assert!(!text.contains("parent =")),
            }
            assert!(text.contains(body_marker));
        }

        let child = override_child_template(
            "user.override-vscode",
            RuleKind::Classification,
            "builtin.classification",
            "builtin.classification/vscode",
        );
        assert!(child.contains("[set]"));
        assert!(child.contains("key = \"user.override-vscode\""));
        assert!(child.contains("kind = \"classification\""));
        assert!(child.contains("parent = \"builtin.classification\""));
        assert!(child.contains("[[disable]]"));
        assert!(child.contains("target = \"builtin.classification/vscode\""));
        assert!(child.contains("# [[override]]"));
    }

    #[test]
    fn late_validation_result_for_older_buffer_generation_is_ignored() {
        let mut editor = RuleEditor::new();
        let export_effect = editor.open_edit("user.tweaks".to_owned(), RuleKind::Classification, 2);
        let RuleEditorEffect::ExportText {
            generation: load_gen,
            ..
        } = export_effect
        else {
            panic!("opening edit exports text");
        };
        let initial_validate = editor.apply_export_loaded(
            load_gen,
            Ok(RulesTextExportResponse {
                text: "format = \"quartz-rules/1\"\n".to_owned(),
                versions: QueryVersions("v1".to_owned()),
            }),
        );
        assert!(matches!(
            initial_validate,
            RuleEditorEffect::Validate { .. }
        ));

        let RuleEditorEffect::ScheduleValidate { generation: gen1 } =
            editor.set_text("first edit".to_owned())
        else {
            panic!("first edit schedules validation");
        };
        let RuleEditorEffect::ScheduleValidate { generation: gen2 } =
            editor.set_text("second edit".to_owned())
        else {
            panic!("second edit schedules validation");
        };
        assert_ne!(gen1, gen2);
        assert_eq!(
            editor.trigger_validate(gen1),
            RuleEditorEffect::None,
            "stale debounce timer must not validate"
        );
        assert!(matches!(
            editor.trigger_validate(gen2),
            RuleEditorEffect::Validate { generation, .. } if generation == gen2
        ));

        editor.apply_validation_finished(
            gen1,
            Ok(RulesTextValidateResponse {
                valid: true,
                diagnostics: Vec::new(),
                truncated: false,
            }),
        );
        assert!(
            matches!(
                editor.session().expect("open").validation,
                ValidationState::Validating
            ),
            "late validation for older buffer generation must be ignored"
        );

        editor.apply_validation_finished(
            gen2,
            Ok(RulesTextValidateResponse {
                valid: false,
                diagnostics: vec![RuleDiagnostic {
                    code: "E-RULE-FORMAT".to_owned(),
                    path: "format".to_owned(),
                    location: Some(RuleDiagnosticLocation { line: 1, column: 1 }),
                    params: Default::default(),
                }],
                truncated: false,
            }),
        );
        assert!(matches!(
            editor.session().expect("open").validation,
            ValidationState::Invalid {
                ref diagnostics, ..
            } if diagnostics.len() == 1
        ));
    }

    #[test]
    fn preview_is_disabled_while_validation_diagnostics_exist() {
        let mut editor = RuleEditor::new();
        let tree = sample_tree();
        let initial = editor.open_override_child(
            "builtin.classification".to_owned(),
            RuleKind::Classification,
            "vscode",
            "builtin.classification/vscode".to_owned(),
            Some(&tree),
        );
        let RuleEditorEffect::Validate { generation, .. } = initial else {
            panic!("override child validates immediately");
        };
        let today: LocalDate = "2026-11-26".parse().expect("date");
        assert!(!editor.can_preview(), "cannot preview while validating");

        editor.apply_validation_finished(
            generation,
            Ok(RulesTextValidateResponse {
                valid: false,
                diagnostics: vec![RuleDiagnostic {
                    code: "E-RULE-FIELD".to_owned(),
                    path: "rule[0].when[0].field".to_owned(),
                    location: Some(RuleDiagnosticLocation { line: 8, column: 3 }),
                    params: Default::default(),
                }],
                truncated: false,
            }),
        );
        assert!(!editor.can_preview(), "preview disabled with diagnostics");
        assert_eq!(editor.request_preview(today), RuleEditorEffect::None);
        assert!(!editor.can_save(), "save disabled with diagnostics");

        let RuleEditorEffect::ScheduleValidate { generation: gen2 } =
            editor.set_text(template_for_kind(
                "user.override-vscode",
                RuleKind::Classification,
                Some("builtin.classification"),
            ))
        else {
            panic!("edit schedules validation");
        };
        let _ = editor.trigger_validate(gen2);
        editor.apply_validation_finished(
            gen2,
            Ok(RulesTextValidateResponse {
                valid: true,
                diagnostics: Vec::new(),
                truncated: false,
            }),
        );
        assert!(editor.can_preview(), "preview enabled after clean validate");

        let preview_effect = editor.request_preview(today);
        let RuleEditorEffect::Preview {
            request,
            buffer_generation,
            preview_generation,
        } = preview_effect
        else {
            panic!("preview emits RulesPreviewRequest");
        };
        assert!(request.validate_dates().is_ok());
        assert_eq!(request.start_date.to_string(), "2026-11-20");
        assert_eq!(request.end_date.to_string(), "2026-11-27");

        editor.apply_preview_finished(
            buffer_generation,
            preview_generation,
            Ok(RulesPreviewResponse {
                rules: Vec::new(),
                totals: wire::RulePreviewTotals {
                    before: wire::RulePreviewSideTotals {
                        active_ms: 0,
                        categories: Vec::new(),
                    },
                    after: wire::RulePreviewSideTotals {
                        active_ms: 0,
                        categories: Vec::new(),
                    },
                },
                document_change_count: 0,
                further_hidden_count: 0,
                samples: Vec::new(),
                samples_truncated: false,
            }),
        );
        assert!(matches!(
            editor.session().expect("open").preview,
            PreviewState::Ready(_)
        ));
    }

    #[test]
    fn stale_save_error_maps_to_reload_notice() {
        let mut editor = RuleEditor::new();
        let RuleEditorEffect::ExportText {
            generation: load_gen,
            ..
        } = editor.open_edit(
            "user.local.classification".to_owned(),
            RuleKind::Classification,
            3,
        )
        else {
            panic!("open edit");
        };
        let RuleEditorEffect::Validate {
            generation: val_gen,
            ..
        } = editor.apply_export_loaded(
            load_gen,
            Ok(RulesTextExportResponse {
                text: template_for_kind(
                    "user.local.classification",
                    RuleKind::Classification,
                    Some("builtin.classification"),
                ),
                versions: QueryVersions("v1".to_owned()),
            }),
        )
        else {
            panic!("initial validate");
        };
        editor.apply_validation_finished(
            val_gen,
            Ok(RulesTextValidateResponse {
                valid: true,
                diagnostics: Vec::new(),
                truncated: false,
            }),
        );

        let RuleEditorEffect::Save {
            request,
            generation: save_gen,
        } = editor.submit_save(None)
        else {
            panic!("save request");
        };
        assert_eq!(request.base_version, 3);

        let outcome = editor.apply_save_finished(
            save_gen,
            Err(protocol_error(ErrorKind::RuleSetStale {
                key: "user.local.classification".to_owned(),
                current_version: RuleSetVersion(4),
            })),
        );
        assert_eq!(outcome, SaveOutcome::Failed);
        assert!(editor.is_open(), "stale save keeps editor open");
        assert_eq!(
            editor.session().expect("open").stale_notice,
            Some(StaleNotice { current_version: 4 })
        );
        assert!(!editor.can_save(), "save blocked until reloaded");

        let RuleEditorEffect::ReloadStale {
            key,
            generation: reload_gen,
        } = editor.reload_after_stale()
        else {
            panic!("reload emits ReloadStale");
        };
        assert_eq!(key, "user.local.classification");
        let after_reload = editor.apply_stale_reloaded(
            reload_gen,
            Ok((
                4,
                RulesTextExportResponse {
                    text: template_for_kind(
                        "user.local.classification",
                        RuleKind::Classification,
                        Some("builtin.classification"),
                    ),
                    versions: QueryVersions("v2".to_owned()),
                },
            )),
        );
        assert!(matches!(after_reload, RuleEditorEffect::Validate { .. }));
        assert_eq!(editor.session().expect("open").base_version(), 4);
        assert_eq!(editor.session().expect("open").stale_notice, None);
    }

    #[test]
    fn leaving_dirty_editor_requires_inline_confirmation() {
        let mut editor = RuleEditor::new();
        let tree = sample_tree();
        editor.open_new(RuleKind::Classification, Some(&tree));
        assert!(!editor.is_dirty());
        assert_eq!(
            editor.request_leave(LeaveTarget::CloseEditor),
            LeaveDecision::Proceed(LeaveTarget::CloseEditor)
        );
        assert!(!editor.is_open());

        editor.open_new(RuleKind::Classification, Some(&tree));
        let _ = editor.set_text("modified rule text".to_owned());
        assert!(editor.is_dirty());
        assert_eq!(
            editor.request_leave(LeaveTarget::SelectRuleSet("builtin.privacy".to_owned())),
            LeaveDecision::NeedsConfirm
        );
        assert!(editor.is_open());
        assert_eq!(
            editor.session().expect("open").leave_confirm,
            LeaveConfirmation::Pending {
                target: LeaveTarget::SelectRuleSet("builtin.privacy".to_owned())
            }
        );

        editor.cancel_leave();
        assert_eq!(
            editor.session().expect("open").leave_confirm,
            LeaveConfirmation::Idle
        );
        assert!(editor.is_open());

        assert_eq!(
            editor.request_leave(LeaveTarget::SwitchTab(RulesTab::Categories)),
            LeaveDecision::NeedsConfirm
        );
        assert_eq!(
            editor.confirm_leave(),
            Some(LeaveTarget::SwitchTab(RulesTab::Categories))
        );
        assert!(!editor.is_open());
    }

    #[test]
    fn new_rule_set_key_must_start_with_user_prefix_and_be_non_empty() {
        let tree = sample_tree();
        assert_eq!(
            validate_new_set_key("", Some(&tree)),
            Err(ValidateRuleSetKeyError::Empty)
        );
        assert_eq!(
            validate_new_set_key("   ", Some(&tree)),
            Err(ValidateRuleSetKeyError::Empty)
        );
        assert_eq!(
            validate_new_set_key("builtin.custom", Some(&tree)),
            Err(ValidateRuleSetKeyError::MissingUserPrefix)
        );
        assert_eq!(
            validate_new_set_key("custom.rules", Some(&tree)),
            Err(ValidateRuleSetKeyError::MissingUserPrefix)
        );
        assert_eq!(
            validate_new_set_key("user.", Some(&tree)),
            Err(ValidateRuleSetKeyError::MissingSlug)
        );
        assert_eq!(
            validate_new_set_key("user.MyRules", Some(&tree)),
            Err(ValidateRuleSetKeyError::InvalidCharacters)
        );
        assert_eq!(
            validate_new_set_key("user.has space", Some(&tree)),
            Err(ValidateRuleSetKeyError::InvalidCharacters)
        );
        assert_eq!(
            validate_new_set_key("user.local.classification", Some(&tree)),
            Err(ValidateRuleSetKeyError::AlreadyExists)
        );
        assert_eq!(
            validate_new_set_key("user.study-sites_1", Some(&tree)),
            Ok("user.study-sites_1".to_owned())
        );
    }

    #[test]
    fn create_mode_parent_picker_and_save_flow_work_with_base_version_zero() {
        let tree = sample_tree();
        let parents = valid_parents_for_kind(&tree, RuleKind::Classification);
        assert_eq!(parents.len(), 2);
        assert_eq!(parents.first().expect("root parent").depth, 0);
        assert!(parents.iter().all(|p| p.key.contains("classification")));
        assert!(VALIDATE_DEBOUNCE_DELAY > Duration::ZERO);

        let mut editor = RuleEditor::new();
        editor.open_new(RuleKind::Classification, Some(&tree));
        assert_eq!(
            editor.submit_save(Some(&tree)),
            RuleEditorEffect::None,
            "default user. key without slug cannot save"
        );
        let _ = editor.set_new_kind(RuleKind::Privacy, Some(&tree));
        let _ = editor.set_new_parent(Some("builtin.privacy".to_owned()), Some(&tree));
        let _ = editor.set_new_parent(Some("builtin.classification".to_owned()), Some(&tree));
        assert!(matches!(
            editor.session().expect("open").mode,
            RuleEditorMode::CreateNew {
                parent: Some(ref parent_key),
                ..
            } if parent_key == "builtin.privacy"
        ));
        let _ = editor.set_new_key("user.my-privacy".to_owned(), Some(&tree));
        assert!(
            editor
                .session()
                .expect("open")
                .text()
                .contains("key = \"user.my-privacy\"")
        );
        let RuleEditorEffect::Save {
            request,
            generation,
        } = editor.submit_save(Some(&tree))
        else {
            panic!("valid new key emits save with base_version 0");
        };
        assert_eq!(request.key, "user.my-privacy");
        assert_eq!(request.base_version, 0);
        let outcome = editor.apply_save_finished(
            generation,
            Ok(wire::RuleSetsWriteResponse {
                sets: vec![wire::MutatedRuleSet {
                    key: "user.my-privacy".to_owned(),
                    version: 1,
                    deleted: false,
                }],
                versions: QueryVersions("v2".to_owned()),
            }),
        );
        assert_eq!(
            outcome,
            SaveOutcome::Saved {
                key: "user.my-privacy".to_owned()
            }
        );
        assert!(!editor.is_open());
    }

    #[test]
    fn debug_output_redacts_rule_text_and_diagnostic_codes_map_exhaustively() {
        let mut editor = RuleEditor::new();
        editor.open_new(RuleKind::Privacy, None);
        let _ = editor.set_text("SECRET_RULE_MARKER".to_owned());
        let rendered = format!("{editor:?}");
        assert!(!rendered.contains("SECRET_RULE_MARKER"));
        assert!(rendered.contains("[redacted]"));

        assert_eq!(
            DiagnosticCodeKind::parse("E-RULE-FORMAT"),
            DiagnosticCodeKind::Format
        );
        assert_eq!(
            DiagnosticCodeKind::parse("E-RULE-DUP"),
            DiagnosticCodeKind::Dup
        );
        assert_eq!(
            DiagnosticCodeKind::parse("E-RULE-FUTURE"),
            DiagnosticCodeKind::Unknown
        );
    }

    #[test]
    fn retry_load_and_export_failure_recover_cleanly() {
        let mut editor = RuleEditor::new();
        let RuleEditorEffect::ExportText { generation, .. } =
            editor.open_edit("user.tweaks".to_owned(), RuleKind::Classification, 1)
        else {
            panic!("export");
        };
        let _ = editor.apply_export_loaded(generation, Err(LoadDataError::InvalidData));
        assert!(matches!(
            editor.session().expect("open").load_state,
            EditorLoadState::Failed(_)
        ));
        assert!(matches!(
            editor.retry_load(),
            RuleEditorEffect::ExportText { .. }
        ));
    }
}
