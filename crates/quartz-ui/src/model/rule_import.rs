//! Paste-import view model: lightweight `[set].key` extraction from pasted `quartz-rules/1`
//! TOML text, create-vs-replace resolution against the loaded rule-set tree, debounced validation,
//! 7-day `RulePreviewMutation::Import` preview, and optimistic-concurrency `rules_text.import`.

use std::fmt;

use quartz_protocol::{self as wire, ErrorKind, LocalDate};
use snafu::Snafu;

use crate::{
    link::{LoadDataError, LoadDataErrorCode},
    model::{
        LoadProgress, ProgressUpdate, QueryProgress,
        rule_editor::{PreviewState, StaleNotice, ValidationState, preview_last_seven_days},
        rules::{MutationActivity, RuleTree},
    },
};

/// Reserved prefix for built-in rule sets (`docs/rules/rule-format.md` §1).
const BUILTIN_KEY_PREFIX: &str = "builtin.";
/// Section name in `quartz-rules/1` TOML declaring rule-set metadata.
const SET_SECTION_NAME: &str = "set";
/// Field name inside `[set]` declaring the rule-set key.
const SET_KEY_FIELD: &str = "key";

/// Monotonic generation counter for dropping late validation, preview, or import responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct ImportGeneration(u64);

impl ImportGeneration {
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

/// Failure when extracting or validating `[set].key` from pasted rule-set text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub(crate) enum ExtractImportKeyError {
    #[snafu(display("pasted rule text does not declare [set].key"))]
    MissingKey,
    #[snafu(display("pasted [set].key is invalid"))]
    InvalidKey,
    #[snafu(display("built-in rule sets cannot be imported"))]
    ReservedBuiltinPrefix,
}

/// Whether importing pasted text creates a new rule set or replaces an existing active set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportAction {
    Create,
    Replace { base_version: u32 },
}

impl ImportAction {
    /// Optimistic-concurrency `base_version` sent to `rules_text.validate`, `rules.preview`, and
    /// `rules_text.import` (`0` for new or previously deleted sets).
    #[must_use]
    pub(crate) const fn base_version(self) -> u32 {
        match self {
            Self::Create => 0,
            Self::Replace { base_version } => base_version,
        }
    }
}

/// Resolved target key and create-vs-replace decision derived from pasted text and loaded tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportPlan {
    pub(crate) key: String,
    pub(crate) action: ImportAction,
}

/// Resolution state of `[set].key` in the import buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportTarget {
    Empty,
    InvalidKey(ExtractImportKeyError),
    Resolved(ImportPlan),
}

/// Outcome of finishing a `rules_text.import` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportOutcome {
    Ignored,
    Imported { key: String },
    Failed,
}

/// Side effect emitted by [`RuleImport`] transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuleImportEffect {
    None,
    ScheduleValidate {
        generation: ImportGeneration,
    },
    Validate {
        request: wire::RulesTextValidateRequest,
        generation: ImportGeneration,
    },
    Preview {
        request: wire::RulesPreviewRequest,
        buffer_generation: ImportGeneration,
        preview_generation: ImportGeneration,
    },
    Import {
        request: wire::RulesTextImportRequest,
        generation: ImportGeneration,
    },
}

/// Active paste-import session; redacts pasted rule text in [`fmt::Debug`].
pub(crate) struct RuleImportSession {
    text: String,
    pub(crate) target: ImportTarget,
    pub(crate) validation: ValidationState,
    pub(crate) preview: PreviewState,
    pub(crate) preview_range: Option<(LocalDate, LocalDate)>,
    pub(crate) importing: MutationActivity,
    pub(crate) stale_notice: Option<StaleNotice>,
    pub(crate) import_error: Option<LoadDataError>,
}

impl fmt::Debug for RuleImportSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuleImportSession")
            .field("text", &"[redacted]")
            .field("target", &self.target)
            .field("validation", &self.validation)
            .field("preview", &self.preview)
            .field("preview_range", &self.preview_range)
            .field("importing", &self.importing)
            .field("stale_notice", &self.stale_notice)
            .finish_non_exhaustive()
    }
}

impl RuleImportSession {
    /// Current pasted rule-set text buffer.
    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Resolved import plan when `[set].key` is present and valid.
    #[must_use]
    pub(crate) fn plan(&self) -> Option<&ImportPlan> {
        match &self.target {
            ImportTarget::Resolved(plan) => Some(plan),
            ImportTarget::Empty | ImportTarget::InvalidKey(_) => None,
        }
    }
}

