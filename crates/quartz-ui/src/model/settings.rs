//! Settings page view model: General, Recording, Data, and About groups (07 section 7, 03 section 6).

use quartz_protocol as wire;

use crate::link::LoadDataError;

/// Maximum number of recent backups listed in the Data group.
pub(crate) const MAX_VISIBLE_BACKUPS: usize = 5;
/// Maximum number of logical days accepted by a single `data.delete_range` request (03 section 6).
pub(crate) const MAX_DELETE_RANGE_DAYS: usize = wire::RANGE_MAX_DAYS;
/// Sort rank for [`wire::TitleRetention::Forever`] so finite periods always compare shorter.
const RETENTION_RANK_FOREVER: u32 = u32::MAX;

/// Monotonic generation counter for dropping late Settings page responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct SettingsGeneration(u64);

impl SettingsGeneration {
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

/// Whether a conditional settings row should be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowVisibility {
    Visible,
    Hidden,
}

/// Direction of a requested change to [`wire::TitleRetention`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetentionTransition {
    Unchanged,
    Longer,
    Shorter,
}

/// Classifies whether moving from `current` to `target` keeps, lengthens, or shortens title retention.
#[must_use]
pub(crate) const fn classify_retention_change(
    current: wire::TitleRetention,
    target: wire::TitleRetention,
) -> RetentionTransition {
    let current_rank = match current.days() {
        Some(days) => days,
        None => RETENTION_RANK_FOREVER,
    };
    let target_rank = match target.days() {
        Some(days) => days,
        None => RETENTION_RANK_FOREVER,
    };
    if target_rank == current_rank {
        RetentionTransition::Unchanged
    } else if target_rank < current_rank {
        RetentionTransition::Shorter
    } else {
        RetentionTransition::Longer
    }
}

/// Inline confirmation state when shortening [`wire::TitleRetention`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RetentionConfirm {
    #[default]
    None,
    Pending {
        target: wire::TitleRetention,
    },
}

/// Current settings snapshot displayed by the General and Recording controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SettingsValues {
    pub(crate) autostart: wire::AutostartEnabled,
    pub(crate) title_retention: wire::TitleRetention,
    pub(crate) ui: wire::UiSettings,
    pub(crate) effective: wire::EffectiveUiSettings,
}

impl From<wire::SettingsResponse> for SettingsValues {
    fn from(response: wire::SettingsResponse) -> Self {
        Self {
            autostart: response.autostart,
            title_retention: response.title_retention,
            ui: response.ui,
            effective: response.effective,
        }
    }
}

impl SettingsValues {
    /// Tray hover prewarm is shown only when resident UI is turned off (07 section 7).
    #[must_use]
    pub(crate) const fn prewarm_visibility(&self) -> RowVisibility {
        match self.ui.resident.enabled {
            wire::ResidentEnabled::Disabled => RowVisibility::Visible,
            wire::ResidentEnabled::Enabled => RowVisibility::Hidden,
        }
    }

    /// Battery prewarm is shown only when resident UI is off and prewarm is on (07 section 7).
    #[must_use]
    pub(crate) const fn prewarm_on_battery_visibility(&self) -> RowVisibility {
        match (self.prewarm_visibility(), self.ui.prewarm.enabled) {
            (RowVisibility::Visible, wire::PrewarmEnabled::Enabled) => RowVisibility::Visible,
            (RowVisibility::Hidden, _)
            | (RowVisibility::Visible, wire::PrewarmEnabled::Disabled) => RowVisibility::Hidden,
        }
    }

    /// Applies one General setting change optimistically while `settings.set` is in flight.
    fn apply_general_change(&mut self, change: GeneralSettingChange) {
        match change {
            GeneralSettingChange::Autostart(value) => self.autostart = value,
            GeneralSettingChange::Resident(value) => self.ui.resident.enabled = value,
            GeneralSettingChange::PrewarmEnabled(value) => self.ui.prewarm.enabled = value,
            GeneralSettingChange::PrewarmOnBattery(value) => self.ui.prewarm.on_battery = value,
            GeneralSettingChange::Language(value) => self.ui.language = value,
            GeneralSettingChange::HourCycle(value) => self.ui.hour_cycle = value,
            GeneralSettingChange::FirstDayOfWeek(value) => self.ui.first_day_of_week = value,
        }
    }

    /// Returns whether `change` differs from the currently displayed value.
    fn differs_from_general(&self, change: GeneralSettingChange) -> bool {
        match change {
            GeneralSettingChange::Autostart(value) => self.autostart != value,
            GeneralSettingChange::Resident(value) => self.ui.resident.enabled != value,
            GeneralSettingChange::PrewarmEnabled(value) => self.ui.prewarm.enabled != value,
            GeneralSettingChange::PrewarmOnBattery(value) => self.ui.prewarm.on_battery != value,
            GeneralSettingChange::Language(value) => self.ui.language != value,
            GeneralSettingChange::HourCycle(value) => self.ui.hour_cycle != value,
            GeneralSettingChange::FirstDayOfWeek(value) => self.ui.first_day_of_week != value,
        }
    }
}

/// One single-field update issued from the General settings card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeneralSettingChange {
    Autostart(wire::AutostartEnabled),
    Resident(wire::ResidentEnabled),
    PrewarmEnabled(wire::PrewarmEnabled),
    PrewarmOnBattery(wire::PrewarmOnBattery),
    Language(wire::UiLanguageSetting),
    HourCycle(wire::HourCycleSetting),
    FirstDayOfWeek(wire::FirstDayOfWeekSetting),
}

impl GeneralSettingChange {
    /// Converts this change into a minimal one-field `settings.set` wire payload.
    #[must_use]
    pub(crate) fn into_request(self) -> wire::SettingsSetRequest {
        match self {
            Self::Autostart(value) => wire::SettingsSetRequest {
                autostart: Some(value),
                ..Default::default()
            },
            Self::Resident(value) => wire::SettingsSetRequest {
                resident: Some(wire::ResidentUpdate {
                    enabled: Some(value),
                }),
                ..Default::default()
            },
            Self::PrewarmEnabled(value) => wire::SettingsSetRequest {
                prewarm: Some(wire::PrewarmUpdate {
                    enabled: Some(value),
                    on_battery: None,
                }),
                ..Default::default()
            },
            Self::PrewarmOnBattery(value) => wire::SettingsSetRequest {
                prewarm: Some(wire::PrewarmUpdate {
                    enabled: None,
                    on_battery: Some(value),
                }),
                ..Default::default()
            },
            Self::Language(value) => wire::SettingsSetRequest {
                language: Some(value),
                ..Default::default()
            },
            Self::HourCycle(value) => wire::SettingsSetRequest {
                hour_cycle: Some(value),
                ..Default::default()
            },
            Self::FirstDayOfWeek(value) => wire::SettingsSetRequest {
                first_day_of_week: Some(value),
                ..Default::default()
            },
        }
    }
}

