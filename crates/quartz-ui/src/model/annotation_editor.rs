//! Today saved-annotations list and general annotation editor state machine (M4-H13).
//!
//! Kept free of GPUI dependencies so time validation, DST conversions, Unicode scalar limits,
//! idempotent create retries, inline delete confirmation, and late-response dropping are unit
//! tested on every platform.

use std::fmt;

use jiff::tz::AmbiguousOffset;
use quartz_protocol::{self as wire, ErrorKind};
use snafu::Snafu;

use crate::{
    link::{LoadDataError, LoadDataErrorCode, LocalFailureCode, UtcRange},
    model::{
        BuiltinCategory, CategoryColor, CategoryName,
        rules::{
            CategoryArchiveState, CategoryCatalog, CategoryOrigin, LoadState, MutationActivity,
        },
    },
};

/// Maximum Unicode scalar values shown for a note excerpt in the annotations list.
pub(crate) const ANNOTATION_NOTE_EXCERPT_SCALARS: usize = 120;
/// Default duration in minutes when creating an annotation without a preselected range.
const DEFAULT_CREATE_DURATION_MINUTES: i64 = 30;
/// Default local start hour when creating an annotation from the list button.
const DEFAULT_CREATE_START_HOUR: i8 = 9;
/// Milliseconds in one minute.
const MILLIS_PER_MINUTE: i64 = 60_000;
/// Maximum accepted hour value (`24:00` marks the exclusive end of a midnight-based day).
const MAX_HOUR_END_OF_DAY: u8 = 24;
/// Minutes per hour for `HH:MM` parsing.
const MINUTES_PER_HOUR_U8: u8 = 60;
/// Required length of the minute digits in `HH:MM`.
const MINUTE_DIGITS_LEN: usize = 2;
/// Allowed length range of the hour digits in `H:MM` or `HH:MM`.
const MAX_HOUR_DIGITS_LEN: usize = 2;

/// Monotonic generation counter for dropping late `query.annotations` responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct AnnotationsGeneration(u64);

impl AnnotationsGeneration {
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

/// Logical-day time bounds and time zone copied from the loaded live `Day`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DayTimeContext {
    pub(crate) date: wire::LocalDate,
    pub(crate) bounds: UtcRange,
    pub(crate) zone: jiff::tz::TimeZone,
}

/// Which boundary of a half-open `[start, end)` annotation range is being resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndpointRole {
    Start,
    End,
}

/// Disambiguation choice when a local `HH:MM` repeats during a DST fall-back fold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FoldChoice {
    #[default]
    Earlier,
    Later,
}

/// Parsed `HH:MM` value prior to logical-day and DST resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedClockTime {
    hour: u8,
    minute: u8,
}

/// Client-side validation failure for annotation start/end `HH:MM` inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub(crate) enum ValidateAnnotationTimeError {
    #[snafu(display("annotation time must use HH:MM format"))]
    InvalidFormat,
    #[snafu(display("annotation time falls in a daylight saving time gap"))]
    DstGap,
    #[snafu(display("annotation time is outside the shown logical day"))]
    OutsideDay,
    #[snafu(display("annotation end time must be after start time"))]
    EndNotAfterStart,
}

/// Client-side validation failure for an annotation editor draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub(crate) enum ValidateAnnotationDraftError {
    #[snafu(display("annotation time range is invalid"))]
    Time { source: ValidateAnnotationTimeError },
    #[snafu(display("annotation label exceeds the maximum length in Unicode scalars"))]
    LabelTooLong,
    #[snafu(display("annotation note exceeds the maximum length in Unicode scalars"))]
    NoteTooLong,
    #[snafu(display("category override annotation requires an active category"))]
    CategoryRequired,
    #[snafu(display("note annotation requires a label or note"))]
    NoteContentRequired,
    #[snafu(display("logical day time context is unavailable"))]
    MissingDayContext,
}

fn parse_hm(raw: &str) -> Result<ParsedClockTime, ValidateAnnotationTimeError> {
    let trimmed = raw.trim();
    let (hour_str, minute_str) = trimmed
        .split_once(':')
        .ok_or(ValidateAnnotationTimeError::InvalidFormat)?;
    if hour_str.is_empty()
        || hour_str.len() > MAX_HOUR_DIGITS_LEN
        || minute_str.len() != MINUTE_DIGITS_LEN
        || !hour_str.bytes().all(|byte| byte.is_ascii_digit())
        || !minute_str.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ValidateAnnotationTimeError::InvalidFormat);
    }
    let hour: u8 = hour_str
        .parse()
        .map_err(|_| ValidateAnnotationTimeError::InvalidFormat)?;
    let minute: u8 = minute_str
        .parse()
        .map_err(|_| ValidateAnnotationTimeError::InvalidFormat)?;
    if minute >= MINUTES_PER_HOUR_U8 {
        return Err(ValidateAnnotationTimeError::InvalidFormat);
    }
    if hour > MAX_HOUR_END_OF_DAY || (hour == MAX_HOUR_END_OF_DAY && minute > 0) {
        return Err(ValidateAnnotationTimeError::OutsideDay);
    }
    Ok(ParsedClockTime { hour, minute })
}

impl DayTimeContext {
    /// Formats a UTC millisecond timestamp as `HH:MM` in this day's time zone, using `24:00`
    /// when `role` is [`EndpointRole::End`] at the exclusive midnight end of the day.
    #[must_use]
    pub(crate) fn format_endpoint_hm(&self, utc_ms: i64, role: EndpointRole) -> String {
        let Ok(end_ts) = jiff::Timestamp::from_millisecond(self.bounds.end) else {
            return String::new();
        };
        let end_zdt = end_ts.to_zoned_datetime(self.zone.clone());
        if role == EndpointRole::End
            && utc_ms == self.bounds.end
            && end_zdt.time() == jiff::civil::time(0, 0, 0, 0)
        {
            return "24:00".to_owned();
        }
        let Ok(ts) = jiff::Timestamp::from_millisecond(utc_ms) else {
            return String::new();
        };
        let local = ts.to_zoned_datetime(self.zone.clone());
        format!("{:02}:{:02}", local.hour(), local.minute())
    }

    /// Detects whether `utc_ms` lies in the later occurrence of a DST fall-back fold.
    #[must_use]
    pub(crate) fn detect_fold_choice(&self, utc_ms: i64) -> FoldChoice {
        let Ok(ts) = jiff::Timestamp::from_millisecond(utc_ms) else {
            return FoldChoice::Earlier;
        };
        let zdt = ts.to_zoned_datetime(self.zone.clone());
        match self.zone.to_ambiguous_timestamp(zdt.datetime()).offset() {
            AmbiguousOffset::Fold { after, .. } if zdt.offset() == after => FoldChoice::Later,
            AmbiguousOffset::Unambiguous { .. }
            | AmbiguousOffset::Gap { .. }
            | AmbiguousOffset::Fold { .. } => FoldChoice::Earlier,
        }
    }

    /// Returns `(earlier, later)` UTC offsets when `raw` parses to a repeated local time inside
    /// this logical day.
    #[must_use]
    pub(crate) fn fold_offsets_for_input(
        &self,
        raw: &str,
        role: EndpointRole,
    ) -> Option<(jiff::tz::Offset, jiff::tz::Offset)> {
        let parsed = parse_hm(raw).ok()?;
        if parsed.hour == MAX_HOUR_END_OF_DAY {
            return None;
        }
        let start_zdt = jiff::Timestamp::from_millisecond(self.bounds.start)
            .ok()?
            .to_zoned_datetime(self.zone.clone());
        let end_zdt = jiff::Timestamp::from_millisecond(self.bounds.end)
            .ok()?
            .to_zoned_datetime(self.zone.clone());
        let is_midnight_boundary = end_zdt.time() == jiff::civil::time(0, 0, 0, 0);
        let hour = i8::try_from(parsed.hour).ok()?;
        let minute = i8::try_from(parsed.minute).ok()?;

        let mut dates = vec![start_zdt.date()];
        if end_zdt.date() != start_zdt.date() && !is_midnight_boundary {
            dates.push(end_zdt.date());
        }
        for candidate_date in dates {
            let dt = candidate_date.at(hour, minute, 0, 0);
            if let AmbiguousOffset::Fold { before, after } =
                self.zone.to_ambiguous_timestamp(dt).offset()
            {
                let before_ms = before.to_timestamp(dt).ok()?.as_millisecond();
                let after_ms = after.to_timestamp(dt).ok()?.as_millisecond();
                if self.timestamp_in_day(before_ms, role, is_midnight_boundary)
                    && self.timestamp_in_day(after_ms, role, is_midnight_boundary)
                {
                    return Some((before, after));
                }
            }
        }
        None
    }

