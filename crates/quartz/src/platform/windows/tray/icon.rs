//! Shell notification icon and localized popup menu on the main thread.

use std::fmt;

use snafu::{ResultExt, Snafu};
use windows::Win32::{
    Foundation::{LPARAM, POINT, WPARAM},
    UI::{
        Shell::{
            self, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONDATAW_0,
        },
        WindowsAndMessaging::{
            self, HMENU, IDI_APPLICATION, IDI_WARNING, TPM_NONOTIFY, TPM_RETURNCMD,
            TPM_RIGHTBUTTON, WM_NULL,
        },
    },
};

use super::window::{AppMessage, Window};
use crate::tray_tooltip::{FormatTrayTooltipError, TrayTooltipText};

// A single notification icon is identified by this ID together with its window handle.
const ICON_ID: u32 = 1;

/// Failures while preparing and adding a version 4 notification icon.
#[derive(Debug, Snafu)]
pub(in crate::platform::windows) enum AddTrayIconError {
    #[snafu(display("could not load tray application icon (LoadIconW)"))]
    LoadIcon { source: windows::core::Error },
    #[snafu(display("could not generate tray icon variants"))]
    GenerateIcons {
        source: super::variants::GenerateTrayIconsError,
    },
    #[snafu(display("could not encode initial tray tooltip"))]
    FormatInitialTooltip { source: FormatTrayTooltipError },
    // Shell_NotifyIconW documents BOOL success without an extended GetLastError value.
    #[snafu(display("could not add tray icon (Shell_NotifyIconW, NIM_ADD)"))]
    NotifyAdd,
    #[snafu(display("could not set tray icon version 4 (Shell_NotifyIconW, NIM_SETVERSION)"))]
    SetVersion,
}

/// Failures while encoding or applying a notification icon's tooltip.
#[derive(Debug, Snafu)]
pub(in crate::platform::windows) enum UpdateTrayTooltipError {
    #[snafu(display("could not load tray state icon (LoadIconW)"))]
    LoadUpdateIcon { source: windows::core::Error },
    #[snafu(display("could not encode tray tooltip"))]
    FormatTooltip { source: FormatTrayTooltipError },
    #[snafu(display("could not update tray tooltip (Shell_NotifyIconW, NIM_MODIFY)"))]
    NotifyModify,
}

/// Failures while constructing and positioning the localized tray menu.
#[derive(Debug, Snafu)]
pub(in crate::platform::windows) enum ShowTrayMenuError {
    #[snafu(display("could not create tray menu (CreatePopupMenu)"))]
    CreatePopupMenu { source: windows::core::Error },
    #[snafu(display("could not append tray menu item {key} (AppendMenuW)"))]
    AppendMenu {
        key: quartz_i18n::MessageKey<'static>,
        source: windows::core::Error,
    },
    #[snafu(display("tray menu command exceeds the supported range"))]
    MenuCommand { source: std::num::TryFromIntError },
    #[snafu(display("could not locate tray menu cursor (GetCursorPos)"))]
    GetCursorPosition { source: windows::core::Error },
}

/// Tray commands with their Windows menu IDs and translation keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
#[derive(num_enum::IntoPrimitive)]
pub(super) enum TrayMenuItem {
    Open = 1,
    Exit = 2,
    Pause15 = 3,
    PauseHour = 4,
    PauseTomorrow = 5,
    Resume = 6,
    Keep30 = 7,
    KeepHour = 8,
    Private15 = 9,
    PrivateHour = 10,
    PrivateTomorrow = 11,
    PrivateOff = 12,
}

impl TrayMenuItem {
    const ALL: [Self; 12] = [
        Self::Open,
        Self::Exit,
        Self::Pause15,
        Self::PauseHour,
        Self::PauseTomorrow,
        Self::Resume,
        Self::Keep30,
        Self::KeepHour,
        Self::Private15,
        Self::PrivateHour,
        Self::PrivateTomorrow,
        Self::PrivateOff,
    ];

