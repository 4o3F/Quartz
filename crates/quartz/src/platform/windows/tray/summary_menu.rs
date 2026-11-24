//! Summary, annotation, browser status and default menu sections.

use super::{
    TrayContent,
    icon::{Menu, MenuItemLabel, ShowTrayMenuError, TrayMenuItem},
};
use crate::{
    core::app::annotation::{AnnotationChoice, IdlePeriod},
    platform::windows::WideString,
};
use quartz_i18n::{Language, MessageKey};
use windows::{
    Win32::UI::WindowsAndMessaging::{
        self, MENU_ITEM_FLAGS, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING,
    },
    core::PCWSTR,
};

impl TrayMenuItem {
    pub(super) fn annotation_choice(self) -> Option<AnnotationChoice> {
        match self {
            Self::Rest => Some(AnnotationChoice::Rest),
            Self::Meeting => Some(AnnotationChoice::Meeting),
            Self::Reading => Some(AnnotationChoice::Reading),
            Self::Ignore => Some(AnnotationChoice::Ignore),
            _ => None,
        }
    }
}
impl Menu {
    fn append_text(
        &self,
        item: MenuItemLabel,
        text: &str,
        id: usize,
        flags: MENU_ITEM_FLAGS,
    ) -> Result<(), ShowTrayMenuError> {
        let label = WideString::new(text);
        // SAFETY: The menu is live; AppendMenuW copies the terminated label.
        unsafe { WindowsAndMessaging::AppendMenuW(self.0, flags, id, PCWSTR(label.as_ptr())) }
            .map_err(|source| ShowTrayMenuError::AppendMenu { item, source })
    }
    pub(super) fn set_default(&self, item: TrayMenuItem) -> Result<(), ShowTrayMenuError> {
        let id = u32::try_from(usize::from(item))
            .map_err(|source| ShowTrayMenuError::MenuCommand { source })?;
        // SAFETY: The command identifies an item already inserted into this live menu.
        unsafe { WindowsAndMessaging::SetMenuDefaultItem(self.0, id, 0) }
            .map_err(|source| ShowTrayMenuError::SetDefault { source })
    }
    pub(super) fn separator(&self) -> Result<(), ShowTrayMenuError> {
        self.append_text(MenuItemLabel::Separator, "", 0, MF_SEPARATOR)
    }
    pub(super) fn append_summary(
        &self,
        content: &TrayContent,
        language: Language,
    ) -> Result<(), ShowTrayMenuError> {
        content.append_summary_lines(language, |item, text| {
            self.append_text(item, text, 0, MF_STRING | MF_GRAYED)
        })?;
        self.separator()
    }
    pub(super) fn append_annotation(
        &self,
        idle: Option<IdlePeriod>,
        language: Language,
    ) -> Result<(), ShowTrayMenuError> {
        let Some(period) = idle else {
            return Ok(());
        };
        let submenu = Self::new()?;
        for item in [
            TrayMenuItem::Rest,
            TrayMenuItem::Meeting,
            TrayMenuItem::Reading,
            TrayMenuItem::Ignore,
        ] {
            submenu.append(item, language)?;
        }
        let key = MessageKey::new("tray.annotate");
        let label = key
            .in_language(language)
            .replace("%{minutes}", &period.minutes().to_string());
        self.append_text(key.into(), &label, submenu.0.0.addr(), MF_POPUP)?;
        std::mem::forget(submenu);
        Ok(())
    }
    pub(super) fn append_browser_status(
        &self,
        count: usize,
        language: Language,
    ) -> Result<(), ShowTrayMenuError> {
        let key = MessageKey::new("tray.browsers");
        self.append_text(
            key.into(),
            &key.in_language(language)
                .replace("%{count}", &count.to_string()),
            0,
            MF_STRING | MF_GRAYED,
        )
    }
}

