//! Fixed presentation inputs for the preview; no pipe or background queries.
use super::{AppShell, calendar::CalendarMonth, live::LoadPhase};
use crate::{
    app::preview::{PreviewState, TodayPreviewState},
    link::DayFailure,
    model::Day,
};
use quartz_protocol::{LocaleSettings, StateSnapshot, StopReason};

#[derive(Debug)]
pub(super) struct PreviewDay {
    pub snapshot: StateSnapshot,
    pub phase: LoadPhase,
    pub failure: Option<DayFailure>,
}
impl AppShell {
    pub(crate) fn configure_preview(&mut self, state: PreviewState) {
        let scenario = match state {
            PreviewState::TodayData(scenario) => Some(scenario),
            _ => None,
        };
        let selected = self.day.date();
        let past = matches!(
            scenario,
            Some(TodayPreviewState::EmptyPast | TodayPreviewState::DatePicker)
        );
        let today = if past {
            selected.tomorrow().expect("sample date has a next day")
        } else {
            selected
        };
        let stop_reason = match scenario {
            Some(TodayPreviewState::Paused) => Some(StopReason::Paused),
            Some(TodayPreviewState::PersistenceFailed) => Some(StopReason::PersistenceFailed),
            _ => None,
        };
        if matches!(
            scenario,
            Some(
                TodayPreviewState::Loading
                    | TodayPreviewState::EmptyToday
                    | TodayPreviewState::EmptyPast
                    | TodayPreviewState::QueryFailed
            )
        ) {
            self.day = Day::empty(selected);
        }
        if scenario == Some(TodayPreviewState::DatePicker) {
            self.calendar = Some(CalendarMonth::new(selected));
        }
        self.preview = Some(PreviewDay {
            snapshot: StateSnapshot {
                ui_resident: quartz_protocol::ResidentEnabled::Disabled,
                logical_date: today,
                timed_policies: Default::default(),
                foreground: None,
                recording: stop_reason.is_none(),
                stop_reason,
                browser_streams: Vec::new(),
                ui_language: if self.formatter.language()
                    == quartz_i18n::Language::SimplifiedChinese
                {
                    quartz_protocol::UiLanguage::SimplifiedChinese
                } else {
                    quartz_protocol::UiLanguage::English
                },
                locale: Some(LocaleSettings {
                    hour_cycle: quartz_protocol::HourCycle::H24,
                    first_day_of_week: if self.formatter.language()
                        == quartz_i18n::Language::SimplifiedChinese
                    {
                        quartz_protocol::Weekday::Monday
                    } else {
                        quartz_protocol::Weekday::Sunday
                    },
                }),
            },
            phase: if scenario == Some(TodayPreviewState::Loading) {
                LoadPhase::Loading
            } else {
                LoadPhase::Ready
            },
            failure: (scenario == Some(TodayPreviewState::QueryFailed))
                .then_some(DayFailure::InvalidData),
        });
    }
}
