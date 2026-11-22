//! Shell tray state, native callbacks and semantic commands.

mod icon;
mod summary_menu;
mod timed_menu;
mod variants;
pub(super) mod window;

use icon::{AddTrayIconError, TrayIcon, TrayMenuItem, UpdateTrayTooltipError};
use snafu::{ResultExt, Snafu};
use window::{CoreEvent, CreateCoreWindowError, Window};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrayIconState {
    Normal,
    Grey,
    Private,
    Warning,
}
pub(super) use crate::tray_tooltip::TrayTooltip;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TrayState {
    pub icon: TrayIconState,
    pub tooltip: TrayTooltip,
    pub timed: quartz_protocol::TimedPolicies,
}
impl Default for TrayState {
    fn default() -> Self {
        Self {
            icon: TrayIconState::Normal,
            tooltip: TrayTooltip::Ready,
            timed: quartz_protocol::TimedPolicies::default(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrayCommand {
    CalendarChanged,
    OpenUi,
    Hover,
    Annotate(crate::core::app::annotation::AnnotationChoice),
    Timed(crate::ipc::TimedCommand),
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

#[derive(Default)]
pub(super) struct TrayContent {
    pub summary: Option<std::sync::Arc<crate::tray_summary::TraySummary>>,
    pub foreground: Option<quartz_protocol::ForegroundState>,
    pub browser_count: usize,
    pub idle: Option<crate::core::app::annotation::IdlePeriod>,
}
pub(super) struct Tray {
    // Fields drop in declaration order, removing the icon before its callback window.
    icon: TrayIcon,
    summary: Option<crate::ipc::summary::SummaryPort>,
    state: TrayState,
    window: Window,
}
impl Tray {
    pub(super) fn create(
        state: TrayState,
        language: quartz_i18n::Language,
    ) -> Result<Self, CreateTrayError> {
        let window = Window::create().context(WindowSnafu)?;
        let mut icon = TrayIcon::add(&window, language).context(IconSnafu)?;
        icon.update(state).context(StateSnafu)?;
        Ok(Self {
            icon,
            summary: None,
            state,
            window,
        })
    }
    pub(super) fn set_language(
        &mut self,
        language: quartz_protocol::UiLanguage,
    ) -> Result<(), UpdateTrayStateError> {
        let language = match language {
            quartz_protocol::UiLanguage::English => quartz_i18n::Language::English,
            quartz_protocol::UiLanguage::SimplifiedChinese => {
                quartz_i18n::Language::SimplifiedChinese
            }
        };
        if self.icon.language() == language {
            return Ok(());
        }
        self.icon.set_language(language);
        self.update(self.state)
    }
    pub(super) fn attach_summary(&mut self, summary: crate::ipc::summary::SummaryPort) {
        self.summary = Some(summary);
    }
    pub(super) fn set_content(&mut self, content: TrayContent) {
        self.icon.content = content;
    }
    pub(super) fn raw_window(&self) -> windows::Win32::Foundation::HWND {
        self.window.raw()
    }
    pub(super) fn attach_capture(&self, ingress: super::sources::SystemIngress) {
        self.window.attach_capture(ingress);
    }
    pub(super) fn update(&mut self, state: TrayState) -> Result<(), UpdateTrayStateError> {
        self.icon.update(state).context(UpdateTrayStateSnafu)?;
        self.state = state;
        Ok(())
    }
    fn refresh_summary(&mut self) {
        if let Some(summary) = &self.summary {
            summary.request();
            self.icon.content.summary = summary.latest();
        }
        if let Err(error) = self.icon.update(self.state) {
            tracing::warn!(%error, "could not refresh tray tooltip");
        }
    }
    pub(super) fn commands(&mut self) -> Vec<TrayCommand> {
        let mut commands = Vec::new();
        for event in self.window.drain_events() {
            match event {
                CoreEvent::CalendarChanged => commands.push(TrayCommand::CalendarChanged),
                CoreEvent::EndSession => commands.push(TrayCommand::EndSession),
                CoreEvent::Deadline(at) => commands.push(TrayCommand::Deadline(at)),
                CoreEvent::Quit => commands.push(TrayCommand::Exit),
                CoreEvent::OpenUi => commands.push(TrayCommand::OpenUi),
                CoreEvent::RestoreTray => {
                    if let Err(error) = self.icon.readd() {
                        tracing::error!(?error, "could not restore tray icon");
                    }
                }
                CoreEvent::Hover => {
                    self.refresh_summary();
                    commands.push(TrayCommand::Hover);
                }
                CoreEvent::ShowTrayMenu => {
                    self.refresh_summary();
                    match self.icon.menu(self.state.timed) {
                        Ok(Some(TrayMenuItem::Open | TrayMenuItem::Settings)) => {
                            commands.push(TrayCommand::OpenUi)
                        }
                        Ok(Some(TrayMenuItem::Exit)) => commands.push(TrayCommand::Exit),
                        Ok(Some(item)) if item.annotation_choice().is_some() => {
                            commands.push(TrayCommand::Annotate(
                                item.annotation_choice().expect("annotation item"),
                            ))
                        }
                        Ok(Some(item)) => commands.push(TrayCommand::Timed(
                            item.timed_command()
                                .expect("remaining menu items are timed commands"),
                        )),
                        Ok(None) => {}
                        Err(error) => tracing::error!(?error, "could not show tray menu"),
                    }
                }
            }
        }
        commands
    }
}