    /// Formats a UTC offset as `UTC±HH:MM` for DST disambiguation controls.
    #[must_use]
    #[cfg(windows)]
    pub(crate) fn format_utc_offset(offset: jiff::tz::Offset) -> String {
        format!("UTC{offset}")
    }

    /// Resolves and validates `start_raw` and `end_raw` (`HH:MM`) into a half-open UTC range
    /// inside this logical day.
    ///
    /// # Errors
    /// Returns [`ValidateAnnotationTimeError`] if either input is malformed, falls in a DST gap,
    /// lies outside the shown logical day, or has `end <= start`.
    pub(crate) fn parse_time_range(
        &self,
        start_raw: &str,
        end_raw: &str,
        start_fold: FoldChoice,
        end_fold: FoldChoice,
    ) -> Result<UtcRange, ValidateAnnotationTimeError> {
        let start_clock = parse_hm(start_raw)?;
        let end_clock = parse_hm(end_raw)?;
        let start = self.resolve_endpoint(start_clock, EndpointRole::Start, start_fold)?;
        let end = self.resolve_endpoint(end_clock, EndpointRole::End, end_fold)?;
        if start < self.bounds.start
            || start >= self.bounds.end
            || end < self.bounds.start
            || end > self.bounds.end
        {
            return Err(ValidateAnnotationTimeError::OutsideDay);
        }
        if end <= start {
            return Err(ValidateAnnotationTimeError::EndNotAfterStart);
        }
        Ok(UtcRange { start, end })
    }

    /// Computes a default half-hour UTC range inside this logical day for the "New annotation"
    /// button when no range was preselected.
    #[must_use]
    pub(crate) fn default_create_range(&self) -> UtcRange {
        let duration_ms = DEFAULT_CREATE_DURATION_MINUTES * MILLIS_PER_MINUTE;
        let preferred_start = self
            .date
            .at(DEFAULT_CREATE_START_HOUR, 0, 0, 0)
            .to_zoned_datetime(self.zone.clone())
            .ok()
            .map(|zdt| zdt.timestamp().as_millisecond())
            .filter(|&ms| {
                ms >= self.bounds.start && ms.saturating_add(duration_ms) <= self.bounds.end
            });
        let start = preferred_start.unwrap_or(self.bounds.start);
        let end = start.saturating_add(duration_ms).min(self.bounds.end);
        UtcRange { start, end }
    }

    fn timestamp_in_day(
        &self,
        utc_ms: i64,
        role: EndpointRole,
        is_midnight_boundary: bool,
    ) -> bool {
        if is_midnight_boundary {
            return utc_ms >= self.bounds.start && utc_ms <= self.bounds.end;
        }
        match role {
            EndpointRole::Start => utc_ms >= self.bounds.start && utc_ms < self.bounds.end,
            EndpointRole::End => utc_ms > self.bounds.start && utc_ms <= self.bounds.end,
        }
    }

    fn resolve_endpoint(
        &self,
        clock: ParsedClockTime,
        role: EndpointRole,
        fold: FoldChoice,
    ) -> Result<i64, ValidateAnnotationTimeError> {
        let start_zdt = jiff::Timestamp::from_millisecond(self.bounds.start)
            .map_err(|_| ValidateAnnotationTimeError::OutsideDay)?
            .to_zoned_datetime(self.zone.clone());
        let end_zdt = jiff::Timestamp::from_millisecond(self.bounds.end)
            .map_err(|_| ValidateAnnotationTimeError::OutsideDay)?
            .to_zoned_datetime(self.zone.clone());
        let is_midnight_boundary = end_zdt.time() == jiff::civil::time(0, 0, 0, 0);

        if clock.hour == MAX_HOUR_END_OF_DAY {
            return match role {
                EndpointRole::Start => Err(ValidateAnnotationTimeError::OutsideDay),
                EndpointRole::End if is_midnight_boundary => Ok(self.bounds.end),
                EndpointRole::End => Err(ValidateAnnotationTimeError::OutsideDay),
            };
        }

        let hour =
            i8::try_from(clock.hour).map_err(|_| ValidateAnnotationTimeError::InvalidFormat)?;
        let minute =
            i8::try_from(clock.minute).map_err(|_| ValidateAnnotationTimeError::InvalidFormat)?;

        let mut candidate_dates = vec![start_zdt.date()];
        if end_zdt.date() != start_zdt.date() && !is_midnight_boundary {
            candidate_dates.push(end_zdt.date());
        }

        let mut saw_gap = false;
        for candidate_date in candidate_dates {
            let dt = candidate_date.at(hour, minute, 0, 0);
            let ambiguous = self.zone.to_ambiguous_timestamp(dt);
            match ambiguous.offset() {
                AmbiguousOffset::Gap { before, .. } => {
                    if let Ok(shifted) = before.to_timestamp(dt) {
                        let shifted_ms = shifted.as_millisecond();
                        if shifted_ms >= self.bounds.start && shifted_ms <= self.bounds.end {
                            saw_gap = true;
                        }
                    }
                }
                AmbiguousOffset::Unambiguous { offset } => {
                    let ts = offset
                        .to_timestamp(dt)
                        .map_err(|_| ValidateAnnotationTimeError::OutsideDay)?
                        .as_millisecond();
                    if self.timestamp_in_day(ts, role, is_midnight_boundary) {
                        return Ok(ts);
                    }
                }
                AmbiguousOffset::Fold { before, after } => {
                    let before_ms = before
                        .to_timestamp(dt)
                        .map_err(|_| ValidateAnnotationTimeError::OutsideDay)?
                        .as_millisecond();
                    let after_ms = after
                        .to_timestamp(dt)
                        .map_err(|_| ValidateAnnotationTimeError::OutsideDay)?
                        .as_millisecond();
                    let before_ok = self.timestamp_in_day(before_ms, role, is_midnight_boundary);
                    let after_ok = self.timestamp_in_day(after_ms, role, is_midnight_boundary);
                    match (fold, before_ok, after_ok) {
                        (FoldChoice::Earlier, true, _) => return Ok(before_ms),
                        (FoldChoice::Later, _, true) => return Ok(after_ms),
                        (FoldChoice::Earlier, false, true) => return Ok(after_ms),
                        (FoldChoice::Later, true, false) => return Ok(before_ms),
                        (_, false, false) => {}
                    }
                }
            }
        }

        if saw_gap {
            Err(ValidateAnnotationTimeError::DstGap)
        } else {
            Err(ValidateAnnotationTimeError::OutsideDay)
        }
    }
}

/// Mapped write-error notice shown inline in the annotation editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnnotationWriteNotice {
    BadRequest { code: LoadDataErrorCode },
    InvalidRange { code: LoadDataErrorCode },
    Stale { code: LoadDataErrorCode },
    Busy { code: LoadDataErrorCode },
    Generic { code: LoadDataErrorCode },
}

impl AnnotationWriteNotice {
    /// Maps a core/link error into a typed localized notice class.
    #[must_use]
    pub(crate) fn from_error(error: &LoadDataError) -> Self {
        let code = error.code();
        if matches!(error, LoadDataError::Stale | LoadDataError::StaleExhausted) {
            return Self::Stale { code };
        }
        match &code {
            LoadDataErrorCode::Protocol(
                ErrorKind::StaleDocumentRef | ErrorKind::RuleSetStale { .. },
            ) => Self::Stale { code },
            LoadDataErrorCode::Protocol(ErrorKind::Busy) => Self::Busy { code },
            LoadDataErrorCode::Protocol(ErrorKind::InvalidRange) => Self::InvalidRange { code },
            LoadDataErrorCode::Protocol(ErrorKind::BadRequest) => Self::BadRequest { code },
            LoadDataErrorCode::Protocol(_) | LoadDataErrorCode::Local(_) => Self::Generic { code },
        }
    }

    /// Returns the stable machine-readable error code for diagnostic display.
    #[must_use]
    pub(crate) fn code(&self) -> &LoadDataErrorCode {
        match self {
            Self::BadRequest { code }
            | Self::InvalidRange { code }
            | Self::Stale { code }
            | Self::Busy { code }
            | Self::Generic { code } => code,
        }
    }
}

/// Resolved presentation of an annotation's category against [`CategoryCatalog`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnnotationCategoryDisplay {
    Known {
        name: CategoryName,
        color: CategoryColor,
        archive_state: CategoryArchiveState,
    },
    Missing,
}

