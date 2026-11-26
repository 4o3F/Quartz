//! Unclassified queue view model: last-7-logical-days `query.unclassified` state machine,
//! group-by toggle (application / document), debounced search, generation gating, `WorkEntity`
//! visibility mapping (including hidden entities), and prefilled classification rule templates.

use std::fmt;
#[cfg(windows)]
use std::time::Duration;

use jiff::ToSpan as _;
use quartz_protocol::{self as wire, LocalDate};
use snafu::Snafu;

use crate::{
    link::LoadDataError,
    model::{
        rule_editor::{ClassificationRulePrefill, PrefilledCondition},
        rules::LoadState,
    },
};

/// Number of logical days queried for the unclassified queue.
pub(crate) const UNCLASSIFIED_QUEUE_DAYS: i64 = 7;
/// Delay before firing `query.unclassified` after typing in the search box or receiving a day
/// change notification.
#[cfg(windows)]
pub(crate) const UNCLASSIFIED_DEBOUNCE_DELAY: Duration = Duration::from_millis(350);

/// Monotonic generation counter for dropping late `query.unclassified` responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct UnclassifiedGeneration(u64);

impl UnclassifiedGeneration {
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

/// Computes the half-open `[start_date, end_date)` range covering the last 7 logical days up to
/// and including `today`.
#[must_use]
pub(crate) fn unclassified_last_seven_days(today: LocalDate) -> Option<(LocalDate, LocalDate)> {
    let span = (UNCLASSIFIED_QUEUE_DAYS - 1).days();
    let start_date = today.checked_sub(span).ok()?;
    let end_date = today.tomorrow().ok()?;
    Some((start_date, end_date))
}

/// Client-side validation failure for the unclassified search input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub(crate) enum ValidateUnclassifiedSearchError {
    #[snafu(display("unclassified search exceeds the maximum length"))]
    TooLong,
}

/// Language-independent primary display label for a [`wire::WorkEntity`], preserving privacy
/// redactions without synthesizing hidden fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnclassifiedEntityLabel {
    Visible(String),
    HiddenApp,
    HiddenDocument,
}

/// Whether an unclassified row represents an application or a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnclassifiedEntityKind {
    App,
    Document,
}

/// One sorted row in the unclassified queue.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct UnclassifiedRow {
    pub(crate) kind: UnclassifiedEntityKind,
    pub(crate) label: UnclassifiedEntityLabel,
    pub(crate) secondary: Option<String>,
    pub(crate) active_ms: i64,
    pub(crate) prefill: Option<ClassificationRulePrefill>,
}

impl fmt::Debug for UnclassifiedRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnclassifiedRow")
            .field("kind", &self.kind)
            .field("active_ms", &self.active_ms)
            .field("has_prefill", &self.prefill.is_some())
            .finish_non_exhaustive()
    }
}