/// State machine for the paste-import pane on the Rules page.
#[derive(Debug, Default)]
pub(crate) struct RuleImport {
    session: Option<RuleImportSession>,
    buffer_generation: ImportGeneration,
    preview_generation: ImportGeneration,
    import_generation: ImportGeneration,
}

impl RuleImport {
    /// Creates a closed paste-import state machine.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Active import session, if open.
    #[must_use]
    pub(crate) const fn session(&self) -> Option<&RuleImportSession> {
        self.session.as_ref()
    }

    /// Whether the paste-import pane is open.
    #[must_use]
    pub(crate) const fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// Opens a fresh empty paste-import session.
    pub(crate) fn open(&mut self) {
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        self.import_generation = self.import_generation.next();
        self.session = Some(RuleImportSession {
            text: String::new(),
            target: ImportTarget::Empty,
            validation: ValidationState::Idle,
            preview: PreviewState::Idle,
            preview_range: None,
            importing: MutationActivity::Idle,
            stale_notice: None,
            import_error: None,
        });
    }

    /// Closes the import pane when no import write is currently in flight.
    pub(crate) fn close(&mut self) {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.importing == MutationActivity::Busy)
        {
            return;
        }
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        self.import_generation = self.import_generation.next();
        self.session = None;
    }

    /// Updates the pasted rule-set text, derives create-vs-replace from `[set].key` + `tree`, and
    /// schedules debounced server validation only when the extracted key is valid.
    #[must_use]
    pub(crate) fn set_text(&mut self, text: String, tree: Option<&RuleTree>) -> RuleImportEffect {
        let Some(session) = &mut self.session else {
            return RuleImportEffect::None;
        };
        if session.importing == MutationActivity::Busy {
            return RuleImportEffect::None;
        }
        if session.text == text {
            return RuleImportEffect::None;
        }
        session.text = text;
        session.target = resolve_import_target(&session.text, tree);
        session.stale_notice = None;
        session.import_error = None;
        session.preview = PreviewState::Idle;
        session.preview_range = None;
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        if session.plan().is_some() {
            session.validation = ValidationState::Debouncing;
            RuleImportEffect::ScheduleValidate {
                generation: self.buffer_generation,
            }
        } else {
            session.validation = ValidationState::Idle;
            RuleImportEffect::None
        }
    }

    /// Recomputes create-vs-replace and `base_version` against a newly loaded tree (for example
    /// after reloading from a stale-version conflict) while preserving pasted text.
    #[must_use]
    pub(crate) fn sync_tree(&mut self, tree: Option<&RuleTree>) -> RuleImportEffect {
        let Some(session) = &mut self.session else {
            return RuleImportEffect::None;
        };
        if session.importing == MutationActivity::Busy {
            return RuleImportEffect::None;
        }
        let next_target = resolve_import_target(&session.text, tree);
        let needs_revalidate = session.stale_notice.is_some() || next_target != session.target;
        session.target = next_target;
        if !needs_revalidate {
            return RuleImportEffect::None;
        }
        session.stale_notice = None;
        session.import_error = None;
        session.preview = PreviewState::Idle;
        session.preview_range = None;
        self.buffer_generation = self.buffer_generation.next();
        self.preview_generation = self.preview_generation.next();
        if let Some(request) = session.plan().map(|plan| wire::RulesTextValidateRequest {
            key: plan.key.clone(),
            base_version: plan.action.base_version(),
            text: session.text.clone(),
        }) {
            session.validation = ValidationState::Validating;
            RuleImportEffect::Validate {
                request,
                generation: self.buffer_generation,
            }
        } else {
            session.validation = ValidationState::Idle;
            RuleImportEffect::None
        }
    }

    /// Fires `rules_text.validate` when the debounce timer for `generation` matures.
    #[must_use]
    pub(crate) fn trigger_validate(&mut self, generation: ImportGeneration) -> RuleImportEffect {
        if !self.buffer_generation.accepts(generation) {
            return RuleImportEffect::None;
        }
        let Some(session) = &mut self.session else {
            return RuleImportEffect::None;
        };
        if session.stale_notice.is_some() || session.importing == MutationActivity::Busy {
            return RuleImportEffect::None;
        }
        let Some(plan) = session.plan() else {
            return RuleImportEffect::None;
        };
        let request = wire::RulesTextValidateRequest {
            key: plan.key.clone(),
            base_version: plan.action.base_version(),
            text: session.text.clone(),
        };
        session.validation = ValidationState::Validating;
        RuleImportEffect::Validate {
            request,
            generation,
        }
    }

    /// Applies the result of `rules_text.validate`, dropping responses for older buffer revisions.
    pub(crate) fn apply_validation_finished(
        &mut self,
        generation: ImportGeneration,
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

    /// Whether the 7-day import preview action is enabled (only after clean validation).
    #[must_use]
    pub(crate) fn can_preview(&self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        session.plan().is_some()
            && matches!(session.validation, ValidationState::Valid)
            && session.stale_notice.is_none()
            && session.importing == MutationActivity::Idle
            && !matches!(session.preview, PreviewState::Loading(_))
    }

    /// Requests a 7-day `rules.preview` using `RulePreviewMutation::Import`.
    #[must_use]
    pub(crate) fn request_preview(&mut self, today: LocalDate) -> RuleImportEffect {
        if !self.can_preview() {
            return RuleImportEffect::None;
        }
        let Some((start_date, end_date)) = preview_last_seven_days(today) else {
            return RuleImportEffect::None;
        };
        let Some(session) = &mut self.session else {
            return RuleImportEffect::None;
        };
        let Some(plan) = session.plan().cloned() else {
            return RuleImportEffect::None;
        };
        self.preview_generation = self.preview_generation.next();
        session.preview = PreviewState::Loading(LoadProgress::Waiting);
        session.preview_range = Some((start_date, end_date));
        let edit = wire::RuleSetsEditRequest {
            key: plan.key,
            base_version: plan.action.base_version(),
            text: session.text.clone(),
        };
        RuleImportEffect::Preview {
            request: wire::RulesPreviewRequest {
                mutation: wire::RulePreviewMutation::Import(edit),
                start_date,
                end_date,
            },
            buffer_generation: self.buffer_generation,
            preview_generation: self.preview_generation,
        }
    }

    /// Records per-day progress for an in-flight `rules.preview` request, dropping updates from
    /// older buffers or superseded preview generations.
    pub(crate) fn apply_preview_progress(
        &mut self,
        buffer_generation: ImportGeneration,
        preview_generation: ImportGeneration,
        progress: QueryProgress,
    ) -> ProgressUpdate {
        if !self.buffer_generation.accepts(buffer_generation)
            || !self.preview_generation.accepts(preview_generation)
        {
            return ProgressUpdate::Ignored;
        }
        let Some(session) = &mut self.session else {
            return ProgressUpdate::Ignored;
        };
        let PreviewState::Loading(load_progress) = &mut session.preview else {
            return ProgressUpdate::Ignored;
        };
        load_progress.record(progress);
        ProgressUpdate::Applied
    }

    /// Applies the result of `rules.preview`, dropping responses from older buffers or requests.
    pub(crate) fn apply_preview_finished(
        &mut self,
        buffer_generation: ImportGeneration,
        preview_generation: ImportGeneration,
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

    /// Whether the Import action is currently enabled. Disabled while key is missing/invalid,
    /// diagnostics exist, a stale notice is pending, or an import is already in flight.
    #[must_use]
    pub(crate) fn can_import(&self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        session.plan().is_some()
            && session.importing == MutationActivity::Idle
            && session.stale_notice.is_none()
            && !matches!(session.validation, ValidationState::Invalid { .. })
    }

    /// Emits `rules_text.import` using the resolved `[set].key` and derived `base_version`.
    #[must_use]
    pub(crate) fn submit_import(&mut self, tree: Option<&RuleTree>) -> RuleImportEffect {
        let Some(session) = &mut self.session else {
            return RuleImportEffect::None;
        };
        if session.importing == MutationActivity::Busy
            || session.stale_notice.is_some()
            || matches!(session.validation, ValidationState::Invalid { .. })
        {
            return RuleImportEffect::None;
        }
        session.target = resolve_import_target(&session.text, tree);
        let Some(plan) = session.plan().cloned() else {
            return RuleImportEffect::None;
        };
        self.import_generation = self.import_generation.next();
        session.importing = MutationActivity::Busy;
        session.import_error = None;
        RuleImportEffect::Import {
            request: wire::RulesTextImportRequest(wire::RuleSetsEditRequest {
                key: plan.key,
                base_version: plan.action.base_version(),
                text: session.text.clone(),
            }),
            generation: self.import_generation,
        }
    }

    /// Applies the outcome of `rules_text.import`. Success closes the pane and returns the
    /// imported key so the page can reload the tree and select it.
    #[must_use]
    pub(crate) fn apply_import_finished(
        &mut self,
        generation: ImportGeneration,
        result: Result<wire::RuleSetsWriteResponse, LoadDataError>,
    ) -> ImportOutcome {
        if !self.import_generation.accepts(generation) {
            return ImportOutcome::Ignored;
        }
        let Some(session) = &mut self.session else {
            return ImportOutcome::Ignored;
        };
        session.importing = MutationActivity::Idle;
        match result {
            Ok(response) => {
                let imported_key = response
                    .sets
                    .first()
                    .map(|set| set.key.clone())
                    .or_else(|| session.plan().map(|plan| plan.key.clone()))
                    .unwrap_or_default();
                self.session = None;
                ImportOutcome::Imported { key: imported_key }
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
                        session.import_error = None;
                    }
                    LoadDataErrorCode::Protocol(ErrorKind::RuleValidationFailed {
                        diagnostics,
                        truncated,
                    }) => {
                        session.validation = ValidationState::Invalid {
                            diagnostics,
                            truncated,
                        };
                        session.import_error = None;
                    }
                    _ => {
                        session.import_error = Some(error);
                    }
                }
                ImportOutcome::Failed
            }
        }
    }
}