const fn builtin_from_wire(builtin: wire::BuiltinCategory) -> BuiltinCategory {
    match builtin {
        wire::BuiltinCategory::Development => BuiltinCategory::Development,
        wire::BuiltinCategory::Learning => BuiltinCategory::Learning,
        wire::BuiltinCategory::Communication => BuiltinCategory::Communication,
        wire::BuiltinCategory::Entertainment => BuiltinCategory::Entertainment,
        wire::BuiltinCategory::System => BuiltinCategory::System,
    }
}

/// Resolves an annotation's [`wire::AnnotationCategory`] using the shared [`CategoryCatalog`].
#[must_use]
pub(crate) fn resolve_annotation_category(
    category: &wire::AnnotationCategory,
    catalog: Option<&CategoryCatalog>,
) -> AnnotationCategoryDisplay {
    match category {
        wire::AnnotationCategory::Uuid { uuid } => match catalog.and_then(|cat| cat.find(uuid)) {
            Some(item) => AnnotationCategoryDisplay::Known {
                name: item.display_name.clone(),
                color: item.color,
                archive_state: item.archive_state,
            },
            None => AnnotationCategoryDisplay::Missing,
        },
        wire::AnnotationCategory::Builtin { key } => {
            let builtin = builtin_from_wire(*key);
            if let Some(item) = catalog.and_then(|cat| {
                cat.items
                    .iter()
                    .find(|item| item.origin == CategoryOrigin::Builtin(builtin))
            }) {
                AnnotationCategoryDisplay::Known {
                    name: item.display_name.clone(),
                    color: item.color,
                    archive_state: item.archive_state,
                }
            } else {
                AnnotationCategoryDisplay::Known {
                    name: CategoryName::Builtin(builtin),
                    color: builtin.default_color(),
                    archive_state: CategoryArchiveState::Active,
                }
            }
        }
    }
}

/// Collapses whitespace/newlines and truncates a note to [`ANNOTATION_NOTE_EXCERPT_SCALARS`]
/// Unicode scalar values for list row previews.
#[must_use]
pub(crate) fn format_note_excerpt(note: &str) -> Option<String> {
    let mut normalized = String::new();
    for word in note.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    if normalized.is_empty() {
        return None;
    }
    if normalized.chars().count() <= ANNOTATION_NOTE_EXCERPT_SCALARS {
        return Some(normalized);
    }
    let mut excerpt: String = normalized
        .chars()
        .take(ANNOTATION_NOTE_EXCERPT_SCALARS)
        .collect();
    excerpt.push('…');
    Some(excerpt)
}

/// One saved annotation row shown in the Today annotations list.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SavedAnnotationRow {
    pub(crate) uuid: wire::AnnotationId,
    pub(crate) kind: wire::AnnotationKind,
    pub(crate) range: UtcRange,
    pub(crate) category: Option<wire::AnnotationCategory>,
    pub(crate) label_preset: Option<wire::AnnotationLabelPreset>,
    pub(crate) label: Option<String>,
    pub(crate) note: Option<String>,
}

impl fmt::Debug for SavedAnnotationRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SavedAnnotationRow")
            .field("uuid", &self.uuid)
            .field("kind", &self.kind)
            .field("range", &self.range)
            .field("category", &self.category)
            .field("label_preset", &self.label_preset)
            .field("has_label", &self.label.is_some())
            .field("has_note", &self.note.is_some())
            .finish()
    }
}

impl From<wire::SavedAnnotation> for SavedAnnotationRow {
    fn from(value: wire::SavedAnnotation) -> Self {
        let label = value.label.filter(|text| !text.trim().is_empty());
        let note = value.note.filter(|text| !text.trim().is_empty());
        Self {
            uuid: value.uuid,
            kind: value.kind,
            range: UtcRange {
                start: value.start_utc_ms,
                end: value.end_utc_ms,
            },
            category: value.category,
            label_preset: value.label_preset,
            label,
            note,
        }
    }
}

/// Loaded annotations for one logical day, sorted by start UTC, end UTC, then UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnnotationList {
    pub(crate) date: wire::LocalDate,
    pub(crate) items: Vec<SavedAnnotationRow>,
    pub(crate) truncated: bool,
}

/// Inline confirmation state for deleting an annotation in edit mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DeleteConfirmState {
    #[default]
    Idle,
    Confirming,
}

/// Whether the editor is creating a new annotation or editing an existing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnnotationEditorMode {
    Create {
        /// Client-generated ID assigned once per editor session so retries are idempotent.
        session_id: Option<wire::AnnotationId>,
        kind: wire::AnnotationKind,
    },
    Edit {
        uuid: wire::AnnotationId,
        kind: wire::AnnotationKind,
        label_preset: Option<wire::AnnotationLabelPreset>,
        delete_confirm: DeleteConfirmState,
    },
}

impl AnnotationEditorMode {
    /// Returns the annotation kind for this editor session.
    #[must_use]
    pub(crate) const fn kind(&self) -> wire::AnnotationKind {
        match self {
            Self::Create { kind, .. } | Self::Edit { kind, .. } => *kind,
        }
    }
}

/// Active create or edit form state inside the Today annotations panel.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AnnotationDraft {
    pub(crate) mode: AnnotationEditorMode,
    pub(crate) start_input: String,
    pub(crate) end_input: String,
    pub(crate) start_fold: FoldChoice,
    pub(crate) end_fold: FoldChoice,
    pub(crate) category: Option<wire::Uuid>,
    pub(crate) label_input: String,
    pub(crate) note_input: String,
    pub(crate) validation_error: Option<ValidateAnnotationDraftError>,
    pub(crate) mutation_error: Option<AnnotationWriteNotice>,
}

impl fmt::Debug for AnnotationDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnnotationDraft")
            .field("mode", &self.mode)
            .field("start_input", &self.start_input)
            .field("end_input", &self.end_input)
            .field("start_fold", &self.start_fold)
            .field("end_fold", &self.end_fold)
            .field("category", &self.category)
            .field("label_scalars", &self.label_input.chars().count())
            .field("note_scalars", &self.note_input.chars().count())
            .field("validation_error", &self.validation_error)
            .field("mutation_error", &self.mutation_error)
            .finish()
    }
}

/// Active tab of the Today right-hand side panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TodayPanelTab {
    #[default]
    Details,
    Annotations,
}

/// Whether the Today annotations panel itself keeps the right-hand side panel open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PanelVisibility {
    #[default]
    Closed,
    Open,
}

/// Side-effect returned by [`AnnotationEditor`] state transitions for `AppShell` to execute.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum AnnotationEditorEffect {
    None,
    Query {
        date: wire::LocalDate,
        range: UtcRange,
        generation: AnnotationsGeneration,
    },
    Create {
        request: wire::AnnotationsCreateRequest,
        date: wire::LocalDate,
    },
    Update {
        request: wire::AnnotationsUpdateRequest,
        date: wire::LocalDate,
    },
    Delete {
        request: wire::AnnotationsDeleteRequest,
        date: wire::LocalDate,
    },
}

impl fmt::Debug for AnnotationEditorEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::Query {
                date,
                range,
                generation,
            } => formatter
                .debug_struct("Query")
                .field("date", date)
                .field("range", range)
                .field("generation", generation)
                .finish(),
            Self::Create { request, date } => formatter
                .debug_struct("Create")
                .field("date", date)
                .field("count", &request.entities.len())
                .finish(),
            Self::Update { request, date } => formatter
                .debug_struct("Update")
                .field("date", date)
                .field("count", &request.entities.len())
                .finish(),
            Self::Delete { request, date } => formatter
                .debug_struct("Delete")
                .field("date", date)
                .field("count", &request.uuids.len())
                .finish(),
        }
    }
}

/// View model for the Today saved-annotations list and create/edit/delete editor.
#[derive(Debug)]
pub(crate) struct AnnotationEditor {
    visibility: PanelVisibility,
    active_tab: TodayPanelTab,
    day_context: Option<DayTimeContext>,
    generation: AnnotationsGeneration,
    list_state: Option<LoadState<AnnotationList>>,
    draft: Option<AnnotationDraft>,
    mutating: MutationActivity,
}