    pub(super) fn label_key(self) -> quartz_i18n::MessageKey<'static> {
        quartz_i18n::MessageKey::new(match self {
            Self::Open => "tray.open",
            Self::Exit => "tray.exit",
            Self::Pause15 | Self::Private15 => "tray.minutes_15",
            Self::Keep30 => "tray.minutes_30",
            Self::PauseHour | Self::KeepHour | Self::PrivateHour => "tray.hour_1",
            Self::PauseTomorrow | Self::PrivateTomorrow => "tray.until_tomorrow",
            Self::Resume => "tray.resume",
            Self::PrivateOff => "tray.privacy_off",
        })
    }

    fn from_command(id: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|item| usize::from(*item) == id)
    }
}

pub(super) struct TrayIcon {
    data: NOTIFYICONDATAW,
    language: quartz_i18n::Language,
    variants: super::variants::IconVariants,
    window: windows::Win32::Foundation::HWND,
}

impl fmt::Debug for TrayIcon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrayIcon")
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

impl TrayIcon {
    /// Loads the icon, encodes its tooltip, and registers version 4 callbacks.
    ///
    /// # Errors
    /// Returns icon loading, tooltip encoding, or Shell registration failures.
    pub(super) fn add(
        window: &Window,
        language: quartz_i18n::Language,
    ) -> Result<Self, AddTrayIconError> {
        // TODO(M6): Replace the shared default application icon with the Quartz icon resource.
        // SAFETY: IDI_APPLICATION names a shared system icon; it must not be destroyed by the caller.
        let icon = unsafe { WindowsAndMessaging::LoadIconW(None, IDI_APPLICATION) }
            .context(LoadIconSnafu)?;
        let variants = super::variants::IconVariants::new(icon).context(GenerateIconsSnafu)?;
        let tray = Self {
            variants,
            language,
            data: NOTIFYICONDATAW {
                cbSize: const { super::super::structure_size::<NOTIFYICONDATAW>() },
                hWnd: window.raw(),
                uID: ICON_ID,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
                uCallbackMessage: u32::from(AppMessage::TrayCallback),
                hIcon: icon,
                szTip: TrayTooltipText::try_from(
                    quartz_i18n::MessageKey::new("app.name")
                        .in_language(language)
                        .as_ref(),
                )
                .context(FormatInitialTooltipSnafu)?
                .into_buffer(),
                Anonymous: NOTIFYICONDATAW_0 {
                    uVersion: NOTIFYICON_VERSION_4,
                },
                ..Default::default()
            },
            window: window.raw(),
        };
        tray.readd()?;
        Ok(tray)
    }

    pub(super) fn language(&self) -> quartz_i18n::Language {
        self.language
    }

    pub(super) fn set_language(&mut self, language: quartz_i18n::Language) {
        self.language = language;
    }

    /// Re-registers the configured icon and callback version after Explorer restarts.
    ///
    /// # Errors
    /// Returns Shell icon-addition or version-selection failures.
    pub(super) fn readd(&self) -> Result<(), AddTrayIconError> {
        // SAFETY: The initialized data references our live window and a shared system icon.
        if !unsafe { Shell::Shell_NotifyIconW(NIM_ADD, &self.data) }.as_bool() {
            return NotifyAddSnafu.fail();
        }
        // SAFETY: NIM_ADD succeeded; the version field specifies the supported v4 callback format.
        if !unsafe { Shell::Shell_NotifyIconW(NIM_SETVERSION, &self.data) }.as_bool() {
            return SetVersionSnafu.fail();
        }
        Ok(())
    }

