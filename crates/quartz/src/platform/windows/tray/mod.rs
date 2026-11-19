//! Shell tray state, native callbacks and semantic commands.

mod icon;
pub(super) mod window;

use icon::{AddTrayIconError, TrayIcon, TrayMenuItem, UpdateTrayTooltipError};
use snafu::{ResultExt, Snafu};
use window::{CoreEvent, CreateCoreWindowError, Window};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrayIconState {
    Normal,
    Warning,
}
pub(super) use crate::tray_tooltip::TrayTooltip;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TrayState {
    pub icon: TrayIconState,
    pub tooltip: TrayTooltip,
}
impl Default for TrayState {
    fn default() -> Self {
        Self {
            icon: TrayIconState::Normal,
            tooltip: TrayTooltip::Ready,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrayCommand {
    OpenUi,
    Exit,
    EndSession,
    Deadline(quartz_domain::capture::Timestamp),
}

#[derive(Debug, Snafu)]
pub(super) enum CreateTrayError {
    #[snafu(display("could not create tray callback window"))]
    Window { source: CreateCoreWindowError },
    #[snafu(display("could not register tray icon"))]
    Icon { source: AddTrayIconError },
    #[snafu(display("could not apply initial tray state"))]
    State { source: UpdateTrayTooltipError },
}

#[derive(Debug, Snafu)]
#[snafu(display("could not apply tray state"))]
pub(super) struct UpdateTrayStateError {
    source: UpdateTrayTooltipError,
}

pub(super) struct Tray {
    // Fields drop in declaration order, removing the icon before its callback window.
    icon: TrayIcon,
    window: Window,
}
impl Tray {
    pub(super) fn create(state: TrayState) -> Result<Self, CreateTrayError> {
        super::UiLanguage::detect().language().set_global();
        let window = Window::create().context(WindowSnafu)?;
        let mut icon = TrayIcon::add(&window).context(IconSnafu)?;
        icon.update(state).context(StateSnafu)?;
        Ok(Self { icon, window })
    }
    pub(super) fn raw_window(&self) -> windows::Win32::Foundation::HWND {
        self.window.raw()
    }
    pub(super) fn attach_capture(&self, ingress: super::sources::SystemIngress) {
        self.window.attach_capture(ingress);
    }
    pub(super) fn update(&mut self, state: TrayState) -> Result<(), UpdateTrayStateError> {
        self.icon.update(state).context(UpdateTrayStateSnafu)
    }
    pub(super) fn commands(&self) -> Vec<TrayCommand> {
        let mut commands = Vec::new();
        for event in self.window.drain_events() {
            match event {
                CoreEvent::EndSession => commands.push(TrayCommand::EndSession),
                CoreEvent::Deadline(at) => commands.push(TrayCommand::Deadline(at)),
                CoreEvent::Quit => commands.push(TrayCommand::Exit),
                CoreEvent::OpenUi => commands.push(TrayCommand::OpenUi),
                CoreEvent::RestoreTray => {
                    if let Err(error) = self.icon.readd() {
                        tracing::error!(?error, "could not restore tray icon");
                    }
                }
                CoreEvent::ShowTrayMenu => match self.icon.menu() {
                    Ok(Some(TrayMenuItem::Open)) => commands.push(TrayCommand::OpenUi),
                    Ok(Some(TrayMenuItem::Exit)) => commands.push(TrayCommand::Exit),
                    Ok(None) => {}
                    Err(error) => tracing::error!(?error, "could not show tray menu"),
                },
            }
        }
        commands
    }
}