/// Resolves pasted rule-set text against `tree` into empty, invalid key, create, or replace.
#[must_use]
pub(crate) fn resolve_import_target(text: &str, tree: Option<&RuleTree>) -> ImportTarget {
    if text.trim().is_empty() {
        return ImportTarget::Empty;
    }
    let key = match extract_import_key(text) {
        Ok(key) => key,
        Err(error) => return ImportTarget::InvalidKey(error),
    };
    if let Some(existing) = tree.and_then(|loaded| loaded.find(&key)) {
        if existing.source == wire::RuleSetSource::Builtin {
            return ImportTarget::InvalidKey(ExtractImportKeyError::ReservedBuiltinPrefix);
        }
        return ImportTarget::Resolved(ImportPlan {
            key,
            action: ImportAction::Replace {
                base_version: existing.version,
            },
        });
    }
    ImportTarget::Resolved(ImportPlan {
        key,
        action: ImportAction::Create,
    })
}

/// Extracts and validates `[set].key` from pasted `quartz-rules/1` TOML text.
///
/// # Errors
/// Returns [`ExtractImportKeyError`] when `[set].key` is absent, malformed, duplicated, contains
/// characters outside the rule-set key vocabulary, or uses the reserved `builtin.` prefix.
pub(crate) fn extract_import_key(text: &str) -> Result<String, ExtractImportKeyError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        TopLevel,
        Set,
        Other,
    }

    let source = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut section = Section::TopLevel;
    let mut quote_state = MultilineQuoteState::None;
    let mut found_key: Option<String> = None;

    for raw_line in source.lines() {
        if quote_state != MultilineQuoteState::None {
            quote_state = advance_multiline_quote(raw_line, quote_state);
            continue;
        }
        let line = raw_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(after_double) = line.strip_prefix("[[") {
            let Some((_, tail)) = after_double.split_once("]]") else {
                continue;
            };
            let tail = tail.trim_start();
            if tail.is_empty() || tail.starts_with('#') {
                section = Section::Other;
            }
            continue;
        }
        if let Some(after_bracket) = line.strip_prefix('[') {
            let Some((header, tail)) = after_bracket.split_once(']') else {
                continue;
            };
            let tail = tail.trim_start();
            if tail.is_empty() || tail.starts_with('#') {
                section = if header.trim() == SET_SECTION_NAME {
                    Section::Set
                } else {
                    Section::Other
                };
            }
            continue;
        }
        if let Some((lhs, rhs)) = line.split_once('=') {
            let matches_set_key = match section {
                Section::Set => lhs.trim() == SET_KEY_FIELD,
                Section::TopLevel => is_dotted_set_key(lhs),
                Section::Other => false,
            };
            if matches_set_key {
                if found_key.is_some() {
                    return Err(ExtractImportKeyError::InvalidKey);
                }
                let parsed = parse_single_line_toml_string(rhs.trim_start())
                    .ok_or(ExtractImportKeyError::InvalidKey)?;
                found_key = Some(parsed);
            } else {
                quote_state = detect_unclosed_multiline_quote(rhs.trim_start());
            }
        }
    }

    let key = found_key.ok_or(ExtractImportKeyError::MissingKey)?;
    if key.is_empty()
        || !key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte)
        })
    {
        return Err(ExtractImportKeyError::InvalidKey);
    }
    if key.starts_with(BUILTIN_KEY_PREFIX) {
        return Err(ExtractImportKeyError::ReservedBuiltinPrefix);
    }
    Ok(key)
}