    /// Replaces the tooltip with the translation for the supplied key.
    ///
    /// # Errors
    /// Returns tooltip encoding or Shell update failures.
    pub(super) fn update(&mut self, state: super::TrayState) -> Result<(), UpdateTrayTooltipError> {
        self.data.szTip = TrayTooltipText::try_from(state.tooltip.text(self.language).as_str())
            .context(FormatTooltipSnafu)?
            .into_buffer();
        // SAFETY: Both icon identifiers name shared system resources.
        self.data.hIcon = match state.icon {
            super::TrayIconState::Grey => self.variants.grey(),
            super::TrayIconState::Private => self.variants.private(),
            // SAFETY: Both identifiers name shared system icon resources.
            super::TrayIconState::Normal | super::TrayIconState::Warning => unsafe {
                WindowsAndMessaging::LoadIconW(
                    None,
                    match state.icon {
                        super::TrayIconState::Normal => IDI_APPLICATION,
                        super::TrayIconState::Warning => IDI_WARNING,
                        _ => unreachable!("generated icons handled above"),
                    },
                )
            }
            .context(LoadUpdateIconSnafu)?,
        };
        let update = NOTIFYICONDATAW {
            uFlags: NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            ..self.data
        };
        // SAFETY: The update identifies the same live icon and contains a terminated tooltip.
        if !unsafe { Shell::Shell_NotifyIconW(NIM_MODIFY, &update) }.as_bool() {
            return NotifyModifySnafu.fail();
        }
        Ok(())
    }

    /// Shows the localized tray menu, returning None when dismissed.
    ///
    /// # Errors
    /// Returns menu creation, item insertion, or cursor lookup failures.
    pub(super) fn menu(
        &self,
        timed: quartz_protocol::TimedPolicies,
    ) -> Result<Option<TrayMenuItem>, ShowTrayMenuError> {
        // SAFETY: Creates an empty, uniquely owned menu without pointer arguments.
        let menu =
            Menu(unsafe { WindowsAndMessaging::CreatePopupMenu() }.context(CreatePopupMenuSnafu)?);
        menu.append(TrayMenuItem::Open, self.language)?;
        menu.append_timed(timed, self.language)?;
        menu.append(TrayMenuItem::Exit, self.language)?;
        let mut point = POINT::default();
        // SAFETY: point is writable storage for the screen-coordinate cursor position.
        unsafe { WindowsAndMessaging::GetCursorPos(&mut point) }.context(GetCursorPositionSnafu)?;
        // SAFETY: Our window is live on this thread. Foreground ownership lets clicking elsewhere
        // dismiss the menu; TPM_RETURNCMD returns the selection without borrowing callback state.
        let command = unsafe {
            // Focus restrictions may reject this advisory request; the menu can still be shown.
            let _ = WindowsAndMessaging::SetForegroundWindow(self.window);
            let command = WindowsAndMessaging::TrackPopupMenu(
                menu.0,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                point.x,
                point.y,
                None,
                self.window,
                None,
            );
            if let Err(error) =
                WindowsAndMessaging::PostMessageW(Some(self.window), WM_NULL, WPARAM(0), LPARAM(0))
            {
                tracing::warn!(%error, "could not post tray menu dismissal message");
            }
            usize::try_from(command.0).context(MenuCommandSnafu)?
        };
        Ok(TrayMenuItem::from_command(command))
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        // SAFETY: The window outlives the icon; deleting a missing icon is harmless after Explorer exits.
        // Best-effort teardown: Explorer may already have removed this icon.
        let _ = unsafe { Shell::Shell_NotifyIconW(NIM_DELETE, &self.data) };
    }
}

pub(super) struct Menu(pub(super) HMENU);

impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: The popup menu is uniquely owned and TrackPopupMenu has returned.
        // Best-effort teardown: Drop cannot report menu destruction failures.
        let _ = unsafe { WindowsAndMessaging::DestroyMenu(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_menu_commands_round_trip_and_unknown_ids_are_absent() {
        for item in TrayMenuItem::ALL {
            assert_eq!(TrayMenuItem::from_command(usize::from(item)), Some(item));
        }
        for id in [0, 13, usize::MAX] {
            assert_eq!(TrayMenuItem::from_command(id), None, "unknown command {id}");
        }
        assert_eq!(TrayMenuItem::Open.label_key().as_str(), "tray.open");
        assert_eq!(TrayMenuItem::Exit.label_key().as_str(), "tray.exit");
    }
}