impl UnclassifiedRow {
    /// Maps one protocol [`wire::UnclassifiedItem`] while respecting `WorkEntity` visibility
    /// fields exactly as returned.
    #[must_use]
    pub(crate) fn from_protocol(item: wire::UnclassifiedItem) -> Self {
        let prefill = prefill_from_work_entity(&item.entity);
        let (kind, label, secondary) = match item.entity {
            wire::WorkEntity::App {
                exe_name,
                display_name,
                ..
            } => {
                let visible_display = (!display_name.trim().is_empty()).then_some(display_name);
                let visible_exe = exe_name.filter(|name| !name.trim().is_empty());
                match (visible_display, visible_exe) {
                    (Some(display), Some(exe)) => {
                        let secondary = (exe != display).then_some(exe);
                        (
                            UnclassifiedEntityKind::App,
                            UnclassifiedEntityLabel::Visible(display),
                            secondary,
                        )
                    }
                    (Some(display), None) => (
                        UnclassifiedEntityKind::App,
                        UnclassifiedEntityLabel::Visible(display),
                        None,
                    ),
                    (None, Some(exe)) => (
                        UnclassifiedEntityKind::App,
                        UnclassifiedEntityLabel::Visible(exe),
                        None,
                    ),
                    (None, None) => (
                        UnclassifiedEntityKind::App,
                        UnclassifiedEntityLabel::HiddenApp,
                        None,
                    ),
                }
            }
            wire::WorkEntity::Document {
                document,
                project,
                profile,
                ..
            } => {
                let mut meta_parts = Vec::new();
                if let Some(proj) = project.filter(|value| !value.trim().is_empty()) {
                    meta_parts.push(proj);
                }
                if let Some(prof) = profile.filter(|value| !value.trim().is_empty()) {
                    meta_parts.push(prof);
                }
                let secondary = (!meta_parts.is_empty()).then(|| meta_parts.join(" · "));
                let label = if document.trim().is_empty() {
                    UnclassifiedEntityLabel::HiddenDocument
                } else {
                    UnclassifiedEntityLabel::Visible(document)
                };
                (UnclassifiedEntityKind::Document, label, secondary)
            }
        };

        Self {
            kind,
            label,
            secondary,
            active_ms: item.active_ms,
            prefill,
        }
    }
}

/// Builds a prefilled classification rule template input from a visible [`wire::WorkEntity`], or
/// `None` when every matchable field is hidden or empty.
#[must_use]
pub(crate) fn prefill_from_work_entity(
    entity: &wire::WorkEntity,
) -> Option<ClassificationRulePrefill> {
    match entity {
        wire::WorkEntity::App {
            exe_name,
            display_name,
            ..
        } => {
            let visible_exe = exe_name.as_deref().filter(|name| !name.trim().is_empty());
            let visible_display =
                (!display_name.trim().is_empty()).then_some(display_name.as_str());
            if let Some(exe) = visible_exe {
                let slug_hint = exe
                    .strip_suffix(".exe")
                    .or_else(|| exe.strip_suffix(".EXE"))
                    .unwrap_or(exe)
                    .to_owned();
                Some(ClassificationRulePrefill {
                    slug_hint,
                    rule_label: visible_display.unwrap_or(exe).to_owned(),
                    conditions: vec![PrefilledCondition {
                        field: "app.exe_name",
                        op: "equals",
                        value: exe.to_owned(),
                    }],
                })
            } else {
                let display = visible_display?;
                Some(ClassificationRulePrefill {
                    slug_hint: display.to_owned(),
                    rule_label: display.to_owned(),
                    conditions: vec![PrefilledCondition {
                        field: "app.display_name",
                        op: "equals",
                        value: display.to_owned(),
                    }],
                })
            }
        }
        wire::WorkEntity::Document {
            document,
            project,
            profile,
            ..
        } => {
            if document.trim().is_empty() {
                return None;
            }
            let mut conditions = vec![PrefilledCondition {
                field: "doc.document",
                op: "equals",
                value: document.clone(),
            }];
            if let Some(proj) = project.as_deref().filter(|value| !value.trim().is_empty()) {
                conditions.push(PrefilledCondition {
                    field: "doc.project",
                    op: "equals",
                    value: proj.to_owned(),
                });
            }
            if let Some(prof) = profile.as_deref().filter(|value| !value.trim().is_empty()) {
                conditions.push(PrefilledCondition {
                    field: "doc.profile",
                    op: "equals",
                    value: prof.to_owned(),
                });
            }
            Some(ClassificationRulePrefill {
                slug_hint: document.clone(),
                rule_label: document.clone(),
                conditions,
            })
        }
    }
}

/// Loaded unclassified queue snapshot sorted by `active_ms` descending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnclassifiedList {
    pub(crate) items: Vec<UnclassifiedRow>,
    pub(crate) truncated: bool,
}