impl TrayContent {
    fn append_summary_lines(
        &self,
        language: Language,
        mut append: impl FnMut(MenuItemLabel, &str) -> Result<(), ShowTrayMenuError>,
    ) -> Result<(), ShowTrayMenuError> {
        if let Some(summary) = &self.summary {
            append(
                MessageKey::new("tray.today").into(),
                &summary.today_text(language),
            )?;
            append(
                MenuItemLabel::Categories,
                &summary.categories_text(language),
            )?;
            append(
                MessageKey::new("tray.recent").into(),
                &summary.recent_text(language),
            )?;
        } else {
            let key = MessageKey::new("tray.loading");
            append(key.into(), &key.in_language(language))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::DayMetrics;
    #[test]
    fn summary_append_failures_identify_the_actual_information_line() {
        let content = TrayContent {
            summary: Some(std::sync::Arc::new(crate::tray_summary::TraySummary {
                metrics: DayMetrics {
                    active_ms: 0,
                    focus_ratio: 0.0,
                    distract_ratio: 0.0,
                    unclassified_ms: 0,
                    longest_focus_ms: 0,
                    switches_per_hour: 0.0,
                },
                categories: Vec::new(),
                recent: None,
            })),
            ..Default::default()
        };
        struct FailureCase {
            position: usize,
            item: MenuItemLabel,
        }
        for case in [
            FailureCase {
                position: 0,
                item: MenuItemLabel::Catalog(MessageKey::new("tray.today")),
            },
            FailureCase {
                position: 1,
                item: MenuItemLabel::Categories,
            },
            FailureCase {
                position: 2,
                item: MenuItemLabel::Catalog(MessageKey::new("tray.recent")),
            },
        ] {
            let mut position = 0;
            let error = content
                .append_summary_lines(Language::English, |item, _| {
                    if position == case.position {
                        return Err(ShowTrayMenuError::AppendMenu {
                            item,
                            source: windows::core::Error::from_hresult(
                                windows::core::HRESULT::from_win32(
                                    windows::Win32::Foundation::ERROR_ACCESS_DENIED.0,
                                ),
                            ),
                        });
                    }
                    position += 1;
                    Ok(())
                })
                .expect_err("injected append failure");
            assert!(
                matches!(error, ShowTrayMenuError::AppendMenu { item, .. } if item == case.item)
            );
        }
    }
    #[test]
    fn native_summary_is_disabled_open_is_default_and_annotation_is_localized() {
        for language in [Language::English, Language::SimplifiedChinese] {
            let menu = Menu::new().expect("menu");
            let content = TrayContent {
                summary: Some(std::sync::Arc::new(crate::tray_summary::TraySummary {
                    metrics: DayMetrics {
                        active_ms: 0,
                        focus_ratio: 0.0,
                        distract_ratio: 0.0,
                        unclassified_ms: 0,
                        longest_focus_ms: 0,
                        switches_per_hour: 0.0,
                    },
                    categories: Vec::new(),
                    recent: None,
                })),
                idle: Some(IdlePeriod {
                    start_utc_ms: 0,
                    end_utc_ms: 25 * crate::tray_summary::MINUTE_MS,
                }),
                ..Default::default()
            };
            menu.append_summary(&content, language).expect("summary");
            menu.append(TrayMenuItem::Open, language).expect("open");
            menu.set_default(TrayMenuItem::Open).expect("default");
            menu.append_annotation(content.idle, language)
                .expect("annotation");
            menu.append_browser_status(2, language)
                .expect("browser status");
            // SAFETY: All inspected menus are owned by the live parent for this iteration.
            unsafe {
                for position in 0..3 {
                    assert_ne!(
                        WindowsAndMessaging::GetMenuState(
                            menu.0,
                            position,
                            WindowsAndMessaging::MF_BYPOSITION
                        ) & MF_GRAYED.0,
                        0
                    );
                }
                assert_eq!(
                    WindowsAndMessaging::GetMenuDefaultItem(menu.0, 0, Default::default()),
                    u32::try_from(usize::from(TrayMenuItem::Open)).expect("command ID")
                );
                let submenu = WindowsAndMessaging::GetSubMenu(menu.0, 5);
                assert_eq!(WindowsAndMessaging::GetMenuItemCount(Some(submenu)), 4);
                let mut label = [0_u16; 128];
                let count = WindowsAndMessaging::GetMenuStringW(
                    menu.0,
                    5,
                    Some(&mut label),
                    WindowsAndMessaging::MF_BYPOSITION,
                );
                assert_eq!(
                    String::from_utf16(&label[..usize::try_from(count).expect("label length")])
                        .expect("label"),
                    MessageKey::new("tray.annotate")
                        .in_language(language)
                        .replace("%{minutes}", "25")
                );
                assert_ne!(
                    WindowsAndMessaging::GetMenuState(
                        menu.0,
                        6,
                        WindowsAndMessaging::MF_BYPOSITION
                    ) & MF_GRAYED.0,
                    0
                );
            }
        }
    }
}
