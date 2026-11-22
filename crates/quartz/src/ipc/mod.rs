//! Core IPC sessions, registry and outbox actors, snapshots, and command clients.

pub(crate) mod client;
pub(crate) mod connection;
mod logical_day;
pub(crate) mod outbox;
pub(crate) mod registry;
mod session;
mod status;
pub(crate) use logical_day::{LogicalDay, RefreshLogicalDay};
pub(crate) mod summary;
mod timed;
pub(crate) use timed::{TimedCommand, TimedDeadline, TimedPolicyPort};

#[cfg(test)]
pub(crate) use crate::persistence::dispatch::WriteChannel;

pub(crate) use crate::process_id::ProcessId;
pub(crate) use session::{AllowedRoles, ConnectionSession, UiCommand};
pub(crate) use status::{PublishedStatus, StatusPublisher};

use std::sync::Arc;

/// Application services shared by transport, registry, and connection tasks.
#[derive(Clone)]
pub(crate) struct Services {
    pub summary: Option<summary::SummaryTask>,
    pub writes: crate::persistence::dispatch::WritePort,
    pub queries: crate::query::dispatch::QueryPort,
    pub versions: crate::query::versions::VersionPort,
    pub status: tokio::sync::watch::Receiver<PublishedStatus>,
    pub ui: UiPort,
    pub timed: TimedPolicyPort,
}
impl Services {
    pub(crate) fn start_registry(&self) -> registry::RegistryHandle {
        if let Some(task) = &self.summary {
            task.clone()
                .start(self.queries.clone(), self.versions.clone());
        }
        registry::RegistryHandle::start(self.status.clone(), self.versions.clone(), self.ui.clone())
    }
}

/// Bounded low-frequency UI command queue between I/O and main threads (01 §6).
pub(crate) const MAIN_COMMAND_CAPACITY: usize = 64;

/// Non-blocking delivery to the main thread; failure closes or rejects the request.
pub(crate) type UiPort = Arc<dyn Fn(UiCommand) -> Result<(), QueueUiError> + Send + Sync>;

#[derive(Debug, snafu::Snafu)]
#[snafu(display("main-thread UI queue is unavailable"))]
pub(crate) struct QueueUiError;

/// Main-thread UI queue with one coalesced, committed settings notification.
pub(crate) struct UiCommands {
    commands: crossbeam_channel::Receiver<UiCommand>,
    settings: tokio::sync::watch::Receiver<Option<quartz_protocol::EffectiveUiSettings>>,
}
impl UiCommands {
    pub(crate) fn channel(wake: UiPort) -> UiCommandChannel {
        let (sender, commands) = crossbeam_channel::bounded(MAIN_COMMAND_CAPACITY);
        let (settings_sender, settings) = tokio::sync::watch::channel(None);
        let port: UiPort = Arc::new(move |command| {
            match command {
                UiCommand::SettingsChanged(effective) => {
                    settings_sender.send_replace(Some(effective));
                }
                UiCommand::Open
                | UiCommand::FirstFrame { .. }
                | UiCommand::Registered { .. }
                | UiCommand::Unregistered { .. } => sender.try_send(command).map_err(|error| {
                    tracing::warn!(%error, "could not queue IPC command for main thread");
                    QueueUiError
                })?,
            }
            wake(command)
        });
        UiCommandChannel {
            port,
            commands: Self { commands, settings },
        }
    }
    pub(crate) fn try_recv(&self) -> Result<UiCommand, crossbeam_channel::TryRecvError> {
        self.commands.try_recv()
    }
    pub(crate) fn take_settings_change(&mut self) -> Option<quartz_protocol::EffectiveUiSettings> {
        if self
            .settings
            .has_changed()
            .expect("runtime retains settings sender through its write port")
        {
            *self.settings.borrow_and_update()
        } else {
            None
        }
    }
}
pub(crate) struct UiCommandChannel {
    pub port: UiPort,
    pub commands: UiCommands,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_notification_coalesces_without_waiting_for_a_full_ui_command_queue() {
        let mut channel = UiCommands::channel(Arc::new(|_| Ok(())));
        for _ in 0..MAIN_COMMAND_CAPACITY {
            (channel.port)(UiCommand::Open).expect("fill UI queue");
        }
        let english = quartz_protocol::EffectiveUiSettings {
            ui_language: quartz_protocol::UiLanguage::English,
            locale: None,
        };
        let chinese = quartz_protocol::EffectiveUiSettings {
            ui_language: quartz_protocol::UiLanguage::SimplifiedChinese,
            ..english
        };
        (channel.port)(UiCommand::SettingsChanged(english))
            .expect("settings bypass full command queue");
        (channel.port)(UiCommand::SettingsChanged(chinese))
            .expect("latest settings replace pending value");
        assert_eq!(channel.commands.take_settings_change(), Some(chinese));
        assert_eq!(channel.commands.take_settings_change(), None);
        assert_eq!(
            channel
                .commands
                .try_recv()
                .expect("ordinary command retained"),
            UiCommand::Open
        );
    }
}
