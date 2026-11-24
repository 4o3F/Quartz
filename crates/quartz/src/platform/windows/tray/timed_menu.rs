//! Timed menu sections and semantic commands from 07 §1.3.

use super::icon::{Menu, ShowTrayMenuError, TrayMenuItem};
use crate::{
    ipc::{TimedCommand, TimedDeadline},
    platform::windows::WideString,
};
use quartz_i18n::{Language, MessageKey};
use std::time::Duration;
use windows::{
    Win32::UI::WindowsAndMessaging::{self, MF_POPUP, MF_STRING},
    core::PCWSTR,
};

/// Pause and privacy short option from 07 §1.3.
const SHORT_DURATION: Duration = Duration::from_secs(15 * 60);
/// Keep timing short option from 07 §1.3.
const KEEP_DURATION: Duration = Duration::from_secs(30 * 60);
/// One-hour option shared by all timed menus in 07 §1.3.
const HOUR_DURATION: Duration = Duration::from_secs(60 * 60);

impl TrayMenuItem {
    pub(super) fn timed_command(self) -> Option<TimedCommand> {
        let short = TimedDeadline::After(SHORT_DURATION);
        let hour = TimedDeadline::After(HOUR_DURATION);
        let tomorrow = TimedDeadline::NextLogicalDay;
        Some(match self {
            Self::Pause15 => TimedCommand::Pause(short),
            Self::PauseHour => TimedCommand::Pause(hour),
            Self::PauseTomorrow => TimedCommand::Pause(tomorrow),
            Self::Resume => TimedCommand::Resume,
            Self::Keep30 => TimedCommand::KeepTiming(TimedDeadline::After(KEEP_DURATION)),
            Self::KeepHour => TimedCommand::KeepTiming(hour),
            Self::Private15 => TimedCommand::EnablePrivacy(short),
            Self::PrivateHour => TimedCommand::EnablePrivacy(hour),
            Self::PrivateTomorrow => TimedCommand::EnablePrivacy(tomorrow),
            Self::PrivateOff => TimedCommand::DisablePrivacy,
            Self::Open
            | Self::Exit
            | Self::Settings
            | Self::Rest
            | Self::Meeting
            | Self::Reading
            | Self::Ignore => return None,
        })
    }
}
impl Menu {
    pub(super) fn new() -> Result<Self, ShowTrayMenuError> {
        // SAFETY: Creates a uniquely owned empty menu.
        unsafe { WindowsAndMessaging::CreatePopupMenu() }
            .map(Self)
            .map_err(|source| ShowTrayMenuError::CreatePopupMenu { source })
    }
    pub(super) fn append(
        &self,
        item: TrayMenuItem,
        language: Language,
    ) -> Result<(), ShowTrayMenuError> {
        self.append_label(item.label_key(), usize::from(item), MF_STRING, language)
    }
    pub(super) fn append_label(
        &self,
        key: MessageKey<'static>,
        id: usize,
        flags: windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS,
        language: Language,
    ) -> Result<(), ShowTrayMenuError> {
        let label = WideString::new(&key.in_language(language));
        // SAFETY: The menu is live and AppendMenuW copies the terminated label.
        unsafe { WindowsAndMessaging::AppendMenuW(self.0, flags, id, PCWSTR(label.as_ptr())) }
            .map_err(|source| ShowTrayMenuError::AppendMenu {
                item: key.into(),
                source,
            })
    }
    pub(super) fn submenu(
        &self,
        key: MessageKey<'static>,
        items: &[TrayMenuItem],
        language: Language,
    ) -> Result<(), ShowTrayMenuError> {
        let submenu = Self::new()?;
        for item in items {
            submenu.append(*item, language)?;
        }
        // Windows represents popup handles in the command slot; retain ownership until attachment succeeds.
        self.append_label(key, submenu.0.0.addr(), MF_POPUP, language)?;
        std::mem::forget(submenu);
        Ok(())
    }
    pub(super) fn append_timed(
        &self,
        timed: quartz_protocol::TimedPolicies,
        language: Language,
    ) -> Result<(), ShowTrayMenuError> {
        use TrayMenuItem::*;
        let mut pause = vec![Pause15, PauseHour, PauseTomorrow];
        if timed.pause_until_utc_ms.is_some() {
            pause.push(Resume);
        }
        self.submenu(MessageKey::new("tray.pause"), &pause, language)?;
        self.submenu(
            MessageKey::new("tray.keep_timing"),
            &[Keep30, KeepHour],
            language,
        )?;
        let mut privacy = vec![Private15, PrivateHour, PrivateTomorrow];
        if timed.privacy_mode.is_some() {
            privacy.push(PrivateOff);
        }
        self.submenu(MessageKey::new("tray.privacy"), &privacy, language)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::{PrivacyMode, TimedPolicies, TimedPrivacyMode};
    #[test]
    fn native_submenus_follow_active_snapshot_and_selected_language() {
        for language in [Language::English, Language::SimplifiedChinese] {
            for timed in [
                TimedPolicies::default(),
                TimedPolicies {
                    pause_until_utc_ms: Some(100),
                    privacy_mode: Some(TimedPrivacyMode {
                        mode: PrivacyMode::Private,
                        until_utc_ms: 200,
                    }),
                    keep_timing_until_utc_ms: None,
                },
            ] {
                let menu = Menu::new().expect("menu");
                menu.append_timed(timed, language).expect("timed sections");
                // SAFETY: The parent owns three live submenu handles until the end of this iteration.
                unsafe {
                    assert_eq!(WindowsAndMessaging::GetMenuItemCount(Some(menu.0)), 3);
                    let pause = WindowsAndMessaging::GetSubMenu(menu.0, 0);
                    let keep = WindowsAndMessaging::GetSubMenu(menu.0, 1);
                    let privacy = WindowsAndMessaging::GetSubMenu(menu.0, 2);
                    assert_eq!(
                        WindowsAndMessaging::GetMenuItemCount(Some(pause)),
                        if timed.pause_until_utc_ms.is_some() {
                            4
                        } else {
                            3
                        }
                    );
                    assert_eq!(WindowsAndMessaging::GetMenuItemCount(Some(keep)), 2);
                    assert_eq!(
                        WindowsAndMessaging::GetMenuItemCount(Some(privacy)),
                        if timed.privacy_mode.is_some() { 4 } else { 3 }
                    );
                    let mut label = [0_u16; 128];
                    let count = WindowsAndMessaging::GetMenuStringW(
                        menu.0,
                        0,
                        Some(&mut label),
                        WindowsAndMessaging::MF_BYPOSITION,
                    );
                    let count = usize::try_from(count).expect("positive label length");
                    assert_eq!(
                        String::from_utf16(&label[..count]).expect("menu label"),
                        MessageKey::new("tray.pause").in_language(language)
                    );
                }
            }
        }
    }
    #[test]
    fn every_timed_menu_action_and_label_is_available() {
        for language in [Language::English, Language::SimplifiedChinese] {
            for item in [
                TrayMenuItem::Pause15,
                TrayMenuItem::PauseHour,
                TrayMenuItem::PauseTomorrow,
                TrayMenuItem::Resume,
                TrayMenuItem::Keep30,
                TrayMenuItem::KeepHour,
                TrayMenuItem::Private15,
                TrayMenuItem::PrivateHour,
                TrayMenuItem::PrivateTomorrow,
                TrayMenuItem::PrivateOff,
            ] {
                assert!(item.timed_command().is_some());
                assert!(item.label_key().is_in_catalog(language));
            }
        }
    }
}