impl AnnotationEditor {
    /// Creates an initially closed annotations model with no day loaded.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            visibility: PanelVisibility::Closed,
            active_tab: TodayPanelTab::Details,
            day_context: None,
            generation: AnnotationsGeneration::default(),
            list_state: None,
            draft: None,
            mutating: MutationActivity::Idle,
        }
    }

    /// Whether the Today annotations panel is currently open.
    #[must_use]
    pub(crate) const fn is_panel_open(&self) -> bool {
        matches!(self.visibility, PanelVisibility::Open)
    }

    /// Which tab of the Today side panel is active.
    #[must_use]
    pub(crate) const fn active_tab(&self) -> TodayPanelTab {
        self.active_tab
    }

    /// Current logical-day time context, when a live day is loaded.
    #[must_use]
    pub(crate) const fn day_context(&self) -> Option<&DayTimeContext> {
        self.day_context.as_ref()
    }

    /// Current annotations list load state.
    #[must_use]
    pub(crate) const fn list_state(&self) -> Option<&LoadState<AnnotationList>> {
        self.list_state.as_ref()
    }

    /// Loaded annotations list when ready.
    #[must_use]
    pub(crate) fn list(&self) -> Option<&AnnotationList> {
        match &self.list_state {
            Some(LoadState::Ready(list)) => Some(list),
            _ => None,
        }
    }

    /// Active create/edit form draft, if open.
    #[must_use]
    pub(crate) const fn draft(&self) -> Option<&AnnotationDraft> {
        self.draft.as_ref()
    }

    /// Whether a create, update, or delete request is currently in flight.
    #[must_use]
    pub(crate) const fn mutating(&self) -> MutationActivity {
        self.mutating
    }

    /// Updates the shown logical day context (on day load or date switch). Changing dates
    /// invalidates in-flight annotation queries and closes any open draft for the old day.
    #[must_use]
    pub(crate) fn set_day_context(
        &mut self,
        context: Option<DayTimeContext>,
    ) -> AnnotationEditorEffect {
        let Some(next) = context else {
            self.generation = self.generation.next();
            self.day_context = None;
            self.list_state = None;
            self.draft = None;
            self.mutating = MutationActivity::Idle;
            return AnnotationEditorEffect::None;
        };
        let date_changed = self
            .day_context
            .as_ref()
            .is_none_or(|current| current.date != next.date);
        let bounds_changed = self
            .day_context
            .as_ref()
            .is_none_or(|current| current.bounds != next.bounds);
        self.day_context = Some(next);
        if date_changed {
            self.generation = self.generation.next();
            self.draft = None;
            self.mutating = MutationActivity::Idle;
            self.list_state = None;
        }
        if self.is_panel_open()
            && self.active_tab == TodayPanelTab::Annotations
            && (date_changed
                || bounds_changed
                || matches!(
                    self.list_state,
                    None | Some(LoadState::Stale | LoadState::Failed(_))
                ))
        {
            return self.query_current_day();
        }
        AnnotationEditorEffect::None
    }

    /// Opens the Today side panel on the Annotations tab and loads the shown day's annotations
    /// if needed.
    #[must_use]
    pub(crate) fn open_panel(&mut self) -> AnnotationEditorEffect {
        self.visibility = PanelVisibility::Open;
        self.active_tab = TodayPanelTab::Annotations;
        self.load_if_needed()
    }

    /// Switches the Today side panel tab between Details and Annotations.
    #[must_use]
    pub(crate) fn select_tab(&mut self, tab: TodayPanelTab) -> AnnotationEditorEffect {
        self.active_tab = tab;
        match tab {
            TodayPanelTab::Details => AnnotationEditorEffect::None,
            TodayPanelTab::Annotations => {
                self.visibility = PanelVisibility::Open;
                self.load_if_needed()
            }
        }
    }

    /// Closes the Today annotations panel and clears any draft (ignored while a write is busy).
    pub(crate) fn close_panel(&mut self) {
        if self.mutating == MutationActivity::Busy {
            return;
        }
        self.visibility = PanelVisibility::Closed;
        self.active_tab = TodayPanelTab::Details;
        self.draft = None;
    }

    /// Forces a fresh `query.annotations` load for the currently shown logical day.
    #[must_use]
    pub(crate) fn refresh(&mut self) -> AnnotationEditorEffect {
        self.query_current_day()
    }

    /// Handles a `day` topic notification from the core link. Refreshes when `date` matches the
    /// shown day and annotations are visible; otherwise marks the cached list stale.
    #[must_use]
    pub(crate) fn on_day_changed(&mut self, date: wire::LocalDate) -> AnnotationEditorEffect {
        let Some(ctx) = &self.day_context else {
            return AnnotationEditorEffect::None;
        };
        if ctx.date != date {
            return AnnotationEditorEffect::None;
        }
        if self.is_panel_open() && self.active_tab == TodayPanelTab::Annotations {
            self.query_current_day()
        } else {
            if self.list_state.is_some() {
                self.list_state = Some(LoadState::Stale);
            }
            AnnotationEditorEffect::None
        }
    }

    /// Applies the response of a `query.annotations` call, ignoring late responses for older
    /// generations or earlier days.
    pub(crate) fn apply_loaded(
        &mut self,
        generation: AnnotationsGeneration,
        date: wire::LocalDate,
        result: Result<wire::QueryAnnotationsResponse, LoadDataError>,
    ) {
        if !self.generation.accepts(generation) {
            return;
        }
        if self.day_context.as_ref().is_none_or(|ctx| ctx.date != date) {
            return;
        }
        self.list_state = Some(match result {
            Ok(response) => {
                let mut items: Vec<SavedAnnotationRow> = response
                    .entities
                    .into_iter()
                    .map(SavedAnnotationRow::from)
                    .collect();
                items.sort_by(|left, right| {
                    left.range
                        .start
                        .cmp(&right.range.start)
                        .then_with(|| left.range.end.cmp(&right.range.end))
                        .then_with(|| left.uuid.cmp(&right.uuid))
                });
                LoadState::Ready(AnnotationList {
                    date,
                    items,
                    truncated: response.truncated,
                })
            }
            Err(LoadDataError::Stale) => LoadState::Stale,
            Err(error) => LoadState::Failed(error),
        });
    }

    /// Opens the annotation editor in create mode for `range` and optional preselected `category`.
    ///
    /// Exposed at the model level so timeline selection (H14) and quick classification (H15) can
    /// seed the editor directly.
    pub(crate) fn open_create(&mut self, range: UtcRange, category: Option<wire::CategoryUuid>) {
        if self.mutating == MutationActivity::Busy {
            return;
        }
        self.visibility = PanelVisibility::Open;
        self.active_tab = TodayPanelTab::Annotations;
        let category_uuid = category.and_then(|value| value.0.parse::<wire::Uuid>().ok());
        let kind = if category_uuid.is_some() {
            wire::AnnotationKind::CategoryOverride
        } else {
            wire::AnnotationKind::Note
        };
        let (start_input, end_input, start_fold, end_fold) = match &self.day_context {
            Some(ctx) => (
                ctx.format_endpoint_hm(range.start, EndpointRole::Start),
                ctx.format_endpoint_hm(range.end, EndpointRole::End),
                ctx.detect_fold_choice(range.start),
                ctx.detect_fold_choice(range.end),
            ),
            None => (
                format_utc_hm(range.start),
                format_utc_hm(range.end),
                FoldChoice::Earlier,
                FoldChoice::Earlier,
            ),
        };
        self.draft = Some(AnnotationDraft {
            mode: AnnotationEditorMode::Create {
                session_id: None,
                kind,
            },
            start_input,
            end_input,
            start_fold,
            end_fold,
            category: category_uuid,
            label_input: String::new(),
            note_input: String::new(),
            validation_error: None,
            mutation_error: None,
        });
    }

    /// Opens create mode with a default range inside the shown logical day (used by the list's
    /// "New annotation" button).
    #[cfg(windows)]
    pub(crate) fn open_create_default(&mut self) {
        let Some(ctx) = &self.day_context else {
            return;
        };
        let range = ctx.default_create_range();
        self.open_create(range, None);
    }

    /// Opens the annotation editor in edit mode for an existing saved annotation from the list.
    pub(crate) fn open_edit(
        &mut self,
        uuid: &wire::AnnotationId,
        catalog: Option<&CategoryCatalog>,
    ) {
        if self.mutating == MutationActivity::Busy {
            return;
        }
        let Some(row) = self
            .list()
            .and_then(|list| list.items.iter().find(|item| &item.uuid == uuid))
            .cloned()
        else {
            return;
        };
        self.visibility = PanelVisibility::Open;
        self.active_tab = TodayPanelTab::Annotations;
        let (start_input, end_input, start_fold, end_fold) = match &self.day_context {
            Some(ctx) => (
                ctx.format_endpoint_hm(row.range.start, EndpointRole::Start),
                ctx.format_endpoint_hm(row.range.end, EndpointRole::End),
                ctx.detect_fold_choice(row.range.start),
                ctx.detect_fold_choice(row.range.end),
            ),
            None => (
                format_utc_hm(row.range.start),
                format_utc_hm(row.range.end),
                FoldChoice::Earlier,
                FoldChoice::Earlier,
            ),
        };
        let category = row.category.as_ref().and_then(|cat| match cat {
            wire::AnnotationCategory::Uuid { uuid } => Some(uuid.clone()),
            wire::AnnotationCategory::Builtin { key } => {
                let builtin = builtin_from_wire(*key);
                catalog.and_then(|items| {
                    items
                        .items
                        .iter()
                        .find(|item| item.origin == CategoryOrigin::Builtin(builtin))
                        .map(|item| item.uuid.clone())
                })
            }
        });
        self.draft = Some(AnnotationDraft {
            mode: AnnotationEditorMode::Edit {
                uuid: row.uuid,
                kind: row.kind,
                label_preset: row.label_preset,
                delete_confirm: DeleteConfirmState::Idle,
            },
            start_input,
            end_input,
            start_fold,
            end_fold,
            category,
            label_input: row.label.unwrap_or_default(),
            note_input: row.note.unwrap_or_default(),
            validation_error: None,
            mutation_error: None,
        });
    }

    /// Closes the active create/edit draft and returns to the annotations list.
    pub(crate) fn close_draft(&mut self) {
        if self.mutating == MutationActivity::Busy {
            return;
        }
        self.draft = None;
    }

    /// Changes the annotation kind while in create mode (kind is fixed after creation).
    pub(crate) fn set_kind(&mut self, next_kind: wire::AnnotationKind) {
        if self.mutating == MutationActivity::Busy {
            return;
        }
        if let Some(draft) = &mut self.draft
            && let AnnotationEditorMode::Create { kind, .. } = &mut draft.mode
        {
            *kind = next_kind;
            draft.validation_error = None;
            draft.mutation_error = None;
        }
    }

    /// Updates the start time `HH:MM` input string.
    pub(crate) fn set_start_input(&mut self, value: String) {
        if let Some(draft) = &mut self.draft
            && draft.start_input != value
        {
            draft.start_input = value;
            draft.validation_error = None;
            draft.mutation_error = None;
        }
    }

    /// Updates the end time `HH:MM` input string.
    pub(crate) fn set_end_input(&mut self, value: String) {
        if let Some(draft) = &mut self.draft
            && draft.end_input != value
        {
            draft.end_input = value;
            draft.validation_error = None;
            draft.mutation_error = None;
        }
    }

    /// Selects the DST fold disambiguation choice for the start or end endpoint.
    pub(crate) fn set_fold_choice(&mut self, role: EndpointRole, choice: FoldChoice) {
        if let Some(draft) = &mut self.draft {
            match role {
                EndpointRole::Start => draft.start_fold = choice,
                EndpointRole::End => draft.end_fold = choice,
            }
            draft.validation_error = None;
            draft.mutation_error = None;
        }
    }

    /// Selects an active category UUID or clears the category (`None`).
    pub(crate) fn set_category(&mut self, category: Option<wire::Uuid>) {
        if let Some(draft) = &mut self.draft {
            draft.category = category;
            draft.validation_error = None;
            draft.mutation_error = None;
        }
    }

    /// Updates the label input string.
    pub(crate) fn set_label_input(&mut self, value: String) {
        if let Some(draft) = &mut self.draft
            && draft.label_input != value
        {
            draft.label_input = value;
            draft.validation_error = None;
            draft.mutation_error = None;
        }
    }

    /// Updates the multi-line note input string.
    pub(crate) fn set_note_input(&mut self, value: String) {
        if let Some(draft) = &mut self.draft
            && draft.note_input != value
        {
            draft.note_input = value;
            draft.validation_error = None;
            draft.mutation_error = None;
        }
    }

    /// Validates the active draft and produces an `annotations.create` or `annotations.update`
    /// effect. In create mode, `generate_id` is invoked at most once per editor session so retries
    /// reuse the same [`wire::AnnotationId`].
    #[must_use]
    pub(crate) fn submit(
        &mut self,
        generate_id: impl FnOnce() -> Option<wire::AnnotationId>,
    ) -> AnnotationEditorEffect {
        if self.mutating == MutationActivity::Busy {
            return AnnotationEditorEffect::None;
        }
        let Some(ctx) = self.day_context.clone() else {
            if let Some(draft) = &mut self.draft {
                draft.validation_error = Some(ValidateAnnotationDraftError::MissingDayContext);
            }
            return AnnotationEditorEffect::None;
        };
        let Some(draft) = &mut self.draft else {
            return AnnotationEditorEffect::None;
        };

        let range = match ctx.parse_time_range(
            &draft.start_input,
            &draft.end_input,
            draft.start_fold,
            draft.end_fold,
        ) {
            Ok(range) => range,
            Err(source) => {
                draft.validation_error = Some(ValidateAnnotationDraftError::Time { source });
                return AnnotationEditorEffect::None;
            }
        };

        if draft.label_input.chars().count() > wire::ANNOTATION_MAX_LABEL_SCALARS {
            draft.validation_error = Some(ValidateAnnotationDraftError::LabelTooLong);
            return AnnotationEditorEffect::None;
        }
        if draft.note_input.chars().count() > wire::ANNOTATION_MAX_NOTE_SCALARS {
            draft.validation_error = Some(ValidateAnnotationDraftError::NoteTooLong);
            return AnnotationEditorEffect::None;
        }

        let kind = draft.mode.kind();
        let trimmed_label = draft.label_input.trim();
        let label = (!trimmed_label.is_empty()).then(|| trimmed_label.to_owned());
        let note = (!draft.note_input.trim().is_empty()).then(|| draft.note_input.clone());
        let preserved_preset = match &draft.mode {
            AnnotationEditorMode::Edit {
                label_preset: Some(preset),
                ..
            } if kind == wire::AnnotationKind::Note && label.is_none() => Some(*preset),
            _ => None,
        };

        let (category, final_label, final_note, label_preset) = match kind {
            wire::AnnotationKind::CategoryOverride => {
                let Some(uuid) = draft.category.clone() else {
                    draft.validation_error = Some(ValidateAnnotationDraftError::CategoryRequired);
                    return AnnotationEditorEffect::None;
                };
                (
                    Some(wire::AnnotationCategory::Uuid { uuid }),
                    label,
                    note,
                    None,
                )
            }
            wire::AnnotationKind::Note => {
                if preserved_preset.is_none() && label.is_none() && note.is_none() {
                    draft.validation_error =
                        Some(ValidateAnnotationDraftError::NoteContentRequired);
                    return AnnotationEditorEffect::None;
                }
                (
                    draft
                        .category
                        .clone()
                        .map(|uuid| wire::AnnotationCategory::Uuid { uuid }),
                    label,
                    note,
                    preserved_preset,
                )
            }
            wire::AnnotationKind::OfflineActivity | wire::AnnotationKind::ActiveOverride => (
                draft
                    .category
                    .clone()
                    .map(|uuid| wire::AnnotationCategory::Uuid { uuid }),
                label,
                note,
                None,
            ),
            wire::AnnotationKind::DismissedAway => (None, None, None, None),
        };

        let values = wire::AnnotationValues {
            start_utc_ms: range.start,
            end_utc_ms: range.end,
            category,
            label_preset,
            label: final_label,
            note: final_note,
        };

        draft.validation_error = None;
        draft.mutation_error = None;

        match &mut draft.mode {
            AnnotationEditorMode::Create { session_id, kind } => {
                let uuid = match session_id {
                    Some(existing) => existing.clone(),
                    None => match generate_id() {
                        Some(generated) => {
                            *session_id = Some(generated.clone());
                            generated
                        }
                        None => {
                            draft.mutation_error = Some(AnnotationWriteNotice::Generic {
                                code: LoadDataErrorCode::Local(LocalFailureCode::InvalidResponse),
                            });
                            return AnnotationEditorEffect::None;
                        }
                    },
                };
                self.mutating = MutationActivity::Busy;
                AnnotationEditorEffect::Create {
                    request: wire::AnnotationsCreateRequest {
                        entities: vec![wire::CreateAnnotation {
                            uuid,
                            kind: *kind,
                            values,
                        }],
                    },
                    date: ctx.date,
                }
            }
            AnnotationEditorMode::Edit {
                uuid,
                kind,
                delete_confirm,
                ..
            } => {
                *delete_confirm = DeleteConfirmState::Idle;
                let uuid = uuid.clone();
                let kind = *kind;
                self.mutating = MutationActivity::Busy;
                AnnotationEditorEffect::Update {
                    request: wire::AnnotationsUpdateRequest {
                        entities: vec![wire::UpdateAnnotation {
                            uuid,
                            kind: Some(kind),
                            values,
                        }],
                    },
                    date: ctx.date,
                }
            }
        }
    }

    /// Arms inline delete confirmation while editing an existing annotation. Does not delete.
    pub(crate) fn request_delete(&mut self) {
        if self.mutating == MutationActivity::Busy {
            return;
        }
        if let Some(draft) = &mut self.draft
            && let AnnotationEditorMode::Edit { delete_confirm, .. } = &mut draft.mode
        {
            *delete_confirm = DeleteConfirmState::Confirming;
        }
    }

    /// Cancels inline delete confirmation without closing the editor.
    pub(crate) fn cancel_delete(&mut self) {
        if let Some(draft) = &mut self.draft
            && let AnnotationEditorMode::Edit { delete_confirm, .. } = &mut draft.mode
        {
            *delete_confirm = DeleteConfirmState::Idle;
        }
    }

    /// Emits `annotations.delete` only when inline confirmation is armed in edit mode.
    #[must_use]
    pub(crate) fn confirm_delete(&mut self) -> AnnotationEditorEffect {
        if self.mutating == MutationActivity::Busy {
            return AnnotationEditorEffect::None;
        }
        let Some(date) = self.day_context.as_ref().map(|ctx| ctx.date) else {
            return AnnotationEditorEffect::None;
        };
        let Some(draft) = &mut self.draft else {
            return AnnotationEditorEffect::None;
        };
        let AnnotationEditorMode::Edit {
            uuid,
            delete_confirm,
            ..
        } = &draft.mode
        else {
            return AnnotationEditorEffect::None;
        };
        if *delete_confirm != DeleteConfirmState::Confirming {
            return AnnotationEditorEffect::None;
        }
        let uuid = uuid.clone();
        draft.mutation_error = None;
        self.mutating = MutationActivity::Busy;
        AnnotationEditorEffect::Delete {
            request: wire::AnnotationsDeleteRequest { uuids: vec![uuid] },
            date,
        }
    }

    /// Finishes an in-flight create, update, or delete write. On success, closes the draft and
    /// refreshes the shown day's annotations; on error, preserves inputs and session ID for retry.
    #[must_use]
    pub(crate) fn apply_write_finished(
        &mut self,
        date: wire::LocalDate,
        result: Result<(), LoadDataError>,
    ) -> AnnotationEditorEffect {
        self.mutating = MutationActivity::Idle;
        match result {
            Ok(()) => {
                self.draft = None;
                if self
                    .day_context
                    .as_ref()
                    .is_some_and(|ctx| ctx.date == date)
                {
                    self.query_current_day()
                } else {
                    AnnotationEditorEffect::None
                }
            }
            Err(error) => {
                if let Some(draft) = &mut self.draft {
                    if let AnnotationEditorMode::Edit { delete_confirm, .. } = &mut draft.mode {
                        *delete_confirm = DeleteConfirmState::Idle;
                    }
                    draft.mutation_error = Some(AnnotationWriteNotice::from_error(&error));
                }
                AnnotationEditorEffect::None
            }
        }
    }

    fn load_if_needed(&mut self) -> AnnotationEditorEffect {
        let needs_load = match (&self.day_context, &self.list_state) {
            (None, _) => false,
            (Some(_), None | Some(LoadState::Stale | LoadState::Failed(_))) => true,
            (Some(ctx), Some(LoadState::Ready(list))) => list.date != ctx.date,
            (Some(_), Some(LoadState::Loading)) => false,
        };
        if needs_load {
            self.query_current_day()
        } else {
            AnnotationEditorEffect::None
        }
    }

    fn query_current_day(&mut self) -> AnnotationEditorEffect {
        let Some(ctx) = &self.day_context else {
            return AnnotationEditorEffect::None;
        };
        self.generation = self.generation.next();
        self.list_state = Some(LoadState::Loading);
        AnnotationEditorEffect::Query {
            date: ctx.date,
            range: ctx.bounds,
            generation: self.generation,
        }
    }
}