/// Load state of `settings.get`.
#[derive(Debug)]
pub(crate) enum SettingsLoadState {
    Loading,
    Ready(SettingsValues),
    Failed(LoadDataError),
}

/// Whether a `settings.set` mutation is currently in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveActivity {
    Idle,
    Saving,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveGroup {
    General,
    Recording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingSave {
    Idle,
    InFlight {
        generation: SettingsGeneration,
        previous: SettingsValues,
        group: SaveGroup,
    },
}

/// Sorts backup entries newest-first (breaking ties by descending file name) and keeps at most
/// [`MAX_VISIBLE_BACKUPS`] entries.
#[must_use]
pub(crate) fn visible_backups(entries: &[wire::BackupEntry]) -> Vec<wire::BackupEntry> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|left, right| {
        right
            .created_utc_ms
            .cmp(&left.created_utc_ms)
            .then_with(|| right.file_name.cmp(&left.file_name))
    });
    sorted.truncate(MAX_VISIBLE_BACKUPS);
    sorted
}

/// Normalized `data.info` snapshot displayed in the Data and About groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DataInfoView {
    pub(crate) database_path: String,
    pub(crate) database_bytes: u64,
    pub(crate) logs_path: String,
    pub(crate) backups: Vec<wire::BackupEntry>,
}

impl From<wire::DataInfoResponse> for DataInfoView {
    fn from(response: wire::DataInfoResponse) -> Self {
        Self {
            database_path: response.database_path,
            database_bytes: response.database_bytes,
            logs_path: response.logs_path,
            backups: visible_backups(&response.backups),
        }
    }
}

/// Load state of `data.info`.
#[derive(Debug)]
pub(crate) enum DataInfoLoadState {
    Loading,
    Ready(DataInfoView),
    Failed(LoadDataError),
}

/// Load state of `core.status`.
#[derive(Debug)]
pub(crate) enum CoreStatusLoadState {
    Loading,
    Ready(wire::CoreStatusResponse),
    Failed(LoadDataError),
}

/// Currently running long-lived data maintenance operation, blocking concurrent submissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DataOperation {
    #[default]
    Idle,
    BackingUp,
    Compacting,
    Deleting,
}

impl DataOperation {
    /// Returns whether any data maintenance operation is currently in flight.
    #[must_use]
    pub(crate) const fn is_busy(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// Result or failure notice for the Compact database section.
#[derive(Debug, Default)]
pub(crate) enum CompactNotice {
    #[default]
    None,
    Succeeded(wire::DataCompactResponse),
    Failed(LoadDataError),
}

/// Result or failure notice for the Delete by date range section.
#[derive(Debug, Default)]
pub(crate) enum DeleteNotice {
    #[default]
    None,
    Succeeded {
        deleted_ms: i64,
    },
    Failed(LoadDataError),
}

/// Validation failure for the inclusive UI date-range deletion selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValidateDeleteRangeError {
    MissingDates,
    EndBeforeStart,
    EndAfterYesterday,
    ExceedsMaxSpan,
}

/// Validated inclusive UI date range paired with its half-open protocol request `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedDeleteRange {
    pub(crate) start_inclusive: wire::LocalDate,
    pub(crate) end_inclusive: wire::LocalDate,
    pub(crate) request: wire::DataDeleteRangeRequest,
    pub(crate) days: usize,
}

/// Latest selectable inclusive logical date for deletion: yesterday relative to `logical_today`.
#[must_use]
pub(crate) fn max_deletable_date(logical_today: wire::LocalDate) -> Option<wire::LocalDate> {
    logical_today.yesterday().ok()
}

/// Validates an inclusive `[start_inclusive, end_inclusive]` selection against `logical_today`
/// and converts it to the half-open `data.delete_range` protocol request `[start_date, end_date)`.
///
/// # Errors
/// Rejects reversed ranges, dates on or after `logical_today`, or spans longer than
/// [`MAX_DELETE_RANGE_DAYS`].
pub(crate) fn build_delete_range_request(
    start_inclusive: wire::LocalDate,
    end_inclusive: wire::LocalDate,
    logical_today: wire::LocalDate,
) -> Result<ValidatedDeleteRange, ValidateDeleteRangeError> {
    let Some(yesterday) = max_deletable_date(logical_today) else {
        return Err(ValidateDeleteRangeError::EndAfterYesterday);
    };
    if start_inclusive > end_inclusive {
        return Err(ValidateDeleteRangeError::EndBeforeStart);
    }
    if end_inclusive > yesterday {
        return Err(ValidateDeleteRangeError::EndAfterYesterday);
    }
    let Ok(end_exclusive) = end_inclusive.tomorrow() else {
        return Err(ValidateDeleteRangeError::EndAfterYesterday);
    };
    let mut cursor = start_inclusive;
    let mut days: usize = 0;
    while cursor < end_exclusive {
        if days == MAX_DELETE_RANGE_DAYS {
            return Err(ValidateDeleteRangeError::ExceedsMaxSpan);
        }
        days += 1;
        let Ok(next) = cursor.tomorrow() else {
            return Err(ValidateDeleteRangeError::ExceedsMaxSpan);
        };
        cursor = next;
    }
    if days == 0 {
        return Err(ValidateDeleteRangeError::EndBeforeStart);
    }
    Ok(ValidatedDeleteRange {
        start_inclusive,
        end_inclusive,
        request: wire::DataDeleteRangeRequest {
            start_date: start_inclusive,
            end_date: end_exclusive,
        },
        days,
    })
}

/// Inline confirmation state for date-range deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DeleteRangeConfirm {
    #[default]
    None,
    Pending(ValidatedDeleteRange),
}