impl UnclassifiedList {
    /// Maps a `query.unclassified` response into a list guaranteed to be sorted by `active_ms`
    /// descending while preserving core tie-breaking order.
    #[must_use]
    pub(crate) fn from_protocol(response: wire::QueryUnclassifiedResponse) -> Self {
        let mut items: Vec<UnclassifiedRow> = response
            .items
            .into_iter()
            .map(UnclassifiedRow::from_protocol)
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.active_ms));
        Self {
            items,
            truncated: response.truncated,
        }
    }
}

/// Whether the queue has a current response or should refresh next time it becomes visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum QueueFreshness {
    #[default]
    Unloaded,
    Fresh,
    Stale,
}

/// Side effect emitted by [`UnclassifiedQueue`] transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnclassifiedEffect {
    None,
    ScheduleQuery {
        generation: UnclassifiedGeneration,
    },
    Query {
        request: wire::QueryUnclassifiedRequest,
        range: (LocalDate, LocalDate),
        generation: UnclassifiedGeneration,
    },
}

/// State machine for the Rules page "Unclassified" tab.
pub(crate) struct UnclassifiedQueue {
    group_by: wire::UnclassifiedGroupBy,
    search_text: String,
    search_error: Option<ValidateUnclassifiedSearchError>,
    state: LoadState<UnclassifiedList>,
    loaded_range: Option<(LocalDate, LocalDate)>,
    freshness: QueueFreshness,
    generation: UnclassifiedGeneration,
}

