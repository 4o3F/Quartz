//! Settings page view model: General group values, prewarm visibility rules, one-field updates,
//! and rollback with inline error reporting on failed `settings.set` requests (07 section 7).

use quartz_protocol as wire;

use crate::link::LoadDataError;

/// Monotonic generation counter for dropping late `settings.get` / `settings.set` responses.
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

/// Current General settings snapshot displayed by the controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SettingsValues {
    pub(crate) autostart: wire::AutostartEnabled,
    pub(crate) ui: wire::UiSettings,
    pub(crate) effective: wire::EffectiveUiSettings,
}

impl From<wire::SettingsResponse> for SettingsValues {
    fn from(response: wire::SettingsResponse) -> Self {
        Self {
            autostart: response.autostart,
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
    fn apply_change(&mut self, change: GeneralSettingChange) {
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
    fn differs_from(&self, change: GeneralSettingChange) -> bool {
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

/// Load state of the Settings page.
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
enum PendingSave {
    Idle,
    InFlight {
        generation: SettingsGeneration,
        previous: SettingsValues,
    },
}

/// Side effect emitted by [`SettingsPage`] state transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsEffect {
    None,
    Load {
        generation: SettingsGeneration,
    },
    Save {
        request: wire::SettingsSetRequest,
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
        }
    }

    /// Current load state of the Settings page.
    #[must_use]
    pub(crate) const fn state(&self) -> &SettingsLoadState {
        &self.state
    }

    /// Currently displayed General settings values once loaded.
    #[must_use]
    pub(crate) const fn values(&self) -> Option<&SettingsValues> {
        match &self.state {
            SettingsLoadState::Ready(values) => Some(values),
            SettingsLoadState::Loading | SettingsLoadState::Failed(_) => None,
        }
    }

    /// Inline error shown inside the General card after a failed update.
    #[must_use]
    pub(crate) const fn general_error(&self) -> Option<&LoadDataError> {
        self.general_error.as_ref()
    }

    /// Whether a General setting update is currently in flight.
    #[must_use]
    pub(crate) const fn save_activity(&self) -> SaveActivity {
        match self.pending_save {
            PendingSave::Idle => SaveActivity::Idle,
            PendingSave::InFlight { .. } => SaveActivity::Saving,
        }
    }

    /// Requests fresh settings from core when opening or retrying the Settings page.
    #[must_use]
    pub(crate) fn open(&mut self) -> SettingsEffect {
        self.load_generation = self.load_generation.next();
        if !matches!(self.state, SettingsLoadState::Ready(_)) {
            self.state = SettingsLoadState::Loading;
        }
        self.general_error = None;
        SettingsEffect::Load {
            generation: self.load_generation,
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

    /// Starts a one-field `settings.set` mutation with optimistic display and rollback snapshot.
    #[must_use]
    pub(crate) fn change_general(&mut self, change: GeneralSettingChange) -> SettingsEffect {
        if matches!(self.pending_save, PendingSave::InFlight { .. }) {
            return SettingsEffect::None;
        }
        let SettingsLoadState::Ready(current) = &mut self.state else {
            return SettingsEffect::None;
        };
        if !current.differs_from(change) {
            return SettingsEffect::None;
        }
        let previous = *current;
        current.apply_change(change);
        self.general_error = None;
        self.save_generation = self.save_generation.next();
        self.pending_save = PendingSave::InFlight {
            generation: self.save_generation,
            previous,
        };
        SettingsEffect::Save {
            request: change.into_request(),
            generation: self.save_generation,
        }
    }

    /// Applies the reply from `settings.set`; on failure reverts the displayed control value and
    /// records an inline error for the General card.
    pub(crate) fn apply_saved(
        &mut self,
        generation: SettingsGeneration,
        result: Result<wire::SettingsResponse, LoadDataError>,
    ) {
        let PendingSave::InFlight {
            generation: active,
            previous,
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
                self.general_error = None;
            }
            Err(error) => {
                self.state = SettingsLoadState::Ready(previous);
                self.general_error = Some(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::RequestCoreError;

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
        let mut page = SettingsPage::new();
        let SettingsEffect::Load { generation } = page.open() else {
            panic!("opening settings emits Load");
        };
        let initial = sample_response(
            wire::ResidentEnabled::Enabled,
            wire::PrewarmEnabled::Enabled,
            wire::PrewarmOnBattery::Blocked,
        );
        page.apply_loaded(generation, Ok(initial));

        let SettingsEffect::Save {
            request,
            generation: save_gen,
        } = page.change_general(GeneralSettingChange::Autostart(
            wire::AutostartEnabled::Enabled,
        ))
        else {
            panic!("toggling autostart emits Save");
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
        let SettingsEffect::Load { generation } = page.open() else {
            panic!("open emits Load");
        };
        page.apply_loaded(
            generation,
            Err(LoadDataError::from(RequestCoreError::Disconnected)),
        );
        assert!(matches!(page.state(), SettingsLoadState::Failed(_)));

        let SettingsEffect::Load { generation } = page.open() else {
            panic!("retry open emits Load");
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

        let SettingsEffect::Save {
            request,
            generation: save_gen,
        } = page.change_general(GeneralSettingChange::Language(wire::UiLanguageSetting::En))
        else {
            panic!("changing language emits Save");
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
}