fn is_dotted_set_key(lhs: &str) -> bool {
    let Some((table, field)) = lhs.split_once('.') else {
        return false;
    };
    table.trim() == SET_SECTION_NAME && field.trim() == SET_KEY_FIELD
}

fn parse_single_line_toml_string(raw: &str) -> Option<String> {
    if raw.starts_with("\"\"\"") || raw.starts_with("'''") {
        return None;
    }
    if let Some(rest) = raw.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        let mut closed = false;
        while let Some(ch) = chars.next() {
            match ch {
                '"' => {
                    closed = true;
                    break;
                }
                '\\' => match chars.next()? {
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    _ => return None,
                },
                other => out.push(other),
            }
        }
        if !closed {
            return None;
        }
        let tail = chars.as_str().trim_start();
        if tail.is_empty() || tail.starts_with('#') {
            return Some(out);
        }
        return None;
    }
    if let Some(rest) = raw.strip_prefix('\'') {
        let (value, after) = rest.split_once('\'')?;
        let tail = after.trim_start();
        if tail.is_empty() || tail.starts_with('#') {
            return Some(value.to_owned());
        }
        return None;
    }
    None
}

fn detect_unclosed_multiline_quote(rhs: &str) -> MultilineQuoteState {
    if let Some(rest) = rhs.strip_prefix("\"\"\"") {
        return advance_multiline_quote(rest, MultilineQuoteState::Basic);
    }
    if let Some(rest) = rhs.strip_prefix("'''") {
        return advance_multiline_quote(rest, MultilineQuoteState::Literal);
    }
    MultilineQuoteState::None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MultilineQuoteState {
    None,
    Basic,
    Literal,
}

fn advance_multiline_quote(line: &str, state: MultilineQuoteState) -> MultilineQuoteState {
    match state {
        MultilineQuoteState::None => MultilineQuoteState::None,
        MultilineQuoteState::Literal => {
            if line.contains("'''") {
                MultilineQuoteState::None
            } else {
                MultilineQuoteState::Literal
            }
        }
        MultilineQuoteState::Basic => {
            let mut chars = line.chars();
            let mut consecutive_quotes = 0_usize;
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    consecutive_quotes = 0;
                    let _ = chars.next();
                } else if ch == '"' {
                    consecutive_quotes = consecutive_quotes.saturating_add(1);
                    if consecutive_quotes == 3 {
                        return MultilineQuoteState::None;
                    }
                } else {
                    consecutive_quotes = 0;
                }
            }
            MultilineQuoteState::Basic
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_pipe::CallCoreError;
    use quartz_protocol::{
        MutatedRuleSet, QueryVersions, RuleDiagnostic, RuleDiagnosticLocation, RuleKind,
        RulePreviewSideTotals, RulePreviewTotals, RuleSetCounts, RuleSetNode, RuleSetSource,
        RuleSetVersion, RuleSetsTreeResponse, RuleSetsWriteResponse, RulesPreviewResponse,
        RulesTextValidateResponse,
    };

    use crate::link::RequestCoreError;

    fn sample_tree() -> RuleTree {
        let make_node =
            |key: &str, parent: Option<&str>, version: u32, source: RuleSetSource| RuleSetNode {
                key: key.to_owned(),
                name: key.to_owned(),
                parent: parent.map(str::to_owned),
                version,
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
        let responses = vec![RuleSetsTreeResponse {
            kind: RuleKind::Classification,
            effective_version: "c1".to_owned(),
            nodes: vec![
                make_node("builtin.classification", None, 2, RuleSetSource::Builtin),
                make_node(
                    "user.study-sites",
                    Some("builtin.classification"),
                    4,
                    RuleSetSource::User,
                ),
            ],
            versions: QueryVersions("v1".to_owned()),
            truncated: false,
        }];
        RuleTree::from_responses(&responses).expect("valid tree")
    }

    fn protocol_error(kind: ErrorKind) -> LoadDataError {
        LoadDataError::from(RequestCoreError::Call {
            source: CallCoreError::Protocol { kind },
        })
    }

    #[test]
    fn extracts_set_key_and_rejects_missing_invalid_or_builtin_keys() {
        let valid_double = "format = \"quartz-rules/1\"\n\n[set]\nkey = \"user.study-sites\" # comment\nkind = \"classification\"\nname = \"Study\"\n";
        assert_eq!(
            extract_import_key(valid_double),
            Ok("user.study-sites".to_owned())
        );

        let valid_single = "format = 'quartz-rules/1'\n[set]\nkey = 'user.my_rules-1'\nkind = 'classification'\nname = 'Rules'\n";
        assert_eq!(
            extract_import_key(valid_single),
            Ok("user.my_rules-1".to_owned())
        );

        let dotted = "format = \"quartz-rules/1\"\nset.key = \"user.dotted\"\n";
        assert_eq!(extract_import_key(dotted), Ok("user.dotted".to_owned()));

        let key_only_in_rule = "format = \"quartz-rules/1\"\n[set]\nkind = \"classification\"\n[[rule]]\nkey = \"user.wrong-section\"\n";
        assert_eq!(
            extract_import_key(key_only_in_rule),
            Err(ExtractImportKeyError::MissingKey)
        );

        let missing =
            "format = \"quartz-rules/1\"\n[set]\nkind = \"classification\"\nname = \"No key\"\n";
        assert_eq!(
            extract_import_key(missing),
            Err(ExtractImportKeyError::MissingKey)
        );

        let invalid_chars = "format = \"quartz-rules/1\"\n[set]\nkey = \"User.Bad!\"\n";
        assert_eq!(
            extract_import_key(invalid_chars),
            Err(ExtractImportKeyError::InvalidKey)
        );

        let unquoted = "format = \"quartz-rules/1\"\n[set]\nkey = user.unquoted\n";
        assert_eq!(
            extract_import_key(unquoted),
            Err(ExtractImportKeyError::InvalidKey)
        );

        let builtin = "format = \"quartz-rules/1\"\n[set]\nkey = \"builtin.classification\"\n";
        assert_eq!(
            extract_import_key(builtin),
            Err(ExtractImportKeyError::ReservedBuiltinPrefix)
        );
    }

    #[test]
    fn missing_or_invalid_key_produces_error_and_emits_no_request() {
        let tree = sample_tree();
        let mut import = RuleImport::new();
        import.open();

        let missing_effect = import.set_text(
            "format = \"quartz-rules/1\"\n[set]\nkind = \"classification\"\n".to_owned(),
            Some(&tree),
        );
        assert_eq!(missing_effect, RuleImportEffect::None);
        assert_eq!(
            import.session().expect("open").target,
            ImportTarget::InvalidKey(ExtractImportKeyError::MissingKey)
        );
        assert!(!import.can_preview());
        assert!(!import.can_import());
        assert_eq!(import.submit_import(Some(&tree)), RuleImportEffect::None);

        let invalid_effect = import.set_text(
            "format = \"quartz-rules/1\"\n[set]\nkey = \"BAD KEY\"\n".to_owned(),
            Some(&tree),
        );
        assert_eq!(invalid_effect, RuleImportEffect::None);
        assert_eq!(
            import.session().expect("open").target,
            ImportTarget::InvalidKey(ExtractImportKeyError::InvalidKey)
        );
        assert!(!import.can_import());
    }

    #[test]
    fn derives_create_or_replace_plan_and_base_version_from_tree() {
        let tree = sample_tree();

        let new_text = "format = \"quartz-rules/1\"\n[set]\nkey = \"user.brand-new\"\nkind = \"classification\"\nname = \"New\"\n";
        assert_eq!(
            resolve_import_target(new_text, Some(&tree)),
            ImportTarget::Resolved(ImportPlan {
                key: "user.brand-new".to_owned(),
                action: ImportAction::Create,
            })
        );
        assert_eq!(ImportAction::Create.base_version(), 0);

        let replace_text = "format = \"quartz-rules/1\"\n[set]\nkey = \"user.study-sites\"\nkind = \"classification\"\nname = \"Study\"\n";
        assert_eq!(
            resolve_import_target(replace_text, Some(&tree)),
            ImportTarget::Resolved(ImportPlan {
                key: "user.study-sites".to_owned(),
                action: ImportAction::Replace { base_version: 4 },
            })
        );
        assert_eq!(ImportAction::Replace { base_version: 4 }.base_version(), 4);
    }

    #[test]
    fn import_is_disabled_while_diagnostics_exist_and_preview_uses_import_mutation() {
        let tree = sample_tree();
        let mut import = RuleImport::new();
        import.open();
        let text = "format = \"quartz-rules/1\"\n[set]\nkey = \"user.study-sites\"\nkind = \"classification\"\nname = \"Study\"\n";

        let RuleImportEffect::ScheduleValidate { generation: gen1 } =
            import.set_text(text.to_owned(), Some(&tree))
        else {
            panic!("valid key schedules debounced validation");
        };
        let RuleImportEffect::Validate {
            request,
            generation,
        } = import.trigger_validate(gen1)
        else {
            panic!("debounce triggers validate");
        };
        assert_eq!(generation, gen1);
        assert_eq!(request.key, "user.study-sites");
        assert_eq!(request.base_version, 4);

        import.apply_validation_finished(
            gen1,
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
        assert!(
            !import.can_import(),
            "import must be disabled while diagnostics exist"
        );
        assert!(!import.can_preview());
        assert_eq!(import.submit_import(Some(&tree)), RuleImportEffect::None);

        let fixed_text = format!("{text}# fixed\n");
        let RuleImportEffect::ScheduleValidate { generation: gen2 } =
            import.set_text(fixed_text.clone(), Some(&tree))
        else {
            panic!("edit schedules validation");
        };
        let _ = import.trigger_validate(gen2);
        // Late error from gen1 is dropped.
        import.apply_validation_finished(
            gen1,
            Ok(RulesTextValidateResponse {
                valid: false,
                diagnostics: vec![RuleDiagnostic {
                    code: "E-RULE-SET".to_owned(),
                    path: "set".to_owned(),
                    location: None,
                    params: Default::default(),
                }],
                truncated: false,
            }),
        );
        assert!(matches!(
            import.session().expect("open").validation,
            ValidationState::Validating
        ));

        import.apply_validation_finished(
            gen2,
            Ok(RulesTextValidateResponse {
                valid: true,
                diagnostics: Vec::new(),
                truncated: false,
            }),
        );
        assert!(import.can_preview());
        assert!(import.can_import());

        let today: LocalDate = "2026-11-26".parse().expect("date");
        let RuleImportEffect::Preview {
            request: preview_req,
            buffer_generation,
            preview_generation,
        } = import.request_preview(today)
        else {
            panic!("valid import can preview");
        };
        assert_eq!(
            preview_req.start_date,
            "2026-11-20".parse::<LocalDate>().expect("start")
        );
        assert_eq!(
            preview_req.end_date,
            "2026-11-27".parse::<LocalDate>().expect("end")
        );
        assert!(matches!(
            preview_req.mutation,
            wire::RulePreviewMutation::Import(wire::RuleSetsEditRequest {
                ref key,
                base_version: 4,
                ..
            }) if key == "user.study-sites"
        ));
        import.apply_preview_finished(
            buffer_generation,
            preview_generation,
            Ok(RulesPreviewResponse {
                rules: Vec::new(),
                totals: RulePreviewTotals {
                    before: RulePreviewSideTotals {
                        active_ms: 0,
                        categories: Vec::new(),
                    },
                    after: RulePreviewSideTotals {
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
            import.session().expect("open").preview,
            PreviewState::Ready(_)
        ));

        let RuleImportEffect::Import {
            request: import_req,
            generation: import_gen,
        } = import.submit_import(Some(&tree))
        else {
            panic!("valid import submits RulesTextImportRequest");
        };
        assert_eq!(import_req.0.key, "user.study-sites");
        assert_eq!(import_req.0.base_version, 4);
        assert_eq!(import_req.0.text, fixed_text);

        let outcome = import.apply_import_finished(
            import_gen,
            Ok(RuleSetsWriteResponse {
                sets: vec![MutatedRuleSet {
                    key: "user.study-sites".to_owned(),
                    version: 5,
                    deleted: false,
                }],
                versions: QueryVersions("v2".to_owned()),
            }),
        );
        assert_eq!(
            outcome,
            ImportOutcome::Imported {
                key: "user.study-sites".to_owned()
            }
        );
        assert!(!import.is_open());
    }

    #[test]
    fn stale_import_sets_notice_and_tree_sync_revalidates_with_new_version() {
        let tree = sample_tree();
        let mut import = RuleImport::new();
        import.open();
        let text = "format = \"quartz-rules/1\"\n[set]\nkey = \"user.study-sites\"\nkind = \"classification\"\nname = \"Study\"\n";
        let _ = import.set_text(text.to_owned(), Some(&tree));

        let RuleImportEffect::Import { generation, .. } = import.submit_import(Some(&tree)) else {
            panic!("submit");
        };
        let outcome = import.apply_import_finished(
            generation,
            Err(protocol_error(ErrorKind::RuleSetStale {
                key: "user.study-sites".to_owned(),
                current_version: RuleSetVersion(7),
            })),
        );
        assert_eq!(outcome, ImportOutcome::Failed);
        assert_eq!(
            import.session().expect("open").stale_notice,
            Some(StaleNotice { current_version: 7 })
        );
        assert!(!import.can_import());

        let refreshed_tree = RuleTree::from_responses(&[RuleSetsTreeResponse {
            kind: RuleKind::Classification,
            effective_version: "c2".to_owned(),
            nodes: vec![RuleSetNode {
                key: "user.study-sites".to_owned(),
                name: "Study sites".to_owned(),
                parent: None,
                version: 7,
                enabled: true,
                sibling_order: 0,
                source: RuleSetSource::User,
                counts: RuleSetCounts {
                    added: 1,
                    overridden: 0,
                    disabled: 0,
                },
                warnings: Vec::new(),
            }],
            versions: QueryVersions("v2".to_owned()),
            truncated: false,
        }])
        .expect("refreshed tree");

        let RuleImportEffect::Validate { request, .. } = import.sync_tree(Some(&refreshed_tree))
        else {
            panic!("sync_tree revalidates with updated version");
        };
        assert_eq!(request.base_version, 7);
        assert!(import.session().expect("open").stale_notice.is_none());
    }

    #[test]
    fn debug_redacts_pasted_rule_text() {
        let mut import = RuleImport::new();
        import.open();
        let _ = import.set_text("secret-rule-marker".to_owned(), None);
        let session = import.session().expect("open");
        assert_eq!(session.text(), "secret-rule-marker");
        let debug = format!("{session:?}");
        assert!(!debug.contains("secret-rule-marker"));
        import.close();
        assert!(!import.is_open());
    }

    #[test]
    fn superseded_import_preview_generation_drops_progress() {
        let tree = sample_tree();
        let mut import = RuleImport::new();
        import.open();
        let text = "format = \"quartz-rules/1\"\n[set]\nkey = \"user.study-sites\"\nkind = \"classification\"\nname = \"Study\"\n";
        let RuleImportEffect::ScheduleValidate { generation } =
            import.set_text(text.to_owned(), Some(&tree))
        else {
            panic!("schedules validate");
        };
        let _ = import.trigger_validate(generation);
        import.apply_validation_finished(
            generation,
            Ok(RulesTextValidateResponse {
                valid: true,
                diagnostics: Vec::new(),
                truncated: false,
            }),
        );
        let today: LocalDate = "2026-11-26".parse().expect("date");
        let RuleImportEffect::Preview {
            buffer_generation,
            preview_generation,
            ..
        } = import.request_preview(today)
        else {
            panic!("requests preview");
        };
        assert_eq!(
            import.apply_preview_progress(
                buffer_generation,
                preview_generation,
                QueryProgress { done: 4, total: 7 },
            ),
            ProgressUpdate::Applied
        );
        assert!(matches!(
            import.session().expect("open").preview,
            PreviewState::Loading(LoadProgress::Counting { done: 4, total: 7 })
        ));

        let _ = import.set_text(format!("{text}# updated\n"), Some(&tree));
        assert_eq!(
            import.apply_preview_progress(
                buffer_generation,
                preview_generation,
                QueryProgress { done: 6, total: 7 },
            ),
            ProgressUpdate::Ignored
        );
        assert!(matches!(
            import.session().expect("open").preview,
            PreviewState::Idle
        ));
    }
}