/// Side effect emitted by [`SettingsPage`] state transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsEffect {
    None,
    LoadSettings {
        generation: SettingsGeneration,
    },
    SaveSettings {
        request: wire::SettingsSetRequest,
        generation: SettingsGeneration,
    },
    LoadDataInfo {
        generation: SettingsGeneration,
    },
    LoadCoreStatus {
        generation: SettingsGeneration,
    },
    Backup {
        generation: SettingsGeneration,
    },
    Compact {
        generation: SettingsGeneration,
    },
    DeleteRange {
        request: wire::DataDeleteRangeRequest,
        generation: SettingsGeneration,
    },
}

/// Pure state machine for the Settings page (07 section 7).
#[derive(Debug)]
pub(crate) struct SettingsPage {
    state: SettingsLoadState,
    load_generation: SettingsGeneration,
    save_generation: SettingsGeneration,
    pending_save: PendingSave,
    general_error: Option<LoadDataError>,
    recording_error: Option<LoadDataError>,
    retention_confirm: RetentionConfirm,
    data_info: DataInfoLoadState,
    data_info_generation: SettingsGeneration,
    core_status: CoreStatusLoadState,
    core_status_generation: SettingsGeneration,
    data_op: DataOperation,
    data_op_generation: SettingsGeneration,
    backup_error: Option<LoadDataError>,
    compact_notice: CompactNotice,
    delete_start: Option<wire::LocalDate>,
    delete_end: Option<wire::LocalDate>,
    delete_confirm: DeleteRangeConfirm,
    delete_notice: DeleteNotice,
}