impl fmt::Debug for UnclassifiedQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnclassifiedQueue")
            .field("group_by", &self.group_by)
            .field("has_search", &!self.search_text.trim().is_empty())
            .field("search_error", &self.search_error)
            .field("loaded_range", &self.loaded_range)
            .field("freshness", &self.freshness)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Default for UnclassifiedQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl UnclassifiedQueue {
    /// Creates an unloaded unclassified queue defaulted to grouping by application.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            group_by: wire::UnclassifiedGroupBy::App,
            search_text: String::new(),
            search_error: None,
            state: LoadState::Loading,
            loaded_range: None,
            freshness: QueueFreshness::Unloaded,
            generation: UnclassifiedGeneration::default(),
        }
    }

    /// Active grouping mode (application vs document).
    #[must_use]
    pub(crate) const fn group_by(&self) -> wire::UnclassifiedGroupBy {
        self.group_by
    }

    /// Current raw search box text.
    #[cfg(windows)]
    #[must_use]
    pub(crate) fn search_text(&self) -> &str {
        &self.search_text
    }

    /// Whether a non-empty search filter is active.
    #[cfg(windows)]
    #[must_use]
    pub(crate) fn has_search_filter(&self) -> bool {
        !self.search_text.trim().is_empty()
    }

    /// Client-side search validation error, if the query exceeds protocol bounds.
    #[must_use]
    pub(crate) const fn search_error(&self) -> Option<ValidateUnclassifiedSearchError> {
        self.search_error
    }

    /// Current load state of the queue.
    #[must_use]
    pub(crate) const fn state(&self) -> &LoadState<UnclassifiedList> {
        &self.state
    }

    /// Currently loaded or active 7-day logical range.
    #[cfg(windows)]
    #[must_use]
    pub(crate) const fn loaded_range(&self) -> Option<(LocalDate, LocalDate)> {
        self.loaded_range
    }

    /// Starts or refreshes the queue when the Unclassified tab is opened.
    #[must_use]
    pub(crate) fn open(&mut self, today: LocalDate) -> UnclassifiedEffect {
        self.reload(today)
    }

    /// Forces a fresh `query.unclassified` request for the last 7 logical days ending on `today`.
    #[must_use]
    pub(crate) fn reload(&mut self, today: LocalDate) -> UnclassifiedEffect {
        if self.search_error.is_some() {
            return UnclassifiedEffect::None;
        }
        self.issue_query(today)
    }

    /// Switches between grouping by application and document, issuing a new request and
    /// invalidating any in-flight response for the previous grouping.
    #[must_use]
    pub(crate) fn set_group_by(
        &mut self,
        group_by: wire::UnclassifiedGroupBy,
        today: LocalDate,
    ) -> UnclassifiedEffect {
        if self.group_by == group_by {
            return UnclassifiedEffect::None;
        }
        self.group_by = group_by;
        if self.search_error.is_some() {
            self.generation = self.generation.next();
            return UnclassifiedEffect::None;
        }
        self.issue_query(today)
    }

    /// Updates the search text and schedules a debounced `query.unclassified` request when within
    /// protocol scalar limits.
    #[must_use]
    pub(crate) fn set_search(&mut self, raw: String) -> UnclassifiedEffect {
        if self.search_text == raw {
            return UnclassifiedEffect::None;
        }
        self.search_text = raw;
        self.generation = self.generation.next();
        if self.search_text.trim().chars().count() > wire::UNCLASSIFIED_MAX_SEARCH_SCALARS {
            self.search_error = Some(ValidateUnclassifiedSearchError::TooLong);
            return UnclassifiedEffect::None;
        }
        self.search_error = None;
        UnclassifiedEffect::ScheduleQuery {
            generation: self.generation,
        }
    }

    /// Executes the debounced query if `generation` is still current.
    #[must_use]
    pub(crate) fn trigger_debounced_query(
        &mut self,
        generation: UnclassifiedGeneration,
        today: LocalDate,
    ) -> UnclassifiedEffect {
        if !self.generation.accepts(generation) || self.search_error.is_some() {
            return UnclassifiedEffect::None;
        }
        let Some((start_date, end_date)) = unclassified_last_seven_days(today) else {
            return UnclassifiedEffect::None;
        };
        let request = self.build_request(start_date, end_date);
        self.state = LoadState::Loading;
        self.loaded_range = Some((start_date, end_date));
        UnclassifiedEffect::Query {
            request,
            range: (start_date, end_date),
            generation,
        }
    }

    /// Applies the outcome of `query.unclassified`, dropping responses from older generations.
    pub(crate) fn apply_loaded(
        &mut self,
        generation: UnclassifiedGeneration,
        range: (LocalDate, LocalDate),
        result: Result<wire::QueryUnclassifiedResponse, LoadDataError>,
    ) {
        if !self.generation.accepts(generation) {
            return;
        }
        match result {
            Ok(response) => {
                self.loaded_range = Some(range);
                self.freshness = QueueFreshness::Fresh;
                self.state = LoadState::Ready(UnclassifiedList::from_protocol(response));
            }
            Err(LoadDataError::Stale) => {
                self.freshness = QueueFreshness::Stale;
                self.state = LoadState::Stale;
            }
            Err(error) => {
                self.freshness = QueueFreshness::Stale;
                self.state = LoadState::Failed(error);
            }
        }
    }

    /// Marks the queue stale after rule mutations so it refreshes when visible or next opened.
    #[cfg(windows)]
    pub(crate) fn mark_stale(&mut self) {
        if self.freshness == QueueFreshness::Fresh {
            self.freshness = QueueFreshness::Stale;
        }
    }

    /// Schedules a coalesced refresh when a `day` topic event touches any covered logical date.
    #[must_use]
    pub(crate) fn on_day_changed(
        &mut self,
        changed_date: LocalDate,
        today: LocalDate,
    ) -> UnclassifiedEffect {
        if self.search_error.is_some() {
            return UnclassifiedEffect::None;
        }
        let covered = self
            .loaded_range
            .or_else(|| unclassified_last_seven_days(today));
        let Some((start_date, end_date)) = covered else {
            return UnclassifiedEffect::None;
        };
        if changed_date < start_date || changed_date >= end_date {
            return UnclassifiedEffect::None;
        }
        self.freshness = QueueFreshness::Stale;
        self.generation = self.generation.next();
        UnclassifiedEffect::ScheduleQuery {
            generation: self.generation,
        }
    }

    fn issue_query(&mut self, today: LocalDate) -> UnclassifiedEffect {
        let Some((start_date, end_date)) = unclassified_last_seven_days(today) else {
            return UnclassifiedEffect::None;
        };
        self.generation = self.generation.next();
        self.state = LoadState::Loading;
        self.loaded_range = Some((start_date, end_date));
        UnclassifiedEffect::Query {
            request: self.build_request(start_date, end_date),
            range: (start_date, end_date),
            generation: self.generation,
        }
    }

    fn build_request(
        &self,
        start_date: LocalDate,
        end_date: LocalDate,
    ) -> wire::QueryUnclassifiedRequest {
        let trimmed = self.search_text.trim();
        let search = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        wire::QueryUnclassifiedRequest {
            start_date,
            end_date,
            group_by: self.group_by,
            search,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::{
        ApplicationId, DocumentRef, QueryUnclassifiedResponse, QueryVersions, UnclassifiedGroupBy,
        UnclassifiedItem, WorkEntity,
    };

    use crate::model::rule_editor::{
        RuleEditor, RuleEditorMode, prefilled_classification_template, suggested_classification_key,
    };

    #[test]
    fn maps_visible_and_hidden_entities_sorted_by_active_ms_desc() {
        let response = QueryUnclassifiedResponse {
            time_zone: "Asia/Shanghai".to_owned(),
            day_start_offset_min: 240,
            reference_range: None,
            items: vec![
                UnclassifiedItem {
                    entity: WorkEntity::App {
                        id: ApplicationId(1),
                        exe_name: None,
                        display_name: "   ".to_owned(),
                    },
                    active_ms: 5_000,
                    sample_ranges: Vec::new(),
                },
                UnclassifiedItem {
                    entity: WorkEntity::App {
                        id: ApplicationId(2),
                        exe_name: Some("editor.exe".to_owned()),
                        display_name: "Editor".to_owned(),
                    },
                    active_ms: 42_000,
                    sample_ranges: Vec::new(),
                },
                UnclassifiedItem {
                    entity: WorkEntity::Document {
                        document_ref: DocumentRef::from_encoded("doc:hidden".to_owned()),
                        app: ApplicationId(3),
                        document: String::new(),
                        project: None,
                        profile: None,
                    },
                    active_ms: 12_000,
                    sample_ranges: Vec::new(),
                },
                UnclassifiedItem {
                    entity: WorkEntity::Document {
                        document_ref: DocumentRef::from_encoded("doc:visible".to_owned()),
                        app: ApplicationId(2),
                        document: "notes.md".to_owned(),
                        project: Some("Quartz".to_owned()),
                        profile: Some("Work".to_owned()),
                    },
                    active_ms: 20_000,
                    sample_ranges: Vec::new(),
                },
            ],
            versions: QueryVersions("v1".to_owned()),
            truncated: true,
        };

        let list = UnclassifiedList::from_protocol(response);
        assert!(list.truncated);
        let durations: Vec<i64> = list.items.iter().map(|row| row.active_ms).collect();
        assert_eq!(durations, vec![42_000, 20_000, 12_000, 5_000]);

        let first = list.items.first().expect("first row");
        assert_eq!(first.kind, UnclassifiedEntityKind::App);
        assert_eq!(
            first.label,
            UnclassifiedEntityLabel::Visible("Editor".to_owned())
        );
        assert_eq!(first.secondary.as_deref(), Some("editor.exe"));
        assert!(first.prefill.is_some());

        let second = list.items.get(1).expect("second row");
        assert_eq!(second.kind, UnclassifiedEntityKind::Document);
        assert_eq!(
            second.label,
            UnclassifiedEntityLabel::Visible("notes.md".to_owned())
        );
        assert_eq!(second.secondary.as_deref(), Some("Quartz · Work"));
        assert!(second.prefill.is_some());

        let hidden_doc = list.items.get(2).expect("hidden document row");
        assert_eq!(hidden_doc.label, UnclassifiedEntityLabel::HiddenDocument);
        assert!(hidden_doc.prefill.is_none());

        let hidden_app = list.items.get(3).expect("hidden app row");
        assert_eq!(hidden_app.label, UnclassifiedEntityLabel::HiddenApp);
        assert!(hidden_app.secondary.is_none());
        assert!(hidden_app.prefill.is_none());
    }

    #[test]
    fn group_toggle_issues_new_request_and_drops_late_results_of_old_generation() {
        let today: LocalDate = "2026-11-26".parse().expect("today");
        let mut queue = UnclassifiedQueue::new();

        let UnclassifiedEffect::Query {
            request: req_app,
            range: range_app,
            generation: gen_app,
        } = queue.open(today)
        else {
            panic!("opening unclassified tab queries immediately");
        };
        assert_eq!(req_app.group_by, UnclassifiedGroupBy::App);
        assert_eq!(
            req_app.start_date,
            "2026-11-20".parse::<LocalDate>().expect("start")
        );
        assert_eq!(
            req_app.end_date,
            "2026-11-27".parse::<LocalDate>().expect("end")
        );

        let UnclassifiedEffect::Query {
            request: req_doc,
            range: range_doc,
            generation: gen_doc,
        } = queue.set_group_by(UnclassifiedGroupBy::Document, today)
        else {
            panic!("toggling group_by issues a new request");
        };
        assert_eq!(queue.group_by(), UnclassifiedGroupBy::Document);
        assert_eq!(req_doc.group_by, UnclassifiedGroupBy::Document);
        assert_ne!(gen_app, gen_doc);

        // Late result from old App request must be ignored.
        queue.apply_loaded(
            gen_app,
            range_app,
            Ok(QueryUnclassifiedResponse {
                time_zone: "UTC".to_owned(),
                day_start_offset_min: 0,
                reference_range: None,
                items: vec![UnclassifiedItem {
                    entity: WorkEntity::App {
                        id: ApplicationId(1),
                        exe_name: Some("stale.exe".to_owned()),
                        display_name: "Stale".to_owned(),
                    },
                    active_ms: 99_000,
                    sample_ranges: Vec::new(),
                }],
                versions: QueryVersions("v1".to_owned()),
                truncated: false,
            }),
        );
        assert!(
            matches!(queue.state(), LoadState::Loading),
            "late response from older group_by generation must be dropped"
        );

        queue.apply_loaded(
            gen_doc,
            range_doc,
            Ok(QueryUnclassifiedResponse {
                time_zone: "UTC".to_owned(),
                day_start_offset_min: 0,
                reference_range: None,
                items: vec![UnclassifiedItem {
                    entity: WorkEntity::Document {
                        document_ref: DocumentRef::from_encoded("doc:1".to_owned()),
                        app: ApplicationId(1),
                        document: "design.md".to_owned(),
                        project: Some("Quartz".to_owned()),
                        profile: None,
                    },
                    active_ms: 15_000,
                    sample_ranges: Vec::new(),
                }],
                versions: QueryVersions("v2".to_owned()),
                truncated: false,
            }),
        );
        let LoadState::Ready(list) = queue.state() else {
            panic!("current generation response populates state");
        };
        assert_eq!(list.items.len(), 1);
        assert_eq!(
            list.items.first().expect("doc").label,
            UnclassifiedEntityLabel::Visible("design.md".to_owned())
        );
    }

    #[test]
    fn search_debounces_and_rejects_oversized_queries() {
        let today: LocalDate = "2026-11-26".parse().expect("today");
        let mut queue = UnclassifiedQueue::new();
        let UnclassifiedEffect::ScheduleQuery { generation } =
            queue.set_search("  notes  ".to_owned())
        else {
            panic!("valid search schedules debounced query");
        };
        assert!(queue.search_error().is_none());
        let UnclassifiedEffect::Query { request, .. } =
            queue.trigger_debounced_query(generation, today)
        else {
            panic!("debounced trigger emits query");
        };
        assert_eq!(request.search.as_deref(), Some("notes"));

        let oversized = "a".repeat(wire::UNCLASSIFIED_MAX_SEARCH_SCALARS + 1);
        assert_eq!(queue.set_search(oversized), UnclassifiedEffect::None);
        assert_eq!(
            queue.search_error(),
            Some(ValidateUnclassifiedSearchError::TooLong)
        );
        assert_eq!(queue.reload(today), UnclassifiedEffect::None);
    }

    #[test]
    fn day_topic_change_refreshes_only_for_covered_dates() {
        let today: LocalDate = "2026-11-26".parse().expect("today");
        let mut queue = UnclassifiedQueue::new();
        let _ = queue.open(today);

        let covered_date: LocalDate = "2026-11-22".parse().expect("covered");
        assert!(matches!(
            queue.on_day_changed(covered_date, today),
            UnclassifiedEffect::ScheduleQuery { .. }
        ));

        let old_date: LocalDate = "2026-11-19".parse().expect("before range");
        assert_eq!(
            queue.on_day_changed(old_date, today),
            UnclassifiedEffect::None
        );

        let future_date: LocalDate = "2026-11-27".parse().expect("after range");
        assert_eq!(
            queue.on_day_changed(future_date, today),
            UnclassifiedEffect::None
        );
    }

    #[test]
    fn prefilled_templates_contain_entity_condition_and_valid_shape_with_empty_category() {
        let app_entity = WorkEntity::App {
            id: ApplicationId(3),
            exe_name: Some("notepad.exe".to_owned()),
            display_name: "Notepad".to_owned(),
        };
        let app_prefill = prefill_from_work_entity(&app_entity).expect("app prefill");
        let app_key = suggested_classification_key(&app_prefill.slug_hint, None);
        assert_eq!(app_key, "user.classify-notepad");
        let app_toml = prefilled_classification_template(
            &app_key,
            Some("builtin.classification"),
            &app_prefill,
        );
        assert!(app_toml.contains("format = \"quartz-rules/1\""));
        assert!(app_toml.contains("[set]"));
        assert!(app_toml.contains("key = \"user.classify-notepad\""));
        assert!(app_toml.contains("kind = \"classification\""));
        assert!(app_toml.contains("parent = \"builtin.classification\""));
        assert!(app_toml.contains("[[rule]]"));
        assert!(app_toml.contains("category = \"\""));
        assert!(
            app_toml
                .contains("{ field = \"app.exe_name\", op = \"equals\", value = \"notepad.exe\" }")
        );

        let doc_entity = WorkEntity::Document {
            document_ref: DocumentRef::from_encoded("doc:1".to_owned()),
            app: ApplicationId(3),
            document: "budget.xlsx".to_owned(),
            project: Some("Finance".to_owned()),
            profile: None,
        };
        let doc_prefill = prefill_from_work_entity(&doc_entity).expect("doc prefill");
        let doc_key = suggested_classification_key(&doc_prefill.slug_hint, None);
        let doc_toml = prefilled_classification_template(
            &doc_key,
            Some("builtin.classification"),
            &doc_prefill,
        );
        assert!(doc_toml.contains("format = \"quartz-rules/1\""));
        assert!(doc_toml.contains("kind = \"classification\""));
        assert!(doc_toml.contains("category = \"\""));
        assert!(
            doc_toml
                .contains("{ field = \"doc.document\", op = \"equals\", value = \"budget.xlsx\" }")
        );
        assert!(
            doc_toml.contains("{ field = \"doc.project\", op = \"equals\", value = \"Finance\" }")
        );

        let mut editor = RuleEditor::new();
        let _ = editor.open_prefilled_classification(app_prefill, None);
        let session = editor.session().expect("editor opened");
        assert!(matches!(
            session.mode,
            RuleEditorMode::CreateNew {
                kind: wire::RuleKind::Classification,
                ..
            }
        ));
        assert!(session.text().contains("app.exe_name"));
        assert!(session.text().contains("notepad.exe"));
        assert!(session.text().contains("category = \"\""));
    }
}