fn format_utc_hm(utc_ms: i64) -> String {
    match jiff::Timestamp::from_millisecond(utc_ms) {
        Ok(ts) => {
            let utc = ts.to_zoned_datetime(jiff::tz::TimeZone::UTC);
            format!("{:02}:{:02}", utc.hour(), utc.minute())
        }
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::RequestCoreError;
    use quartz_pipe::CallCoreError;

    const CAT_DEV_UUID: &str = "11111111-1111-4111-8111-111111111111";
    const CAT_LEARN_UUID: &str = "22222222-2222-4222-8222-222222222222";
    const ANNO_ID_1: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const ANNO_ID_2: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    fn utc_day_context(date_str: &str) -> DayTimeContext {
        let date: wire::LocalDate = date_str.parse().expect("date");
        let start = date
            .at(0, 0, 0, 0)
            .to_zoned_datetime(jiff::tz::TimeZone::UTC)
            .expect("day start")
            .timestamp()
            .as_millisecond();
        let end = date
            .tomorrow()
            .expect("tomorrow")
            .at(0, 0, 0, 0)
            .to_zoned_datetime(jiff::tz::TimeZone::UTC)
            .expect("day end")
            .timestamp()
            .as_millisecond();
        DayTimeContext {
            date,
            bounds: UtcRange { start, end },
            zone: jiff::tz::TimeZone::UTC,
        }
    }

    fn zone_day_context(date_str: &str, tz_name: &str) -> DayTimeContext {
        let date: wire::LocalDate = date_str.parse().expect("date");
        let zone = jiff::tz::TimeZone::get(tz_name).expect("IANA time zone");
        let start = date
            .at(0, 0, 0, 0)
            .to_zoned_datetime(zone.clone())
            .expect("start")
            .timestamp()
            .as_millisecond();
        let end = date
            .tomorrow()
            .expect("tomorrow")
            .at(0, 0, 0, 0)
            .to_zoned_datetime(zone.clone())
            .expect("end")
            .timestamp()
            .as_millisecond();
        DayTimeContext {
            date,
            bounds: UtcRange { start, end },
            zone,
        }
    }

    fn sample_id(text: &str) -> wire::AnnotationId {
        wire::AnnotationId(text.parse().expect("UUID"))
    }

    fn sample_catalog() -> CategoryCatalog {
        let response = wire::CategoriesListResponse {
            entities: vec![
                wire::EditableCategory {
                    uuid: CAT_DEV_UUID.parse().expect("uuid"),
                    builtin_key: Some(wire::BuiltinCategory::Development),
                    parent_uuid: None,
                    name: None,
                    color: "cat.8".into(),
                    tendency: wire::CategoryTendency::Focus,
                    sort_order: 0,
                    archived: false,
                },
                wire::EditableCategory {
                    uuid: CAT_LEARN_UUID.parse().expect("uuid"),
                    builtin_key: Some(wire::BuiltinCategory::Learning),
                    parent_uuid: None,
                    name: None,
                    color: "cat.1".into(),
                    tendency: wire::CategoryTendency::Focus,
                    sort_order: 1,
                    archived: true,
                },
            ],
            versions: wire::QueryVersions("v1".into()),
            truncated: false,
        };
        CategoryCatalog::from_protocol(response).expect("catalog")
    }

    #[test]
    fn time_input_validation_rejects_inverted_outside_day_and_dst_gap() {
        let utc_ctx = utc_day_context("2026-11-21");
        assert_eq!(
            utc_ctx.parse_time_range("14:00", "13:00", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::EndNotAfterStart)
        );
        assert_eq!(
            utc_ctx.parse_time_range("14:00", "14:00", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::EndNotAfterStart)
        );
        assert_eq!(
            utc_ctx.parse_time_range("24:00", "24:00", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::OutsideDay)
        );
        assert_eq!(
            utc_ctx.parse_time_range("09:00", "25:00", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::OutsideDay)
        );
        assert_eq!(
            utc_ctx.parse_time_range("09:00", "24:30", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::OutsideDay)
        );
        assert_eq!(
            utc_ctx.parse_time_range("09:60", "10:00", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::InvalidFormat)
        );
        assert_eq!(
            utc_ctx.parse_time_range("bad", "10:00", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::InvalidFormat)
        );
        let valid_end_of_day = utc_ctx
            .parse_time_range("23:00", "24:00", FoldChoice::Earlier, FoldChoice::Earlier)
            .expect("24:00 is valid exclusive end");
        assert_eq!(valid_end_of_day.end, utc_ctx.bounds.end);

        // Partial day bounds reject local times outside [bounds.start, bounds.end].
        let narrow = DayTimeContext {
            date: utc_ctx.date,
            bounds: UtcRange {
                start: utc_ctx.bounds.start + 8 * 60 * MILLIS_PER_MINUTE,
                end: utc_ctx.bounds.start + 20 * 60 * MILLIS_PER_MINUTE,
            },
            zone: jiff::tz::TimeZone::UTC,
        };
        assert_eq!(
            narrow.parse_time_range("07:30", "09:00", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::OutsideDay)
        );
        assert_eq!(
            narrow.parse_time_range("19:00", "21:00", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::OutsideDay)
        );

        // America/Los_Angeles spring-forward gap on 2026-03-08 skips 02:00..03:00.
        let spring = zone_day_context("2026-03-08", "America/Los_Angeles");
        assert_eq!(
            spring.parse_time_range("02:30", "03:30", FoldChoice::Earlier, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::DstGap)
        );
        let across_gap = spring
            .parse_time_range("01:30", "03:30", FoldChoice::Earlier, FoldChoice::Earlier)
            .expect("range spanning DST gap");
        assert_eq!(across_gap.end - across_gap.start, 60 * MILLIS_PER_MINUTE);
    }

    #[test]
    fn dst_fold_disambiguates_earlier_and_later_occurrences() {
        let fall = zone_day_context("2026-11-01", "America/Los_Angeles");
        let offsets = fall
            .fold_offsets_for_input("01:20", EndpointRole::Start)
            .expect("repeated hour has two offsets");
        assert_ne!(offsets.0, offsets.1);

        let span = fall
            .parse_time_range("01:20", "01:45", FoldChoice::Earlier, FoldChoice::Later)
            .expect("earlier -07 to later -08");
        assert_eq!(span.end - span.start, 85 * MILLIS_PER_MINUTE);
        assert_eq!(fall.detect_fold_choice(span.start), FoldChoice::Earlier);
        assert_eq!(fall.detect_fold_choice(span.end), FoldChoice::Later);
        assert_eq!(
            fall.parse_time_range("01:45", "01:20", FoldChoice::Later, FoldChoice::Earlier),
            Err(ValidateAnnotationTimeError::EndNotAfterStart)
        );
    }

    #[test]
    fn label_and_note_limits_are_counted_in_unicode_scalars() {
        let ctx = utc_day_context("2026-11-21");
        let mut editor = AnnotationEditor::new();
        let _ = editor.set_day_context(Some(ctx.clone()));
        let range = ctx.default_create_range();
        editor.open_create(range, None);

        // 256 multi-byte scalars (4 UTF-8 bytes each) must be accepted; 257 must fail.
        let exact_label: String = "🦀".repeat(wire::ANNOTATION_MAX_LABEL_SCALARS);
        editor.set_label_input(exact_label);
        let mut next_id = Some(sample_id(ANNO_ID_1));
        assert!(matches!(
            editor.submit(|| next_id.take()),
            AnnotationEditorEffect::Create { .. }
        ));
        let _ = editor.apply_write_finished(ctx.date, Ok(()));

        editor.open_create(range, None);
        editor.set_label_input("🦀".repeat(wire::ANNOTATION_MAX_LABEL_SCALARS + 1));
        assert_eq!(
            editor.submit(|| Some(sample_id(ANNO_ID_2))),
            AnnotationEditorEffect::None
        );
        assert_eq!(
            editor.draft().and_then(|draft| draft.validation_error),
            Some(ValidateAnnotationDraftError::LabelTooLong)
        );

        // 4096 multi-byte scalars in note must pass; 4097 must fail.
        editor.set_label_input(String::new());
        editor.set_note_input("注".repeat(wire::ANNOTATION_MAX_NOTE_SCALARS + 1));
        assert_eq!(
            editor.submit(|| Some(sample_id(ANNO_ID_2))),
            AnnotationEditorEffect::None
        );
        assert_eq!(
            editor.draft().and_then(|draft| draft.validation_error),
            Some(ValidateAnnotationDraftError::NoteTooLong)
        );

        editor.set_note_input("注".repeat(wire::ANNOTATION_MAX_NOTE_SCALARS));
        assert!(matches!(
            editor.submit(|| Some(sample_id(ANNO_ID_2))),
            AnnotationEditorEffect::Create { .. }
        ));
    }

    #[test]
    fn create_reuses_same_annotation_id_on_retry_and_uses_new_id_next_session() {
        let ctx = utc_day_context("2026-11-21");
        let mut editor = AnnotationEditor::new();
        let _ = editor.set_day_context(Some(ctx.clone()));
        editor.open_create(
            ctx.default_create_range(),
            Some(wire::CategoryUuid(CAT_DEV_UUID.to_owned())),
        );
        editor.set_label_input("Deep work".into());

        let mut generated = vec![sample_id(ANNO_ID_1), sample_id(ANNO_ID_2)].into_iter();
        let first = editor.submit(|| generated.next());
        let AnnotationEditorEffect::Create { request: req1, .. } = first else {
            panic!("expected create effect");
        };
        let first_uuid = req1.entities.first().expect("entity").uuid.clone();
        assert_eq!(first_uuid, sample_id(ANNO_ID_1));

        // Simulate core busy failure; draft stays open and retry must reuse ANNO_ID_1.
        let _ =
            editor.apply_write_finished(ctx.date, Err(LoadDataError::from(RequestCoreError::Busy)));
        assert!(matches!(
            editor
                .draft()
                .and_then(|draft| draft.mutation_error.as_ref()),
            Some(AnnotationWriteNotice::Busy { .. })
        ));

        let retry = editor.submit(|| generated.next());
        let AnnotationEditorEffect::Create { request: req2, .. } = retry else {
            panic!("expected create retry effect");
        };
        assert_eq!(req2.entities.first().expect("entity").uuid, first_uuid);

        // Complete write and start a new create session; it must allocate ANNO_ID_2.
        let refresh = editor.apply_write_finished(ctx.date, Ok(()));
        assert!(matches!(refresh, AnnotationEditorEffect::Query { .. }));
        assert!(editor.draft().is_none());

        editor.open_create(ctx.default_create_range(), None);
        editor.set_note_input("Second session".into());
        let next = editor.submit(|| generated.next());
        let AnnotationEditorEffect::Create { request: req3, .. } = next else {
            panic!("expected second session create");
        };
        assert_eq!(
            req3.entities.first().expect("entity").uuid,
            sample_id(ANNO_ID_2)
        );
    }

    #[test]
    fn edit_sends_update_with_unchanged_id_and_locked_kind() {
        let ctx = utc_day_context("2026-11-21");
        let mut editor = AnnotationEditor::new();
        let _ = editor.set_day_context(Some(ctx.clone()));
        let AnnotationEditorEffect::Query { generation, .. } = editor.open_panel() else {
            panic!("expected initial query");
        };
        let saved = wire::SavedAnnotation {
            uuid: sample_id(ANNO_ID_1),
            kind: wire::AnnotationKind::CategoryOverride,
            start_utc_ms: ctx.bounds.start + 9 * 60 * MILLIS_PER_MINUTE,
            end_utc_ms: ctx.bounds.start + 10 * 60 * MILLIS_PER_MINUTE,
            category: Some(wire::AnnotationCategory::Uuid {
                uuid: CAT_DEV_UUID.parse().expect("uuid"),
            }),
            label_preset: None,
            label: Some("Old label".into()),
            note: Some("Old note".into()),
            created_utc_ms: 100,
            updated_utc_ms: 200,
        };
        editor.apply_loaded(
            generation,
            ctx.date,
            Ok(wire::QueryAnnotationsResponse {
                entities: vec![saved],
                versions: wire::QueryVersions("v1".into()),
                truncated: false,
            }),
        );

        let catalog = sample_catalog();
        editor.open_edit(&sample_id(ANNO_ID_1), Some(&catalog));
        editor.set_kind(wire::AnnotationKind::Note); // Must be ignored in edit mode.
        editor.set_end_input("10:30".into());
        editor.set_label_input("Updated label".into());

        let effect = editor.submit(|| panic!("edit must not allocate a new ID"));
        let AnnotationEditorEffect::Update { request, .. } = effect else {
            panic!("expected update effect");
        };
        let entity = request.entities.first().expect("update entity");
        assert_eq!(entity.uuid, sample_id(ANNO_ID_1));
        assert_eq!(entity.kind, Some(wire::AnnotationKind::CategoryOverride));
        assert_eq!(
            entity.values.end_utc_ms,
            ctx.bounds.start + (10 * 60 + 30) * MILLIS_PER_MINUTE
        );
        assert_eq!(entity.values.label.as_deref(), Some("Updated label"));
    }

    #[test]
    fn delete_requires_inline_confirmation_before_emitting_request() {
        let ctx = utc_day_context("2026-11-21");
        let mut editor = AnnotationEditor::new();
        let _ = editor.set_day_context(Some(ctx.clone()));
        let AnnotationEditorEffect::Query { generation, .. } = editor.open_panel() else {
            panic!("expected query");
        };
        editor.apply_loaded(
            generation,
            ctx.date,
            Ok(wire::QueryAnnotationsResponse {
                entities: vec![wire::SavedAnnotation {
                    uuid: sample_id(ANNO_ID_1),
                    kind: wire::AnnotationKind::Note,
                    start_utc_ms: ctx.bounds.start + 60 * MILLIS_PER_MINUTE,
                    end_utc_ms: ctx.bounds.start + 90 * MILLIS_PER_MINUTE,
                    category: None,
                    label_preset: None,
                    label: Some("Note".into()),
                    note: None,
                    created_utc_ms: 1,
                    updated_utc_ms: 2,
                }],
                versions: wire::QueryVersions("v1".into()),
                truncated: false,
            }),
        );

        editor.open_edit(&sample_id(ANNO_ID_1), None);
        assert_eq!(editor.confirm_delete(), AnnotationEditorEffect::None);

        editor.request_delete();
        editor.cancel_delete();
        assert_eq!(editor.confirm_delete(), AnnotationEditorEffect::None);

        editor.request_delete();
        let effect = editor.confirm_delete();
        let AnnotationEditorEffect::Delete { request, date } = effect else {
            panic!("expected delete effect after confirm");
        };
        assert_eq!(date, ctx.date);
        assert_eq!(request.uuids, vec![sample_id(ANNO_ID_1)]);
    }

    #[test]
    fn late_annotations_response_for_earlier_day_is_ignored() {
        let day_one = utc_day_context("2026-11-20");
        let day_two = utc_day_context("2026-11-21");
        let mut editor = AnnotationEditor::new();
        let _ = editor.set_day_context(Some(day_one.clone()));
        let AnnotationEditorEffect::Query {
            generation: gen_day_one,
            ..
        } = editor.open_panel()
        else {
            panic!("expected day 1 query");
        };

        // Switch to day 2 before day 1's response arrives.
        let AnnotationEditorEffect::Query {
            generation: gen_day_two,
            ..
        } = editor.set_day_context(Some(day_two.clone()))
        else {
            panic!("expected day 2 query");
        };

        editor.apply_loaded(
            gen_day_one,
            day_one.date,
            Ok(wire::QueryAnnotationsResponse {
                entities: vec![wire::SavedAnnotation {
                    uuid: sample_id(ANNO_ID_1),
                    kind: wire::AnnotationKind::Note,
                    start_utc_ms: day_one.bounds.start,
                    end_utc_ms: day_one.bounds.start + 30 * MILLIS_PER_MINUTE,
                    category: None,
                    label_preset: None,
                    label: Some("Stale day 1".into()),
                    note: None,
                    created_utc_ms: 1,
                    updated_utc_ms: 1,
                }],
                versions: wire::QueryVersions("v1".into()),
                truncated: false,
            }),
        );
        assert!(
            matches!(editor.list_state(), Some(LoadState::Loading)),
            "late response from day 1 must not overwrite day 2 loading state"
        );

        // Even with the current generation, a mismatched date must still be ignored.
        editor.apply_loaded(
            gen_day_two,
            day_one.date,
            Ok(wire::QueryAnnotationsResponse {
                entities: Vec::new(),
                versions: wire::QueryVersions("v1".into()),
                truncated: false,
            }),
        );
        assert!(matches!(editor.list_state(), Some(LoadState::Loading)));

        editor.apply_loaded(
            gen_day_two,
            day_two.date,
            Ok(wire::QueryAnnotationsResponse {
                entities: Vec::new(),
                versions: wire::QueryVersions("v2".into()),
                truncated: false,
            }),
        );
        assert_eq!(editor.list().map(|list| list.date), Some(day_two.date));
    }

    #[test]
    fn write_errors_map_bad_request_stale_and_busy_to_distinct_notices() {
        let bad_req = LoadDataError::from(RequestCoreError::Call {
            source: CallCoreError::Protocol {
                kind: ErrorKind::BadRequest,
            },
        });
        assert!(matches!(
            AnnotationWriteNotice::from_error(&bad_req),
            AnnotationWriteNotice::BadRequest { .. }
        ));
        assert!(matches!(
            AnnotationWriteNotice::from_error(&LoadDataError::Stale),
            AnnotationWriteNotice::Stale { .. }
        ));
        let busy = LoadDataError::from(RequestCoreError::Call {
            source: CallCoreError::Protocol {
                kind: ErrorKind::Busy,
            },
        });
        assert!(matches!(
            AnnotationWriteNotice::from_error(&busy),
            AnnotationWriteNotice::Busy { .. }
        ));
    }

    #[test]
    fn category_resolution_and_note_excerpts_handle_missing_categories_and_scalars() {
        let catalog = sample_catalog();
        let active = resolve_annotation_category(
            &wire::AnnotationCategory::Uuid {
                uuid: CAT_DEV_UUID.parse().expect("uuid"),
            },
            Some(&catalog),
        );
        assert!(matches!(
            active,
            AnnotationCategoryDisplay::Known {
                archive_state: CategoryArchiveState::Active,
                ..
            }
        ));
        let missing = resolve_annotation_category(
            &wire::AnnotationCategory::Uuid {
                uuid: "99999999-9999-4999-8999-999999999999"
                    .parse()
                    .expect("uuid"),
            },
            Some(&catalog),
        );
        assert_eq!(missing, AnnotationCategoryDisplay::Missing);

        let long_note = format!("line 1\n\n{}", "字".repeat(ANNOTATION_NOTE_EXCERPT_SCALARS));
        let excerpt = format_note_excerpt(&long_note).expect("excerpt");
        assert!(excerpt.ends_with('…'));
        assert!(!excerpt.contains('\n'));
        assert_eq!(excerpt.chars().count(), ANNOTATION_NOTE_EXCERPT_SCALARS + 1);
    }

    #[test]
    fn day_topic_refresh_panel_controls_and_live_day_context_work_together() {
        let ctx = utc_day_context("2026-11-21");
        let other_date = ctx.date.tomorrow().expect("tomorrow");
        let mut editor = AnnotationEditor::new();
        assert!(!editor.is_panel_open());
        assert_eq!(editor.active_tab(), TodayPanelTab::Details);
        assert_eq!(editor.mutating(), MutationActivity::Idle);
        assert!(editor.day_context().is_none());

        let _ = editor.set_day_context(Some(ctx.clone()));
        assert_eq!(editor.day_context(), Some(&ctx));
        assert!(matches!(
            editor.select_tab(TodayPanelTab::Annotations),
            AnnotationEditorEffect::Query { .. }
        ));
        assert!(editor.is_panel_open());
        assert_eq!(editor.active_tab(), TodayPanelTab::Annotations);

        assert!(matches!(
            editor.on_day_changed(ctx.date),
            AnnotationEditorEffect::Query { .. }
        ));
        assert_eq!(
            editor.on_day_changed(other_date),
            AnnotationEditorEffect::None
        );
        assert!(matches!(
            editor.refresh(),
            AnnotationEditorEffect::Query { .. }
        ));

        editor.open_create(ctx.default_create_range(), None);
        editor.set_start_input("09:15".into());
        editor.set_fold_choice(EndpointRole::Start, FoldChoice::Earlier);
        editor.set_category(Some(CAT_DEV_UUID.parse().expect("uuid")));
        editor.close_draft();
        assert!(editor.draft().is_none());

        editor.close_panel();
        assert!(!editor.is_panel_open());
        assert_eq!(
            editor.on_day_changed(ctx.date),
            AnnotationEditorEffect::None
        );
        assert!(matches!(editor.list_state(), Some(LoadState::Stale)));

        let busy = AnnotationWriteNotice::from_error(&LoadDataError::from(RequestCoreError::Busy));
        assert_eq!(busy.code().as_str(), "busy");
        assert!(
            crate::model::Day::empty(ctx.date)
                .live_time_context()
                .is_none()
        );
    }
}