impl Default for SettingsPage {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsPage {
    /// Creates an unloaded Settings page state machine.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            state: SettingsLoadState::Loading,
            load_generation: SettingsGeneration::default(),
            save_generation: SettingsGeneration::default(),
            pending_save: PendingSave::Idle,
            general_error: None,
            recording_error: None,
            retention_confirm: RetentionConfirm::None,
            data_info: DataInfoLoadState::Loading,
            data_info_generation: SettingsGeneration::default(),
            core_status: CoreStatusLoadState::Loading,
            core_status_generation: SettingsGeneration::default(),
            data_op: DataOperation::Idle,
            data_op_generation: SettingsGeneration::default(),
            backup_error: None,
            compact_notice: CompactNotice::None,
            delete_start: None,
            delete_end: None,
            delete_confirm: DeleteRangeConfirm::None,
            delete_notice: DeleteNotice::None,
        }
    }

    /// Current load state of `settings.get`.
    #[must_use]
    pub(crate) const fn state(&self) -> &SettingsLoadState {
        &self.state
    }

    /// Currently displayed settings values once loaded.
    #[must_use]
    pub(crate) const fn values(&self) -> Option<&SettingsValues> {
        match &self.state {
            SettingsLoadState::Ready(values) => Some(values),
            SettingsLoadState::Loading | SettingsLoadState::Failed(_) => None,
        }
    }

    /// Title retention value shown in the dropdown (reflects pending confirmation target if open).
    #[must_use]
    pub(crate) const fn displayed_title_retention(&self) -> Option<wire::TitleRetention> {
        match self.retention_confirm {
            RetentionConfirm::Pending { target } => Some(target),
            RetentionConfirm::None => match self.values() {
                Some(values) => Some(values.title_retention),
                None => None,
            },
        }
    }

    /// Inline error shown inside the General card after a failed update.
    #[must_use]
    pub(crate) const fn general_error(&self) -> Option<&LoadDataError> {
        self.general_error.as_ref()
    }

    /// Inline error shown inside the Recording card after a failed retention update.
    #[must_use]
    pub(crate) const fn recording_error(&self) -> Option<&LoadDataError> {
        self.recording_error.as_ref()
    }

    /// Pending inline confirmation when shortening title retention.
    #[must_use]
    pub(crate) const fn retention_confirm(&self) -> RetentionConfirm {
        self.retention_confirm
    }

    /// Whether a `settings.set` update is currently in flight.
    #[must_use]
    pub(crate) const fn save_activity(&self) -> SaveActivity {
        match self.pending_save {
            PendingSave::Idle => SaveActivity::Idle,
            PendingSave::InFlight { .. } => SaveActivity::Saving,
        }
    }

    /// Current load state of `data.info`.
    #[must_use]
    pub(crate) const fn data_info(&self) -> &DataInfoLoadState {
        &self.data_info
    }

    /// Current load state of `core.status`.
    #[must_use]
    pub(crate) const fn core_status(&self) -> &CoreStatusLoadState {
        &self.core_status
    }

    /// Currently active long-running data maintenance operation.
    #[must_use]
    pub(crate) const fn data_operation(&self) -> DataOperation {
        self.data_op
    }

    /// Inline error from the last failed manual backup request.
    #[must_use]
    pub(crate) const fn backup_error(&self) -> Option<&LoadDataError> {
        self.backup_error.as_ref()
    }

    /// Latest success or failure notice from database compaction.
    #[must_use]
    pub(crate) const fn compact_notice(&self) -> &CompactNotice {
        &self.compact_notice
    }

    /// Selected inclusive start date for deletion.
    #[must_use]
    pub(crate) const fn delete_start(&self) -> Option<wire::LocalDate> {
        self.delete_start
    }

    /// Selected inclusive end date for deletion.
    #[must_use]
    pub(crate) const fn delete_end(&self) -> Option<wire::LocalDate> {
        self.delete_end
    }

    /// Active inline confirmation block for date-range deletion.
    #[must_use]
    pub(crate) const fn delete_confirm(&self) -> DeleteRangeConfirm {
        self.delete_confirm
    }

    /// Latest success or failure notice from date-range deletion.
    #[must_use]
    pub(crate) const fn delete_notice(&self) -> &DeleteNotice {
        &self.delete_notice
    }

    /// Validates the currently picked start and end dates against `logical_today`.
    ///
    /// # Errors
    /// Returns [`ValidateDeleteRangeError`] when dates are incomplete, reversed, include today or
    /// the future, or exceed [`MAX_DELETE_RANGE_DAYS`].
    pub(crate) fn delete_range_validation(
        &self,
        logical_today: wire::LocalDate,
    ) -> Result<ValidatedDeleteRange, ValidateDeleteRangeError> {
        let (Some(start), Some(end)) = (self.delete_start, self.delete_end) else {
            return Err(ValidateDeleteRangeError::MissingDates);
        };
        build_delete_range_request(start, end, logical_today)
    }

    /// Requests fresh settings from core when opening or retrying the Settings page.
    #[must_use]
    pub(crate) fn open(&mut self) -> SettingsEffect {
        self.load_generation = self.load_generation.next();
        if !matches!(self.state, SettingsLoadState::Ready(_)) {
            self.state = SettingsLoadState::Loading;
        }
        self.general_error = None;
        self.recording_error = None;
        self.retention_confirm = RetentionConfirm::None;
        SettingsEffect::LoadSettings {
            generation: self.load_generation,
        }
    }

    /// Requests fresh `data.info` metadata for the Data and About cards.
    #[must_use]
    pub(crate) fn load_data_info(&mut self) -> SettingsEffect {
        self.data_info_generation = self.data_info_generation.next();
        if !matches!(self.data_info, DataInfoLoadState::Ready(_)) {
            self.data_info = DataInfoLoadState::Loading;
        }
        SettingsEffect::LoadDataInfo {
            generation: self.data_info_generation,
        }
    }

    /// Requests fresh `core.status` metadata for the About card.
    #[must_use]
    pub(crate) fn load_core_status(&mut self) -> SettingsEffect {
        self.core_status_generation = self.core_status_generation.next();
        if !matches!(self.core_status, CoreStatusLoadState::Ready(_)) {
            self.core_status = CoreStatusLoadState::Loading;
        }
        SettingsEffect::LoadCoreStatus {
            generation: self.core_status_generation,
        }
    }

    /// Applies the reply from `settings.get`, dropping stale generations and preserving any active
    /// save in flight.
    pub(crate) fn apply_loaded(
        &mut self,
        generation: SettingsGeneration,
        result: Result<wire::SettingsResponse, LoadDataError>,
    ) {
        if !self.load_generation.accepts(generation) {
            return;
        }
        if matches!(self.pending_save, PendingSave::InFlight { .. }) {
            return;
        }
        match result {
            Ok(response) => {
                self.state = SettingsLoadState::Ready(SettingsValues::from(response));
                self.general_error = None;
                self.recording_error = None;
            }
            Err(error) => {
                if matches!(self.state, SettingsLoadState::Ready(_)) {
                    self.general_error = Some(error);
                } else {
                    self.state = SettingsLoadState::Failed(error);
                }
            }
        }
    }

    /// Applies the reply from `data.info`, dropping stale generations.
    pub(crate) fn apply_data_info_loaded(
        &mut self,
        generation: SettingsGeneration,
        result: Result<wire::DataInfoResponse, LoadDataError>,
    ) {
        if !self.data_info_generation.accepts(generation) {
            return;
        }
        match result {
            Ok(response) => {
                self.data_info = DataInfoLoadState::Ready(DataInfoView::from(response));
            }
            Err(error) => {
                if !matches!(self.data_info, DataInfoLoadState::Ready(_)) {
                    self.data_info = DataInfoLoadState::Failed(error);
                }
            }
        }
    }

    /// Applies the reply from `core.status`, dropping stale generations.
    pub(crate) fn apply_core_status_loaded(
        &mut self,
        generation: SettingsGeneration,
        result: Result<wire::CoreStatusResponse, LoadDataError>,
    ) {
        if !self.core_status_generation.accepts(generation) {
            return;
        }
        match result {
            Ok(response) => {
                self.core_status = CoreStatusLoadState::Ready(response);
            }
            Err(error) => {
                if !matches!(self.core_status, CoreStatusLoadState::Ready(_)) {
                    self.core_status = CoreStatusLoadState::Failed(error);
                }
            }
        }
    }

    /// Keeps the About recording status synchronized with live `StateSnapshot` pushes.
    pub(crate) fn sync_recording_state(
        &mut self,
        recording: bool,
        stop_reason: Option<wire::StopReason>,
    ) {
        if let CoreStatusLoadState::Ready(status) = &mut self.core_status {
            status.recording = recording;
            status.stop_reason = stop_reason;
        }
    }

    /// Revalidates open date-range confirmation if the logical day changes.
    pub(crate) fn on_logical_date_changed(&mut self, logical_today: wire::LocalDate) {
        if let DeleteRangeConfirm::Pending(range) = self.delete_confirm
            && build_delete_range_request(range.start_inclusive, range.end_inclusive, logical_today)
                .is_err()
        {
            self.delete_confirm = DeleteRangeConfirm::None;
        }
    }

    /// Starts a one-field `settings.set` mutation for the General group with optimistic display
    /// and rollback snapshot.
    #[must_use]
    pub(crate) fn change_general(&mut self, change: GeneralSettingChange) -> SettingsEffect {
        if matches!(self.pending_save, PendingSave::InFlight { .. }) {
            return SettingsEffect::None;
        }
        let SettingsLoadState::Ready(current) = &mut self.state else {
            return SettingsEffect::None;
        };
        if !current.differs_from_general(change) {
            return SettingsEffect::None;
        }
        let previous = *current;
        current.apply_general_change(change);
        self.general_error = None;
        self.save_generation = self.save_generation.next();
        self.pending_save = PendingSave::InFlight {
            generation: self.save_generation,
            previous,
            group: SaveGroup::General,
        };
        SettingsEffect::SaveSettings {
            request: change.into_request(),
            generation: self.save_generation,
        }
    }

    /// Handles a user selection in the Title retention dropdown: shortening asks for inline
    /// confirmation first; lengthening sends `settings.set` immediately.
    #[must_use]
    pub(crate) fn select_title_retention(
        &mut self,
        target: wire::TitleRetention,
    ) -> SettingsEffect {
        if matches!(self.pending_save, PendingSave::InFlight { .. }) {
            return SettingsEffect::None;
        }
        let SettingsLoadState::Ready(current) = &self.state else {
            return SettingsEffect::None;
        };
        match classify_retention_change(current.title_retention, target) {
            RetentionTransition::Unchanged => {
                self.retention_confirm = RetentionConfirm::None;
                SettingsEffect::None
            }
            RetentionTransition::Shorter => {
                self.recording_error = None;
                self.retention_confirm = RetentionConfirm::Pending { target };
                SettingsEffect::None
            }
            RetentionTransition::Longer => {
                self.retention_confirm = RetentionConfirm::None;
                self.begin_title_retention_save(target)
            }
        }
    }

    /// Confirms a pending shorter title retention selection and emits `settings.set`.
    #[must_use]
    pub(crate) fn confirm_title_retention(&mut self) -> SettingsEffect {
        if matches!(self.pending_save, PendingSave::InFlight { .. }) {
            return SettingsEffect::None;
        }
        let RetentionConfirm::Pending { target } = self.retention_confirm else {
            return SettingsEffect::None;
        };
        self.retention_confirm = RetentionConfirm::None;
        self.begin_title_retention_save(target)
    }

    /// Dismisses a pending shorter title retention confirmation without saving.
    pub(crate) fn cancel_title_retention(&mut self) {
        self.retention_confirm = RetentionConfirm::None;
    }

    fn begin_title_retention_save(&mut self, target: wire::TitleRetention) -> SettingsEffect {
        let SettingsLoadState::Ready(current) = &mut self.state else {
            return SettingsEffect::None;
        };
        if current.title_retention == target {
            return SettingsEffect::None;
        }
        let previous = *current;
        current.title_retention = target;
        self.recording_error = None;
        self.save_generation = self.save_generation.next();
        self.pending_save = PendingSave::InFlight {
            generation: self.save_generation,
            previous,
            group: SaveGroup::Recording,
        };
        SettingsEffect::SaveSettings {
            request: wire::SettingsSetRequest {
                title_retention: Some(target),
                ..Default::default()
            },
            generation: self.save_generation,
        }
    }

    /// Applies the reply from `settings.set`; on failure reverts the displayed control value and
    /// records an inline error on the originating card (General or Recording).
    pub(crate) fn apply_saved(
        &mut self,
        generation: SettingsGeneration,
        result: Result<wire::SettingsResponse, LoadDataError>,
    ) {
        let PendingSave::InFlight {
            generation: active,
            previous,
            group,
        } = self.pending_save
        else {
            return;
        };
        if !active.accepts(generation) {
            return;
        }
        self.pending_save = PendingSave::Idle;
        match result {
            Ok(response) => {
                self.state = SettingsLoadState::Ready(SettingsValues::from(response));
                match group {
                    SaveGroup::General => self.general_error = None,
                    SaveGroup::Recording => self.recording_error = None,
                }
            }
            Err(error) => {
                self.state = SettingsLoadState::Ready(previous);
                match group {
                    SaveGroup::General => self.general_error = Some(error),
                    SaveGroup::Recording => self.recording_error = Some(error),
                }
            }
        }
    }

    /// Triggers `data.backup` unless another data maintenance operation is already running.
    #[must_use]
    pub(crate) fn start_backup(&mut self) -> SettingsEffect {
        if self.data_op.is_busy() {
            return SettingsEffect::None;
        }
        self.backup_error = None;
        self.data_op = DataOperation::BackingUp;
        self.data_op_generation = self.data_op_generation.next();
        SettingsEffect::Backup {
            generation: self.data_op_generation,
        }
    }

    /// Applies the `data.backup` reply and refreshes `data.info` on success.
    #[must_use]
    pub(crate) fn apply_backup_finished(
        &mut self,
        generation: SettingsGeneration,
        result: Result<wire::BackupEntry, LoadDataError>,
    ) -> SettingsEffect {
        if self.data_op != DataOperation::BackingUp || !self.data_op_generation.accepts(generation)
        {
            return SettingsEffect::None;
        }
        self.data_op = DataOperation::Idle;
        match result {
            Ok(_) => {
                self.backup_error = None;
                self.load_data_info()
            }
            Err(error) => {
                self.backup_error = Some(error);
                SettingsEffect::None
            }
        }
    }

    /// Triggers `data.compact` unless another data maintenance operation is already running.
    #[must_use]
    pub(crate) fn start_compact(&mut self) -> SettingsEffect {
        if self.data_op.is_busy() {
            return SettingsEffect::None;
        }
        self.compact_notice = CompactNotice::None;
        self.data_op = DataOperation::Compacting;
        self.data_op_generation = self.data_op_generation.next();
        SettingsEffect::Compact {
            generation: self.data_op_generation,
        }
    }

    /// Applies the `data.compact` reply and refreshes `data.info` on success.
    #[must_use]
    pub(crate) fn apply_compact_finished(
        &mut self,
        generation: SettingsGeneration,
        result: Result<wire::DataCompactResponse, LoadDataError>,
    ) -> SettingsEffect {
        if self.data_op != DataOperation::Compacting || !self.data_op_generation.accepts(generation)
        {
            return SettingsEffect::None;
        }
        self.data_op = DataOperation::Idle;
        match result {
            Ok(response) => {
                self.compact_notice = CompactNotice::Succeeded(response);
                self.load_data_info()
            }
            Err(error) => {
                self.compact_notice = CompactNotice::Failed(error);
                SettingsEffect::None
            }
        }
    }

    /// Sets the inclusive deletion start date, capped at yesterday relative to `logical_today`.
    pub(crate) fn set_delete_start(
        &mut self,
        date: wire::LocalDate,
        logical_today: wire::LocalDate,
    ) {
        let Some(yesterday) = max_deletable_date(logical_today) else {
            return;
        };
        let capped = date.min(yesterday);
        self.delete_start = Some(capped);
        self.delete_confirm = DeleteRangeConfirm::None;
        self.delete_notice = DeleteNotice::None;
    }

    /// Sets the inclusive deletion end date, capped at yesterday relative to `logical_today`.
    pub(crate) fn set_delete_end(&mut self, date: wire::LocalDate, logical_today: wire::LocalDate) {
        let Some(yesterday) = max_deletable_date(logical_today) else {
            return;
        };
        let capped = date.min(yesterday);
        self.delete_end = Some(capped);
        self.delete_confirm = DeleteRangeConfirm::None;
        self.delete_notice = DeleteNotice::None;
    }

    /// Opens the inline confirmation block for the current valid range without issuing a request.
    pub(crate) fn request_delete_range(&mut self, logical_today: wire::LocalDate) {
        if self.data_op.is_busy() {
            return;
        }
        if let Ok(validated) = self.delete_range_validation(logical_today) {
            self.delete_notice = DeleteNotice::None;
            self.delete_confirm = DeleteRangeConfirm::Pending(validated);
        }
    }

    /// Dismisses the inline date-range deletion confirmation block.
    pub(crate) fn cancel_delete_range(&mut self) {
        self.delete_confirm = DeleteRangeConfirm::None;
    }

    /// Confirms date-range deletion after inline confirmation and emits `data.delete_range`.
    #[must_use]
    pub(crate) fn confirm_delete_range(
        &mut self,
        logical_today: wire::LocalDate,
    ) -> SettingsEffect {
        if self.data_op.is_busy() {
            return SettingsEffect::None;
        }
        let DeleteRangeConfirm::Pending(pending) = self.delete_confirm else {
            return SettingsEffect::None;
        };
        let Ok(validated) = build_delete_range_request(
            pending.start_inclusive,
            pending.end_inclusive,
            logical_today,
        ) else {
            self.delete_confirm = DeleteRangeConfirm::None;
            return SettingsEffect::None;
        };
        self.delete_confirm = DeleteRangeConfirm::None;
        self.delete_notice = DeleteNotice::None;
        self.data_op = DataOperation::Deleting;
        self.data_op_generation = self.data_op_generation.next();
        SettingsEffect::DeleteRange {
            request: validated.request,
            generation: self.data_op_generation,
        }
    }

    /// Applies the `data.delete_range` reply and refreshes `data.info` on success.
    #[must_use]
    pub(crate) fn apply_delete_finished(
        &mut self,
        generation: SettingsGeneration,
        result: Result<wire::DataDeleteRangeResponse, LoadDataError>,
    ) -> SettingsEffect {
        if self.data_op != DataOperation::Deleting || !self.data_op_generation.accepts(generation) {
            return SettingsEffect::None;
        }
        self.data_op = DataOperation::Idle;
        match result {
            Ok(response) => {
                self.delete_notice = DeleteNotice::Succeeded {
                    deleted_ms: response.deleted_ms,
                };
                self.load_data_info()
            }
            Err(error) => {
                self.delete_notice = DeleteNotice::Failed(error);
                SettingsEffect::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::RequestCoreError;
    use jiff::civil::date;

    fn sample_response(
        resident: wire::ResidentEnabled,
        prewarm_enabled: wire::PrewarmEnabled,
        prewarm_on_battery: wire::PrewarmOnBattery,
    ) -> wire::SettingsResponse {
        let ui = wire::UiSettings {
            resident: wire::ResidentSettings { enabled: resident },
            prewarm: wire::PrewarmSettings {
                enabled: prewarm_enabled,
                on_battery: prewarm_on_battery,
            },
            language: wire::UiLanguageSetting::Auto,
            hour_cycle: wire::HourCycleSetting::Auto,
            first_day_of_week: wire::FirstDayOfWeekSetting::Auto,
        };
        let system = wire::EffectiveUiSettings {
            resident: wire::ResidentSettings::default(),
            prewarm: wire::PrewarmSettings::default(),
            ui_language: wire::UiLanguage::SimplifiedChinese,
            locale: Some(wire::LocaleSettings {
                hour_cycle: wire::HourCycle::H24,
                first_day_of_week: wire::Weekday::Monday,
            }),
        };
        wire::SettingsResponse {
            ui,
            effective: ui.resolve(system),
            autostart: wire::AutostartEnabled::Disabled,
            title_retention: wire::TitleRetention::Forever,
        }
    }

    fn ready_page() -> SettingsPage {
        let mut page = SettingsPage::new();
        let SettingsEffect::LoadSettings { generation } = page.open() else {
            panic!("opening settings emits LoadSettings");
        };
        page.apply_loaded(
            generation,
            Ok(sample_response(
                wire::ResidentEnabled::Enabled,
                wire::PrewarmEnabled::Enabled,
                wire::PrewarmOnBattery::Blocked,
            )),
        );
        page
    }

    #[test]
    fn prewarm_rows_appear_only_when_resident_is_disabled_and_battery_only_when_prewarm_is_enabled()
    {
        let resident_on = SettingsValues::from(sample_response(
            wire::ResidentEnabled::Enabled,
            wire::PrewarmEnabled::Enabled,
            wire::PrewarmOnBattery::Allowed,
        ));
        assert_eq!(resident_on.prewarm_visibility(), RowVisibility::Hidden);
        assert_eq!(
            resident_on.prewarm_on_battery_visibility(),
            RowVisibility::Hidden
        );

        let resident_off_prewarm_on = SettingsValues::from(sample_response(
            wire::ResidentEnabled::Disabled,
            wire::PrewarmEnabled::Enabled,
            wire::PrewarmOnBattery::Blocked,
        ));
        assert_eq!(
            resident_off_prewarm_on.prewarm_visibility(),
            RowVisibility::Visible
        );
        assert_eq!(
            resident_off_prewarm_on.prewarm_on_battery_visibility(),
            RowVisibility::Visible
        );

        let resident_off_prewarm_off = SettingsValues::from(sample_response(
            wire::ResidentEnabled::Disabled,
            wire::PrewarmEnabled::Disabled,
            wire::PrewarmOnBattery::Allowed,
        ));
        assert_eq!(
            resident_off_prewarm_off.prewarm_visibility(),
            RowVisibility::Visible
        );
        assert_eq!(
            resident_off_prewarm_off.prewarm_on_battery_visibility(),
            RowVisibility::Hidden
        );
    }

    #[test]
    fn failed_settings_set_reverts_displayed_value_and_records_inline_error() {
        let mut page = ready_page();
        let SettingsEffect::SaveSettings {
            request,
            generation: save_gen,
        } = page.change_general(GeneralSettingChange::Autostart(
            wire::AutostartEnabled::Enabled,
        ))
        else {
            panic!("toggling autostart emits SaveSettings");
        };
        assert_eq!(
            request,
            wire::SettingsSetRequest {
                autostart: Some(wire::AutostartEnabled::Enabled),
                ..Default::default()
            }
        );
        assert_eq!(
            page.values().expect("optimistic values").autostart,
            wire::AutostartEnabled::Enabled
        );
        assert_eq!(page.save_activity(), SaveActivity::Saving);

        page.apply_saved(
            save_gen,
            Err(LoadDataError::from(RequestCoreError::Disconnected)),
        );
        assert_eq!(page.save_activity(), SaveActivity::Idle);
        assert_eq!(
            page.values().expect("reverted values").autostart,
            wire::AutostartEnabled::Disabled
        );
        assert!(page.general_error().is_some());
    }

    #[test]
    fn settings_open_loads_and_one_field_set_applies_committed_response() {
        let mut page = SettingsPage::new();
        assert!(matches!(page.state(), SettingsLoadState::Loading));
        let SettingsEffect::LoadSettings { generation } = page.open() else {
            panic!("open emits LoadSettings");
        };
        page.apply_loaded(
            generation,
            Err(LoadDataError::from(RequestCoreError::Disconnected)),
        );
        assert!(matches!(page.state(), SettingsLoadState::Failed(_)));

        let SettingsEffect::LoadSettings { generation } = page.open() else {
            panic!("retry open emits LoadSettings");
        };
        assert!(matches!(page.state(), SettingsLoadState::Loading));
        let initial = sample_response(
            wire::ResidentEnabled::Enabled,
            wire::PrewarmEnabled::Enabled,
            wire::PrewarmOnBattery::Blocked,
        );
        page.apply_loaded(generation, Ok(initial));
        assert!(matches!(page.state(), SettingsLoadState::Ready(_)));

        assert_eq!(
            page.change_general(GeneralSettingChange::Resident(
                wire::ResidentEnabled::Enabled,
            )),
            SettingsEffect::None
        );

        let SettingsEffect::SaveSettings {
            request,
            generation: save_gen,
        } = page.change_general(GeneralSettingChange::Language(wire::UiLanguageSetting::En))
        else {
            panic!("changing language emits SaveSettings");
        };
        assert_eq!(
            request,
            wire::SettingsSetRequest {
                language: Some(wire::UiLanguageSetting::En),
                ..Default::default()
            }
        );

        let mut committed = initial;
        committed.ui.language = wire::UiLanguageSetting::En;
        committed.effective.ui_language = wire::UiLanguage::English;
        page.apply_saved(save_gen, Ok(committed));

        assert_eq!(page.save_activity(), SaveActivity::Idle);
        assert!(page.general_error().is_none());
        assert_eq!(
            page.values().expect("committed values").ui.language,
            wire::UiLanguageSetting::En
        );
    }

    #[test]
    fn shortening_title_retention_requires_confirmation_and_failure_reverts_with_recording_error() {
        let mut page = ready_page();
        assert_eq!(
            page.select_title_retention(wire::TitleRetention::Days90),
            SettingsEffect::None
        );
        assert_eq!(
            page.retention_confirm(),
            RetentionConfirm::Pending {
                target: wire::TitleRetention::Days90
            }
        );
        assert_eq!(
            page.displayed_title_retention(),
            Some(wire::TitleRetention::Days90)
        );
        assert_eq!(
            page.values()
                .expect("unchanged before confirm")
                .title_retention,
            wire::TitleRetention::Forever
        );

        page.cancel_title_retention();
        assert_eq!(page.retention_confirm(), RetentionConfirm::None);
        assert_eq!(
            page.displayed_title_retention(),
            Some(wire::TitleRetention::Forever)
        );

        assert_eq!(
            page.select_title_retention(wire::TitleRetention::Days30),
            SettingsEffect::None
        );
        let SettingsEffect::SaveSettings {
            request,
            generation,
        } = page.confirm_title_retention()
        else {
            panic!("confirming shorter retention emits SaveSettings");
        };
        assert_eq!(
            request,
            wire::SettingsSetRequest {
                title_retention: Some(wire::TitleRetention::Days30),
                ..Default::default()
            }
        );
        assert_eq!(page.retention_confirm(), RetentionConfirm::None);

        page.apply_saved(
            generation,
            Err(LoadDataError::from(RequestCoreError::Timeout)),
        );
        assert_eq!(
            page.values().expect("reverted retention").title_retention,
            wire::TitleRetention::Forever
        );
        assert!(page.recording_error().is_some());
        assert!(page.general_error().is_none());

        // Lengthening from 30d to 365d sends immediately without confirmation.
        let mut committed = sample_response(
            wire::ResidentEnabled::Enabled,
            wire::PrewarmEnabled::Enabled,
            wire::PrewarmOnBattery::Blocked,
        );
        committed.title_retention = wire::TitleRetention::Days30;
        let SettingsEffect::LoadSettings { generation: load } = page.open() else {
            panic!("reload");
        };
        page.apply_loaded(load, Ok(committed));
        let SettingsEffect::SaveSettings { request, .. } =
            page.select_title_retention(wire::TitleRetention::Days365)
        else {
            panic!("lengthening retention sends immediately");
        };
        assert_eq!(
            request,
            wire::SettingsSetRequest {
                title_retention: Some(wire::TitleRetention::Days365),
                ..Default::default()
            }
        );
    }

    #[test]
    fn inclusive_date_range_converts_to_half_open_and_caps_at_yesterday() {
        let today = date(2026, 11, 27);
        let yesterday = date(2026, 11, 26);
        assert_eq!(max_deletable_date(today), Some(yesterday));

        let single_day =
            build_delete_range_request(yesterday, yesterday, today).expect("single finished day");
        assert_eq!(single_day.days, 1);
        assert_eq!(
            single_day.request,
            wire::DataDeleteRangeRequest {
                start_date: yesterday,
                end_date: today,
            }
        );

        let multi_day = build_delete_range_request(date(2026, 11, 20), yesterday, today)
            .expect("multi-day range");
        assert_eq!(multi_day.days, 7);
        assert_eq!(
            multi_day.request,
            wire::DataDeleteRangeRequest {
                start_date: date(2026, 11, 20),
                end_date: today,
            }
        );

        assert_eq!(
            build_delete_range_request(date(2026, 11, 20), today, today),
            Err(ValidateDeleteRangeError::EndAfterYesterday)
        );
        assert_eq!(
            build_delete_range_request(yesterday, date(2026, 11, 20), today),
            Err(ValidateDeleteRangeError::EndBeforeStart)
        );
        assert_eq!(
            build_delete_range_request(date(2025, 11, 20), yesterday, today),
            Err(ValidateDeleteRangeError::ExceedsMaxSpan)
        );

        let mut page = ready_page();
        page.set_delete_start(date(2026, 11, 20), today);
        page.set_delete_end(date(2026, 11, 30), today);
        assert_eq!(page.delete_start(), Some(date(2026, 11, 20)));
        assert_eq!(page.delete_end(), Some(yesterday));
    }

    #[test]
    fn date_range_deletion_requires_confirmation_and_refreshes_info_after_success() {
        let today = date(2026, 11, 27);
        let mut page = ready_page();
        page.set_delete_start(date(2026, 11, 20), today);
        page.set_delete_end(date(2026, 11, 22), today);

        // Confirming before opening inline confirmation is a no-op.
        assert_eq!(page.confirm_delete_range(today), SettingsEffect::None);

        page.request_delete_range(today);
        assert!(matches!(
            page.delete_confirm(),
            DeleteRangeConfirm::Pending(range) if range.days == 3
        ));

        page.cancel_delete_range();
        assert_eq!(page.delete_confirm(), DeleteRangeConfirm::None);

        page.request_delete_range(today);
        let SettingsEffect::DeleteRange {
            request,
            generation,
        } = page.confirm_delete_range(today)
        else {
            panic!("confirmed delete emits DeleteRange");
        };
        assert_eq!(
            request,
            wire::DataDeleteRangeRequest {
                start_date: date(2026, 11, 20),
                end_date: date(2026, 11, 23),
            }
        );
        assert_eq!(page.data_operation(), DataOperation::Deleting);

        let follow_up = page.apply_delete_finished(
            generation,
            Ok(wire::DataDeleteRangeResponse {
                deleted_ms: 5_400_000,
                versions: wire::QueryVersions("v1".into()),
            }),
        );
        assert!(matches!(follow_up, SettingsEffect::LoadDataInfo { .. }));
        assert_eq!(page.data_operation(), DataOperation::Idle);
        assert!(matches!(
            page.delete_notice(),
            DeleteNotice::Succeeded {
                deleted_ms: 5_400_000
            }
        ));
    }

    #[test]
    fn busy_data_operation_blocks_double_submit_until_completion() {
        let today = date(2026, 11, 27);
        let mut page = ready_page();
        page.set_delete_start(date(2026, 11, 20), today);
        page.set_delete_end(date(2026, 11, 21), today);

        let SettingsEffect::Compact { generation } = page.start_compact() else {
            panic!("start_compact emits Compact");
        };
        assert_eq!(page.data_operation(), DataOperation::Compacting);
        assert_eq!(page.start_compact(), SettingsEffect::None);
        assert_eq!(page.start_backup(), SettingsEffect::None);
        page.request_delete_range(today);
        assert_eq!(page.delete_confirm(), DeleteRangeConfirm::None);

        let follow_up = page.apply_compact_finished(
            generation,
            Ok(wire::DataCompactResponse {
                before_bytes: 20_000_000,
                after_bytes: 12_000_000,
            }),
        );
        assert!(matches!(follow_up, SettingsEffect::LoadDataInfo { .. }));
        assert_eq!(page.data_operation(), DataOperation::Idle);
        assert!(matches!(
            page.compact_notice(),
            CompactNotice::Succeeded(wire::DataCompactResponse {
                before_bytes: 20_000_000,
                after_bytes: 12_000_000,
            })
        ));

        let SettingsEffect::Backup {
            generation: backup_gen,
        } = page.start_backup()
        else {
            panic!("start_backup emits Backup once idle");
        };
        assert_eq!(page.start_backup(), SettingsEffect::None);
        assert_eq!(
            page.apply_backup_finished(
                backup_gen,
                Err(LoadDataError::from(RequestCoreError::Timeout)),
            ),
            SettingsEffect::None
        );
        assert_eq!(page.data_operation(), DataOperation::Idle);
        assert!(page.backup_error().is_some());
    }

    #[test]
    fn backup_list_orders_newest_first_and_truncates_to_five_entries() {
        let entries: Vec<wire::BackupEntry> = [300, 100, 600, 400, 700, 200, 500]
            .into_iter()
            .map(|ts| wire::BackupEntry {
                file_name: format!("manual-{ts}.db"),
                bytes: 1024,
                created_utc_ms: ts,
                kind: wire::BackupKind::Manual,
            })
            .collect();

        let mut page = ready_page();
        let SettingsEffect::LoadDataInfo { generation } = page.load_data_info() else {
            panic!("load_data_info emits LoadDataInfo");
        };
        page.apply_data_info_loaded(
            generation,
            Ok(wire::DataInfoResponse {
                database_path: r"C:\Quartz\quartz.db".into(),
                database_bytes: 4096,
                logs_path: r"C:\Quartz\logs".into(),
                backups_path: r"C:\Quartz\backups".into(),
                backups: entries,
            }),
        );
        let DataInfoLoadState::Ready(info) = page.data_info() else {
            panic!("data info ready");
        };
        assert_eq!(info.backups.len(), MAX_VISIBLE_BACKUPS);
        let timestamps: Vec<i64> = info.backups.iter().map(|b| b.created_utc_ms).collect();
        assert_eq!(timestamps, vec![700, 600, 500, 400, 300]);
    }

    #[test]
    fn core_status_load_and_live_recording_sync_keep_about_state_current() {
        let mut page = ready_page();
        let SettingsEffect::LoadCoreStatus { generation } = page.load_core_status() else {
            panic!("load_core_status emits LoadCoreStatus");
        };
        page.apply_core_status_loaded(
            generation,
            Ok(wire::CoreStatusResponse {
                recording: true,
                stop_reason: None,
                database: wire::DatabaseState::Ok,
                browser_streams: Vec::new(),
                instance_id: wire::InstanceId("inst".into()),
                core_version: wire::CoreVersion("0.2.0".into()),
            }),
        );
        page.sync_recording_state(false, Some(wire::StopReason::Paused));
        let CoreStatusLoadState::Ready(status) = page.core_status() else {
            panic!("core status ready");
        };
        assert!(!status.recording);
        assert_eq!(status.stop_reason, Some(wire::StopReason::Paused));
        assert_eq!(status.core_version.0, "0.2.0");

        let today = date(2026, 11, 27);
        page.set_delete_start(date(2026, 11, 25), today);
        page.set_delete_end(date(2026, 11, 26), today);
        page.request_delete_range(today);
        assert!(matches!(
            page.delete_confirm(),
            DeleteRangeConfirm::Pending(_)
        ));
        page.on_logical_date_changed(date(2026, 11, 26));
        assert_eq!(page.delete_confirm(), DeleteRangeConfirm::None);
    }
}
